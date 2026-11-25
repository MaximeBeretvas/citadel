//! `daemon.json`: metadata about the running daemon, read by `citadel doctor`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, VaultError};
use crate::update::check::format_iso8601_utc;

/// Facts about a running daemon process, recorded next to `daemon.pid`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub version: String,
    pub exe: PathBuf,
    pub exe_mtime_secs: Option<i64>,
    pub started_at: String,
}

impl DaemonInfo {
    /// Describe the current process.
    pub fn for_current_process() -> Result<Self> {
        let exe = std::env::current_exe()?;
        let exe_mtime_secs = std::fs::metadata(&exe)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0))
            .unwrap_or(0);
        Ok(Self {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            exe,
            exe_mtime_secs,
            started_at: format_iso8601_utc(now),
        })
    }
}

/// Read `daemon.json`; a missing or unparsable file yields `None`.
pub fn read_daemon_info(path: &Path) -> Option<DaemonInfo> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Owns `daemon.json` for the daemon's lifetime; removes it on drop.
#[derive(Debug)]
pub struct DaemonInfoGuard {
    path: PathBuf,
}

impl DaemonInfoGuard {
    /// Write `info` atomically (temp file + rename).
    pub fn write(path: &Path, info: &DaemonInfo) -> Result<Self> {
        let json = serde_json::to_string_pretty(info)
            .map_err(|err| VaultError::Other(format!("failed to encode daemon info: {err}")))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        if let Err(err) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(err.into());
        }
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for DaemonInfoGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::PidLock;
    use std::fs;

    fn sample() -> DaemonInfo {
        DaemonInfo {
            pid: 42,
            version: "9.9.9".into(),
            exe: PathBuf::from("/x/citadel"),
            exe_mtime_secs: Some(100),
            started_at: "2026-11-25T00:00:00Z".into(),
        }
    }

    #[test]
    fn daemon_info_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        let _guard = DaemonInfoGuard::write(&path, &sample()).unwrap();
        assert_eq!(read_daemon_info(&path), Some(sample()));
    }

    #[test]
    fn guard_drop_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        let guard = DaemonInfoGuard::write(&path, &sample()).unwrap();
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn read_daemon_info_returns_none_for_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        fs::write(&path, "37556\n").unwrap();
        assert_eq!(read_daemon_info(&path), None);
        assert_eq!(read_daemon_info(&dir.path().join("missing.json")), None);
    }

    #[test]
    fn for_current_process_describes_this_process() {
        let info = DaemonInfo::for_current_process().unwrap();
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert!(info.exe_mtime_secs.is_some());
    }

    #[test]
    fn pid_file_format_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("daemon.pid");
        let _lock = PidLock::acquire(&pid_path).unwrap();
        let contents = fs::read_to_string(&pid_path).unwrap();
        assert_eq!(contents.trim(), std::process::id().to_string());
    }
}
