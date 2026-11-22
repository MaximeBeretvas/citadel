pub mod agents;
pub mod merge;

use std::path::{Path, PathBuf};

use crate::error::{Result, VaultError};
use crate::setup::agents::{ActionKind, PlannedAction, SetupPlan};
use crate::setup::merge::{
    agy_plugin_manifest_json, json_has_citadel_entry, merge_json_mcp_config, merge_toml_mcp_config,
    toml_has_citadel_entry,
};
use crate::skill::ROUTER_SKILL_MD;

/// Summary of paths written or updated by [`apply_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyReport {
    pub written_paths: Vec<PathBuf>,
}

/// Execute all actions in `plan`, refusing to overwrite existing `citadel` entries unless `force` is true.
pub fn apply_plan(plan: &SetupPlan, force: bool) -> Result<ApplyReport> {
    if !force {
        for action in &plan.actions {
            check_no_conflict(action)?;
        }
    }

    let mut written_paths = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        if let Some(parent) = action.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        match &action.kind {
            ActionKind::WriteSkillFile => {
                if force {
                    remove_existing_path(&action.path)?;
                }
                std::fs::write(&action.path, ROUTER_SKILL_MD)?;
            }
            ActionKind::CreateSkillSymlink { target } => {
                if force {
                    remove_existing_path(&action.path)?;
                }
                create_dir_symlink(target, &action.path)?;
            }
            ActionKind::WriteAgyPluginManifest => {
                if force {
                    remove_existing_path(&action.path)?;
                }
                std::fs::write(&action.path, agy_plugin_manifest_json())?;
            }
            ActionKind::MergeJsonMcp { include_http_type } => {
                let existing = read_optional_file(&action.path)?;
                let merged = merge_json_mcp_config(
                    existing.as_deref(),
                    &plan.transport,
                    *include_http_type,
                    force,
                )?;
                std::fs::write(&action.path, merged)?;
            }
            ActionKind::MergeTomlMcp => {
                let existing = read_optional_file(&action.path)?;
                let merged = merge_toml_mcp_config(existing.as_deref(), &plan.transport, force)?;
                std::fs::write(&action.path, merged)?;
            }
        }
        written_paths.push(action.path.clone());
    }

    Ok(ApplyReport { written_paths })
}

fn check_no_conflict(action: &PlannedAction) -> Result<()> {
    let exists = std::fs::symlink_metadata(&action.path).is_ok();
    if !exists {
        return Ok(());
    }

    match &action.kind {
        ActionKind::WriteSkillFile
        | ActionKind::CreateSkillSymlink { .. }
        | ActionKind::WriteAgyPluginManifest => Err(VaultError::Validation(format!(
            "target already exists at {}; pass --force to overwrite",
            action.path.display()
        ))),
        ActionKind::MergeJsonMcp { .. } => {
            let existing = std::fs::read_to_string(&action.path)?;
            if json_has_citadel_entry(&existing) {
                return Err(VaultError::Validation(format!(
                    "MCP config at {} already contains `mcpServers.citadel`; pass --force to overwrite",
                    action.path.display()
                )));
            }
            Ok(())
        }
        ActionKind::MergeTomlMcp => {
            let existing = std::fs::read_to_string(&action.path)?;
            if toml_has_citadel_entry(&existing) {
                return Err(VaultError::Validation(format!(
                    "MCP config at {} already contains `[mcp_servers.citadel]`; pass --force to overwrite",
                    action.path.display()
                )));
            }
            Ok(())
        }
    }
}

fn read_optional_file(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn remove_existing_path(path: &Path) -> Result<()> {
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
fn create_dir_symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(windows)]
fn create_dir_symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::windows::fs::symlink_dir(target, link)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::agents::{build_plan, AgentKind, McpTransport, SetupComponents, SetupScope};
    use crate::skill::ROUTER_SKILL_MD;
    use tempfile::tempdir;

    #[test]
    fn apply_plan_scaffolds_agy_local_plugin_and_claude_global_symlink() {
        let cwd = tempdir().expect("cwd tempdir");
        let home = tempdir().expect("home tempdir");

        // 1. Antigravity local with HTTP transport
        let agy_plan = build_plan(
            cwd.path(),
            Some(home.path()),
            &[AgentKind::Agy],
            SetupScope::Local,
            SetupComponents::Both,
            McpTransport::Http("http://127.0.0.1:4317/mcp".to_string()),
        )
        .expect("agy local plan");
        apply_plan(&agy_plan, false).expect("agy local apply");

        let agy_skill = cwd.path().join(".agents/skills/citadel/SKILL.md");
        assert_eq!(
            std::fs::read_to_string(&agy_skill).expect("read agy SKILL.md"),
            ROUTER_SKILL_MD
        );
        let plugin_manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(cwd.path().join(".agents/plugins/citadel/plugin.json"))
                .expect("read plugin.json"),
        )
        .expect("parse plugin.json");
        assert_eq!(plugin_manifest["name"], "citadel");

        let agy_mcp: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(cwd.path().join(".agents/plugins/citadel/mcp_config.json"))
                .expect("read mcp_config.json"),
        )
        .expect("parse mcp_config.json");
        assert_eq!(
            agy_mcp["mcpServers"]["citadel"]["url"],
            "http://127.0.0.1:4317/mcp"
        );
        assert!(agy_mcp["mcpServers"]["citadel"].get("type").is_none());

        // 2. Claude global with stdio transport
        let claude_plan = build_plan(
            cwd.path(),
            Some(home.path()),
            &[AgentKind::Claude],
            SetupScope::Global,
            SetupComponents::Both,
            McpTransport::Stdio,
        )
        .expect("claude global plan");
        apply_plan(&claude_plan, false).expect("claude global apply");

        let canonical_dir = home.path().join(".agents/skills/citadel");
        let claude_link = home.path().join(".claude/skills/citadel");
        let link_meta = std::fs::symlink_metadata(&claude_link).expect("claude link metadata");
        assert!(link_meta.file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(&claude_link).expect("read claude symlink"),
            canonical_dir
        );
        assert_eq!(
            std::fs::read_to_string(claude_link.join("SKILL.md")).expect("read via symlink"),
            ROUTER_SKILL_MD
        );

        let claude_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.path().join(".claude.json"))
                .expect("read ~/.claude.json"),
        )
        .expect("parse ~/.claude.json");
        assert_eq!(claude_json["mcpServers"]["citadel"]["command"], "citadel");
        assert_eq!(
            claude_json["mcpServers"]["citadel"]["args"],
            serde_json::json!(["mcp"])
        );
    }
}
