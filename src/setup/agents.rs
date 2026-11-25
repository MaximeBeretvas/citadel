use std::collections::HashSet;
use std::path::{Path, PathBuf};

use clap::ValueEnum;

use crate::error::{Result, VaultError};
use crate::setup::merge::{json_has_citadel_entry, toml_has_citadel_entry};

/// Supported AI coding agents for `citadel setup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum AgentKind {
    Claude,
    Agy,
    Codex,
}

impl AgentKind {
    /// Human-readable display label for interactive prompts and summaries.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Agy => "Antigravity (agy)",
            Self::Codex => "Codex",
        }
    }
}

/// Target installation scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SetupScope {
    Local,
    Global,
}

/// Which components to install/configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SetupComponents {
    Both,
    Skill,
    Mcp,
}

impl SetupComponents {
    pub fn includes_skill(self) -> bool {
        matches!(self, Self::Both | Self::Skill)
    }

    pub fn includes_mcp(self) -> bool {
        matches!(self, Self::Both | Self::Mcp)
    }
}

/// MCP transport configuration for the `citadel` server entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    Stdio,
    Http(String),
}

/// Concrete filesystem operation planned by `build_plan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionKind {
    /// Write the embedded `SKILL.md` file.
    WriteSkillFile,
    /// Create a directory symlink pointing to `target`.
    CreateSkillSymlink { target: PathBuf },
    /// Write the minimal local Antigravity `plugin.json`.
    WriteAgyPluginManifest,
    /// Merge `mcpServers.citadel` into a JSON config file.
    MergeJsonMcp { include_http_type: bool },
    /// Merge `[mcp_servers.citadel]` into a TOML config file.
    MergeTomlMcp,
}

/// A single planned file or symlink mutation with pre-flight conflict inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedAction {
    pub path: PathBuf,
    pub kind: ActionKind,
    pub exists: bool,
    pub would_overwrite_citadel: bool,
}

/// Complete resolved setup plan for one or more agents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupPlan {
    pub transport: McpTransport,
    pub actions: Vec<PlannedAction>,
}

impl SetupPlan {
    /// Whether any planned action would overwrite an existing `citadel` skill/config entry.
    pub fn has_conflicts(&self) -> bool {
        self.actions.iter().any(|a| a.would_overwrite_citadel)
    }
}

/// Resolve all planned skill and MCP actions for the requested agents and scope.
pub fn build_plan(
    cwd: &Path,
    home: Option<&Path>,
    agents: &[AgentKind],
    scope: SetupScope,
    components: SetupComponents,
    transport: McpTransport,
) -> Result<SetupPlan> {
    if agents.is_empty() {
        return Err(VaultError::Validation(
            "at least one target agent must be selected".to_string(),
        ));
    }

    let base = match scope {
        SetupScope::Local => cwd,
        SetupScope::Global => home.ok_or_else(|| {
            VaultError::Config("could not determine home directory for global setup".to_string())
        })?,
    };

    let mut actions = Vec::new();
    let mut seen_paths = HashSet::new();
    for &agent in agents {
        let mut targets = Vec::new();
        if components.includes_skill() {
            targets.extend(skill_targets(agent, scope, base));
        }
        if components.includes_mcp() {
            targets.extend(mcp_targets(agent, scope, base));
        }
        for (path, kind) in targets {
            push_action(&mut actions, &mut seen_paths, path, kind);
        }
    }

    Ok(SetupPlan { transport, actions })
}

/// Skill files and symlinks for `agent`, relative to the scope's `base` directory.
pub(crate) fn skill_targets(
    agent: AgentKind,
    scope: SetupScope,
    base: &Path,
) -> Vec<(PathBuf, ActionKind)> {
    let canonical_dir = base.join(".agents/skills/citadel");
    let skill_file = (canonical_dir.join("SKILL.md"), ActionKind::WriteSkillFile);
    let symlink = |rel: &str| {
        (
            base.join(rel),
            ActionKind::CreateSkillSymlink {
                target: canonical_dir.clone(),
            },
        )
    };
    match (agent, scope) {
        (AgentKind::Claude, SetupScope::Local) => vec![(
            base.join(".claude/skills/citadel/SKILL.md"),
            ActionKind::WriteSkillFile,
        )],
        (AgentKind::Claude, SetupScope::Global) => {
            vec![skill_file, symlink(".claude/skills/citadel")]
        }
        (AgentKind::Agy, SetupScope::Global) => {
            vec![skill_file, symlink(".gemini/config/skills/citadel")]
        }
        (AgentKind::Agy | AgentKind::Codex, SetupScope::Local)
        | (AgentKind::Codex, SetupScope::Global) => vec![skill_file],
    }
}

/// MCP config files for `agent`, relative to the scope's `base` directory.
pub(crate) fn mcp_targets(
    agent: AgentKind,
    scope: SetupScope,
    base: &Path,
) -> Vec<(PathBuf, ActionKind)> {
    let json = |rel: &str, include_http_type: bool| {
        (
            base.join(rel),
            ActionKind::MergeJsonMcp { include_http_type },
        )
    };
    match (agent, scope) {
        (AgentKind::Claude, SetupScope::Local) => vec![json(".mcp.json", true)],
        (AgentKind::Claude, SetupScope::Global) => vec![json(".claude.json", true)],
        (AgentKind::Agy, SetupScope::Local) => vec![
            (
                base.join(".agents/plugins/citadel/plugin.json"),
                ActionKind::WriteAgyPluginManifest,
            ),
            json(".agents/plugins/citadel/mcp_config.json", false),
        ],
        (AgentKind::Agy, SetupScope::Global) => vec![json(".gemini/config/mcp_config.json", false)],
        (AgentKind::Codex, _) => vec![(base.join(".codex/config.toml"), ActionKind::MergeTomlMcp)],
    }
}

fn push_action(
    actions: &mut Vec<PlannedAction>,
    seen_paths: &mut HashSet<PathBuf>,
    path: PathBuf,
    kind: ActionKind,
) {
    if !seen_paths.insert(path.clone()) {
        return;
    }

    let (exists, would_overwrite_citadel) = inspect_existing(&path, &kind);
    actions.push(PlannedAction {
        path,
        kind,
        exists,
        would_overwrite_citadel,
    });
}

fn inspect_existing(path: &Path, kind: &ActionKind) -> (bool, bool) {
    let exists = std::fs::symlink_metadata(path).is_ok();
    if !exists {
        return (false, false);
    }

    let conflict = match kind {
        ActionKind::WriteSkillFile
        | ActionKind::CreateSkillSymlink { .. }
        | ActionKind::WriteAgyPluginManifest => true,
        ActionKind::MergeJsonMcp { .. } => std::fs::read_to_string(path)
            .ok()
            .is_some_and(|raw| json_has_citadel_entry(&raw)),
        ActionKind::MergeTomlMcp => std::fs::read_to_string(path)
            .ok()
            .is_some_and(|raw| toml_has_citadel_entry(&raw)),
    };
    (true, conflict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn resolves_all_six_agent_scope_combinations() {
        let cwd = tempdir().expect("cwd tempdir");
        let home = tempdir().expect("home tempdir");

        // 1. Claude Local
        let claude_local = build_plan(
            cwd.path(),
            Some(home.path()),
            &[AgentKind::Claude],
            SetupScope::Local,
            SetupComponents::Both,
            McpTransport::Stdio,
        )
        .expect("claude local plan");
        assert_eq!(
            claude_local
                .actions
                .iter()
                .map(|a| (a.path.clone(), a.kind.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    cwd.path().join(".claude/skills/citadel/SKILL.md"),
                    ActionKind::WriteSkillFile
                ),
                (
                    cwd.path().join(".mcp.json"),
                    ActionKind::MergeJsonMcp {
                        include_http_type: true
                    }
                ),
            ]
        );

        // 2. Claude Global
        let claude_global = build_plan(
            cwd.path(),
            Some(home.path()),
            &[AgentKind::Claude],
            SetupScope::Global,
            SetupComponents::Both,
            McpTransport::Stdio,
        )
        .expect("claude global plan");
        let canonical_skill_dir = home.path().join(".agents/skills/citadel");
        assert_eq!(
            claude_global
                .actions
                .iter()
                .map(|a| (a.path.clone(), a.kind.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    canonical_skill_dir.join("SKILL.md"),
                    ActionKind::WriteSkillFile
                ),
                (
                    home.path().join(".claude/skills/citadel"),
                    ActionKind::CreateSkillSymlink {
                        target: canonical_skill_dir.clone()
                    }
                ),
                (
                    home.path().join(".claude.json"),
                    ActionKind::MergeJsonMcp {
                        include_http_type: true
                    }
                ),
            ]
        );

        // 3. Antigravity Local
        let agy_local = build_plan(
            cwd.path(),
            Some(home.path()),
            &[AgentKind::Agy],
            SetupScope::Local,
            SetupComponents::Both,
            McpTransport::Stdio,
        )
        .expect("agy local plan");
        assert_eq!(
            agy_local
                .actions
                .iter()
                .map(|a| (a.path.clone(), a.kind.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    cwd.path().join(".agents/skills/citadel/SKILL.md"),
                    ActionKind::WriteSkillFile
                ),
                (
                    cwd.path().join(".agents/plugins/citadel/plugin.json"),
                    ActionKind::WriteAgyPluginManifest
                ),
                (
                    cwd.path().join(".agents/plugins/citadel/mcp_config.json"),
                    ActionKind::MergeJsonMcp {
                        include_http_type: false
                    }
                ),
            ]
        );

        // 4. Antigravity Global
        let agy_global = build_plan(
            cwd.path(),
            Some(home.path()),
            &[AgentKind::Agy],
            SetupScope::Global,
            SetupComponents::Both,
            McpTransport::Stdio,
        )
        .expect("agy global plan");
        assert_eq!(
            agy_global
                .actions
                .iter()
                .map(|a| (a.path.clone(), a.kind.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    canonical_skill_dir.join("SKILL.md"),
                    ActionKind::WriteSkillFile
                ),
                (
                    home.path().join(".gemini/config/skills/citadel"),
                    ActionKind::CreateSkillSymlink {
                        target: canonical_dir_clone(&canonical_skill_dir)
                    }
                ),
                (
                    home.path().join(".gemini/config/mcp_config.json"),
                    ActionKind::MergeJsonMcp {
                        include_http_type: false
                    }
                ),
            ]
        );

        // 5. Codex Local
        let codex_local = build_plan(
            cwd.path(),
            Some(home.path()),
            &[AgentKind::Codex],
            SetupScope::Local,
            SetupComponents::Both,
            McpTransport::Stdio,
        )
        .expect("codex local plan");
        assert_eq!(
            codex_local
                .actions
                .iter()
                .map(|a| (a.path.clone(), a.kind.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    cwd.path().join(".agents/skills/citadel/SKILL.md"),
                    ActionKind::WriteSkillFile
                ),
                (
                    cwd.path().join(".codex/config.toml"),
                    ActionKind::MergeTomlMcp
                ),
            ]
        );

        // 6. Codex Global
        let codex_global = build_plan(
            cwd.path(),
            Some(home.path()),
            &[AgentKind::Codex],
            SetupScope::Global,
            SetupComponents::Both,
            McpTransport::Stdio,
        )
        .expect("codex global plan");
        assert_eq!(
            codex_global
                .actions
                .iter()
                .map(|a| (a.path.clone(), a.kind.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    home.path().join(".agents/skills/citadel/SKILL.md"),
                    ActionKind::WriteSkillFile
                ),
                (
                    home.path().join(".codex/config.toml"),
                    ActionKind::MergeTomlMcp
                ),
            ]
        );
    }

    fn canonical_dir_clone(p: &Path) -> PathBuf {
        p.to_path_buf()
    }
}
