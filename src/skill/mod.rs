use std::path::{Path, PathBuf};

use clap::ValueEnum;

use crate::error::{Result, VaultError};
use crate::fsutil::{create_dir_symlink, remove_existing_path, resolves_to_same_dir};

const SKILL_DIR_NAME: &str = "citadel";
const SKILL_FILE_NAME: &str = "SKILL.md";

/// Embedded static router skill explaining how LLMs should query and manage `citadel`.
pub const ROUTER_SKILL_MD: &str = include_str!("SKILL.md");

/// Target agent skill directories when installing globally (`--global`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SkillTarget {
    Agents,
    Claude,
    Copilot,
    Opencode,
    All,
}

/// Summary of files and symlinks written by [`install_skill`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub canonical_path: PathBuf,
    pub symlinks: Vec<PathBuf>,
}

/// Install the embedded `SKILL.md` into the resolved agent skill location.
pub fn install_skill(
    path: Option<&Path>,
    global: bool,
    target: SkillTarget,
    force: bool,
) -> Result<InstallReport> {
    let cwd = std::env::current_dir()?;
    let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    install_skill_with_dirs(&cwd, home.as_deref(), path, global, target, force)
}

/// Install the embedded `SKILL.md` using explicit `cwd` and `home` directories (testable entrypoint).
pub fn install_skill_with_dirs(
    cwd: &Path,
    home: Option<&Path>,
    path: Option<&Path>,
    global: bool,
    target: SkillTarget,
    force: bool,
) -> Result<InstallReport> {
    if path.is_some() && global {
        return Err(VaultError::Validation(
            "--path and --global cannot be used together".to_string(),
        ));
    }

    let (canonical_dir, symlink_targets) = if let Some(custom_dir) = path {
        if custom_dir.as_os_str().is_empty() {
            return Err(VaultError::Validation(
                "--path must not be empty".to_string(),
            ));
        }
        (custom_dir.join(SKILL_DIR_NAME), Vec::new())
    } else if global {
        let home_dir = home.ok_or_else(|| {
            VaultError::Config(
                "could not determine home directory for --global skill install".to_string(),
            )
        })?;
        let canonical = home_dir.join(".agents").join("skills").join(SKILL_DIR_NAME);
        let links = resolve_global_symlinks(home_dir, target);
        (canonical, links)
    } else {
        (
            cwd.join(".agents").join("skills").join(SKILL_DIR_NAME),
            Vec::new(),
        )
    };

    let canonical_path = canonical_dir.join(SKILL_FILE_NAME);

    if !force {
        ensure_not_exists(&canonical_path, "skill file")?;
        if let Ok(meta) = std::fs::symlink_metadata(&canonical_dir) {
            if !meta.file_type().is_dir() {
                return Err(VaultError::Validation(format!(
                    "skill directory path {} already exists as a non-directory; pass --force to replace",
                    canonical_dir.display()
                )));
            }
        }
        for link in &symlink_targets {
            ensure_not_exists(link, "skill target")?;
        }
    }

    // Replace non-directory canonical dir (only possible with --force after pre-check).
    if let Ok(meta) = std::fs::symlink_metadata(&canonical_dir) {
        if !meta.file_type().is_dir() {
            std::fs::remove_file(&canonical_dir)?;
        }
    }
    std::fs::create_dir_all(&canonical_dir)?;

    if force {
        remove_existing_path(&canonical_path)?;
    }
    std::fs::write(&canonical_path, ROUTER_SKILL_MD)?;

    let mut symlinks = Vec::with_capacity(symlink_targets.len());
    for link_path in symlink_targets {
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // The agent's skills dir may itself link to the shared one, so this path already
        // reaches the skill: replacing it would delete the skill instead.
        if !resolves_to_same_dir(&link_path, &canonical_dir) {
            if force {
                remove_existing_path(&link_path)?;
            }
            create_dir_symlink(&canonical_dir, &link_path)?;
        }
        symlinks.push(link_path);
    }

    Ok(InstallReport {
        canonical_path,
        symlinks,
    })
}

fn resolve_global_symlinks(home: &Path, target: SkillTarget) -> Vec<PathBuf> {
    let claude = home.join(".claude").join("skills").join(SKILL_DIR_NAME);
    let copilot = home.join(".copilot").join("skills").join(SKILL_DIR_NAME);
    let opencode = home
        .join(".config")
        .join("opencode")
        .join("skills")
        .join(SKILL_DIR_NAME);

    match target {
        SkillTarget::Agents => Vec::new(),
        SkillTarget::Claude => vec![claude],
        SkillTarget::Copilot => vec![copilot],
        SkillTarget::Opencode => vec![opencode],
        SkillTarget::All => {
            let mut links = vec![claude];
            if home.join(".copilot").exists() {
                links.push(copilot);
            }
            if home.join(".config").join("opencode").exists() {
                links.push(opencode);
            }
            links
        }
    }
}

fn ensure_not_exists(path: &Path, label: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err(VaultError::Validation(format!(
            "{label} already exists at {}; pass --force to overwrite",
            path.display()
        ))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[cfg(unix)]
    fn create_test_symlink(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).expect("create test symlink");
    }

    #[cfg(windows)]
    fn create_test_symlink(target: &Path, link: &Path) {
        std::os::windows::fs::symlink_dir(target, link).expect("create test symlink");
    }

    #[cfg(unix)]
    #[test]
    fn global_install_keeps_the_skill_when_claude_skills_dir_links_to_the_shared_dir() {
        let cwd = tempdir().expect("cwd tempdir");
        let home = tempdir().expect("home tempdir");
        let shared = home.path().join(".agents").join("skills");
        std::fs::create_dir_all(&shared).expect("mkdir shared skills");
        std::fs::create_dir_all(home.path().join(".claude")).expect("mkdir .claude");
        create_test_symlink(&shared, &home.path().join(".claude").join("skills"));

        let canonical = shared.join(SKILL_DIR_NAME);
        let assert_skill_intact = || {
            assert!(
                std::fs::symlink_metadata(&canonical)
                    .expect("canonical skill dir")
                    .file_type()
                    .is_dir(),
                "the canonical skill dir must stay a real directory"
            );
            assert_eq!(
                std::fs::read_to_string(canonical.join(SKILL_FILE_NAME)).expect("read SKILL.md"),
                ROUTER_SKILL_MD
            );
        };

        install_skill_with_dirs(
            cwd.path(),
            Some(home.path()),
            None,
            true,
            SkillTarget::Claude,
            false,
        )
        .expect("first install must not fail with EEXIST");
        assert_skill_intact();

        install_skill_with_dirs(
            cwd.path(),
            Some(home.path()),
            None,
            true,
            SkillTarget::Claude,
            true,
        )
        .expect("forced reinstall");
        assert_skill_intact();
    }

    #[test]
    fn default_installs_into_cwd_agents_skills() {
        let cwd = tempdir().expect("cwd tempdir");
        let report =
            install_skill_with_dirs(cwd.path(), None, None, false, SkillTarget::All, false)
                .expect("default install should succeed");

        let expected = cwd.path().join(".agents/skills/citadel/SKILL.md");
        assert_eq!(report.canonical_path, expected);
        assert!(report.symlinks.is_empty());
        assert_eq!(
            std::fs::read_to_string(&expected).expect("read SKILL.md"),
            ROUTER_SKILL_MD
        );
    }

    #[test]
    fn custom_path_installs_into_target_dir() {
        let cwd = tempdir().expect("cwd tempdir");
        let custom = tempdir().expect("custom tempdir");
        let target_parent = custom.path().join("custom");

        let report = install_skill_with_dirs(
            cwd.path(),
            None,
            Some(&target_parent),
            false,
            SkillTarget::All,
            false,
        )
        .expect("custom path install should succeed");

        let expected = target_parent.join("citadel/SKILL.md");
        assert_eq!(report.canonical_path, expected);
        assert!(report.symlinks.is_empty());
        assert_eq!(
            std::fs::read_to_string(&expected).expect("read SKILL.md"),
            ROUTER_SKILL_MD
        );
    }

    #[test]
    fn global_install_writes_canonical_and_symlinks_claude() {
        let cwd = tempdir().expect("cwd tempdir");
        let home = tempdir().expect("home tempdir");

        let report = install_skill_with_dirs(
            cwd.path(),
            Some(home.path()),
            None,
            true,
            SkillTarget::All,
            false,
        )
        .expect("global install should succeed");

        let canonical_dir = home.path().join(".agents/skills/citadel");
        let canonical_file = canonical_dir.join("SKILL.md");
        let claude_link = home.path().join(".claude/skills/citadel");

        assert_eq!(report.canonical_path, canonical_file);
        assert_eq!(report.symlinks, vec![claude_link.clone()]);
        assert_eq!(
            std::fs::read_to_string(&canonical_file).expect("read canonical SKILL.md"),
            ROUTER_SKILL_MD
        );

        let meta = std::fs::symlink_metadata(&claude_link).expect("claude symlink metadata");
        assert!(meta.file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(&claude_link).expect("read claude symlink target"),
            canonical_dir
        );
        assert_eq!(
            std::fs::read_to_string(claude_link.join("SKILL.md")).expect("read via symlink"),
            ROUTER_SKILL_MD
        );
    }

    #[test]
    fn refuses_overwrite_without_force_and_overwrites_with_force() {
        let cwd = tempdir().expect("cwd tempdir");
        let home = tempdir().expect("home tempdir");

        // Part 1: SKILL.md file overwrite safety (default CWD mode)
        let first = install_skill_with_dirs(cwd.path(), None, None, false, SkillTarget::All, false)
            .expect("initial install succeeds");
        std::fs::write(&first.canonical_path, "custom modified skill").expect("modify SKILL.md");

        let err = install_skill_with_dirs(cwd.path(), None, None, false, SkillTarget::All, false)
            .expect_err("must refuse overwrite without --force");
        assert!(
            matches!(err, VaultError::Validation(_)),
            "expected validation error, got: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&first.canonical_path).expect("read modified SKILL.md"),
            "custom modified skill"
        );

        let forced = install_skill_with_dirs(cwd.path(), None, None, false, SkillTarget::All, true)
            .expect("install with --force should overwrite SKILL.md");
        assert_eq!(forced.canonical_path, first.canonical_path);
        assert_eq!(
            std::fs::read_to_string(&forced.canonical_path).expect("read overwritten SKILL.md"),
            ROUTER_SKILL_MD
        );

        // Part 2: Existing symlink replacement in --global mode
        let other_target = tempdir().expect("other target tempdir");
        let claude_skills = home.path().join(".claude/skills");
        std::fs::create_dir_all(&claude_skills).expect("create .claude/skills");
        let claude_link = claude_skills.join("citadel");
        create_test_symlink(other_target.path(), &claude_link);

        let link_err = install_skill_with_dirs(
            cwd.path(),
            Some(home.path()),
            None,
            true,
            SkillTarget::Claude,
            false,
        )
        .expect_err("must refuse replacing existing symlink without --force");
        assert!(
            matches!(link_err, VaultError::Validation(_)),
            "expected validation error, got: {link_err}"
        );
        assert_eq!(
            std::fs::read_link(&claude_link).expect("existing symlink preserved"),
            other_target.path()
        );
        assert!(
            !home.path().join(".agents/skills/citadel/SKILL.md").exists(),
            "failed pre-check should not partially write canonical SKILL.md"
        );

        let link_forced = install_skill_with_dirs(
            cwd.path(),
            Some(home.path()),
            None,
            true,
            SkillTarget::Claude,
            true,
        )
        .expect("--force should replace existing symlink");
        let expected_canonical_dir = home.path().join(".agents/skills/citadel");
        assert_eq!(
            link_forced.canonical_path,
            expected_canonical_dir.join("SKILL.md")
        );
        assert_eq!(link_forced.symlinks, vec![claude_link.clone()]);
        assert_eq!(
            std::fs::read_link(&claude_link).expect("updated symlink target"),
            expected_canonical_dir
        );
        assert_eq!(
            std::fs::read_to_string(claude_link.join("SKILL.md")).expect("read via new symlink"),
            ROUTER_SKILL_MD
        );
    }

    #[test]
    fn rejects_path_combined_with_global_and_supports_all_targets() {
        let cwd = tempdir().expect("cwd tempdir");
        let home = tempdir().expect("home tempdir");

        let err = install_skill_with_dirs(
            cwd.path(),
            Some(home.path()),
            Some(Path::new("/tmp/ignored")),
            true,
            SkillTarget::All,
            false,
        )
        .expect_err("--path + --global must fail");
        assert!(matches!(err, VaultError::Validation(_)));

        // Target Agents creates canonical copy only, no symlinks.
        let agents_only = install_skill_with_dirs(
            cwd.path(),
            Some(home.path()),
            None,
            true,
            SkillTarget::Agents,
            false,
        )
        .expect("agents-only global install should succeed");
        assert!(agents_only.symlinks.is_empty());

        // When .copilot and .config/opencode exist, --target all links all three.
        std::fs::create_dir_all(home.path().join(".copilot")).expect("create .copilot");
        std::fs::create_dir_all(home.path().join(".config/opencode"))
            .expect("create .config/opencode");

        let all_report = install_skill_with_dirs(
            cwd.path(),
            Some(home.path()),
            None,
            true,
            SkillTarget::All,
            true,
        )
        .expect("global --target all with existing agent dirs should succeed");
        assert_eq!(
            all_report.symlinks,
            vec![
                home.path().join(".claude/skills/citadel"),
                home.path().join(".copilot/skills/citadel"),
                home.path().join(".config/opencode/skills/citadel"),
            ]
        );
        for link in &all_report.symlinks {
            assert_eq!(
                std::fs::read_to_string(link.join("SKILL.md")).expect("read via symlink"),
                ROUTER_SKILL_MD
            );
        }
    }
}
