use std::path::{Path, PathBuf};

use crate::config::VaultConfig;
use crate::error::{Result, VaultError};

/// macOS `launchd` LaunchAgent label.
pub const LAUNCHD_LABEL: &str = "com.citadel.daemon";
/// Linux `systemd --user` service unit filename.
pub const SYSTEMD_SERVICE_NAME: &str = "citadel.service";

/// Generate an Apple `launchd` XML plist running `citadel daemon start --foreground`.
pub fn generate_launchd_plist(bin_path: &Path, data_dir: &Path) -> String {
    let bin_escaped = xml_escape(&bin_path.to_string_lossy());
    let data_dir_escaped = xml_escape(&data_dir.to_string_lossy());
    let log_escaped = xml_escape(&data_dir.join("daemon.log").to_string_lossy());

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin_escaped}</string>
        <string>daemon</string>
        <string>start</string>
        <string>--foreground</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>CITADEL_HOME</key>
        <string>{data_dir_escaped}</string>
    </dict>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_escaped}</string>
    <key>StandardErrorPath</key>
    <string>{log_escaped}</string>
</dict>
</plist>
"#
    )
}

/// Generate a Linux `systemd --user` `.service` unit running `citadel daemon start --foreground`.
pub fn generate_systemd_unit(bin_path: &Path, data_dir: &Path) -> String {
    let bin_str = bin_path.to_string_lossy();
    let exec_bin = if bin_str.contains(char::is_whitespace) || bin_str.contains('"') {
        format!("\"{}\"", systemd_escape(&bin_str))
    } else {
        bin_str.into_owned()
    };
    let env_dir = systemd_escape(&data_dir.to_string_lossy());

    format!(
        "[Unit]\n\
         Description=citadel background sync daemon\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exec_bin} daemon start --foreground\n\
         Environment=\"CITADEL_HOME={env_dir}\"\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

/// Install and enable the OS background service (`launchd` on macOS, `systemd --user` on Linux).
pub fn install_service(config: &VaultConfig) -> Result<PathBuf> {
    config.ensure_data_dir()?;
    let bin_path = std::env::current_exe()?;
    install_service_for_platform(&bin_path, &config.data_dir)
}

/// Disable and remove the OS background service (`launchd` on macOS, `systemd --user` on Linux).
pub fn uninstall_service() -> Result<Option<PathBuf>> {
    uninstall_service_for_platform()
}

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().ok_or_else(|| {
        VaultError::Config("could not determine user home directory for LaunchAgents".into())
    })?;
    Ok(base
        .home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn install_service_for_platform(bin_path: &Path, data_dir: &Path) -> Result<PathBuf> {
    let plist_path = launchd_plist_path()?;
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = generate_launchd_plist(bin_path, data_dir);
    std::fs::write(&plist_path, content)?;

    // Unload any previous registration (best-effort) before loading the updated plist.
    let _ = std::process::Command::new("launchctl")
        .arg("unload")
        .arg(&plist_path)
        .status();

    let status = std::process::Command::new("launchctl")
        .arg("load")
        .arg("-w")
        .arg(&plist_path)
        .status()?;
    if !status.success() {
        return Err(VaultError::Other(format!(
            "launchctl load failed for '{}' (exit status {status})",
            plist_path.display()
        )));
    }

    Ok(plist_path)
}

#[cfg(target_os = "macos")]
fn uninstall_service_for_platform() -> Result<Option<PathBuf>> {
    let plist_path = launchd_plist_path()?;
    if !plist_path.exists() {
        return Ok(None);
    }

    let _ = std::process::Command::new("launchctl")
        .arg("unload")
        .arg(&plist_path)
        .status();
    std::fs::remove_file(&plist_path)?;
    Ok(Some(plist_path))
}

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().ok_or_else(|| {
        VaultError::Config("could not determine user config directory for systemd".into())
    })?;
    Ok(base
        .config_dir()
        .join("systemd")
        .join("user")
        .join(SYSTEMD_SERVICE_NAME))
}

#[cfg(target_os = "linux")]
fn install_service_for_platform(bin_path: &Path, data_dir: &Path) -> Result<PathBuf> {
    let unit_path = systemd_unit_path()?;
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = generate_systemd_unit(bin_path, data_dir);
    std::fs::write(&unit_path, content)?;

    let reload = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()?;
    if !reload.success() {
        return Err(VaultError::Other(format!(
            "systemctl --user daemon-reload failed (exit status {reload})"
        )));
    }

    let enable = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", SYSTEMD_SERVICE_NAME])
        .status()?;
    if !enable.success() {
        return Err(VaultError::Other(format!(
            "systemctl --user enable --now {SYSTEMD_SERVICE_NAME} failed (exit status {enable})"
        )));
    }

    Ok(unit_path)
}

#[cfg(target_os = "linux")]
fn uninstall_service_for_platform() -> Result<Option<PathBuf>> {
    let unit_path = systemd_unit_path()?;
    if !unit_path.exists() {
        return Ok(None);
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", SYSTEMD_SERVICE_NAME])
        .status();
    std::fs::remove_file(&unit_path)?;
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    Ok(Some(unit_path))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn install_service_for_platform(_bin_path: &Path, _data_dir: &Path) -> Result<PathBuf> {
    Err(VaultError::Other(
        "OS service install is only supported on macOS (launchd) and Linux (systemd --user)".into(),
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn uninstall_service_for_platform() -> Result<Option<PathBuf>> {
    Err(VaultError::Other(
        "OS service uninstall is only supported on macOS (launchd) and Linux (systemd --user)"
            .into(),
    ))
}

fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn systemd_escape(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_launchd_plist_and_systemd_unit() {
        let bin = Path::new("/usr/local/bin/citadel");
        let data_dir = Path::new("/Users/alice/Library/Application Support/citadel");

        let plist = generate_launchd_plist(bin, data_dir);
        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(plist.contains("<plist version=\"1.0\">"));
        assert!(plist.contains(&format!("<string>{LAUNCHD_LABEL}</string>")));
        assert!(plist.contains("<string>/usr/local/bin/citadel</string>"));
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<string>start</string>"));
        assert!(plist.contains("<string>--foreground</string>"));
        assert!(plist.contains("<key>CITADEL_HOME</key>"));
        assert!(plist.contains(&format!("<string>{}</string>", data_dir.display())));
        assert!(plist.contains("<key>KeepAlive</key>\n    <true/>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(plist.contains("daemon.log"));

        let unit = generate_systemd_unit(bin, data_dir);
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("ExecStart=/usr/local/bin/citadel daemon start --foreground"));
        assert!(unit.contains("CITADEL_HOME=/Users/alice/Library/Application Support/citadel"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn launchd_plist_escapes_xml_special_characters() {
        let bin = Path::new("/opt/a&b/<vault>/citadel");
        let data_dir = Path::new("/tmp/data&'dir\"");

        let plist = generate_launchd_plist(bin, data_dir);
        assert!(plist.contains("/opt/a&amp;b/&lt;vault&gt;/citadel"));
        assert!(plist.contains("/tmp/data&amp;&apos;dir&quot;"));
    }
}
