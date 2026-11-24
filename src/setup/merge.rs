use serde_json::{Map, Value};
use toml_edit::{Array, DocumentMut, Item, Table, Value as TomlValue};

use crate::error::{Result, VaultError};
use crate::setup::agents::McpTransport;

const CITADEL_KEY: &str = "citadel";
const MCP_SERVERS_JSON_KEY: &str = "mcpServers";
const MCP_SERVERS_TOML_KEY: &str = "mcp_servers";

/// Return `true` if `existing` JSON content already contains `mcpServers.citadel`.
pub fn json_has_citadel_entry(existing: &str) -> bool {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(existing) else {
        return false;
    };
    root.get(MCP_SERVERS_JSON_KEY)
        .and_then(Value::as_object)
        .is_some_and(|servers| servers.contains_key(CITADEL_KEY))
}

/// Insert or update `mcpServers.citadel` in a JSON config file, preserving all other keys/servers.
pub fn merge_json_mcp_config(
    existing: Option<&str>,
    transport: &McpTransport,
    include_http_type: bool,
    force: bool,
) -> Result<String> {
    let mut root = match existing.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => {
            let parsed: Value = serde_json::from_str(raw).map_err(|err| {
                VaultError::Validation(format!("existing JSON MCP config is invalid JSON: {err}"))
            })?;
            match parsed {
                Value::Object(map) => map,
                _ => {
                    return Err(VaultError::Validation(
                        "existing JSON MCP config must be a JSON object".to_string(),
                    ));
                }
            }
        }
        None => Map::new(),
    };

    let servers_val = root
        .entry(MCP_SERVERS_JSON_KEY.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = servers_val.as_object_mut().ok_or_else(|| {
        VaultError::Validation(format!(
            "existing JSON MCP config has non-object `{MCP_SERVERS_JSON_KEY}` field"
        ))
    })?;

    if servers.contains_key(CITADEL_KEY) && !force {
        return Err(VaultError::Validation(
            "JSON MCP config already contains `mcpServers.citadel`; pass --force to overwrite"
                .to_string(),
        ));
    }

    let entry = build_json_entry(transport, include_http_type);
    servers.insert(CITADEL_KEY.to_string(), entry);

    let mut rendered = serde_json::to_string_pretty(&Value::Object(root))
        .map_err(|err| VaultError::Other(format!("failed to serialize JSON MCP config: {err}")))?;
    rendered.push('\n');
    Ok(rendered)
}

fn build_json_entry(transport: &McpTransport, include_http_type: bool) -> Value {
    let mut entry = Map::new();
    match transport {
        McpTransport::Stdio => {
            entry.insert("command".to_string(), Value::String("citadel".to_string()));
            entry.insert(
                "args".to_string(),
                Value::Array(vec![Value::String("mcp".to_string())]),
            );
        }
        McpTransport::Http(url) => {
            if include_http_type {
                entry.insert("type".to_string(), Value::String("http".to_string()));
            }
            entry.insert("url".to_string(), Value::String(url.clone()));
        }
    }
    Value::Object(entry)
}

/// Return `true` if `existing` TOML content already contains `[mcp_servers.citadel]`.
pub fn toml_has_citadel_entry(existing: &str) -> bool {
    let Ok(doc) = existing.parse::<DocumentMut>() else {
        return false;
    };
    doc.get(MCP_SERVERS_TOML_KEY)
        .and_then(Item::as_table_like)
        .is_some_and(|servers| servers.contains_key(CITADEL_KEY))
}

/// Insert or update `[mcp_servers.citadel]` in a TOML config file, preserving comments and formatting.
pub fn merge_toml_mcp_config(
    existing: Option<&str>,
    transport: &McpTransport,
    force: bool,
) -> Result<String> {
    let mut doc = match existing.filter(|s| !s.trim().is_empty()) {
        Some(raw) => raw.parse::<DocumentMut>().map_err(|err| {
            VaultError::Validation(format!("existing TOML MCP config is invalid TOML: {err}"))
        })?,
        None => DocumentMut::new(),
    };

    if doc.get(MCP_SERVERS_TOML_KEY).is_none() {
        let mut servers = Table::new();
        servers.set_implicit(true);
        doc[MCP_SERVERS_TOML_KEY] = Item::Table(servers);
    }
    let servers = doc[MCP_SERVERS_TOML_KEY]
        .as_table_like_mut()
        .ok_or_else(|| {
            VaultError::Validation(format!(
                "existing TOML MCP config has non-table `{MCP_SERVERS_TOML_KEY}` field"
            ))
        })?;
    if servers.contains_key(CITADEL_KEY) && !force {
        return Err(VaultError::Validation(
            "TOML MCP config already contains `[mcp_servers.citadel]`; pass --force to overwrite"
                .to_string(),
        ));
    }

    let mut citadel_table = Table::new();
    match transport {
        McpTransport::Stdio => {
            citadel_table["command"] = toml_edit::value("citadel");
            let mut args = Array::new();
            args.push(TomlValue::from("mcp"));
            citadel_table["args"] = toml_edit::value(args);
        }
        McpTransport::Http(url) => {
            citadel_table["url"] = toml_edit::value(url.as_str());
        }
    }
    servers.insert(CITADEL_KEY, Item::Table(citadel_table));

    let mut rendered = doc.to_string();
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    Ok(rendered)
}

/// Generate the minimal local Antigravity plugin manifest (`plugin.json`).
pub fn agy_plugin_manifest_json() -> String {
    let manifest = serde_json::json!({
        "name": "citadel",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Local knowledge vault and MCP server for web documentation and Markdown folders",
        "mcpServers": "./mcp_config.json"
    });
    format!("{manifest:#}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::VaultError;

    #[test]
    fn json_merge_preserves_other_servers_and_keys() {
        let existing = r#"{
  "theme": "dark",
  "mcpServers": {
    "other-server": {
      "command": "other",
      "args": ["--flag"]
    }
  }
}"#;
        let merged = merge_json_mcp_config(Some(existing), &McpTransport::Stdio, true, false)
            .expect("json merge should succeed");
        let parsed: serde_json::Value =
            serde_json::from_str(&merged).expect("merged json should be valid");

        assert_eq!(parsed["theme"], "dark");
        assert_eq!(parsed["mcpServers"]["other-server"]["command"], "other");
        assert_eq!(parsed["mcpServers"]["citadel"]["command"], "citadel");
        assert_eq!(
            parsed["mcpServers"]["citadel"]["args"],
            serde_json::json!(["mcp"])
        );
    }

    #[test]
    fn json_merge_refuses_overwrite_without_force_and_updates_with_force() {
        let existing = r#"{
  "mcpServers": {
    "citadel": {
      "command": "citadel",
      "args": ["mcp"]
    }
  }
}"#;
        let http = McpTransport::Http("http://127.0.0.1:4317/mcp".to_string());
        let err = merge_json_mcp_config(Some(existing), &http, true, false)
            .expect_err("must refuse overwrite without force");
        assert!(matches!(err, VaultError::Validation(_)));

        let updated = merge_json_mcp_config(Some(existing), &http, true, true)
            .expect("merge with force should succeed");
        let parsed: serde_json::Value =
            serde_json::from_str(&updated).expect("updated json should be valid");
        assert_eq!(parsed["mcpServers"]["citadel"]["type"], "http");
        assert_eq!(
            parsed["mcpServers"]["citadel"]["url"],
            "http://127.0.0.1:4317/mcp"
        );
        assert!(parsed["mcpServers"]["citadel"].get("command").is_none());
    }

    #[test]
    fn toml_merge_preserves_comments_and_other_servers() {
        let existing = r#"# Top-level comment
model = "gpt-5"

# Keep this server comment
[mcp_servers.other]
command = "other-mcp"
args = ["serve"]
"#;
        let merged = merge_toml_mcp_config(Some(existing), &McpTransport::Stdio, false)
            .expect("toml merge should succeed");

        assert!(merged.contains("# Top-level comment"));
        assert!(merged.contains("# Keep this server comment"));
        let doc: toml_edit::DocumentMut = merged.parse().expect("merged toml should parse");
        assert_eq!(doc["model"].as_str(), Some("gpt-5"));
        assert_eq!(
            doc["mcp_servers"]["other"]["command"].as_str(),
            Some("other-mcp")
        );
        assert_eq!(
            doc["mcp_servers"]["citadel"]["command"].as_str(),
            Some("citadel")
        );
        let args = doc["mcp_servers"]["citadel"]["args"]
            .as_array()
            .expect("args should be an array");
        assert_eq!(args.len(), 1);
        assert_eq!(args.get(0).and_then(|v| v.as_str()), Some("mcp"));
    }

    #[test]
    fn toml_merge_refuses_overwrite_without_force_and_updates_with_force() {
        let existing = r#"[mcp_servers.citadel]
command = "citadel"
args = ["mcp"]
"#;
        let http = McpTransport::Http("http://127.0.0.1:4317/mcp".to_string());
        let err = merge_toml_mcp_config(Some(existing), &http, false)
            .expect_err("must refuse overwrite without force");
        assert!(matches!(err, VaultError::Validation(_)));

        let updated = merge_toml_mcp_config(Some(existing), &http, true)
            .expect("toml merge with force should succeed");
        let doc: toml_edit::DocumentMut = updated.parse().expect("updated toml should parse");
        assert_eq!(
            doc["mcp_servers"]["citadel"]["url"].as_str(),
            Some("http://127.0.0.1:4317/mcp")
        );
        assert!(doc["mcp_servers"]["citadel"].get("command").is_none());
    }
}
