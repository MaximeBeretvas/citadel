pub mod agents;
pub mod merge;

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Result, VaultError};
use crate::fsutil::{
    create_dir_symlink, remove_existing_path, resolves_to_same_dir, write_file_atomic,
};
use crate::setup::agents::{
    build_plan, ActionKind, AgentKind, McpTransport, PlannedAction, SetupComponents, SetupPlan,
    SetupScope,
};
use crate::setup::merge::{agy_plugin_manifest_json, merge_json_mcp_config, merge_toml_mcp_config};
use crate::skill::ROUTER_SKILL_MD;

/// Default local Streamable HTTP MCP endpoint served by `citadel serve`.
pub const DEFAULT_HTTP_MCP_URL: &str = "http://127.0.0.1:4317/mcp";

/// Options for non-interactive `citadel setup`.
#[derive(Debug, Clone)]
pub struct NonInteractiveSetupOpts {
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    pub agents: Vec<AgentKind>,
    pub scope: SetupScope,
    pub components: SetupComponents,
    pub transport: Option<McpTransport>,
    pub probe_url: String,
    pub force: bool,
}

/// Probe `probe_url` with a short timeout; returns `Http(probe_url)` if it answers MCP, else `Stdio`.
///
/// The probe is a JSON-RPC `ping` POST, the same kind of request the agent will send: a GET
/// can succeed where every MCP POST is refused (e.g. `serve` returns 403 for a non-loopback
/// `Host`).
pub async fn detect_default_transport(probe_url: &str) -> McpTransport {
    if answers_mcp_ping(probe_url).await {
        McpTransport::Http(probe_url.to_string())
    } else {
        McpTransport::Stdio
    }
}

async fn answers_mcp_ping(probe_url: &str) -> bool {
    let ping = r#"{"jsonrpc":"2.0","id":"citadel-setup-probe","method":"ping"}"#;
    mcp_post(probe_url, ping.to_string(), Duration::from_millis(500))
        .await
        .is_some()
}

/// POST one JSON-RPC request to an MCP Streamable HTTP endpoint. Returns the reply only
/// when the server answers 2xx with a JSON-RPC 2.0 `result`; any failure yields `None`.
pub(crate) async fn mcp_post(
    url: &str,
    body: String,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let client = reqwest::Client::builder().timeout(timeout).build().ok()?;
    let resp = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .body(body)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let reply = serde_json::from_str::<serde_json::Value>(&resp.text().await.ok()?).ok()?;
    (reply.get("jsonrpc").and_then(|v| v.as_str()) == Some("2.0") && reply.get("result").is_some())
        .then_some(reply)
}

/// Run `citadel setup` non-interactively using explicit options.
pub async fn run_non_interactive(opts: NonInteractiveSetupOpts) -> Result<ApplyReport> {
    let transport = match opts.transport {
        Some(t) => t,
        None if opts.components.includes_mcp() => detect_default_transport(&opts.probe_url).await,
        None => McpTransport::Stdio,
    };

    let plan = build_plan(
        &opts.cwd,
        opts.home.as_deref(),
        &opts.agents,
        opts.scope,
        opts.components,
        transport,
    )?;

    apply_plan(&plan, opts.force)
}

/// Run the interactive `cliclack` setup wizard.
pub async fn run_interactive_wizard(
    cwd: &Path,
    home: Option<&Path>,
    probe_url: &str,
) -> Result<Option<ApplyReport>> {
    cliclack::intro("Citadel Setup")?;

    let agents: Vec<AgentKind> = cliclack::multiselect("Select target AI agent(s)")
        .item(AgentKind::Claude, AgentKind::Claude.display_name(), "")
        .item(AgentKind::Agy, AgentKind::Agy.display_name(), "")
        .item(AgentKind::Codex, AgentKind::Codex.display_name(), "")
        .required(true)
        .interact()?;

    let scope: SetupScope = cliclack::select("Select installation scope")
        .item(
            SetupScope::Local,
            "Local (current folder)",
            cwd.display().to_string(),
        )
        .item(
            SetupScope::Global,
            "Global (user home config)",
            home.map(|h| h.display().to_string())
                .unwrap_or_else(|| "unavailable".to_string()),
        )
        .interact()?;

    let components: SetupComponents = cliclack::select("Select components to configure")
        .item(
            SetupComponents::Both,
            "Skill + MCP",
            "Recommended — installs SKILL.md and MCP server config",
        )
        .item(SetupComponents::Skill, "Skill only", "Installs SKILL.md")
        .item(
            SetupComponents::Mcp,
            "MCP only",
            "Merges citadel into agent MCP config",
        )
        .interact()?;

    let transport = if components.includes_mcp() {
        let detected = detect_default_transport(probe_url).await;
        let default_is_http = matches!(detected, McpTransport::Http(_));
        let http_hint = if default_is_http {
            format!("detected reachable at {probe_url}")
        } else {
            format!("connect to running `citadel serve` ({probe_url})")
        };
        let stdio_hint = if default_is_http {
            "spawn `citadel mcp` per session"
        } else {
            "recommended — spawn `citadel mcp` per session"
        };

        let choice: &str = cliclack::select("Select MCP transport")
            .initial_value(if default_is_http { "http" } else { "stdio" })
            .item("stdio", "stdio (citadel mcp)", stdio_hint)
            .item("http", "Streamable HTTP (/mcp)", http_hint)
            .interact()?;

        if choice == "http" {
            let url: String = cliclack::input("HTTP MCP endpoint URL")
                .default_input(probe_url)
                .interact()?;
            McpTransport::Http(url.trim().to_string())
        } else {
            McpTransport::Stdio
        }
    } else {
        McpTransport::Stdio
    };

    let plan = build_plan(cwd, home, &agents, scope, components, transport)?;

    let summary = format_plan_summary(&plan);
    cliclack::note("Planned changes", summary)?;

    let proceed = cliclack::confirm("Apply these changes?")
        .initial_value(true)
        .interact()?;
    if !proceed {
        cliclack::outro_cancel("Setup cancelled.")?;
        return Ok(None);
    }

    let force = if plan.has_conflicts() {
        let overwrite =
            cliclack::confirm("Existing Citadel entries will be overwritten. Continue?")
                .initial_value(false)
                .interact()?;
        if !overwrite {
            cliclack::outro_cancel("Setup cancelled — existing files untouched.")?;
            return Ok(None);
        }
        true
    } else {
        false
    };

    let report = apply_plan(&plan, force)?;
    cliclack::outro(format!(
        "Citadel setup complete ({} path{} updated).",
        report.written_paths.len(),
        if report.written_paths.len() == 1 {
            ""
        } else {
            "s"
        }
    ))?;
    Ok(Some(report))
}

fn format_plan_summary(plan: &SetupPlan) -> String {
    plan.actions
        .iter()
        .map(|a| {
            let status = if a.would_overwrite_citadel {
                "overwrite existing citadel entry"
            } else if a.exists {
                "merge into existing file"
            } else {
                "create"
            };
            format!("• {} ({status})", a.path.display())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

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

    // Merge every MCP config in memory first: a file that cannot be parsed fails the
    // run before anything is written, so setup never stops half-applied.
    let mut merged_configs = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        let merged = match &action.kind {
            ActionKind::MergeJsonMcp { include_http_type } => {
                let existing = read_optional_file(&action.path)?;
                Some(
                    merge_json_mcp_config(
                        existing.as_deref(),
                        &plan.transport,
                        *include_http_type,
                        force,
                    )
                    .map_err(|err| with_path_context(err, &action.path))?,
                )
            }
            ActionKind::MergeTomlMcp => {
                let existing = read_optional_file(&action.path)?;
                Some(
                    merge_toml_mcp_config(existing.as_deref(), &plan.transport, force)
                        .map_err(|err| with_path_context(err, &action.path))?,
                )
            }
            _ => None,
        };
        merged_configs.push(merged);
    }

    let mut written_paths = Vec::with_capacity(plan.actions.len());
    for (action, merged) in plan.actions.iter().zip(merged_configs) {
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
                // The agent's skills dir may itself link to the shared one, so this path
                // already reaches the skill: replacing it would delete the skill instead.
                if !resolves_to_same_dir(&action.path, target) {
                    if force {
                        remove_existing_path(&action.path)?;
                    }
                    create_dir_symlink(target, &action.path)?;
                }
            }
            ActionKind::WriteAgyPluginManifest => {
                if force {
                    remove_existing_path(&action.path)?;
                }
                std::fs::write(&action.path, agy_plugin_manifest_json())?;
            }
            ActionKind::MergeJsonMcp { .. } | ActionKind::MergeTomlMcp => {
                if let Some(merged) = &merged {
                    write_file_atomic(&action.path, merged)?;
                }
            }
        }
        written_paths.push(action.path.clone());
    }

    Ok(ApplyReport { written_paths })
}

/// Refuse to replace an existing skill file, symlink or manifest. MCP configs are
/// checked by the merge functions, which only object to an existing `citadel` entry.
fn check_no_conflict(action: &PlannedAction) -> Result<()> {
    match &action.kind {
        ActionKind::MergeJsonMcp { .. } | ActionKind::MergeTomlMcp => Ok(()),
        _ if std::fs::symlink_metadata(&action.path).is_ok() => {
            Err(VaultError::Validation(format!(
                "target already exists at {}; pass --force to overwrite",
                action.path.display()
            )))
        }
        _ => Ok(()),
    }
}

fn with_path_context(err: VaultError, path: &Path) -> VaultError {
    match err {
        VaultError::Validation(msg) => VaultError::Validation(format!("{}: {msg}", path.display())),
        other => other,
    }
}

fn read_optional_file(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
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

    #[cfg(unix)]
    #[test]
    fn apply_plan_rewrites_claude_json_atomically_and_keeps_order_mode_and_symlink() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let cwd = tempdir().expect("cwd tempdir");
        let home = tempdir().expect("home tempdir");
        let dotfiles = home.path().join("dotfiles");
        std::fs::create_dir_all(&dotfiles).expect("mkdir dotfiles");
        let real_config = dotfiles.join("claude.json");
        std::fs::write(
            &real_config,
            "{\n  \"zeta\": 1,\n  \"mcpServers\": {\"other\": {\"command\": \"x\"}},\n  \"alpha\": 2\n}\n",
        )
        .expect("seed config");
        std::fs::set_permissions(&real_config, std::fs::Permissions::from_mode(0o600))
            .expect("chmod 600");
        let link = home.path().join(".claude.json");
        std::os::unix::fs::symlink(&real_config, &link).expect("symlink config");
        let inode_before = std::fs::metadata(&real_config).expect("meta").ino();

        let plan = build_plan(
            cwd.path(),
            Some(home.path()),
            &[AgentKind::Claude],
            SetupScope::Global,
            SetupComponents::Mcp,
            McpTransport::Stdio,
        )
        .expect("plan");
        apply_plan(&plan, false).expect("apply");

        assert!(std::fs::symlink_metadata(&link)
            .expect("link meta")
            .file_type()
            .is_symlink());
        let meta = std::fs::metadata(&real_config).expect("meta after");
        assert_ne!(
            meta.ino(),
            inode_before,
            "config must be replaced by rename"
        );
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);

        let text = std::fs::read_to_string(&real_config).expect("read");
        let pos = |k: &str| text.find(k).unwrap_or_else(|| panic!("missing {k}"));
        assert!(pos("\"zeta\"") < pos("\"mcpServers\""));
        assert!(pos("\"mcpServers\"") < pos("\"alpha\""));
        assert!(pos("\"other\"") < pos("\"citadel\""));
        let leftovers: Vec<_> = std::fs::read_dir(&dotfiles)
            .expect("read dotfiles")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("claude.json")]);
    }

    #[cfg(unix)]
    #[test]
    fn global_setup_keeps_the_skill_when_the_agent_skills_dir_links_to_the_shared_dir() {
        let cwd = tempdir().expect("cwd tempdir");
        let home = tempdir().expect("home tempdir");
        // A common way to share skills between agents: ~/.claude/skills -> ~/.agents/skills.
        let shared = home.path().join(".agents/skills");
        std::fs::create_dir_all(&shared).expect("mkdir shared skills");
        std::fs::create_dir_all(home.path().join(".claude")).expect("mkdir .claude");
        std::os::unix::fs::symlink(&shared, home.path().join(".claude/skills"))
            .expect("link agent skills dir");

        let plan = || {
            build_plan(
                cwd.path(),
                Some(home.path()),
                &[AgentKind::Claude],
                SetupScope::Global,
                SetupComponents::Both,
                McpTransport::Stdio,
            )
            .expect("plan")
        };
        let canonical = shared.join("citadel");
        let assert_skill_intact = || {
            assert!(
                std::fs::symlink_metadata(&canonical)
                    .expect("canonical skill dir")
                    .file_type()
                    .is_dir(),
                "the canonical skill dir must stay a real directory"
            );
            assert_eq!(
                std::fs::read_to_string(canonical.join("SKILL.md")).expect("read SKILL.md"),
                ROUTER_SKILL_MD
            );
            assert_eq!(
                std::fs::read_to_string(home.path().join(".claude/skills/citadel/SKILL.md"))
                    .expect("read SKILL.md through the agent dir"),
                ROUTER_SKILL_MD
            );
        };

        apply_plan(&plan(), false).expect("first run must not stop half-applied");
        assert_skill_intact();
        assert!(
            home.path().join(".claude.json").is_file(),
            "the MCP config must be merged"
        );

        // A forced re-run (or the wizard's overwrite confirmation) must not delete the skill.
        apply_plan(&plan(), true).expect("forced re-run");
        assert_skill_intact();
    }

    #[test]
    fn apply_plan_writes_nothing_when_a_later_config_cannot_be_merged() {
        let cwd = tempdir().expect("cwd tempdir");
        let home = tempdir().expect("home tempdir");
        let codex_config = home.path().join(".codex/config.toml");
        std::fs::create_dir_all(codex_config.parent().expect("parent")).expect("mkdir .codex");
        std::fs::write(&codex_config, "[mcp_servers\nbroken = ").expect("seed broken toml");

        let plan = build_plan(
            cwd.path(),
            Some(home.path()),
            &[AgentKind::Claude, AgentKind::Codex],
            SetupScope::Global,
            SetupComponents::Both,
            McpTransport::Stdio,
        )
        .expect("plan");
        let err = apply_plan(&plan, false).expect_err("broken codex config must fail");
        assert!(matches!(err, VaultError::Validation(_)));

        assert!(!home.path().join(".claude.json").exists());
        assert!(!home.path().join(".agents").exists());
        assert!(!home.path().join(".claude").exists());
        assert_eq!(
            std::fs::read_to_string(&codex_config).expect("read codex"),
            "[mcp_servers\nbroken = "
        );
    }

    /// Serve `app` on an ephemeral loopback port.
    async fn spawn_router(
        app: axum::Router,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral listener");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, server)
    }

    #[tokio::test]
    async fn detect_transport_returns_http_when_endpoint_reachable_and_stdio_when_closed() {
        let vault = tempdir().expect("vault tempdir");
        let (addr, server) = spawn_router(crate::ui::build_serve_router(
            crate::config::VaultConfig::new(vault.path()),
        ))
        .await;

        let live_url = format!("http://{addr}/mcp");
        assert_eq!(
            detect_default_transport(&live_url).await,
            McpTransport::Http(live_url),
            "`citadel serve` answers the probe"
        );
        server.abort();

        // Bind then drop to obtain an unused ephemeral port.
        let closed_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind closed listener");
        let closed_addr = closed_listener.local_addr().expect("closed addr");
        drop(closed_listener);

        let dead_url = format!("http://{closed_addr}/mcp");
        assert_eq!(
            detect_default_transport(&dead_url).await,
            McpTransport::Stdio
        );
    }

    #[tokio::test]
    async fn detect_transport_returns_stdio_on_404_response() {
        let (addr, server) = spawn_router(axum::Router::new().route(
            "/mcp",
            axum::routing::any(|| async { axum::http::StatusCode::NOT_FOUND }),
        ))
        .await;

        let ui_only_url = format!("http://{addr}/mcp");
        assert_eq!(
            detect_default_transport(&ui_only_url).await,
            McpTransport::Stdio
        );
        server.abort();
    }

    #[tokio::test]
    async fn detect_transport_returns_stdio_when_the_endpoint_refuses_mcp_requests() {
        // Like `citadel serve` behind a LAN address: GET answers, every MCP POST gets 403.
        let (addr, server) = spawn_router(
            axum::Router::new().route(
                "/mcp",
                axum::routing::get(|| async { axum::Json(serde_json::json!({"name": "citadel"})) })
                    .post(|| async { axum::http::StatusCode::FORBIDDEN }),
            ),
        )
        .await;
        let refusing_url = format!("http://{addr}/mcp");
        assert_eq!(
            detect_default_transport(&refusing_url).await,
            McpTransport::Stdio
        );
        server.abort();

        // Any web server that answers 200 without speaking JSON-RPC.
        let (addr, server) = spawn_router(axum::Router::new().fallback(|| async {
            axum::response::Html("<html><body>not an MCP server</body></html>")
        }))
        .await;
        let html_url = format!("http://{addr}/mcp");
        assert_eq!(
            detect_default_transport(&html_url).await,
            McpTransport::Stdio
        );
        server.abort();
    }

    #[tokio::test]
    async fn non_interactive_setup_creates_claude_and_codex_local_configs() {
        let cwd = tempdir().expect("cwd tempdir");
        let home = tempdir().expect("home tempdir");

        let report = run_non_interactive(NonInteractiveSetupOpts {
            cwd: cwd.path().to_path_buf(),
            home: Some(home.path().to_path_buf()),
            agents: vec![AgentKind::Claude, AgentKind::Codex],
            scope: SetupScope::Local,
            components: SetupComponents::Both,
            transport: Some(McpTransport::Stdio),
            probe_url: DEFAULT_HTTP_MCP_URL.to_string(),
            force: false,
        })
        .await
        .expect("non-interactive setup should succeed");

        assert_eq!(report.written_paths.len(), 4);
        assert!(cwd.path().join(".claude/skills/citadel/SKILL.md").is_file());
        assert!(cwd.path().join(".mcp.json").is_file());
        assert!(cwd.path().join(".agents/skills/citadel/SKILL.md").is_file());
        assert!(cwd.path().join(".codex/config.toml").is_file());
    }

    #[tokio::test]
    async fn non_interactive_setup_fails_on_conflict_without_force_and_succeeds_with_force() {
        let cwd = tempdir().expect("cwd tempdir");
        let home = tempdir().expect("home tempdir");

        let make_opts = |force: bool, transport: McpTransport| NonInteractiveSetupOpts {
            cwd: cwd.path().to_path_buf(),
            home: Some(home.path().to_path_buf()),
            agents: vec![AgentKind::Claude],
            scope: SetupScope::Local,
            components: SetupComponents::Both,
            transport: Some(transport),
            probe_url: DEFAULT_HTTP_MCP_URL.to_string(),
            force,
        };

        run_non_interactive(make_opts(false, McpTransport::Stdio))
            .await
            .expect("first setup succeeds");

        let err = run_non_interactive(make_opts(false, McpTransport::Stdio))
            .await
            .expect_err("re-run without force must fail");
        assert!(matches!(err, VaultError::Validation(_)));

        run_non_interactive(make_opts(
            true,
            McpTransport::Http("http://127.0.0.1:4317/mcp".to_string()),
        ))
        .await
        .expect("re-run with force succeeds");

        let mcp: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(cwd.path().join(".mcp.json")).expect("read .mcp.json"),
        )
        .expect("parse .mcp.json");
        assert_eq!(mcp["mcpServers"]["citadel"]["type"], "http");
        assert_eq!(
            mcp["mcpServers"]["citadel"]["url"],
            "http://127.0.0.1:4317/mcp"
        );
    }
}
