//! Running checks: daemon state and the OS background service.

use std::path::{Path, PathBuf};

use super::report::{Check, Section};
use crate::daemon::DaemonStatus;

/// Parsed fields of an installed OS service definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceFile {
    pub path: PathBuf,
    pub binary: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

/// State of the OS background service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    Unsupported,
    NotInstalled,
    Installed { file: ServiceFile, loaded: bool },
}

/// Map the daemon pid-file status to a check row.
pub fn daemon_running_check(status: DaemonStatus, pid_path: &Path) -> Check {
    match status {
        DaemonStatus::Running(pid) => Check::ok(
            Section::Running,
            "daemon",
            format!("daemon running (pid {pid})"),
        ),
        DaemonStatus::Stopped => Check::info(Section::Running, "daemon", "daemon not running")
            .with_hint("citadel daemon start"),
        DaemonStatus::Stale => Check::warn(
            Section::Running,
            "daemon",
            format!(
                "stale daemon.pid at {}; no daemon holds it",
                pid_path.display()
            ),
        )
        .with_hint("citadel daemon start"),
    }
}

fn xml_unescape(input: &str) -> String {
    input
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The `<string>` value that follows `<key>{key}</key>`, XML-unescaped.
fn plist_string_after(text: &str, key: &str) -> Option<String> {
    let marker = format!("<key>{key}</key>");
    let rest = &text[text.find(&marker)? + marker.len()..];
    let rest = &rest[rest.find("<string>")? + "<string>".len()..];
    let value = &rest[..rest.find("</string>")?];
    Some(xml_unescape(value))
}

/// Extract `(binary, CITADEL_HOME)` from a launchd plist.
pub fn parse_launchd_plist(text: &str) -> (Option<PathBuf>, Option<PathBuf>) {
    (
        plist_string_after(text, "ProgramArguments").map(PathBuf::from),
        plist_string_after(text, "CITADEL_HOME").map(PathBuf::from),
    )
}

fn systemd_unescape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => out.push(next),
                None => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// First token of an `ExecStart=` value; supports the double-quoted form.
fn first_exec_token(value: &str) -> String {
    let value = value.trim();
    let Some(quoted) = value.strip_prefix('"') else {
        return value.split_whitespace().next().unwrap_or("").to_string();
    };
    let mut raw = String::new();
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => {
                raw.push(c);
                if let Some(next) = chars.next() {
                    raw.push(next);
                }
            }
            _ => raw.push(c),
        }
    }
    systemd_unescape(&raw)
}

/// Extract `(binary, CITADEL_HOME)` from a systemd user unit.
pub fn parse_systemd_unit(text: &str) -> (Option<PathBuf>, Option<PathBuf>) {
    let mut binary = None;
    let mut data_dir = None;
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("ExecStart=") {
            let token = first_exec_token(value);
            if !token.is_empty() {
                binary = Some(PathBuf::from(token));
            }
        } else if let Some(value) = line.strip_prefix("Environment=\"CITADEL_HOME=") {
            if let Some(value) = value.strip_suffix('"') {
                data_dir = Some(PathBuf::from(systemd_unescape(value)));
            }
        }
    }
    (binary, data_dir)
}

/// Read the service file and ask the OS whether the service is loaded. Read-only.
pub fn gather_service_state() -> ServiceState {
    let path = match crate::daemon::service_file_path() {
        Ok(Some(path)) => path,
        Ok(None) => return ServiceState::Unsupported,
        Err(_) => return ServiceState::NotInstalled,
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) if !path.exists() => return ServiceState::NotInstalled,
        Err(_) => String::new(),
    };
    let (binary, data_dir) = if cfg!(target_os = "macos") {
        parse_launchd_plist(&text)
    } else {
        parse_systemd_unit(&text)
    };
    ServiceState::Installed {
        file: ServiceFile {
            path,
            binary,
            data_dir,
        },
        loaded: service_is_loaded(),
    }
}

#[cfg(target_os = "macos")]
fn service_is_loaded() -> bool {
    std::process::Command::new("launchctl")
        .args(["list", crate::daemon::LAUNCHD_LABEL])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn service_is_loaded() -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "is-active", crate::daemon::SYSTEMD_SERVICE_NAME])
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
        .unwrap_or(false)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn service_is_loaded() -> bool {
    false
}

fn same_path(a: &Path, b: &Path) -> bool {
    let a = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let b = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    a == b
}

/// Turn the gathered service state into check rows.
pub fn service_checks(state: &ServiceState, cli_data_dir: &Path, cli_exe: &Path) -> Vec<Check> {
    const NAME: &str = "service";
    match state {
        ServiceState::Unsupported => vec![Check::info(
            Section::Running,
            NAME,
            "not supported on this platform",
        )],
        ServiceState::NotInstalled => {
            vec![
                Check::info(Section::Running, NAME, "background service not installed")
                    .with_hint("citadel daemon install"),
            ]
        }
        ServiceState::Installed { file, loaded } => {
            let mut checks = Vec::new();
            if *loaded {
                checks.push(Check::ok(
                    Section::Running,
                    NAME,
                    format!("installed and loaded ({})", file.path.display()),
                ));
            } else {
                checks.push(
                    Check::warn(
                        Section::Running,
                        NAME,
                        format!(
                            "service file present but not loaded ({})",
                            file.path.display()
                        ),
                    )
                    .with_hint("citadel daemon install"),
                );
            }
            if let Some(dir) = &file.data_dir {
                if !same_path(dir, cli_data_dir) {
                    checks.push(
                        Check::warn(
                            Section::Running,
                            "service data dir",
                            format!(
                                "service uses data dir {}; this CLI uses {}",
                                dir.display(),
                                cli_data_dir.display()
                            ),
                        )
                        .with_hint("citadel daemon install"),
                    );
                }
            }
            if let Some(bin) = &file.binary {
                if !same_path(bin, cli_exe) {
                    checks.push(
                        Check::warn(
                            Section::Running,
                            "service binary",
                            format!(
                                "service runs {}; this CLI is {}",
                                bin.display(),
                                cli_exe.display()
                            ),
                        )
                        .with_hint("citadel daemon install"),
                    );
                }
            }
            checks
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{generate_launchd_plist, generate_systemd_unit};
    use crate::doctor::report::CheckStatus;

    fn p(s: &str) -> Option<PathBuf> {
        Some(PathBuf::from(s))
    }

    fn installed(binary: &str, data_dir: &str, loaded: bool) -> ServiceState {
        ServiceState::Installed {
            file: ServiceFile {
                path: PathBuf::from("/svc/file"),
                binary: p(binary),
                data_dir: p(data_dir),
            },
            loaded,
        }
    }

    #[test]
    fn plist_round_trips_through_generator() {
        let text = generate_launchd_plist(Path::new("/a & b/citadel"), Path::new("/data"));
        assert_eq!(
            parse_launchd_plist(&text),
            (p("/a & b/citadel"), p("/data"))
        );
    }

    #[test]
    fn unit_round_trips_through_generator() {
        let text = generate_systemd_unit(Path::new("/opt/my citadel/citadel"), Path::new("/data"));
        assert_eq!(
            parse_systemd_unit(&text),
            (p("/opt/my citadel/citadel"), p("/data"))
        );
    }

    #[test]
    fn unit_unquoted_binary_parses() {
        let text = generate_systemd_unit(Path::new("/usr/bin/citadel"), Path::new("/data"));
        assert_eq!(
            parse_systemd_unit(&text),
            (p("/usr/bin/citadel"), p("/data"))
        );
    }

    #[test]
    fn daemon_running_check_maps_each_status() {
        let pid = Path::new("/data/daemon.pid");
        let running = daemon_running_check(DaemonStatus::Running(42), pid);
        assert_eq!(running.status, CheckStatus::Ok);
        assert_eq!(running.detail, "daemon running (pid 42)");

        let stopped = daemon_running_check(DaemonStatus::Stopped, pid);
        assert_eq!(stopped.status, CheckStatus::Info);
        assert_eq!(stopped.hint.as_deref(), Some("citadel daemon start"));

        let stale = daemon_running_check(DaemonStatus::Stale, pid);
        assert_eq!(stale.status, CheckStatus::Warn);
        assert_eq!(
            stale.detail,
            "stale daemon.pid at /data/daemon.pid; no daemon holds it"
        );
        assert_eq!(stale.hint.as_deref(), Some("citadel daemon start"));
    }

    #[test]
    fn service_unsupported_is_one_info_row() {
        let checks = service_checks(
            &ServiceState::Unsupported,
            Path::new("/data"),
            Path::new("/bin/c"),
        );
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, CheckStatus::Info);
        assert_eq!(checks[0].detail, "not supported on this platform");
    }

    #[test]
    fn service_not_installed_is_info_without_fail() {
        let checks = service_checks(
            &ServiceState::NotInstalled,
            Path::new("/data"),
            Path::new("/bin/c"),
        );
        assert!(!checks.is_empty());
        assert!(checks.iter().all(|c| c.status == CheckStatus::Info));
    }

    #[test]
    fn service_installed_and_matching_is_ok() {
        let checks = service_checks(
            &installed("/bin/c", "/data", true),
            Path::new("/data"),
            Path::new("/bin/c"),
        );
        assert!(checks.iter().all(|c| c.status == CheckStatus::Ok));
    }

    #[test]
    fn service_installed_not_loaded_warns() {
        let checks = service_checks(
            &installed("/bin/c", "/data", false),
            Path::new("/data"),
            Path::new("/bin/c"),
        );
        let warn = checks
            .iter()
            .find(|c| c.status == CheckStatus::Warn)
            .expect("a warn row");
        assert!(warn.detail.contains("not loaded"));
        assert!(warn.hint.is_some());
    }

    #[test]
    fn service_other_data_dir_warns() {
        let checks = service_checks(
            &installed("/bin/c", "/other", true),
            Path::new("/data"),
            Path::new("/bin/c"),
        );
        let warn = checks
            .iter()
            .find(|c| c.status == CheckStatus::Warn)
            .expect("a warn row");
        assert!(warn
            .detail
            .contains("service uses data dir /other; this CLI uses /data"));
        assert!(warn.hint.is_some());
    }

    #[test]
    fn service_other_binary_warns_with_install_hint() {
        let checks = service_checks(
            &installed("/old/c", "/data", true),
            Path::new("/data"),
            Path::new("/bin/c"),
        );
        let warn = checks
            .iter()
            .find(|c| c.status == CheckStatus::Warn)
            .expect("a warn row");
        assert!(warn.detail.contains("/old/c"));
        assert_eq!(warn.hint.as_deref(), Some("citadel daemon install"));
    }
}
