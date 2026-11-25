//! `citadel doctor`: read-only diagnostics over the installation and vault.

pub mod report;
pub mod running;
pub mod setup;
pub mod version;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::VaultConfig;
use report::{Check, Report, Section};
use running::ServeProbe;
use setup::{McpEntry, SetupLocation};

/// Inputs doctor reads from its environment, so tests and the CLI can set them.
#[derive(Debug, Clone)]
pub struct DoctorOpts {
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
    pub current_exe: PathBuf,
    pub releases_api_url: String,
    pub serve_url: String,
    pub live_release_check: bool,
    pub in_container: bool,
}

/// What doctor read at one setup location.
struct Inspected {
    loc: SetupLocation,
    skill: setup::SkillState,
    mcp: std::result::Result<Option<McpEntry>, String>,
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Run every check and collect the report. Read-only: creates no file or directory.
pub async fn run_doctor(config: &VaultConfig, opts: &DoctorOpts) -> Report {
    let current = env!("CARGO_PKG_VERSION");
    let mut checks = Vec::new();

    // Gather what the checks need.
    let service = if opts.in_container {
        None
    } else {
        Some(running::gather_service_state())
    };
    let service_installed = matches!(service, Some(running::ServiceState::Installed { .. }));
    let home_is_cwd = opts
        .home
        .as_deref()
        .is_some_and(|home| same_dir(home, &opts.cwd));
    let inspected: Vec<Inspected> = setup::setup_locations(&opts.cwd, opts.home.as_deref())
        .into_iter()
        .filter(|loc| !(home_is_cwd && loc.scope == crate::setup::agents::SetupScope::Local))
        .map(|loc| Inspected {
            skill: setup::inspect_skill(&loc.skill_path),
            mcp: setup::read_mcp_entry(&loc.mcp_path, loc.mcp_is_toml),
            loc,
        })
        .collect();

    // Probe each distinct HTTP URL once.
    let mut probes: HashMap<String, ServeProbe> = HashMap::new();
    let urls =
        std::iter::once(opts.serve_url.clone()).chain(inspected.iter().filter_map(
            |i| match &i.mcp {
                Ok(Some(McpEntry::Http { url })) => Some(url.clone()),
                _ => None,
            },
        ));
    for url in urls {
        if let std::collections::hash_map::Entry::Vacant(slot) = probes.entry(url) {
            let probe = running::probe_serve(slot.key()).await;
            slot.insert(probe);
        }
    }
    let serve_probe = probes
        .get(&opts.serve_url)
        .cloned()
        .unwrap_or(ServeProbe::NotAnswering);
    let http_entry_points_here = inspected
        .iter()
        .any(|i| matches!(&i.mcp, Ok(Some(McpEntry::Http { url })) if *url == opts.serve_url));

    // Version.
    let latest = version::latest_release(
        &config.update_check_path(),
        &opts.releases_api_url,
        current,
        opts.live_release_check,
    )
    .await;
    checks.push(version::cli_version_check(current, &latest));
    let daemon_facts = if opts.in_container {
        None
    } else {
        Some(version::gather_daemon_facts(config, service_installed))
    };
    if let Some(Ok(facts)) = &daemon_facts {
        checks.extend(version::daemon_version_checks(current, facts));
    }
    checks.extend(version::serve_version_check(current, &serve_probe));

    // Running.
    match daemon_facts {
        Some(Ok(facts)) => {
            checks.push(running::daemon_running_check(
                facts.status,
                &config.pid_path(),
            ));
        }
        Some(Err(err)) => checks.push(
            Check::fail(
                Section::Running,
                "daemon",
                format!("cannot read daemon state: {err}"),
            )
            .with_hint(&format!(
                "check permissions on {}, then run citadel daemon status",
                config.pid_path().display()
            )),
        ),
        None => {}
    }
    if let Some(service) = &service {
        checks.extend(running::service_checks(
            service,
            &config.data_dir,
            &opts.current_exe,
        ));
        checks.push(running::process_check(&running::gather_processes()));
    } else {
        checks.push(Check::info(
            Section::Running,
            "container",
            "running in a container: serve is the daemon",
        ));
    }
    checks.push(running::serve_check(
        &opts.serve_url,
        &serve_probe,
        http_entry_points_here,
    ));

    // Setup.
    if home_is_cwd {
        checks.push(Check::info(
            Section::Setup,
            "local scope",
            "local scope skipped: current directory is the home directory",
        ));
    } else {
        checks.push(setup::local_scope_note(&opts.cwd));
    }
    let path_env = std::env::var_os("PATH");
    for item in &inspected {
        let stdio = match &item.mcp {
            Ok(Some(McpEntry::Stdio { command, .. })) => Some(setup::resolve_stdio_command(
                command,
                path_env.as_deref(),
                &opts.current_exe,
            )),
            _ => None,
        };
        let http = match &item.mcp {
            Ok(Some(McpEntry::Http { url })) => probes.get(url),
            _ => None,
        };
        checks.extend(setup::setup_checks(
            &item.loc,
            &item.skill,
            &item.mcp,
            stdio.as_ref(),
            http,
            current,
        ));
    }

    // Data.
    let mut data = format!("data dir {}", config.data_dir.display());
    if !config.data_dir.exists() {
        data.push_str(" (not created yet)");
    }
    checks.push(Check::info(Section::Data, "data dir", data));

    Report { checks }
}
