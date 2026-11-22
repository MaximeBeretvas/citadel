pub mod check;

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::VaultConfig;
use crate::error::{Result, VaultError};
use crate::update::check::{
    current_unix_secs, format_iso8601_utc, is_newer_version, normalize_version_str,
    read_cached_status, write_cache_atomic, UpdateCheckCache,
};

/// Options for [`run_update`].
#[derive(Debug, Clone)]
pub struct UpdateOpts {
    pub config: VaultConfig,
    pub check_only: bool,
    pub yes: bool,
    pub current_version: String,
    pub current_exe: PathBuf,
    pub releases_api_url: String,
    pub force_container: Option<bool>,
    pub stdin_is_tty: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct FullGitHubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

/// Detect whether Citadel is running inside an OCI container.
pub fn is_running_in_container() -> bool {
    if Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists() {
        return true;
    }
    std::env::var("CITADEL_CONTAINER")
        .ok()
        .map(|v| v.trim().to_string())
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Map the compile-time target OS and architecture to the GitHub Release target triple.
pub fn target_triple() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        (os, arch) => Err(VaultError::Validation(format!(
            "unsupported platform for prebuilt self-update: {os}/{arch}"
        ))),
    }
}

/// Parse a `checksums.sha256` manifest and verify `bytes` matches the expected digest for `archive_name`.
pub fn verify_sha256_manifest(manifest: &str, archive_name: &str, bytes: &[u8]) -> Result<()> {
    let mut expected_hex: Option<&str> = None;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(digest) = parts.next() else {
            continue;
        };
        let Some(raw_name) = parts.next() else {
            continue;
        };
        let normalized_name = raw_name.trim_start_matches('*').trim_start_matches("./");
        if normalized_name == archive_name {
            expected_hex = Some(digest);
            break;
        }
    }

    let expected = expected_hex.ok_or_else(|| {
        VaultError::Validation(format!(
            "checksums.sha256 does not contain an entry for '{archive_name}'"
        ))
    })?;

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = format!("{:x}", hasher.finalize());

    if !actual.eq_ignore_ascii_case(expected) {
        return Err(VaultError::Validation(format!(
            "SHA-256 checksum mismatch for '{archive_name}': expected {expected}, got {actual}"
        )));
    }

    Ok(())
}

/// Safely extract the `citadel` binary from a `.tar.gz` archive into `dest`.
pub fn extract_binary_from_tar_gz(archive_bytes: &[u8], dest: &Path) -> Result<()> {
    let decoder = GzDecoder::new(archive_bytes);
    let mut archive = tar::Archive::new(decoder);

    for entry_res in archive.entries()? {
        let mut entry = entry_res?;
        if !entry.header().entry_type().is_file() {
            continue;
        }

        let entry_path = entry.path()?;
        for comp in entry_path.components() {
            if matches!(
                comp,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            ) {
                return Err(VaultError::Validation(format!(
                    "unsafe path in release archive: {}",
                    entry_path.display()
                )));
            }
        }

        let is_citadel_bin = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == "citadel");

        if is_citadel_bin {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out_file = std::fs::File::create(dest)?;
            std::io::copy(&mut entry, &mut out_file)?;
            out_file.sync_all()?;
            return Ok(());
        }
    }

    Err(VaultError::Validation(
        "release archive does not contain a `citadel` binary".to_string(),
    ))
}

/// Atomically replace `current_bin` with `new_bin`, preserving executable permissions.
///
/// `new_bin` must live on the same filesystem as `current_bin`. A single `rename` over
/// the target is atomic on POSIX: there is never a moment without a binary at
/// `current_bin`, and running processes keep the old inode.
pub fn atomic_replace_binary(new_bin: &Path, current_bin: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(current_bin)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0o755);
        let exec_mode = if mode & 0o111 == 0 { 0o755 } else { mode };
        std::fs::set_permissions(new_bin, std::fs::Permissions::from_mode(exec_mode))?;
    }

    std::fs::rename(new_bin, current_bin)?;
    Ok(())
}

/// Execute `citadel update`.
pub async fn run_update<W: Write>(opts: UpdateOpts, out: &mut W) -> Result<()> {
    let current = normalize_version_str(&opts.current_version).to_string();
    let in_container = opts.force_container.unwrap_or_else(is_running_in_container);

    if in_container {
        let (cached, _) = read_cached_status(
            &opts.config.update_check_path(),
            &current,
            current_unix_secs(),
        );
        writeln!(out, "Running inside a container (citadel v{current}).")?;
        if let Some(latest) = cached.latest_version {
            if cached.update_available {
                writeln!(out, "Latest known release: v{latest}")?;
            }
        }
        writeln!(
            out,
            "Self-updating in place is disabled inside containers. Rebuild and recreate with Apple `container`:"
        )?;
        writeln!(out, "  container build --pull -t citadel:latest .")?;
        writeln!(out, "  container stop citadel && container rm citadel")?;
        writeln!(
            out,
            "  container run -d --name citadel -p 127.0.0.1:4317:4317 -e CITADEL_HOME=/data -v citadel-data:/data citadel:latest"
        )?;
        return Ok(());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(format!("citadel/{current}"))
        .build()?;

    let release_resp = client
        .get(&opts.releases_api_url)
        .send()
        .await?
        .error_for_status()?;
    let release_body = release_resp.text().await?;
    let release: FullGitHubRelease = serde_json::from_str(&release_body)
        .map_err(|err| VaultError::Other(format!("invalid GitHub release JSON response: {err}")))?;

    let latest = normalize_version_str(&release.tag_name).to_string();
    if semver::Version::parse(&latest).is_err() {
        return Err(VaultError::Validation(format!(
            "release tag '{}' is not a valid semver version",
            release.tag_name
        )));
    }

    // Refresh update-check.json cache.
    let cache = UpdateCheckCache {
        checked_at: format_iso8601_utc(current_unix_secs()),
        latest_version: latest.clone(),
        release_url: release.html_url.clone(),
    };
    let _ = write_cache_atomic(&opts.config.update_check_path(), &cache);

    if !is_newer_version(&current, &latest) {
        writeln!(out, "citadel v{current} is already up to date")?;
        return Ok(());
    }

    if opts.check_only {
        writeln!(
            out,
            "Update available: v{current} -> v{latest} ({})",
            release.html_url
        )?;
        return Ok(());
    }

    let is_tty = opts
        .stdin_is_tty
        .unwrap_or_else(|| std::io::stdin().is_terminal());
    if !opts.yes {
        if !is_tty {
            return Err(VaultError::Validation(
                "refusing to upgrade without confirmation on non-interactive stdin; pass --yes to proceed"
                    .to_string(),
            ));
        }
        write!(out, "Upgrade citadel from v{current} to v{latest}? [y/N]: ")?;
        out.flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        let trimmed = answer.trim().to_ascii_lowercase();
        if trimmed != "y" && trimmed != "yes" {
            writeln!(out, "Aborted.")?;
            return Ok(());
        }
    }

    let triple = target_triple()?;
    let archive_name = format!("citadel-v{latest}-{triple}.tar.gz");

    let archive_asset = release
        .assets
        .iter()
        .find(|a| a.name == archive_name)
        .ok_or_else(|| {
            VaultError::NotFound(format!(
                "release v{latest} does not include asset '{archive_name}'"
            ))
        })?;
    let checksums_asset = release
        .assets
        .iter()
        .find(|a| a.name == "checksums.sha256")
        .ok_or_else(|| {
            VaultError::NotFound(format!(
                "release v{latest} does not include 'checksums.sha256'"
            ))
        })?;

    let manifest = client
        .get(&checksums_asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let archive_bytes = client
        .get(&archive_asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    verify_sha256_manifest(&manifest, &archive_name, &archive_bytes)?;

    // Follow symlinks (Homebrew, `~/.local/bin`) so the real binary is replaced and the
    // temp file lands on the same filesystem as it.
    let install_path = std::fs::canonicalize(&opts.current_exe)?;
    let parent_dir = install_path.parent().ok_or_else(|| {
        VaultError::Config(format!(
            "cannot determine parent directory for executable {}",
            install_path.display()
        ))
    })?;
    let temp_bin = parent_dir.join(format!(".citadel.update.{}.tmp", std::process::id()));

    if let Err(err) = extract_binary_from_tar_gz(&archive_bytes, &temp_bin) {
        let _ = std::fs::remove_file(&temp_bin);
        return Err(err);
    }

    if let Err(err) = atomic_replace_binary(&temp_bin, &install_path) {
        let _ = std::fs::remove_file(&temp_bin);
        return Err(err);
    }

    writeln!(out, "Updated citadel from v{current} to v{latest}.")?;

    // Restart running native daemon if active.
    if matches!(
        crate::daemon::daemon_status(&opts.config),
        Ok(crate::daemon::DaemonStatus::Running(_))
    ) {
        let _ = crate::daemon::stop_daemon(&opts.config);
        match crate::daemon::start_daemon_with_exe(&opts.config, false, &opts.current_exe) {
            Ok(Some(pid)) => writeln!(out, "Restarted citadel daemon (pid {pid}).")?,
            Ok(None) => {}
            Err(err) => writeln!(out, "Warning: failed to restart daemon: {err}")?,
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::VaultError;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    fn build_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, *name, *data)
                .expect("append tar entry");
        }
        let encoder = builder.into_inner().expect("finish tar");
        encoder.finish().expect("finish gzip")
    }

    #[test]
    fn sha256_manifest_accepts_valid_digest_and_rejects_tampered_bytes() {
        let payload = b"fake-tarball-bytes";
        let digest = sha256_hex(payload);
        let manifest = format!(
            "0000000000000000000000000000000000000000000000000000000000000000  other.tar.gz\n{digest}  citadel-v0.2.0-aarch64-apple-darwin.tar.gz\n"
        );

        verify_sha256_manifest(
            &manifest,
            "citadel-v0.2.0-aarch64-apple-darwin.tar.gz",
            payload,
        )
        .expect("valid digest must pass");

        let err = verify_sha256_manifest(
            &manifest,
            "citadel-v0.2.0-aarch64-apple-darwin.tar.gz",
            b"tampered-bytes",
        )
        .expect_err("tampered bytes must fail");
        assert!(matches!(err, VaultError::Validation(_)));
    }

    #[test]
    fn extract_tar_gz_extracts_citadel_binary_and_rejects_missing_binary() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("citadel.new");

        let valid_archive =
            build_tar_gz(&[("citadel-v0.2.0/citadel", b"#!/bin/sh\necho v0.2.0\n")]);
        extract_binary_from_tar_gz(&valid_archive, &dest).expect("extract should succeed");
        assert_eq!(
            std::fs::read(&dest).expect("read extracted"),
            b"#!/bin/sh\necho v0.2.0\n"
        );

        let missing_archive = build_tar_gz(&[("README.md", b"# Hello\n")]);
        let err = extract_binary_from_tar_gz(&missing_archive, &dir.path().join("missing"))
            .expect_err("archive without citadel binary must fail");
        assert!(matches!(err, VaultError::Validation(_)));
    }

    #[test]
    fn atomic_replace_swaps_binary_and_preserves_exec_mode() {
        let dir = tempdir().expect("tempdir");
        let current = dir.path().join("citadel");
        let replacement = dir.path().join("citadel.new");

        std::fs::write(&current, b"old-binary").expect("write old");
        std::fs::write(&replacement, b"new-binary").expect("write new");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o755))
                .expect("chmod old");
        }

        atomic_replace_binary(&replacement, &current).expect("atomic replace should succeed");
        assert_eq!(
            std::fs::read(&current).expect("read swapped"),
            b"new-binary"
        );
        assert!(!replacement.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&current)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn atomic_replace_does_not_depend_on_a_backup_rename() {
        // The swap must be a single rename over the target, so there is never a moment
        // without a binary at `current`. Occupy the old backup path to prove no
        // rename-to-backup step is attempted.
        let dir = tempdir().expect("tempdir");
        let current = dir.path().join("citadel");
        let replacement = dir.path().join("citadel.new");
        std::fs::write(&current, b"old-binary").expect("write old");
        std::fs::write(&replacement, b"new-binary").expect("write new");
        let backup = dir
            .path()
            .join(format!(".citadel.bak.{}", std::process::id()));
        std::fs::create_dir(&backup).expect("occupy backup path");
        std::fs::write(backup.join("keep"), b"x").expect("make backup dir non-empty");

        atomic_replace_binary(&replacement, &current).expect("swap must not need a backup");
        assert_eq!(std::fs::read(&current).expect("read"), b"new-binary");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn update_through_symlink_replaces_target_and_keeps_symlink() {
        let (addr, server) = spawn_mock_release_server().await;

        let dir = tempdir().expect("tempdir");
        let real_dir = dir.path().join("cellar");
        let link_dir = dir.path().join("bin");
        std::fs::create_dir_all(&real_dir).expect("mkdir real");
        std::fs::create_dir_all(&link_dir).expect("mkdir link");
        let real_bin = real_dir.join("citadel");
        let link_bin = link_dir.join("citadel");
        std::fs::write(&real_bin, b"old-v0.1.0-binary").expect("write old bin");
        std::os::unix::fs::symlink(&real_bin, &link_bin).expect("symlink");

        let mut out = Vec::new();
        run_update(
            UpdateOpts {
                config: VaultConfig::new(dir.path().join("data")),
                check_only: false,
                yes: true,
                current_version: "0.1.0".to_string(),
                current_exe: link_bin.clone(),
                releases_api_url: format!("http://{addr}/releases/latest"),
                force_container: Some(false),
                stdin_is_tty: Some(false),
            },
            &mut out,
        )
        .await
        .expect("update through symlink should succeed");
        server.abort();

        assert!(
            std::fs::symlink_metadata(&link_bin)
                .expect("link metadata")
                .file_type()
                .is_symlink(),
            "the PATH symlink must survive the update"
        );
        assert_eq!(
            std::fs::read(&real_bin).expect("read real bin"),
            b"upgraded-v0.2.0-binary"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&link_dir)
            .expect("read link dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("citadel")]);
    }

    #[tokio::test]
    async fn container_mode_prints_guidance_without_mutating_binary() {
        let dir = tempdir().expect("tempdir");
        let fake_bin = dir.path().join("citadel");
        std::fs::write(&fake_bin, b"container-binary").expect("write fake bin");

        let mut out = Vec::new();
        run_update(
            UpdateOpts {
                config: VaultConfig::new(dir.path()),
                check_only: false,
                yes: true,
                current_version: "0.1.0".to_string(),
                current_exe: fake_bin.clone(),
                releases_api_url: "http://127.0.0.1:9/unused".to_string(),
                force_container: Some(true),
                stdin_is_tty: Some(false),
            },
            &mut out,
        )
        .await
        .expect("container mode should exit cleanly");

        let text = String::from_utf8(out).expect("utf-8 output");
        assert!(
            text.contains("container "),
            "expected Apple container guidance: {text}"
        );
        assert!(
            !text.contains("docker "),
            "must never recommend docker: {text}"
        );
        assert_eq!(
            std::fs::read(&fake_bin).expect("read fake bin"),
            b"container-binary"
        );
    }

    /// Serve a fake `v0.2.0` release whose archive holds `upgraded-v0.2.0-binary`.
    async fn spawn_mock_release_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let triple = target_triple().expect("supported test platform");
        let archive_name = format!("citadel-v0.2.0-{triple}.tar.gz");
        let archive_bytes = build_tar_gz(&[("citadel", b"upgraded-v0.2.0-binary")]);
        let checksum_line = format!("{}  {}\n", sha256_hex(&archive_bytes), archive_name);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local addr");

        let archive_url = format!("http://{addr}/download/{archive_name}");
        let checksums_url = format!("http://{addr}/download/checksums.sha256");
        let archive_name_clone = archive_name.clone();
        let archive_bytes_clone = archive_bytes.clone();
        let checksum_clone = checksum_line.clone();

        let app = axum::Router::new()
            .route(
                "/releases/latest",
                axum::routing::get(move || {
                    let archive_name = archive_name_clone.clone();
                    let archive_url = archive_url.clone();
                    let checksums_url = checksums_url.clone();
                    async move {
                        axum::Json(serde_json::json!({
                            "tag_name": "v0.2.0",
                            "html_url": "https://github.com/mberetvas/citadel/releases/tag/v0.2.0",
                            "assets": [
                                { "name": archive_name, "browser_download_url": archive_url },
                                { "name": "checksums.sha256", "browser_download_url": checksums_url }
                            ]
                        }))
                    }
                }),
            )
            .route(
                &format!("/download/{archive_name}"),
                axum::routing::get(move || {
                    let bytes = archive_bytes_clone.clone();
                    async move { bytes }
                }),
            )
            .route(
                "/download/checksums.sha256",
                axum::routing::get(move || {
                    let text = checksum_clone.clone();
                    async move { text }
                }),
            );

        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, server)
    }

    #[tokio::test]
    async fn end_to_end_update_against_mock_release_server() {
        let (addr, server) = spawn_mock_release_server().await;

        let dir = tempdir().expect("tempdir");
        let fake_bin = dir.path().join("citadel");
        std::fs::write(&fake_bin, b"old-v0.1.0-binary").expect("write old bin");
        let config = VaultConfig::new(dir.path().join("data"));

        let mut out = Vec::new();
        run_update(
            UpdateOpts {
                config: config.clone(),
                check_only: false,
                yes: true,
                current_version: "0.1.0".to_string(),
                current_exe: fake_bin.clone(),
                releases_api_url: format!("http://{addr}/releases/latest"),
                force_container: Some(false),
                stdin_is_tty: Some(false),
            },
            &mut out,
        )
        .await
        .expect("end-to-end update should succeed");
        server.abort();

        assert_eq!(
            std::fs::read(&fake_bin).expect("read upgraded bin"),
            b"upgraded-v0.2.0-binary"
        );
        assert!(config.update_check_path().is_file());
    }

    #[tokio::test]
    async fn non_tty_update_without_yes_refuses_to_mutate_binary() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let app = axum::Router::new().route(
            "/releases/latest",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "tag_name": "v0.2.0",
                    "html_url": "https://github.com/mberetvas/citadel/releases/tag/v0.2.0",
                    "assets": []
                }))
            }),
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let dir = tempdir().expect("tempdir");
        let fake_bin = dir.path().join("citadel");
        std::fs::write(&fake_bin, b"old-v0.1.0-binary").expect("write old bin");
        let config = VaultConfig::new(dir.path().join("data"));

        let mut out = Vec::new();
        let err = run_update(
            UpdateOpts {
                config,
                check_only: false,
                yes: false,
                current_version: "0.1.0".to_string(),
                current_exe: fake_bin.clone(),
                releases_api_url: format!("http://{addr}/releases/latest"),
                force_container: Some(false),
                stdin_is_tty: Some(false),
            },
            &mut out,
        )
        .await
        .expect_err("non-TTY update without --yes must refuse");
        server.abort();

        assert!(
            matches!(err, VaultError::Validation(_)),
            "expected Validation error, got: {err}"
        );
        assert_eq!(
            std::fs::read(&fake_bin).expect("read untouched bin"),
            b"old-v0.1.0-binary"
        );
    }
}
