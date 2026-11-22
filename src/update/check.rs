use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::VaultConfig;
use crate::error::{Result, VaultError};

/// Default GitHub Releases latest endpoint for Citadel.
pub const DEFAULT_RELEASES_API_URL: &str =
    "https://api.github.com/repos/mberetvas/citadel/releases/latest";

/// Cache TTL for `update-check.json`: 24 hours in seconds.
pub const CACHE_TTL_SECS: i64 = 24 * 60 * 60;

/// Minimum gap between two refresh attempts, successful or not (1 hour).
pub const REFRESH_RETRY_SECS: i64 = 60 * 60;

/// Clock skew tolerated before a future-dated timestamp is treated as invalid (5 minutes).
const CLOCK_SKEW_SECS: i64 = 5 * 60;

/// On-disk schema of `<CITADEL_HOME>/update-check.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCheckCache {
    pub checked_at: String,
    pub latest_version: String,
    pub release_url: String,
}

/// Update status returned to CLI notice logic and `GET /api/version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub release_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubLatestRelease {
    tag_name: String,
    html_url: String,
}

/// Strip an optional leading `v` / `V` prefix and trim whitespace.
pub fn normalize_version_str(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed)
}

/// Return `true` if `latest` is strictly newer than `current` according to SemVer (stripping leading `v`).
pub fn is_newer_version(current: &str, latest: &str) -> bool {
    let Ok(cur) = semver::Version::parse(normalize_version_str(current)) else {
        return false;
    };
    let Ok(lat) = semver::Version::parse(normalize_version_str(latest)) else {
        return false;
    };
    lat > cur
}

/// Read `<CITADEL_HOME>/update-check.json` synchronously (0 ms network cost) and evaluate TTL.
pub fn read_cached_status(
    path: &Path,
    current_version: &str,
    now_secs: i64,
) -> (UpdateStatus, bool) {
    let current = normalize_version_str(current_version).to_string();
    let fallback = || {
        (
            UpdateStatus {
                current_version: current.clone(),
                latest_version: None,
                update_available: false,
                release_url: None,
            },
            true,
        )
    };

    let Ok(raw) = std::fs::read_to_string(path) else {
        return fallback();
    };
    let Ok(cache) = serde_json::from_str::<UpdateCheckCache>(&raw) else {
        return fallback();
    };

    let latest = normalize_version_str(&cache.latest_version).to_string();
    if semver::Version::parse(&latest).is_err() {
        return fallback();
    }

    // A timestamp from the future (written under a wrong clock) is stale, not fresh forever.
    let is_stale = parse_iso8601_utc(&cache.checked_at).is_none_or(|checked| {
        let age = now_secs.saturating_sub(checked);
        !(-CLOCK_SKEW_SECS..CACHE_TTL_SECS).contains(&age)
    });
    let update_available = is_newer_version(&current, &latest);

    (
        UpdateStatus {
            current_version: current,
            latest_version: Some(latest),
            update_available,
            release_url: Some(cache.release_url),
        },
        is_stale,
    )
}

/// Write `update-check.json` atomically via a sibling temporary file and rename.
pub fn write_cache_atomic(path: &Path, cache: &UpdateCheckCache) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let payload = serde_json::to_string_pretty(cache)
        .map_err(|err| VaultError::Other(format!("failed to serialize update cache: {err}")))?;

    // pid + per-process counter: two refreshes in one process never share a temp file.
    static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp_name = format!(
        ".update-check.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let tmp_path = path
        .parent()
        .map(|p| p.join(&tmp_name))
        .unwrap_or_else(|| PathBuf::from(&tmp_name));

    std::fs::write(&tmp_path, format!("{payload}\n"))?;
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err.into());
    }
    Ok(())
}

/// Decide whether a stale cache may start a refresh now, and record the attempt if so.
///
/// The attempt time is kept in a sibling `update-check.attempt` file, written before
/// any network call. It rate-limits refreshes whether they succeed or fail (offline,
/// GitHub rate limit) and acts as an in-flight guard for concurrent callers.
pub fn claim_refresh_attempt(cache_path: &Path, now_secs: i64) -> bool {
    static CLAIM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = CLAIM_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let marker = cache_path.with_extension("attempt");
    let last_attempt = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok());
    if let Some(last) = last_attempt {
        let age = now_secs - last;
        let recent = age < REFRESH_RETRY_SECS && age > -CLOCK_SKEW_SECS;
        if recent {
            return false;
        }
    }

    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Refuse to refresh if the attempt cannot be recorded: without it every call would retry.
    std::fs::write(&marker, format!("{now_secs}\n")).is_ok()
}

/// Fetch latest release metadata from `api_url` and write `update-check.json` atomically.
pub async fn refresh_cache(api_url: &str, path: &Path) -> Result<UpdateCheckCache> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .user_agent(format!("citadel/{}", env!("CARGO_PKG_VERSION")))
        .build()?;

    let resp = client.get(api_url).send().await?.error_for_status()?;
    let body = resp.text().await?;
    let release: GitHubLatestRelease = serde_json::from_str(&body)
        .map_err(|err| VaultError::Other(format!("invalid GitHub release JSON response: {err}")))?;

    let latest_version = normalize_version_str(&release.tag_name).to_string();
    if semver::Version::parse(&latest_version).is_err() {
        return Err(VaultError::Validation(format!(
            "release tag '{}' is not a valid semver version",
            release.tag_name
        )));
    }

    let cache = UpdateCheckCache {
        checked_at: format_iso8601_utc(current_unix_secs()),
        latest_version,
        release_url: release.html_url,
    };
    write_cache_atomic(path, &cache)?;
    Ok(cache)
}

/// Spawn a detached, non-blocking background refresh that swallows all errors silently.
pub fn spawn_background_refresh(path: PathBuf) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let _ = refresh_cache(DEFAULT_RELEASES_API_URL, &path).await;
        });
        return;
    }

    let _ = std::thread::Builder::new()
        .name("citadel-update-check".to_string())
        .spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                let _ = rt.block_on(refresh_cache(DEFAULT_RELEASES_API_URL, &path));
            }
        });
}

/// Spawn a detached child process running `__update-check-refresh` so the refresh outlives short-lived CLI commands.
pub fn spawn_detached_cli_refresh_with_url(exe: &Path, data_dir: &Path, api_url: &str) {
    let _ = std::process::Command::new(exe)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("__update-check-refresh")
        .arg("--api-url")
        .arg(api_url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Print a single-line update notice to `stderr` in interactive CLI sessions and refresh stale cache.
pub fn maybe_print_cli_update_notice(config: &VaultConfig) {
    if config.no_update_check || !std::io::stderr().is_terminal() {
        return;
    }

    let cache_path = config.update_check_path();
    let (status, is_stale) =
        read_cached_status(&cache_path, env!("CARGO_PKG_VERSION"), current_unix_secs());

    if status.update_available {
        if let Some(latest) = status.latest_version {
            eprintln!("Update available: v{latest} -> run `citadel update`");
        }
    }

    if is_stale && claim_refresh_attempt(&cache_path, current_unix_secs()) {
        if let Ok(exe) = std::env::current_exe() {
            spawn_detached_cli_refresh_with_url(&exe, &config.data_dir, DEFAULT_RELEASES_API_URL);
        }
    }
}

/// Current UTC Unix timestamp in seconds.
pub fn current_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Format Unix seconds as an ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn format_iso8601_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Parse an ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) into Unix seconds.
pub fn parse_iso8601_utc(raw: &str) -> Option<i64> {
    let s = raw.trim();
    if s.len() != 20 {
        return None;
    }
    let bytes = s.as_bytes();
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }

    let year: i64 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let minute: i64 = s[14..16].parse().ok()?;
    let second: i64 = s[17..19].parse().ok()?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return None;
    }

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Howard Hinnant's civil-from-days algorithm (days since 1970-01-01).
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    (year, m as u32, d as u32)
}

/// Howard Hinnant's days-from-civil algorithm (days since 1970-01-01).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = month as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn semver_comparison_detects_newer_release_only() {
        assert!(is_newer_version("0.1.0", "0.2.0"));
        assert!(is_newer_version("0.1.0", "v0.2.0"));
        assert!(is_newer_version("v0.1.0", "v0.1.1"));
        assert!(!is_newer_version("0.1.0", "0.1.0"));
        assert!(!is_newer_version("0.1.0", "v0.1.0"));
        assert!(!is_newer_version("0.1.0", "0.0.9"));
        assert!(!is_newer_version("0.1.0", "not-a-semver"));
    }

    #[test]
    fn refresh_attempts_are_rate_limited_even_when_they_fail() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("update-check.json");
        let t0: i64 = 1_767_225_600;

        assert!(
            claim_refresh_attempt(&path, t0),
            "first stale read starts a refresh"
        );
        // The refresh failed (offline): no cache was written, so the cache is still stale.
        assert!(read_cached_status(&path, "0.1.0", t0 + 60).1);
        assert!(
            !claim_refresh_attempt(&path, t0 + 60),
            "a recent attempt must suppress another refresh"
        );
        assert!(!claim_refresh_attempt(&path, t0 + REFRESH_RETRY_SECS - 1));
        assert!(claim_refresh_attempt(&path, t0 + REFRESH_RETRY_SECS));
        // An attempt recorded in the future (clock was ahead) does not block forever.
        assert!(claim_refresh_attempt(&path, t0));
    }

    #[test]
    fn future_dated_cache_is_stale() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("update-check.json");
        let cache = UpdateCheckCache {
            checked_at: "2030-01-01T00:00:00Z".to_string(),
            latest_version: "0.2.0".to_string(),
            release_url: "https://github.com/mberetvas/citadel/releases/tag/v0.2.0".to_string(),
        };
        std::fs::write(&path, serde_json::to_string(&cache).expect("serialize")).expect("write");
        // 2026-01-01T00:00:00Z: the cache claims to come from 4 years in the future.
        let (_, is_stale) = read_cached_status(&path, "0.1.0", 1_767_225_600);
        assert!(
            is_stale,
            "a cache written under a wrong clock must not stay fresh"
        );
    }

    #[test]
    fn concurrent_cache_writes_in_the_same_second_do_not_collide() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("update-check.json");
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let cache = UpdateCheckCache {
                        checked_at: "2026-01-01T00:00:00Z".to_string(),
                        latest_version: format!("0.{i}.0"),
                        release_url: "https://example.invalid/r".to_string(),
                    };
                    write_cache_atomic(&path, &cache)
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("join").expect("every write succeeds");
        }
        let raw = std::fs::read_to_string(&path).expect("read");
        serde_json::from_str::<UpdateCheckCache>(&raw).expect("final cache is valid JSON");
    }

    #[test]
    fn cache_ttl_marks_fresh_within_24h_and_stale_after_24h() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("update-check.json");
        // 2026-01-01T00:00:00Z == 1767225600
        let base_secs: i64 = 1_767_225_600;
        let cache = UpdateCheckCache {
            checked_at: "2026-01-01T00:00:00Z".to_string(),
            latest_version: "0.2.0".to_string(),
            release_url: "https://github.com/mberetvas/citadel/releases/tag/v0.2.0".to_string(),
        };
        std::fs::write(&path, serde_json::to_string(&cache).expect("serialize")).expect("write");

        let (status_fresh, is_stale_fresh) = read_cached_status(&path, "0.1.0", base_secs + 3600);
        assert!(!is_stale_fresh);
        assert!(status_fresh.update_available);
        assert_eq!(status_fresh.latest_version.as_deref(), Some("0.2.0"));

        let (status_stale, is_stale_after) =
            read_cached_status(&path, "0.1.0", base_secs + CACHE_TTL_SECS + 1);
        assert!(is_stale_after);
        assert!(status_stale.update_available);
    }

    #[test]
    fn missing_or_corrupt_cache_degrades_silently_as_stale() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist.json");

        let (status, is_stale) = read_cached_status(&missing, "0.1.0", 1_767_225_600);
        assert!(is_stale);
        assert!(!status.update_available);
        assert_eq!(status.latest_version, None);

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, "{ not valid json").expect("write corrupt");
        let (status2, is_stale2) = read_cached_status(&corrupt, "0.1.0", 1_767_225_600);
        assert!(is_stale2);
        assert!(!status2.update_available);
        assert_eq!(status2.latest_version, None);
    }

    #[tokio::test]
    async fn refresh_cache_writes_atomic_json_from_mock_github_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let app = axum::Router::new().route(
            "/releases/latest",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "tag_name": "v0.2.0",
                    "html_url": "https://github.com/mberetvas/citadel/releases/tag/v0.2.0"
                }))
            }),
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let dir = tempdir().expect("tempdir");
        let cache_path = dir.path().join("nested/update-check.json");
        let api_url = format!("http://{addr}/releases/latest");

        let written = refresh_cache(&api_url, &cache_path)
            .await
            .expect("refresh_cache should succeed");
        server.abort();

        assert_eq!(written.latest_version, "0.2.0");
        assert_eq!(
            written.release_url,
            "https://github.com/mberetvas/citadel/releases/tag/v0.2.0"
        );
        assert!(cache_path.is_file());
        let parsed: UpdateCheckCache =
            serde_json::from_str(&std::fs::read_to_string(&cache_path).expect("read cache"))
                .expect("parse written cache");
        assert_eq!(parsed, written);
    }
}
