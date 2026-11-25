//! End-to-end checks of `citadel doctor` against a throwaway home and project directory.

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
    Command::new(env!("CARGO_BIN_EXE_citadel"))
        .arg("doctor")
        .args(args)
        .env("HOME", &env.home)
        .env("CITADEL_HOME", &env.data)
        .env("CITADEL_NO_UPDATE_CHECK", "1")
        .current_dir(&env.project)
        .output()
        .expect("run citadel doctor")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn doctor_on_fresh_home_creates_nothing() {
    let env = env();
    let out = doctor(&env, &["--json"]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert!(!json["checks"].as_array().expect("checks").is_empty());
    assert!(!Path::new(&env.data).exists(), "data dir was created");
    assert!(!env.project.join(".mcp.json").exists());
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
