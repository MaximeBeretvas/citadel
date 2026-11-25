//! Setup checks: where the Citadel skill and MCP entry are configured per agent and scope.
//!
//! Read-only: nothing here creates or modifies a file.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::report::{Check, Section};
use super::running::ServeProbe;
use crate::setup::agents::{mcp_targets, skill_targets, ActionKind, AgentKind, SetupScope};
use crate::skill::ROUTER_SKILL_MD;

const AGENTS: [AgentKind; 3] = [AgentKind::Claude, AgentKind::Agy, AgentKind::Codex];
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// Where one agent keeps its Citadel skill and MCP entry in one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupLocation {
    pub agent: AgentKind,
    pub scope: SetupScope,
    /// The last skill target: the symlink where one exists, else the `SKILL.md` file.
    pub skill_path: PathBuf,
    pub mcp_path: PathBuf,
    pub mcp_is_toml: bool,
}

/// Every agent x scope location; global ones only when `home` is known.
pub fn setup_locations(cwd: &Path, home: Option<&Path>) -> Vec<SetupLocation> {
    let mut out = Vec::new();
    for (scope, base) in [(SetupScope::Local, Some(cwd)), (SetupScope::Global, home)] {
        let Some(base) = base else { continue };
        for agent in AGENTS {
            let skill_path = skill_targets(agent, scope, base)
                .pop()
                .map(|(path, _)| path);
            let mcp = mcp_targets(agent, scope, base)
                .into_iter()
                .find(|(_, kind)| {
                    matches!(
                        kind,
                        ActionKind::MergeJsonMcp { .. } | ActionKind::MergeTomlMcp
                    )
                });
            if let (Some(skill_path), Some((mcp_path, kind))) = (skill_path, mcp) {
                out.push(SetupLocation {
                    agent,
                    scope,
                    skill_path,
                    mcp_path,
                    mcp_is_toml: kind == ActionKind::MergeTomlMcp,
                });
            }
        }
    }
    out
}

/// What is installed at a skill path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillState {
    Missing,
    Present {
        path: PathBuf,
        link_target: Option<PathBuf>,
        outdated: bool,
    },
    Dangling {
        path: PathBuf,
        link_target: PathBuf,
    },
}

/// Inspect the skill at `path` (a `SKILL.md` file or a directory symlink holding one).
pub fn inspect_skill(path: &Path) -> SkillState {
    let Ok(link_meta) = std::fs::symlink_metadata(path) else {
        return SkillState::Missing;
    };
    let link_target = if link_meta.file_type().is_symlink() {
        std::fs::read_link(path).ok()
    } else {
        None
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return SkillState::Dangling {
            path: path.to_path_buf(),
            link_target: link_target.unwrap_or_default(),
        };
    };
    let file = if meta.is_dir() {
        path.join("SKILL.md")
    } else {
        path.to_path_buf()
    };
    let outdated = std::fs::read_to_string(file).map_or(true, |text| text != ROUTER_SKILL_MD);
    SkillState::Present {
        path: path.to_path_buf(),
        link_target,
        outdated,
    }
}

/// The `citadel` MCP server entry of an agent config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpEntry {
    Stdio { command: String, args: Vec<String> },
    Http { url: String },
}

/// Read the `citadel` entry: `Ok(None)` for a missing or empty file or no entry,
/// `Err(message)` when the file does not parse.
pub fn read_mcp_entry(path: &Path, toml: bool) -> std::result::Result<Option<McpEntry>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let server = if toml {
        toml_server(&raw)?
    } else {
        json_server(&raw)?
    };
    let Some((url, command, args)) = server else {
        return Ok(None);
    };
    match (url, command) {
        (Some(url), _) => Ok(Some(McpEntry::Http { url })),
        (None, Some(command)) => Ok(Some(McpEntry::Stdio { command, args })),
        (None, None) => Err("citadel entry has neither `command` nor `url`".to_string()),
    }
}

type RawServer = Option<(Option<String>, Option<String>, Vec<String>)>;

fn json_server(raw: &str) -> std::result::Result<RawServer, String> {
    let root: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let serde_json::Value::Object(root) = root else {
        return Err("config is not a JSON object".to_string());
    };
    let Some(server) = root.get("mcpServers").and_then(|s| s.get("citadel")) else {
        return Ok(None);
    };
    let text = |key: &str| server.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let args = server
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some((text("url"), text("command"), args)))
}

fn toml_server(raw: &str) -> std::result::Result<RawServer, String> {
    let doc: toml_edit::DocumentMut = raw
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    let Some(server) = doc
        .get("mcp_servers")
        .and_then(|s| s.get("citadel"))
        .and_then(|s| s.as_table_like())
    else {
        return Ok(None);
    };
    let text = |key: &str| server.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let args = server
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some((text("url"), text("command"), args)))
}

/// Where a stdio MCP command points, relative to the running doctor binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdioResolution {
    NotFound,
    SameAsDoctor,
    Other {
        path: PathBuf,
        version: Option<String>,
    },
}

/// Resolve `command` (absolute, or searched on `path_env`) and compare it with `current_exe`.
pub fn resolve_stdio_command(
    command: &str,
    path_env: Option<&OsStr>,
    current_exe: &Path,
) -> StdioResolution {
    let found = if command.contains(std::path::MAIN_SEPARATOR) {
        Some(PathBuf::from(command))
    } else {
        path_env.and_then(|paths| {
            std::env::split_paths(paths)
                .map(|dir| dir.join(command))
                .find(|candidate| candidate.is_file())
        })
    };
    let Some(resolved) = found.and_then(|p| std::fs::canonicalize(p).ok()) else {
        return StdioResolution::NotFound;
    };
    let current = std::fs::canonicalize(current_exe).unwrap_or_else(|_| current_exe.to_path_buf());
    if resolved == current {
        return StdioResolution::SameAsDoctor;
    }
    let version = binary_version(&resolved);
    StdioResolution::Other {
        path: resolved,
        version,
    }
}

/// Run `<path> --version` with a hard timeout and parse `citadel X.Y.Z`.
fn binary_version(path: &Path) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + VERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    out.trim()
        .strip_prefix("citadel ")
        .map(|v| v.trim().to_string())
}

fn scope_label(scope: SetupScope) -> &'static str {
    match scope {
        SetupScope::Local => "local",
        SetupScope::Global => "global",
    }
}

fn agent_cli_name(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "claude",
        AgentKind::Agy => "agy",
        AgentKind::Codex => "codex",
    }
}

/// Turn the inspected state of one agent x scope location into report rows.
pub fn setup_checks(
    loc: &SetupLocation,
    skill: &SkillState,
    mcp: &std::result::Result<Option<McpEntry>, String>,
    stdio: Option<&StdioResolution>,
    http: Option<&ServeProbe>,
    current: &str,
) -> Vec<Check> {
    let agent = loc.agent.display_name();
    let scope = scope_label(loc.scope);
    let prefix = format!("{agent}  {scope}");
    let mut rows = Vec::new();

    if matches!(skill, SkillState::Missing) && matches!(mcp, Ok(None)) {
        return vec![Check::info(
            Section::Setup,
            "setup",
            format!("{agent} {scope} not configured"),
        )];
    }

    let skill_hint = format!(
        "citadel setup --agent {} --scope {scope} --only skill --force",
        agent_cli_name(loc.agent)
    );
    match skill {
        SkillState::Missing => {}
        SkillState::Present {
            path,
            link_target,
            outdated,
        } => {
            let mut detail = format!("{prefix}  skill  {}", path.display());
            if let Some(target) = link_target {
                detail.push_str(&format!(" → {}", target.display()));
            }
            if *outdated {
                rows.push(
                    Check::warn(
                        Section::Setup,
                        "skill",
                        format!("{detail}  (outdated: differs from this binary's skill)"),
                    )
                    .with_hint(&skill_hint),
                );
            } else {
                rows.push(Check::ok(Section::Setup, "skill", detail));
            }
        }
        SkillState::Dangling { path, link_target } => rows.push(
            Check::fail(
                Section::Setup,
                "skill",
                format!(
                    "{prefix}  skill  {} → {} (target missing)",
                    path.display(),
                    link_target.display()
                ),
            )
            .with_hint(&skill_hint),
        ),
    }

    let mcp_path = loc.mcp_path.display();
    match mcp {
        Err(message) => rows.push(
            Check::fail(
                Section::Setup,
                "mcp",
                format!("{prefix}  MCP  {mcp_path}  (unreadable: {message})"),
            )
            .with_hint(&format!("fix or remove the citadel entry in {mcp_path}")),
        ),
        Ok(None) => {}
        Ok(Some(McpEntry::Stdio { command, args })) => {
            let shown = std::iter::once(command.as_str())
                .chain(args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            let base = format!("{prefix}  MCP  {mcp_path}  (stdio: {shown}");
            rows.push(match stdio {
                Some(StdioResolution::NotFound) => Check::fail(
                    Section::Setup,
                    "mcp",
                    format!("{base} — command not found)"),
                )
                .with_hint("install citadel on PATH or re-run citadel setup"),
                Some(StdioResolution::Other { path, version }) if version.as_deref() != Some(current) => {
                    let found = version.as_deref().unwrap_or("unknown version");
                    Check::warn(
                        Section::Setup,
                        "mcp",
                        format!("{base} — {} is {found}, doctor is {current})", path.display()),
                    )
                    .with_hint("update the citadel on PATH (citadel update) so the agent runs the same version")
                }
                _ => Check::ok(Section::Setup, "mcp", format!("{base})")),
            });
        }
        Ok(Some(McpEntry::Http { url })) => {
            let base = format!("{prefix}  MCP  {mcp_path}  (http: {url}");
            rows.push(match http {
                Some(ServeProbe::NotAnswering) => {
                    Check::warn(Section::Setup, "mcp", format!("{base} — not answering)"))
                        .with_hint(
                            "start it with `make run`, or re-run `citadel setup --transport stdio`",
                        )
                }
                _ => Check::ok(Section::Setup, "mcp", format!("{base})")),
            });
        }
    }
    rows
}

/// Say which directory the local scope was checked in.
pub fn local_scope_note(cwd: &Path) -> Check {
    Check::info(
        Section::Setup,
        "local scope",
        format!("local scope checked in {}", cwd.display()),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::doctor::report::CheckStatus;
    use crate::skill::ROUTER_SKILL_MD;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    fn loc(agent: AgentKind, scope: SetupScope, dir: &Path) -> SetupLocation {
        setup_locations(dir, Some(dir))
            .into_iter()
            .find(|l| l.agent == agent && l.scope == scope)
            .expect("location")
    }

    fn present() -> SkillState {
        SkillState::Present {
            path: PathBuf::from("/s/citadel"),
            link_target: None,
            outdated: false,
        }
    }

    #[test]
    fn locations_cover_three_agents_two_scopes() {
        let dir = tempdir().unwrap();
        let locs = setup_locations(dir.path(), Some(dir.path()));
        assert_eq!(locs.len(), 6);
        let claude = loc(AgentKind::Claude, SetupScope::Global, dir.path());
        assert!(claude.skill_path.ends_with(".claude/skills/citadel"));
        assert!(claude.mcp_path.ends_with(".claude.json"));
        assert!(!claude.mcp_is_toml);
        assert!(loc(AgentKind::Codex, SetupScope::Local, dir.path()).mcp_is_toml);
        let agy = loc(AgentKind::Agy, SetupScope::Local, dir.path());
        assert!(agy.mcp_path.ends_with("mcp_config.json"));
        assert_eq!(setup_locations(dir.path(), None).len(), 3);
    }

    #[test]
    fn skill_symlink_to_current_skill_is_present_not_outdated() {
        let dir = tempdir().unwrap();
        let canonical = dir.path().join("canon");
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::write(canonical.join("SKILL.md"), ROUTER_SKILL_MD).unwrap();
        let link = dir.path().join("link");
        symlink(&canonical, &link).unwrap();
        assert_eq!(
            inspect_skill(&link),
            SkillState::Present {
                path: link,
                link_target: Some(canonical),
                outdated: false
            }
        );
    }

    #[test]
    fn skill_with_old_content_is_outdated() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("SKILL.md");
        std::fs::write(&file, "old skill").unwrap();
        assert_eq!(
            inspect_skill(&file),
            SkillState::Present {
                path: file,
                link_target: None,
                outdated: true
            }
        );
    }

    #[test]
    fn missing_skill_is_missing() {
        let dir = tempdir().unwrap();
        assert_eq!(inspect_skill(&dir.path().join("nope")), SkillState::Missing);
    }

    #[test]
    fn dangling_symlink_is_dangling() {
        let dir = tempdir().unwrap();
        let link = dir.path().join("link");
        let gone = dir.path().join("gone");
        symlink(&gone, &link).unwrap();
        assert_eq!(
            inspect_skill(&link),
            SkillState::Dangling {
                path: link,
                link_target: gone
            }
        );
    }

    #[test]
    fn mcp_entry_absent_in_valid_config_is_none() {
        let dir = tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        std::fs::write(&file, r#"{"mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        assert_eq!(read_mcp_entry(&file, false), Ok(None));
        std::fs::write(&file, "").unwrap();
        assert_eq!(read_mcp_entry(&file, false), Ok(None));
        assert_eq!(read_mcp_entry(&dir.path().join("absent"), false), Ok(None));
    }

    #[test]
    fn mcp_stdio_json_and_http_json_parse() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.json");
        std::fs::write(
            &file,
            r#"{"mcpServers":{"citadel":{"command":"citadel","args":["mcp"]}}}"#,
        )
        .unwrap();
        assert_eq!(
            read_mcp_entry(&file, false),
            Ok(Some(McpEntry::Stdio {
                command: "citadel".into(),
                args: vec!["mcp".into()]
            }))
        );
        std::fs::write(
            &file,
            r#"{"mcpServers":{"citadel":{"type":"http","url":"http://127.0.0.1:4317/mcp"}}}"#,
        )
        .unwrap();
        assert_eq!(
            read_mcp_entry(&file, false),
            Ok(Some(McpEntry::Http {
                url: "http://127.0.0.1:4317/mcp".into()
            }))
        );
    }

    #[test]
    fn mcp_toml_parse() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(
            &file,
            "[mcp_servers.other]\ncommand = \"x\"\n\n[mcp_servers.citadel]\ncommand = \"citadel\"\nargs = [\"mcp\"]\n",
        )
        .unwrap();
        assert_eq!(
            read_mcp_entry(&file, true),
            Ok(Some(McpEntry::Stdio {
                command: "citadel".into(),
                args: vec!["mcp".into()]
            }))
        );
        std::fs::write(&file, "[mcp_servers.citadel]\nurl = \"http://h/mcp\"\n").unwrap();
        assert_eq!(
            read_mcp_entry(&file, true),
            Ok(Some(McpEntry::Http {
                url: "http://h/mcp".into()
            }))
        );
    }

    #[test]
    fn mcp_broken_json_is_err() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.json");
        std::fs::write(&file, "{not json").unwrap();
        assert!(read_mcp_entry(&file, false).is_err());
        std::fs::write(&file, "= nope").unwrap();
        assert!(read_mcp_entry(&file, true).is_err());
    }

    #[test]
    fn stdio_command_symlinked_to_current_exe_is_same() {
        let dir = tempdir().unwrap();
        let exe = dir.path().join("real-citadel");
        std::fs::write(&exe, "x").unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        symlink(&exe, bin.join("citadel")).unwrap();
        assert_eq!(
            resolve_stdio_command("citadel", Some(bin.as_os_str()), &exe),
            StdioResolution::SameAsDoctor
        );
    }

    #[test]
    fn stdio_command_missing_is_not_found() {
        let dir = tempdir().unwrap();
        let exe = dir.path().join("exe");
        std::fs::write(&exe, "x").unwrap();
        assert_eq!(
            resolve_stdio_command("citadel", Some(dir.path().as_os_str()), &exe),
            StdioResolution::NotFound
        );
        assert_eq!(
            resolve_stdio_command("citadel", None, &exe),
            StdioResolution::NotFound
        );
    }

    #[test]
    fn stdio_other_binary_reports_its_version() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let exe = dir.path().join("exe");
        std::fs::write(&exe, "x").unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let fake = bin.join("citadel");
        std::fs::write(&fake, "#!/bin/sh\necho 'citadel 9.9.9'\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            resolve_stdio_command("citadel", Some(bin.as_os_str()), &exe),
            StdioResolution::Other {
                path: std::fs::canonicalize(&fake).unwrap(),
                version: Some("9.9.9".into())
            }
        );
    }

    #[test]
    fn unconfigured_location_is_single_info_row() {
        let dir = tempdir().unwrap();
        let l = loc(AgentKind::Codex, SetupScope::Global, dir.path());
        let rows = setup_checks(&l, &SkillState::Missing, &Ok(None), None, None, "1.0.0");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, CheckStatus::Info);
        assert_eq!(rows[0].detail, "Codex global not configured");
    }

    #[test]
    fn broken_config_fails() {
        let dir = tempdir().unwrap();
        let l = loc(AgentKind::Claude, SetupScope::Global, dir.path());
        let rows = setup_checks(&l, &present(), &Err("bad json".into()), None, None, "1.0.0");
        let fail = rows
            .iter()
            .find(|c| c.status == CheckStatus::Fail)
            .expect("fail row");
        assert!(fail.detail.contains("bad json"));
        assert!(fail.hint.as_deref().unwrap().contains(".claude.json"));
    }

    #[test]
    fn missing_command_fails() {
        let dir = tempdir().unwrap();
        let l = loc(AgentKind::Claude, SetupScope::Global, dir.path());
        let entry = McpEntry::Stdio {
            command: "citadel".into(),
            args: vec!["mcp".into()],
        };
        let rows = setup_checks(
            &l,
            &SkillState::Missing,
            &Ok(Some(entry)),
            Some(&StdioResolution::NotFound),
            None,
            "1.0.0",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, CheckStatus::Fail);
        assert!(rows[0].detail.contains("stdio: citadel mcp"));
        assert_eq!(
            rows[0].hint.as_deref(),
            Some("install citadel on PATH or re-run citadel setup")
        );
    }

    #[test]
    fn outdated_skill_warns() {
        let dir = tempdir().unwrap();
        let l = loc(AgentKind::Claude, SetupScope::Global, dir.path());
        let skill = SkillState::Present {
            path: l.skill_path.clone(),
            link_target: None,
            outdated: true,
        };
        let rows = setup_checks(&l, &skill, &Ok(None), None, None, "1.0.0");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, CheckStatus::Warn);
        let hint = rows[0].hint.as_deref().unwrap();
        assert!(hint.contains("--only skill") && hint.contains("--force"));
    }

    #[test]
    fn dangling_skill_fails_and_healthy_skill_is_ok_with_arrow() {
        let dir = tempdir().unwrap();
        let l = loc(AgentKind::Claude, SetupScope::Global, dir.path());
        let dangling = SkillState::Dangling {
            path: l.skill_path.clone(),
            link_target: PathBuf::from("/gone"),
        };
        let rows = setup_checks(&l, &dangling, &Ok(None), None, None, "1.0.0");
        assert_eq!(rows[0].status, CheckStatus::Fail);
        assert!(rows[0].hint.is_some());
        let linked = SkillState::Present {
            path: l.skill_path.clone(),
            link_target: Some(PathBuf::from("/canon")),
            outdated: false,
        };
        let rows = setup_checks(&l, &linked, &Ok(None), None, None, "1.0.0");
        assert_eq!(rows[0].status, CheckStatus::Ok);
        assert!(rows[0].detail.contains(" → /canon"));
    }

    #[test]
    fn other_binary_version_mismatch_warns_and_http_not_answering_warns() {
        let dir = tempdir().unwrap();
        let l = loc(AgentKind::Claude, SetupScope::Local, dir.path());
        let stdio = McpEntry::Stdio {
            command: "citadel".into(),
            args: vec!["mcp".into()],
        };
        let other = StdioResolution::Other {
            path: PathBuf::from("/usr/bin/citadel"),
            version: Some("0.1.0".into()),
        };
        let rows = setup_checks(
            &l,
            &SkillState::Missing,
            &Ok(Some(stdio.clone())),
            Some(&other),
            None,
            "1.0.0",
        );
        assert_eq!(rows[0].status, CheckStatus::Warn);
        assert!(rows[0].hint.is_some());
        let same = setup_checks(
            &l,
            &SkillState::Missing,
            &Ok(Some(stdio)),
            Some(&StdioResolution::SameAsDoctor),
            None,
            "1.0.0",
        );
        assert_eq!(same[0].status, CheckStatus::Ok);
        let http = McpEntry::Http {
            url: "http://127.0.0.1:4317/mcp".into(),
        };
        let rows = setup_checks(
            &l,
            &SkillState::Missing,
            &Ok(Some(http)),
            None,
            Some(&ServeProbe::NotAnswering),
            "1.0.0",
        );
        assert_eq!(rows[0].status, CheckStatus::Warn);
        assert!(rows[0].detail.contains("not answering"));
        assert!(rows[0].hint.is_some());
    }

    #[test]
    fn local_scope_note_names_the_directory() {
        let note = local_scope_note(Path::new("/work/proj"));
        assert_eq!(note.status, CheckStatus::Info);
        assert_eq!(note.detail, "local scope checked in /work/proj");
    }
}
