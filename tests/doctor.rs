//! End-to-end checks of `citadel doctor` against a throwaway home and project directory.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Output};

use tempfile::{tempdir, TempDir};

struct Env {
    _tmp: TempDir,
    home: std::path::PathBuf,
    data: std::path::PathBuf,
    project: std::path::PathBuf,
}

fn env() -> Env {
    let tmp = tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&project).expect("project");
    Env {
        data: tmp.path().join("data"),
        home,
        project,
        _tmp: tmp,
    }
}

fn doctor(env: &Env, args: &[&str]) -> Output {
    doctor_with(env, &env.project, &[], args)
}

/// Run doctor in `cwd` with extra environment variables.
fn doctor_with(env: &Env, cwd: &Path, vars: &[(&str, &OsStr)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_citadel"));
    cmd.arg("doctor")
        .args(args)
        .env("HOME", &env.home)
        .env("CITADEL_HOME", &env.data)
        .env("CITADEL_NO_UPDATE_CHECK", "1")
        .env_remove("CITADEL_CONTAINER")
        .current_dir(cwd);
    for (key, value) in vars {
        cmd.env(key, value);
    }
    cmd.output().expect("run citadel doctor")
}

/// Parse `--json` output and check that every warn/fail row carries a hint.
fn checks(out: &Output) -> Vec<serde_json::Value> {
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let rows = json["checks"].as_array().expect("checks").clone();
    for row in &rows {
        if matches!(row["status"].as_str(), Some("warn" | "fail")) {
            assert!(
                row["hint"].as_str().is_some_and(|h| !h.is_empty()),
                "warn/fail row without hint: {row}"
            );
        }
    }
    rows
}

fn str_of<'a>(row: &'a serde_json::Value, key: &str) -> &'a str {
    row[key].as_str().unwrap_or_default()
}

/// Rows of the Setup section for the local scope (not the `local scope` note).
fn local_rows(rows: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    rows.iter()
        .filter(|r| str_of(r, "section") == "setup" && str_of(r, "name") != "local scope")
        .filter(|r| str_of(r, "detail").contains(" local "))
        .collect()
}

/// Write an executable shell script.
#[cfg(unix)]
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[cfg(unix)]
/// `PATH` with `dir` in front of the inherited one.
fn path_with(dir: &Path) -> OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![dir.to_path_buf()];
    dirs.extend(std::env::split_paths(&inherited));
    std::env::join_paths(dirs).expect("join PATH")
}

#[cfg(unix)]
fn claude_local_mcp(rows: &[serde_json::Value]) -> &serde_json::Value {
    rows.iter()
        .find(|r| {
            str_of(r, "name") == "mcp" && str_of(r, "detail").starts_with("Claude Code  local")
        })
        .unwrap_or_else(|| panic!("no Claude Code local mcp row: {rows:?}"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn doctor_on_fresh_home_creates_nothing() {
    let env = env();
    let out = doctor(&env, &["--json"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(!checks(&out).is_empty());
    assert!(!Path::new(&env.data).exists(), "data dir was created");
    assert!(!env.project.join(".mcp.json").exists());
    let home_entries: Vec<_> = std::fs::read_dir(&env.home).expect("read home").collect();
    assert!(home_entries.is_empty(), "home not empty: {home_entries:?}");
}

#[test]
fn doctor_in_container_skips_daemon_service_and_processes() {
    let env = env();
    let out = doctor_with(
        &env,
        &env.project,
        &[("CITADEL_CONTAINER", OsStr::new("1"))],
        &["--json"],
    );
    let rows = checks(&out);
    for row in &rows {
        let name = str_of(row, "name");
        assert!(
            !name.starts_with("daemon") && name != "service" && name != "processes",
            "unexpected row in container mode: {row}"
        );
    }
    assert!(
        rows.iter()
            .any(|r| str_of(r, "detail") == "running in a container: serve is the daemon"),
        "container note missing: {rows:?}"
    );
}

#[test]
fn doctor_with_home_as_cwd_skips_local_scope() {
    let env = env();
    let out = doctor_with(&env, &env.home, &[], &["--json"]);
    let rows = checks(&out);
    assert!(
        local_rows(&rows).is_empty(),
        "local rows present: {:?}",
        local_rows(&rows)
    );
    assert!(
        rows.iter().any(|r| str_of(r, "detail")
            == "local scope skipped: current directory is the home directory"),
        "skip note missing: {rows:?}"
    );
    // Sanity: the same check sees local rows when run from the project.
    let rows = checks(&doctor(&env, &["--json"]));
    assert!(!local_rows(&rows).is_empty());
}

#[cfg(unix)]
#[test]
fn doctor_never_runs_a_relative_stdio_command() {
    let env = env();
    let marker = env.home.parent().expect("tmp").join("relative-ran");
    script(
        &env.project.join("x"),
        &format!("touch '{}'", marker.display()),
    );
    std::fs::write(
        env.project.join(".mcp.json"),
        r#"{"mcpServers":{"citadel":{"command":"./x","args":["mcp"]}}}"#,
    )
    .expect("write .mcp.json");
    let rows = checks(&doctor(&env, &["--json"]));
    assert!(!marker.exists(), "doctor ran ./x");
    let mcp = claude_local_mcp(&rows);
    assert_eq!(str_of(mcp, "status"), "info", "{mcp}");
    assert!(str_of(mcp, "detail").contains("not checked"), "{mcp}");
}

#[cfg(unix)]
#[test]
fn doctor_does_not_run_docker_from_a_stdio_entry() {
    let env = env();
    let tmp = env.home.parent().expect("tmp").to_path_buf();
    let bin = tmp.join("bin");
    std::fs::create_dir(&bin).expect("bin");
    let marker = tmp.join("docker-ran");
    script(
        &bin.join("docker"),
        &format!("touch '{}'\necho 'citadel 0.0.1'", marker.display()),
    );
    std::fs::write(
        env.project.join(".mcp.json"),
        r#"{"mcpServers":{"citadel":{"command":"docker","args":["exec","-i","citadel","citadel","mcp"]}}}"#,
    )
    .expect("write .mcp.json");
    let path = path_with(&bin);
    let out = doctor_with(&env, &env.project, &[("PATH", &path)], &["--json"]);
    let rows = checks(&out);
    assert!(!marker.exists(), "doctor ran docker");
    let mcp = claude_local_mcp(&rows);
    assert_eq!(str_of(mcp, "status"), "info", "{mcp}");
    assert!(
        str_of(mcp, "detail").contains("version not checked"),
        "{mcp}"
    );
    assert!(mcp["hint"].is_null(), "{mcp}");
}

#[test]
fn doctor_fails_on_broken_mcp_config() {
    let env = env();
    std::fs::write(env.project.join(".mcp.json"), "{not json").expect("write");
    let out = doctor(&env, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stdout).contains('✗'), "{}", text(&out.stdout));
    assert!(text(&out.stderr).contains("doctor check(s) failed"));
}

#[test]
fn doctor_text_has_all_sections() {
    let env = env();
    let out = doctor(&env, &[]);
    let stdout = text(&out.stdout);
    for section in ["Version", "Running", "Setup", "Data"] {
        assert!(stdout.contains(section), "missing {section}:\n{stdout}");
    }
}

#[test]
fn doctor_prints_no_update_notice() {
    let env = env();
    let out = doctor(&env, &[]);
    assert!(!text(&out.stderr).contains("Update available"));
}
