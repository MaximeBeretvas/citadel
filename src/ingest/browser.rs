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

use spider::chromiumoxide::error::CdpError;
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

/// Cap on each step of closing the browser (close request, kill, process exit).
const SHUTDOWN_STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// Environment variable spider reads to reach an already-running browser.
pub(crate) const CHROME_URL_ENV: &str = "CHROME_URL";

/// Environment variable that launches Chrome without its sandbox when set to `1` or `true`.
/// Explicit opt-out only: Citadel never drops the sandbox on its own outside a container.
pub(crate) const CHROME_NO_SANDBOX_ENV: &str = "CITADEL_CHROME_NO_SANDBOX";

pub(crate) const NO_BROWSER_ERROR: &str = "render_js needs a Chrome or Chromium browser, but none could be launched or reached: install Google Chrome or Chromium, or set CHROME_URL to a running instance (e.g. http://127.0.0.1:9222)";

/// Chrome's own guide to the AppArmor restriction on unprivileged user namespaces.
const APPARMOR_USERNS_DOC: &str = "https://chromium.googlesource.com/chromium/src/+/main/docs/security/apparmor-userns-restrictions.md";

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
    /// browser executable is installed or it fails to start. A start that fails because Chrome's
    /// sandbox is unavailable (AppArmor, root) returns an error naming those fixes instead.
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
        let in_container = crate::update::is_running_in_container();
        let opt_out = std::env::var(CHROME_NO_SANDBOX_ENV).ok();
        if !chrome_sandbox_enabled(in_container, opt_out.as_deref()) {
            if !in_container {
                tracing::warn!(
                    "{CHROME_NO_SANDBOX_ENV} is set: launching Chrome for render_js without its sandbox"
                );
            }
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
            // Keep driving the connection through ordinary protocol errors: once this loop ends
            // no command (including the final close) is ever answered. Only a dead connection
            // or browser process ends it.
            while let Some(event) = handler.next().await {
                if let Err(
                    CdpError::Ws(_)
                    | CdpError::LaunchExit(_, _)
                    | CdpError::LaunchTimeout(_)
                    | CdpError::LaunchIo(_, _),
                ) = event
                {
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
        // Every step is bounded: an unresponsive browser must not hang the Sync. Dropping
        // `browser` afterwards kills the process if it is somehow still alive.
        let closed = tokio::time::timeout(SHUTDOWN_STEP_TIMEOUT, self.browser.close()).await;
        if !matches!(closed, Ok(Ok(_))) {
            let _ = tokio::time::timeout(SHUTDOWN_STEP_TIMEOUT, self.browser.kill()).await;
        }
        let _ = tokio::time::timeout(SHUTDOWN_STEP_TIMEOUT, self.browser.wait()).await;
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

/// Whether Chrome keeps its own sandbox. Native installs do unless the user opts out through
/// [`CHROME_NO_SANDBOX_ENV`] (`no_sandbox_env` is its raw value); inside an OCI container
/// Chrome's sandbox cannot start as a non-root user (no user namespaces), and the container is
/// the isolation boundary instead.
fn chrome_sandbox_enabled(in_container: bool, no_sandbox_env: Option<&str>) -> bool {
    !in_container && !crate::config::parse_flag(no_sandbox_env)
}

/// For a launch that failed because Chrome's sandbox could not start: Chrome's own phrase for
/// the failure and the fixes. `None` when the failure has another cause. Matched on the whole
/// cause: chromey puts Chrome's stderr on the same line, after the exit status.
fn sandbox_failure(cause: &str) -> Option<(&'static str, String)> {
    if cause.contains("No usable sandbox") {
        Some((
            "No usable sandbox!",
            format!(
                "render_js could not start Chrome because its sandbox is unavailable (Ubuntu 23.10+ and other distros restrict unprivileged user namespaces through AppArmor). Fix: give this browser an AppArmor profile or allow user namespaces ({APPARMOR_USERNS_DOC}), use Google Chrome's .deb or the Chromium snap (both ship a profile), set {CHROME_URL_ENV} to a running instance, or set {CHROME_NO_SANDBOX_ENV}=1 to run Chrome without its sandbox (less isolation from crawled pages)"
            ),
        ))
    } else if cause.contains("Running as root without --no-sandbox") {
        Some((
            "Running as root without --no-sandbox is not supported",
            format!(
                "render_js could not start Chrome because Chrome refuses to run as root with its sandbox. Fix: run Citadel as a non-root user, set {CHROME_URL_ENV} to a running instance, or set {CHROME_NO_SANDBOX_ENV}=1 to run Chrome without its sandbox (less isolation from crawled pages)"
            ),
        ))
    } else {
        None
    }
}

fn browser_error(cause: &dyn std::fmt::Display) -> VaultError {
    let cause = cause.to_string();
    tracing::warn!(error = %cause, "failed to launch headless browser for render_js");
    // A sandbox failure keeps only Chrome's phrase as the cause: the full launch output (exit
    // status, Chrome's stderr with its own copy of the AppArmor link) stays in the log above.
    if let Some((phrase, hint)) = sandbox_failure(&cause) {
        return VaultError::Other(format!("{hint} (cause: {phrase})"));
    }
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

    use super::{
        browser_error, chrome_sandbox_enabled, find_browser_on_path, CHROME_NO_SANDBOX_ENV,
    };

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
    fn chrome_sandbox_is_kept_natively_unless_opted_out_and_dropped_in_containers() {
        for (in_container, opt_out, expected) in [
            (false, None, true),
            (false, Some("0"), true),
            (false, Some(""), true),
            (false, Some("1"), false),
            (false, Some("true"), false),
            (true, None, false),
            (true, Some("1"), false),
        ] {
            assert_eq!(
                chrome_sandbox_enabled(in_container, opt_out),
                expected,
                "in_container={in_container} {CHROME_NO_SANDBOX_ENV}={opt_out:?}"
            );
        }
    }

    /// How chromey reports a browser that exits during launch: stderr is `{:?}`-formatted, so
    /// Chrome's whole output sits on one line with escaped newlines.
    fn launch_exit_cause(stderr: &str) -> String {
        format!(
            "Browser process exited with status ExitStatus(unix_wait_status(1280)) before websocket URL could be resolved, stderr: BrowserStderr({stderr:?})"
        )
    }

    #[test]
    fn sandbox_launch_error_names_the_fixes() {
        let cause = launch_exit_cause(
            "[4821:4821:1123/101010.123456:FATAL:zygote_host_impl_linux.cc:144] No usable sandbox! If you are running on Ubuntu 23.10+ or another Linux distro that has disabled unprivileged user namespaces with AppArmor, see https://chromium.googlesource.com/chromium/src/+/main/docs/security/apparmor-userns-restrictions.md\n",
        );
        let message = browser_error(&cause).to_string();
        assert!(message.contains("CITADEL_CHROME_NO_SANDBOX=1"), "{message}");
        assert_eq!(
            message.matches("apparmor-userns-restrictions.md").count(),
            1,
            "the AppArmor guide once, not again from Chrome's stderr: {message}"
        );
        assert!(message.contains("CHROME_URL"), "{message}");
        assert!(message.contains("(cause: No usable sandbox!)"), "{message}");
        assert!(
            !message.contains("Browser process exited"),
            "the hint replaces chromey's raw launch output: {message}"
        );
        assert!(
            !message.contains("install Google Chrome"),
            "the browser is installed; do not tell the user to install it: {message}"
        );
    }

    #[test]
    fn root_launch_error_names_the_fixes() {
        let cause = launch_exit_cause(
            "[77:77:1123/101010.123456:ERROR:zygote_host_impl_linux.cc:100] Running as root without --no-sandbox is not supported. See https://crbug.com/638180.\n",
        );
        let message = browser_error(&cause).to_string();
        assert!(message.contains("non-root"), "{message}");
        assert!(message.contains("CITADEL_CHROME_NO_SANDBOX=1"), "{message}");
        assert!(
            message.contains("(cause: Running as root without --no-sandbox is not supported)"),
            "{message}"
        );
        assert!(!message.contains("Browser process exited"), "{message}");
        assert!(!message.contains("install Google Chrome"), "{message}");
    }

    #[test]
    fn launch_error_keeps_the_fix_hint_and_the_cause() {
        let message =
            browser_error(&"Could not auto detect a chrome executable\nmore detail").to_string();
        assert!(message.contains("CHROME_URL"), "{message}");
        assert!(message.contains("install Google Chrome"), "{message}");
        assert!(!message.contains(CHROME_NO_SANDBOX_ENV), "{message}");
        assert!(
            message.contains("Could not auto detect a chrome executable"),
            "{message}"
        );
        assert!(
            !message.contains("more detail"),
            "only the first line of the cause: {message}"
        );
    }
}
