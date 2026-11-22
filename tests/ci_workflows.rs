use std::fs;
use std::path::PathBuf;

fn read_repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("expected {} to exist and be readable: {e}", path.display()))
}

#[test]
fn ci_workflow_runs_the_checks_gate_on_prs() {
    let ci = read_repo_file(".github/workflows/ci.yml");
    for needle in [
        "pull_request",
        "branches: [main]",
        "ubuntu-latest",
        "macos-latest",
        "cargo fmt --check",
        "cargo clippy --all-targets --locked -- -D warnings",
        "cargo test --locked",
        "contents: read",
    ] {
        assert!(ci.contains(needle), "ci.yml must contain {needle:?}");
    }
    assert!(
        !ci.contains("write"),
        "ci.yml must not grant write permissions"
    );
}
