use std::path::PathBuf;

use crate::error::{Result, VaultError};

/// Resolved filesystem paths for a `citadel` data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultConfig {
    pub data_dir: PathBuf,
    /// Allow Sources whose Origin is a Private origin (`CITADEL_ALLOW_PRIVATE_ORIGINS`).
    pub allow_private_origins: bool,
    /// Disable automatic background update checks (`CITADEL_NO_UPDATE_CHECK`).
    pub no_update_check: bool,
    /// GitHub Releases API endpoint used by update checks and `citadel update`.
    pub update_api_url: String,
}

/// Environment variable that allows Private origins when set to `1` or `true`.
pub const ALLOW_PRIVATE_ORIGINS_ENV: &str = "CITADEL_ALLOW_PRIVATE_ORIGINS";

/// Environment variable that disables automatic update checks when set to `1` or `true`.
pub const NO_UPDATE_CHECK_ENV: &str = "CITADEL_NO_UPDATE_CHECK";

impl VaultConfig {
    /// Construct a configuration pointing directly at `data_dir`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            allow_private_origins: false,
            no_update_check: false,
            update_api_url: crate::update::check::DEFAULT_RELEASES_API_URL.to_string(),
        }
    }

    /// Return this configuration with Private origins allowed.
    pub fn allowing_private_origins(mut self) -> Self {
        self.allow_private_origins = true;
        self
    }

    /// Resolve configuration using `--data-dir` > `CITADEL_HOME` > OS data dir.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        let env_val = std::env::var("CITADEL_HOME").ok();
        let mut config = Self::resolve_with_env(explicit, env_val.as_deref())?;
        config.allow_private_origins =
            parse_flag(std::env::var(ALLOW_PRIVATE_ORIGINS_ENV).ok().as_deref());
        config.no_update_check = parse_flag(std::env::var(NO_UPDATE_CHECK_ENV).ok().as_deref());
        Ok(config)
    }

    /// Resolve configuration with an explicit optional `CITADEL_HOME` value.
    pub fn resolve_with_env(explicit: Option<PathBuf>, env_val: Option<&str>) -> Result<Self> {
        if let Some(dir) = explicit.filter(|p| !p.as_os_str().is_empty()) {
            return Ok(Self::new(dir));
        }

        if let Some(val) = env_val.map(str::trim).filter(|s| !s.is_empty()) {
            return Ok(Self::new(val));
        }

        let dirs =
            directories::ProjectDirs::from("com", "citadel", "citadel").ok_or_else(|| {
                VaultError::Config(
                    "could not determine OS data directory; pass --data-dir or set CITADEL_HOME"
                        .to_string(),
                )
            })?;

        Ok(Self::new(dirs.data_dir()))
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

    /// Path to the cached release update check file (`update-check.json`).
    pub fn update_check_path(&self) -> PathBuf {
        self.data_dir.join("update-check.json")
    }

    /// Create the data directory if it does not already exist.
    pub fn ensure_data_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        Ok(())
    }
}

/// Parse a boolean environment flag: `1` or `true` (any case) enables it.
pub(crate) fn parse_flag(raw: Option<&str>) -> bool {
    raw.map(str::trim)
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_no_update_check_env_and_derives_cache_path() {
        let cfg = VaultConfig::new("/var/lib/citadel");
        assert!(!cfg.no_update_check);
        assert_eq!(
            cfg.update_check_path(),
            PathBuf::from("/var/lib/citadel/update-check.json")
        );
        assert_eq!(NO_UPDATE_CHECK_ENV, "CITADEL_NO_UPDATE_CHECK");
        assert!(parse_flag(Some("1")));
        assert!(parse_flag(Some("true")));
        assert!(!parse_flag(Some("0")));
    }

    #[test]
    fn parses_allow_private_origins_flag() {
        assert!(parse_flag(Some("1")));
        assert!(parse_flag(Some("TRUE")));
        assert!(!parse_flag(Some("0")));
        assert!(!parse_flag(Some("yes")));
        assert!(!parse_flag(None));
        assert!(!VaultConfig::new("/tmp/x").allow_private_origins);
    }

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
        let cfg = VaultConfig::resolve_with_env(Some(PathBuf::from("/var/lib/citadel")), None)
            .expect("config should resolve");

        assert_eq!(cfg.db_path(), PathBuf::from("/var/lib/citadel/vault.db"));
        assert_eq!(cfg.pid_path(), PathBuf::from("/var/lib/citadel/daemon.pid"));
        assert_eq!(cfg.log_path(), PathBuf::from("/var/lib/citadel/daemon.log"));
    }

    #[test]
    fn falls_back_to_os_data_dir_when_overrides_absent_or_blank() {
        if let Some(expected) = directories::ProjectDirs::from("com", "citadel", "citadel") {
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
