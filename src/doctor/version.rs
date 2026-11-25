//! Version checks: CLI against the latest release, daemon against the CLI.

use std::path::Path;
use std::time::Duration;

use crate::config::VaultConfig;
use crate::daemon::info::{read_daemon_info, DaemonInfo};
use crate::daemon::{daemon_status, DaemonStatus};
use crate::doctor::report::{Check, Section};
use crate::error::Result;
use crate::update::check::{
    fetch_release, is_newer_version, normalize_version_str, UpdateCheckCache,
};

const LIVE_CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// Latest release known to doctor, and where that knowledge came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatestRelease {
    Live(String),
    Cached { version: String, checked_at: String },
    Unknown,
}

/// Look up the latest release: live (3 s timeout) when `live`, else or on failure the cache.
/// Doctor is read-only: it never writes the cache.
pub async fn latest_release(
    cache_path: &Path,
    api_url: &str,
    current: &str,
    live: bool,
) -> LatestRelease {
    if live {
        let check = async {
            let client = reqwest::Client::builder()
                .connect_timeout(LIVE_CHECK_TIMEOUT)
                .user_agent(format!("citadel/{current}"))
                .build()?;
            fetch_release(&client, api_url).await
        };
        if let Ok(Ok((_, latest))) = tokio::time::timeout(LIVE_CHECK_TIMEOUT, check).await {
            return LatestRelease::Live(latest);
        }
    }
    read_cache(cache_path)
}

fn read_cache(path: &Path) -> LatestRelease {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return LatestRelease::Unknown;
    };
    let Ok(cache) = serde_json::from_str::<UpdateCheckCache>(&raw) else {
        return LatestRelease::Unknown;
    };
    let version = normalize_version_str(&cache.latest_version).to_string();
    if semver::Version::parse(&version).is_err() {
        return LatestRelease::Unknown;
    }
    LatestRelease::Cached {
        version,
        checked_at: cache.checked_at,
    }
}

/// Compare the running CLI with the latest known release.
pub fn cli_version_check(current: &str, latest: &LatestRelease) -> Check {
    let name = "cli version";
    let (version, source) = match latest {
        LatestRelease::Live(version) => (version, String::new()),
        LatestRelease::Cached {
            version,
            checked_at,
        } => {
            let day = checked_at.get(..10).unwrap_or(checked_at);
            (version, format!(" (cached, checked {day})"))
        }
        LatestRelease::Unknown => {
            return Check::info(
                Section::Version,
                name,
                format!("citadel {current}, latest version unknown (offline)"),
            );
        }
    };
    if is_newer_version(current, version) {
        Check::warn(
            Section::Version,
            name,
            format!("citadel {current}, latest: {version}{source}"),
        )
        .with_hint("citadel update")
    } else {
        Check::ok(
            Section::Version,
            name,
            format!("citadel {current}, latest: {version}{source}"),
        )
    }
}

/// What doctor knows about the daemon process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonFacts {
    pub status: DaemonStatus,
    /// `None` unless `daemon.json` describes the pid that currently holds the lock.
    pub info: Option<DaemonInfo>,
    /// Current mtime (unix secs) of the file at `info.exe`; `None` if it is gone.
    pub exe_mtime_now: Option<i64>,
    pub service_installed: bool,
}

pub fn gather_daemon_facts(config: &VaultConfig, service_installed: bool) -> Result<DaemonFacts> {
    let status = daemon_status(config)?;
    let info = match status {
        DaemonStatus::Running(pid) => {
            read_daemon_info(&config.daemon_info_path()).filter(|info| info.pid == pid)
        }
        _ => None,
    };
    let exe_mtime_now = info.as_ref().and_then(|info| {
        std::fs::metadata(&info.exe)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
    });
    Ok(DaemonFacts {
        status,
        info,
        exe_mtime_now,
        service_installed,
    })
}

/// Command that restarts the daemon.
pub fn restart_hint(service_installed: bool) -> String {
    if !service_installed {
        "citadel daemon stop && citadel daemon start".to_string()
    } else if cfg!(target_os = "macos") {
        "launchctl kickstart -k gui/$(id -u)/com.citadel.daemon".to_string()
    } else {
        "systemctl --user restart citadel.service".to_string()
    }
}

/// Compare the running daemon with this CLI. Empty unless a daemon is running.
pub fn daemon_version_checks(current: &str, facts: &DaemonFacts) -> Vec<Check> {
    if !matches!(facts.status, DaemonStatus::Running(_)) {
        return Vec::new();
    }
    let name = "daemon version";
    let hint = restart_hint(facts.service_installed);
    let Some(info) = &facts.info else {
        return vec![Check::warn(
            Section::Version,
            name,
            "daemon started by an older citadel (version unknown)",
        )
        .with_hint(&hint)];
    };
    if normalize_version_str(&info.version) != normalize_version_str(current) {
        return vec![Check::warn(
            Section::Version,
            name,
            format!("daemon runs {}, cli is {current}", info.version),
        )
        .with_hint(&hint)];
    }
    let Some(now) = facts.exe_mtime_now else {
        return vec![Check::warn(
            Section::Version,
            name,
            format!("daemon runs {}, binary no longer exists", info.version),
        )
        .with_hint(&hint)];
    };
    if info.exe_mtime_secs.is_some_and(|started| started != now) {
        return vec![Check::warn(
            Section::Version,
            name,
            format!(
                "daemon runs {}, binary replaced since the daemon started",
                info.version
            ),
        )
        .with_hint(&hint)];
    }
    vec![Check::ok(
        Section::Version,
        name,
        format!("daemon runs {}", info.version),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::info::DaemonInfoGuard;
    use crate::daemon::PidLock;
    use crate::doctor::report::CheckStatus;
    use std::path::PathBuf;

    fn info(version: &str, mtime: Option<i64>) -> DaemonInfo {
        DaemonInfo {
            pid: 5,
            version: version.to_string(),
            exe: PathBuf::from("/bin/citadel"),
            exe_mtime_secs: mtime,
            started_at: "2026-11-25T10:00:00Z".to_string(),
        }
    }

    fn facts(info: Option<DaemonInfo>, now: Option<i64>) -> DaemonFacts {
        DaemonFacts {
            status: DaemonStatus::Running(5),
            info,
            exe_mtime_now: now,
            service_installed: false,
        }
    }

    #[test]
    fn cli_up_to_date_is_ok() {
        let check = cli_version_check("0.3.0", &LatestRelease::Live("0.3.0".into()));
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.detail.contains("latest: 0.3.0"));
    }

    #[test]
    fn cli_behind_is_warn_with_update_hint() {
        let check = cli_version_check("0.3.0", &LatestRelease::Live("0.4.0".into()));
        assert_eq!(check.status, CheckStatus::Warn);
        assert_eq!(check.hint.as_deref(), Some("citadel update"));
    }

    #[test]
    fn cli_cached_says_cached() {
        let latest = LatestRelease::Cached {
            version: "0.3.0".into(),
            checked_at: "2026-11-24T10:00:00Z".into(),
        };
        let check = cli_version_check("0.3.0", &latest);
        assert!(check.detail.contains("cached, checked 2026-11-24"));
    }

    #[test]
    fn cli_unknown_is_info() {
        let check = cli_version_check("0.3.0", &LatestRelease::Unknown);
        assert_eq!(check.status, CheckStatus::Info);
        assert!(check.detail.contains("latest version unknown (offline)"));
    }

    #[tokio::test]
    async fn latest_release_falls_back_to_cache_on_http_error() {
        let app = axum::Router::new().route(
            "/latest",
            axum::routing::get(|| async { axum::http::StatusCode::FORBIDDEN }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("update-check.json");
        let content = r#"{"checked_at":"2026-11-24T10:00:00Z","latest_version":"0.3.0","release_url":"https://example.invalid/r"}"#;
        std::fs::write(&cache_path, content).unwrap();

        let started = std::time::Instant::now();
        let result =
            latest_release(&cache_path, &format!("http://{addr}/latest"), "0.3.0", true).await;
        assert!(started.elapsed() < Duration::from_millis(3500));
        assert!(matches!(result, LatestRelease::Cached { .. }));
        assert_eq!(std::fs::read_to_string(&cache_path).unwrap(), content);
    }

    #[test]
    fn daemon_same_version_same_binary_is_ok() {
        let checks =
            daemon_version_checks("0.3.0", &facts(Some(info("0.3.0", Some(100))), Some(100)));
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, CheckStatus::Ok);
    }

    #[test]
    fn daemon_other_version_warns_with_restart_hint() {
        let checks =
            daemon_version_checks("0.3.0", &facts(Some(info("0.2.0", Some(100))), Some(100)));
        assert_eq!(checks[0].status, CheckStatus::Warn);
        assert_eq!(
            checks[0].hint.as_deref(),
            Some("citadel daemon stop && citadel daemon start")
        );
    }

    #[test]
    fn daemon_binary_replaced_warns() {
        let checks =
            daemon_version_checks("0.3.0", &facts(Some(info("0.3.0", Some(100))), Some(200)));
        assert_eq!(checks[0].status, CheckStatus::Warn);
        assert!(checks[0]
            .detail
            .contains("binary replaced since the daemon started"));
    }

    #[test]
    fn daemon_binary_missing_warns() {
        let checks = daemon_version_checks("0.3.0", &facts(Some(info("0.3.0", Some(100))), None));
        assert_eq!(checks[0].status, CheckStatus::Warn);
        assert!(checks[0].detail.contains("binary no longer exists"));
    }

    #[test]
    fn daemon_without_info_warns_older_citadel() {
        let checks = daemon_version_checks("0.3.0", &facts(None, None));
        assert_eq!(checks[0].status, CheckStatus::Warn);
        assert!(checks[0].detail.contains("started by an older citadel"));
    }

    #[test]
    fn stopped_daemon_has_no_version_rows() {
        let mut f = facts(None, None);
        f.status = DaemonStatus::Stopped;
        assert!(daemon_version_checks("0.3.0", &f).is_empty());
    }

    #[test]
    fn daemon_info_with_other_pid_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let config = VaultConfig::new(dir.path());
        let _lock = PidLock::acquire(&config.pid_path()).unwrap();
        let mut stale = info("0.3.0", Some(100));
        stale.pid = 1;
        let _guard = DaemonInfoGuard::write(&config.daemon_info_path(), &stale).unwrap();

        let facts = gather_daemon_facts(&config, false).unwrap();
        assert_eq!(facts.status, DaemonStatus::Running(std::process::id()));
        assert_eq!(facts.info, None);
    }
}
