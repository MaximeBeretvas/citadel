//! Filesystem helpers shared by `skill`, `setup` and `update`.

use std::path::{Path, PathBuf};

use crate::error::{Result, VaultError};

/// Most symlinks followed when resolving where a write lands (Linux's own ELOOP limit).
const MAX_SYMLINK_HOPS: usize = 40;

/// Replace `path` with `contents` through a sibling temp file and a rename.
///
/// Agents such as Claude Code rewrite their config files while running, so a reader
/// must never see a half-written file. A symlinked config (dotfiles) is followed so the
/// link survives, even when its target does not exist yet. An existing file keeps its
/// mode (e.g. `0600` on `~/.claude.json`); a new file gets the default mode, as with
/// `std::fs::write`.
pub(crate) fn write_file_atomic(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let target = resolve_write_target(path)?;
    let parent = target.parent().ok_or_else(|| {
        VaultError::Config(format!(
            "cannot determine parent directory of {}",
            target.display()
        ))
    })?;
    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config");
    let tmp_path = parent.join(format!(
        ".{file_name}.citadel.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let result = (|| -> Result<()> {
        let existing_permissions = std::fs::metadata(&target)
            .ok()
            .map(|meta| meta.permissions());
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if existing_permissions.is_some() {
            // Owner-only until the original mode is copied, so secrets never leak.
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp_path)?;
        if let Some(permissions) = existing_permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp_path, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

/// Where a write to `path` must land: its canonical path when it exists, otherwise the
/// end of its symlink chain, so a dangling dotfiles link is written through, not replaced.
fn resolve_write_target(path: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(resolved) => return Ok(resolved),
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err.into()),
        Err(_) => {}
    }

    let mut current = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link_target = std::fs::read_link(&current)?;
                // A relative link target is relative to the directory holding the link.
                current = match current.parent() {
                    Some(dir) => dir.join(link_target),
                    None => link_target,
                };
            }
            _ => return Ok(current),
        }
    }
    Err(VaultError::Config(format!(
        "too many levels of symbolic links at {}",
        path.display()
    )))
}

/// Whether `a` and `b` name the same file once symlinks are resolved; a path that
/// cannot be canonicalized (e.g. it does not exist) is compared as written.
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    let a = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let b = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    a == b
}

/// Whether `link` already resolves to the directory `target`, e.g. because a parent of
/// `link` is itself a symlink into `target`'s tree. Replacing such a path would delete
/// `target` through the parent link and leave a link pointing at itself.
pub(crate) fn resolves_to_same_dir(link: &Path, target: &Path) -> bool {
    match (std::fs::canonicalize(link), std::fs::canonicalize(target)) {
        (Ok(link), Ok(target)) => link == target,
        _ => false,
    }
}

pub(crate) fn remove_existing_path(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_dir() {
                std::fs::remove_dir_all(path)?;
            } else {
                std::fs::remove_file(path)?;
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(unix)]
pub(crate) fn create_dir_symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(windows)]
pub(crate) fn create_dir_symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::windows::fs::symlink_dir(target, link)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn new_file_gets_the_same_mode_as_fs_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reference = dir.path().join("SKILL.md");
        std::fs::write(&reference, "skill").expect("write reference");

        let created = dir.path().join(".mcp.json");
        write_file_atomic(&created, "{}\n").expect("atomic write");

        assert_eq!(
            mode(&created),
            mode(&reference),
            "a new config must honour the umask like fs::write, not stay owner-only"
        );
    }

    #[test]
    fn existing_file_keeps_its_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join(".claude.json");
        std::fs::write(&config, "{}").expect("seed");
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o640)).expect("chmod");

        write_file_atomic(&config, "{\"a\": 1}\n").expect("atomic write");

        assert_eq!(mode(&config), 0o640);
        assert_eq!(
            std::fs::read_to_string(&config).expect("read"),
            "{\"a\": 1}\n"
        );
    }

    #[test]
    fn dangling_symlink_is_written_through_and_survives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dotfiles = dir.path().join("dotfiles");
        std::fs::create_dir_all(&dotfiles).expect("mkdir dotfiles");

        // Absolute link target that does not exist yet.
        let absolute_target = dotfiles.join("config.toml");
        let absolute_link = dir.path().join("config.toml");
        symlink(&absolute_target, &absolute_link).expect("absolute symlink");
        write_file_atomic(&absolute_link, "x = 1\n").expect("write through dangling link");
        assert!(std::fs::symlink_metadata(&absolute_link)
            .expect("link meta")
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&absolute_target).expect("read target"),
            "x = 1\n"
        );

        // Relative link target, resolved against the link's own directory.
        let relative_link = dir.path().join("mcp_config.json");
        symlink("dotfiles/mcp_config.json", &relative_link).expect("relative symlink");
        write_file_atomic(&relative_link, "{}\n").expect("write through relative link");
        assert!(std::fs::symlink_metadata(&relative_link)
            .expect("link meta")
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(dotfiles.join("mcp_config.json")).expect("read target"),
            "{}\n"
        );
    }
}
