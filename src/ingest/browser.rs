//! Per-crawl headless browser for `render_js` web Sources.
//!
//! spider launches Chrome without a profile directory, so every launch shares one
//! `$TMPDIR/chromiumoxide-runner` profile and a second concurrent launch fails on its lock.
//! Citadel instead launches its own Chrome with a private, temporary profile for each crawl and
//! hands spider the browser's DevTools address, so concurrent Syncs (daemon, CLI, UI) never
//! collide and a missing browser surfaces as one actionable error.

use std::time::Duration;

use spider::chromiumoxide::{Browser, BrowserConfig};
use spider::tokio_stream::StreamExt;
use tempfile::TempDir;

use crate::error::{Result, VaultError};

/// How long to wait for Chrome to start and expose its DevTools endpoint.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

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
        let config = BrowserConfig::builder()
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

fn browser_error(cause: &dyn std::fmt::Display) -> VaultError {
    tracing::warn!(error = %cause, "failed to launch headless browser for render_js");
    VaultError::Other(NO_BROWSER_ERROR.to_string())
}
