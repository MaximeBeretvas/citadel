use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("expected {} to exist and be readable: {e}", path.display()))
}

#[test]
fn dockerfile_meets_multi_stage_non_root_spec() {
    let df = read_repo_file("Dockerfile");

    assert!(
        df.contains("FROM rust:1-slim AS builder"),
        "Dockerfile must use rust:1-slim AS builder"
    );
    assert!(
        df.contains("FROM debian:trixie-slim AS runtime"),
        "Dockerfile must use debian:trixie-slim AS runtime"
    );
    assert!(
        df.contains("ARG FEATURES=\"chrome\""),
        "Dockerfile must default ARG FEATURES=\"chrome\" (JS rendering on by default)"
    );
    assert!(
        df.contains("--no-default-features --features \"$FEATURES\""),
        "Dockerfile must let FEATURES fully control the build so an empty value builds lite"
    );
    assert!(
        df.contains("ca-certificates"),
        "Dockerfile runtime stage must install ca-certificates"
    );
    assert!(
        df.contains("chromium"),
        "Dockerfile runtime stage must conditionally install chromium when chrome feature is enabled"
    );
    assert!(
        df.contains("rm -rf /var/lib/apt/lists/*"),
        "Dockerfile must clean apt lists"
    );
    assert!(
        df.contains("citadel"),
        "Dockerfile must create non-root citadel user"
    );
    assert!(
        df.contains("USER citadel"),
        "Dockerfile must switch to USER citadel"
    );
    assert!(
        df.contains("COPY --from=builder /app/target/release/citadel /usr/local/bin/citadel"),
        "Dockerfile must copy compiled release binary into /usr/local/bin/citadel"
    );
    assert!(
        df.contains("ENV CITADEL_HOME=/data"),
        "Dockerfile must set ENV CITADEL_HOME=/data"
    );
    assert!(
        df.contains("VOLUME [\"/data\"]"),
        "Dockerfile must declare VOLUME [\"/data\"]"
    );
    assert!(
        df.contains("EXPOSE 4317"),
        "Dockerfile must expose port 4317"
    );
    assert!(
        df.contains("ENTRYPOINT [\"citadel\", \"serve\", \"--bind\", \"0.0.0.0:4317\"]"),
        "Dockerfile must set ENTRYPOINT to citadel serve --bind 0.0.0.0:4317"
    );
}

#[test]
fn dockerignore_excludes_build_and_runtime_artifacts() {
    let ignore = read_repo_file(".dockerignore");
    let lines: Vec<&str> = ignore
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    for required in [
        "target/",
        ".git/",
        "*.db",
        "*.db-wal",
        "*.db-shm",
        ".DS_Store",
        "*.png",
    ] {
        assert!(
            lines.contains(&required),
            ".dockerignore must exclude {required}"
        );
    }
}

#[test]
fn makefile_uses_strictly_apple_container_cli() {
    let mk = read_repo_file("Makefile");

    // Check required variables
    for var in [
        "IMAGE ?=",
        "CONTAINER_NAME ?=",
        "VOLUME ?=",
        "PORT ?=",
        "BOOKS_DIR ?=",
        "FEATURES ?= chrome",
    ] {
        assert!(mk.contains(var), "Makefile must define {var}");
    }

    for (target, command) in [
        ("install:", "cargo install --path . --locked --force"),
        (
            "install-lite:",
            "cargo install --path . --locked --force --no-default-features",
        ),
        ("build-lite:", "FEATURES="),
    ] {
        assert!(mk.contains(target), "Makefile must define target {target}");
        assert!(
            mk.contains(command),
            "Makefile {target} must run `{command}`"
        );
    }

    assert!(mk.contains(".PHONY:"), "Makefile must declare .PHONY");

    // Check required targets and verify tab-indented recipes exist
    for target in ["build:", "run:", "stop:", "logs:", "exec:", "clean:"] {
        let mut lines = mk.lines().skip_while(|l| !l.starts_with(target));
        let header = lines.next();
        assert!(header.is_some(), "Makefile must define target {target}");
        let has_tab_recipe = lines
            .take_while(|l| l.starts_with('\t') || l.trim().is_empty())
            .any(|l| l.starts_with('\t'));
        assert!(
            has_tab_recipe,
            "Makefile target {target} must have tab-indented recipe lines"
        );
    }

    // Check Apple container CLI commands
    for cmd in [
        "container build",
        "container system start",
        "container volume create",
        "container run -d",
        "127.0.0.1:$(PORT):4317",
        "/sources/books:ro",
        // Apple `container` creates no /.dockerenv, so `citadel update` relies on this.
        "-e CITADEL_CONTAINER=1",
        "container stop",
        "container logs",
        "container exec",
        "container rm",
    ] {
        assert!(
            mk.contains(cmd),
            "Makefile must include Apple container command/flag: {cmd}"
        );
    }

    // Assert zero docker CLI invocations on any non-comment line
    for (idx, line) in mk.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        for token in trimmed.split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&')) {
            let lower = token.to_ascii_lowercase();
            assert!(
                lower != "docker" && lower != "docker-compose" && !lower.starts_with("docker."),
                "Makefile line {} must not invoke docker CLI (found token '{token}'): {line}",
                idx + 1
            );
        }
    }
}

#[test]
fn readme_documents_quickstart_native_mcp_and_skill() {
    let readme = read_repo_file("README.md");

    for required in [
        "make build",
        "make run",
        "BOOKS_DIR=",
        "http://127.0.0.1:4317",
        "cargo install --path .",
        "citadel daemon install",
        "http://127.0.0.1:4317/mcp",
        "citadel mcp",
        "container exec -i citadel citadel mcp",
        "`query`",
        "`manage`",
        "citadel skill install",
        "--global",
        "--path",
    ] {
        assert!(
            readme.contains(required),
            "README.md must document: {required}"
        );
    }

    for cmd in [
        "add", "list", "show", "sync", "rm", "query", "ui", "serve", "daemon", "skill",
    ] {
        let pattern = format!("citadel {cmd}");
        assert!(
            readme.contains(&pattern),
            "README.md must document CLI command: {pattern}"
        );
    }
}
