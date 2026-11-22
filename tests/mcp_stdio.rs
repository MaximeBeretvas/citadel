use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::tempdir;

#[test]
fn stdio_binary_emits_only_valid_jsonrpc_frames_with_debug_logs() {
    let vault_dir = tempdir().expect("vault tempdir");
    let bin = env!("CARGO_BIN_EXE_citadel");

    // Seed a newer cached version to prove `citadel mcp` never emits update notices.
    std::fs::write(
        vault_dir.path().join("update-check.json"),
        r#"{"checked_at":"2099-01-01T00:00:00Z","latest_version":"99.0.0","release_url":"https://github.com/mberetvas/citadel/releases/tag/v99.0.0"}"#,
    )
    .expect("seed update-check.json");

    let mut child = Command::new(bin)
        .arg("mcp")
        .env("CITADEL_HOME", vault_dir.path())
        .env("RUST_LOG", "debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn citadel mcp");

    {
        let stdin = child.stdin.as_mut().expect("stdin pipe");
        stdin
            .write_all(
                concat!(
                    "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"integration\",\"version\":\"0.1\"}}}\n",
                    "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
                    "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
                    "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"query\",\"arguments\":{\"sql\":\"SELECT name FROM v_sources\"}}}\n"
                )
                .as_bytes(),
            )
            .expect("write stdin");
    }

    let output = child.wait_with_output().expect("wait for citadel mcp");
    assert!(
        output.status.success(),
        "citadel mcp failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(output.stderr).expect("utf-8 stderr");

    assert!(
        !stderr.trim().is_empty(),
        "expected RUST_LOG=debug output on stderr"
    );
    assert!(
        !stderr.contains("Update available"),
        "citadel mcp must never emit update notices on stderr: {stderr}"
    );

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        3,
        "expected exactly 3 JSON-RPC frames on stdout, got: {stdout:?}"
    );

    let frames: Vec<Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).expect("stdout line must be valid JSON"))
        .collect();
    let tool_names: Vec<&str> = frames[1]["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name"))
        .collect();
    assert_eq!(tool_names, vec!["query", "manage"]);
    assert_eq!(frames[2]["result"]["isError"], false);
}
