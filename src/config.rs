use std::path::PathBuf;

use crate::error::{Result, VaultError};

/// Resolved filesystem paths for a `web-vault` data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultConfig {
    pub data_dir: PathBuf,
}

impl VaultConfig {
    /// Construct a configuration pointing directly at `data_dir`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// Resolve configuration using `--data-dir` > `WEB_VAULT_HOME` > OS data dir.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        let env_val = std::env::var("WEB_VAULT_HOME").ok();
        Self::resolve_with_env(explicit, env_val.as_deref())
    }

    /// Resolve configuration with an explicit optional `WEB_VAULT_HOME` value.
    pub fn resolve_with_env(explicit: Option<PathBuf>, env_val: Option<&str>) -> Result<Self> {
        if let Some(dir) = explicit.filter(|p| !p.as_os_str().is_empty()) {
            return Ok(Self { data_dir: dir });
        }

        if let Some(val) = env_val.map(str::trim).filter(|s| !s.is_empty()) {
            return Ok(Self {
                data_dir: PathBuf::from(val),
            });
        }

        let dirs =
            directories::ProjectDirs::from("com", "webvault", "web-vault").ok_or_else(|| {
                VaultError::Config(
                    "could not determine OS data directory; pass --data-dir or set WEB_VAULT_HOME"
                        .to_string(),
                )
            })?;

        Ok(Self {
            data_dir: dirs.data_dir().to_path_buf(),
        })
    }

    /// Path to the primary SQLite database (`vault.db`).
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("vault.db")
    }

    /// Path to the daemon PID lockfile (`daemon.pid`).
    pub fn pid_path(&self) -> PathBuf {
        self.data_dir.join("daemon.pid")
    }

    /// Path to the daemon log file (`daemon.log`).
    pub fn log_path(&self) -> PathBuf {
        self.data_dir.join("daemon.log")
    }

    /// Create the data directory if it does not already exist.
    pub fn ensure_data_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_explicit_data_dir_over_env() {
        let cfg = VaultConfig::resolve_with_env(
            Some(PathBuf::from("/tmp/explicit-vault")),
            Some("/tmp/env-vault"),
        )
        .expect("explicit path should resolve");

        assert_eq!(cfg.data_dir, PathBuf::from("/tmp/explicit-vault"));
    }

    #[test]
    fn resolves_env_var_when_flag_absent() {
        let cfg = VaultConfig::resolve_with_env(None, Some("/tmp/env-vault"))
            .expect("env path should resolve");

        assert_eq!(cfg.data_dir, PathBuf::from("/tmp/env-vault"));
    }

    #[test]
    fn derives_vault_db_and_pid_paths() {
        let cfg = VaultConfig::resolve_with_env(Some(PathBuf::from("/var/lib/web-vault")), None)
            .expect("config should resolve");

        assert_eq!(cfg.db_path(), PathBuf::from("/var/lib/web-vault/vault.db"));
        assert_eq!(
            cfg.pid_path(),
            PathBuf::from("/var/lib/web-vault/daemon.pid")
        );
        assert_eq!(
            cfg.log_path(),
            PathBuf::from("/var/lib/web-vault/daemon.log")
        );
    }

    #[test]
    fn falls_back_to_os_data_dir_when_overrides_absent_or_blank() {
        if let Some(expected) = directories::ProjectDirs::from("com", "webvault", "web-vault") {
            let cfg = VaultConfig::resolve_with_env(None, Some("   "))
                .expect("OS fallback should resolve");
            assert_eq!(cfg.data_dir, expected.data_dir());
        }
    }

    #[test]
    fn ensure_data_dir_creates_nested_directory() {
        let tmp = tempfile::tempdir().expect("tempdir should succeed");
        let nested = tmp.path().join("nested").join("vault");
        let cfg = VaultConfig::new(&nested);

        assert!(!nested.exists());
        cfg.ensure_data_dir()
            .expect("ensure_data_dir should create directory");
        assert!(nested.is_dir());
    }
}
