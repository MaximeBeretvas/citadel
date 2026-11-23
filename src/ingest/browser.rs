//! Per-crawl headless browser for `render_js` web Sources.
//!
//! spider launches Chrome without a profile directory, so every launch shares one
//! `$TMPDIR/chromiumoxide-runner` profile and a second concurrent launch fails on its lock.
//! Citadel instead launches its own Chrome with a private, temporary profile for each crawl and
//! hands spider the browser's DevTools address, so concurrent Syncs (daemon, CLI, UI) never
//! collide and a missing browser surfaces as one actionable error.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use spider::chromiumoxide::{Browser, BrowserConfig};
use spider::tokio_stream::StreamExt;
use tempfile::TempDir;

use crate::error::{Result, VaultError};

/// How long to wait for Chrome to start and expose its DevTools endpoint.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Browser executable names searched on `PATH`, in order. chromey (as built by spider) only
/// checks the `CHROME` env var and a few fixed install paths, so Debian's `/usr/bin/chromium`
/// (container image, apt installs) would otherwise never be found.
const BROWSER_NAMES: &[&str] = &[
    "chromium",
    "chromium-browser",
    "google-chrome-stable",
    "google-chrome",
    "chrome",
];

/// Environment variable spider reads to reach an already-running browser.
pub(crate) const CHROME_URL_ENV: &str = "CHROME_URL";

pub(crate) const NO_BROWSER_ERROR: &str = "render_js needs a Chrome or Chromium browser, but none could be launched or reached: install Google Chrome or Chromium, or set CHROME_URL to a running instance (e.g. http://127.0.0.1:9222)";

/// A headless Chrome owned by one crawl. Close it with [`RenderBrowser::shutdown`]; dropping it
/// without shutting down still stops the event loop and removes the profile directory.
pub(crate) struct RenderBrowser {
    browser: Browser,
    handler: tokio::task::JoinHandle<()>,
    // Dropped last: Chrome must be gone before its profile directory is deleted.
    _profile: TempDir,
}

impl RenderBrowser {
    /// Launch a headless Chrome with a private profile, or return [`NO_BROWSER_ERROR`] when no
    /// browser executable is installed or it fails to start.
    pub(crate) async fn launch() -> Result<Self> {
        let profile = tempfile::Builder::new()
            .prefix("citadel-chrome-")
            .tempdir()
            .map_err(VaultError::Io)?;
        let mut builder = BrowserConfig::builder();
        // An explicit `CHROME` path is honoured by chromey itself; otherwise search `PATH`.
        if std::env::var_os("CHROME").is_none() {
            if let Some(executable) = find_browser_on_path(std::env::var_os("PATH").as_deref()) {
                builder = builder.chrome_executable(executable);
            }
        }
        if !chrome_sandbox_enabled(crate::update::is_running_in_container()) {
            builder = builder.no_sandbox();
        }
        let config = builder
            .user_data_dir(profile.path())
            .launch_timeout(LAUNCH_TIMEOUT)
            .build()
            .map_err(|err| browser_error(&err))?;
        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|err| browser_error(&err))?;
        let handler = tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if event.is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            browser,
            handler,
            _profile: profile,
        })
    }

    /// DevTools WebSocket address that spider connects to.
    pub(crate) fn websocket_url(&self) -> String {
        self.browser.websocket_address().clone()
    }

    /// Close Chrome and wait for its process to exit.
    pub(crate) async fn shutdown(mut self) {
        if self.browser.close().await.is_err() {
            let _ = self.browser.kill().await;
        }
        let _ = self.browser.wait().await;
        self.handler.abort();
    }
}

impl Drop for RenderBrowser {
    fn drop(&mut self) {
        self.handler.abort();
    }
}

/// First executable from [`BROWSER_NAMES`] found on the given `PATH` value.
fn find_browser_on_path(path: Option<&OsStr>) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = std::env::split_paths(path?).collect();
    BROWSER_NAMES.iter().find_map(|name| {
        dirs.iter()
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    })
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Whether Chrome keeps its own sandbox. Native installs always do; inside an OCI container
/// Chrome's sandbox cannot start as a non-root user (no user namespaces), and the container is
/// the isolation boundary instead.
fn chrome_sandbox_enabled(in_container: bool) -> bool {
    !in_container
}

fn browser_error(cause: &dyn std::fmt::Display) -> VaultError {
    let cause = cause.to_string();
    tracing::warn!(error = %cause, "failed to launch headless browser for render_js");
    let summary = cause
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches(|c: char| !c.is_alphanumeric());
    VaultError::Other(format!("{NO_BROWSER_ERROR} (cause: {summary})"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::{browser_error, chrome_sandbox_enabled, find_browser_on_path};

    fn fake_executable(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\n").expect("write fake browser");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[test]
    fn finds_debian_chromium_on_path() {
        let empty = tempfile::tempdir().expect("tempdir");
        let bin = tempfile::tempdir().expect("tempdir");
        let chromium = fake_executable(bin.path(), "chromium");
        let path = std::env::join_paths([empty.path(), bin.path()]).expect("join");

        assert_eq!(find_browser_on_path(Some(&path)), Some(chromium));
    }

    #[test]
    fn ignores_non_executable_files_and_missing_path() {
        let bin = tempfile::tempdir().expect("tempdir");
        fs::write(bin.path().join("google-chrome"), "not executable").expect("write");
        let path = std::env::join_paths([bin.path()]).expect("join");

        assert_eq!(find_browser_on_path(Some(&path)), None);
        assert_eq!(find_browser_on_path(None), None);
    }

    #[test]
    fn chrome_sandbox_is_kept_natively_and_dropped_only_in_containers() {
        assert!(chrome_sandbox_enabled(false));
        assert!(!chrome_sandbox_enabled(true));
    }

    #[test]
    fn launch_error_keeps_the_fix_hint_and_the_cause() {
        let message = browser_error(&"No usable sandbox!\nmore detail").to_string();
        assert!(message.contains("CHROME_URL"), "{message}");
        assert!(message.contains("No usable sandbox!"), "{message}");
        assert!(
            !message.contains("more detail"),
            "only the first line of the cause: {message}"
        );
    }
}
