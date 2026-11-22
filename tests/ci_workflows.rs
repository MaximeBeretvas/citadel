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

#[test]
fn release_workflow_matches_update_asset_contract() {
    let rel = read_repo_file(".github/workflows/release.yml");
    for needle in [
        "'v[0-9]+.[0-9]+.[0-9]+'",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "ubuntu-22.04",
        "ubuntu-22.04-arm",
        "cargo build --release --locked --target",
        "citadel-$GITHUB_REF_NAME-${{ matrix.target }}.tar.gz",
        "-C target/${{ matrix.target }}/release citadel",
        "sha256sum *.tar.gz > checksums.sha256",
        "gh release create",
        "contents: write",
    ] {
        assert!(rel.contains(needle), "release.yml must contain {needle:?}");
    }
    assert!(
        rel.contains("GITHUB_REF_NAME") && rel.contains("cargo metadata"),
        "release.yml must guard tag == v<Cargo.toml version>"
    );
    assert!(!rel.contains("windows"), "Windows is out of scope");
    assert!(
        !rel.contains("'v*'"),
        "a pre-release tag like v1.0.0-rc.1 must not publish a Latest release"
    );
}

#[test]
fn readme_prebuilt_install_works_on_a_fresh_mac() {
    let readme = read_repo_file("README.md");
    assert!(
        readme.contains("mkdir -p ~/.local/bin"),
        "README must create ~/.local/bin before installing into it"
    );
    assert!(
        readme.contains("sha256sum -c --ignore-missing checksums.sha256"),
        "README must show the Linux checksum command"
    );
}
