//! Merge Discodex's MCP server into Antigravity CLI workspace configuration.
//! The merge is intentionally scoped to a single marked entry so project MCP
//! servers owned by the user remain unchanged.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

pub const SERVER_NAME: &str = "discodex_tasks";
pub const NATIVE_ENDPOINT_ENV: &str = "DISCODEX_NATIVE_ENDPOINT";
pub const NATIVE_RUN_ENV: &str = "DISCODEX_NATIVE_RUN";

/// One native external response. A queued response in the same turn gets a
/// different run ID; reconnects also change the connection generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeRunBinding {
    pub thread_id: String,
    pub turn_id: String,
    pub run_id: String,
    pub generation: u64,
}
const OWNER_KEY: &str = "DISCODEX_MANAGED_MCP";
const OWNER_VALUE: &str = "discodex-v1";

/// Location of the documented Antigravity CLI project-level MCP config.
pub fn workspace_config_path(project_dir: &Path) -> std::path::PathBuf {
    project_dir.join(".agents").join("mcp_config.json")
}

/// Stable MCP entry. The AGY process passes its private endpoint and bound
/// thread identity through inherited environment variables, avoiding shared
/// project-config races between concurrent main and child AGY sessions.
pub fn native_server_entry(command: &Path, cwd: &Path) -> Result<Value> {
    let command = command
        .to_str()
        .context("Discodex MCP executable path is not valid UTF-8")?;
    let cwd = cwd
        .to_str()
        .context("Discodex MCP working directory is not valid UTF-8")?;
    Ok(json!({
        "command": command,
        "args": ["harness-tools", "--native"],
        "cwd": cwd,
        "env": { OWNER_KEY: OWNER_VALUE }
    }))
}

fn is_owned_entry(entry: &Value) -> bool {
    entry
        .get("env")
        .and_then(|env| env.get(OWNER_KEY))
        .and_then(Value::as_str)
        == Some(OWNER_VALUE)
        && entry
            .get("args")
            .and_then(Value::as_array)
            .is_some_and(|args| {
                args.first().and_then(Value::as_str) == Some("harness-tools")
                    && matches!(
                        args.get(1).and_then(Value::as_str),
                        Some("--endpoint" | "--native")
                    )
            })
}

/// Insert or refresh only our uniquely marked server entry, preserving every
/// unrelated top-level and MCP server property.
pub fn merge_config(existing: Option<&str>, desired_entry: Value) -> Result<String> {
    if !is_owned_entry(&desired_entry) {
        bail!("requested Discodex MCP entry lacks its ownership marker");
    }
    let mut config = match existing {
        Some(text) => {
            serde_json::from_str::<Value>(text).context("parse Antigravity MCP configuration")?
        }
        None => json!({}),
    };
    let root = config
        .as_object_mut()
        .context("Antigravity MCP configuration root must be an object")?;
    if !root.contains_key("mcpServers") {
        root.insert("mcpServers".into(), Value::Object(Map::new()));
    }
    let servers = root
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .context("Antigravity mcpServers must be an object")?;
    if servers
        .get(SERVER_NAME)
        .is_some_and(|current| !is_owned_entry(current))
    {
        bail!("MCP server name `{SERVER_NAME}` is already used by an unowned entry")
    }
    servers.insert(SERVER_NAME.into(), desired_entry);
    Ok(serde_json::to_string_pretty(&config)?)
}

/// Atomically update just the owned project MCP server entry. A malformed
/// existing file or an unowned name collision is left untouched.
pub fn write_workspace_config(project_dir: &Path, desired_entry: Value) -> Result<PathBuf> {
    static CONFIG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = CONFIG_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("AGY MCP config lock poisoned"))?;
    let path = workspace_config_path(project_dir);
    let parent = path.parent().context("MCP config path has no parent")?;
    fs::create_dir_all(parent).context("create Antigravity project config directory")?;
    let original = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("read Antigravity MCP configuration"),
    };
    let merged = merge_config(original.as_deref(), desired_entry)?;
    let prior_permissions = match fs::metadata(&path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).context("inspect existing Antigravity MCP configuration metadata");
        }
    };
    let mut staged =
        tempfile::NamedTempFile::new_in(parent).context("stage Antigravity MCP configuration")?;
    staged
        .write_all(merged.as_bytes())
        .context("write Antigravity MCP configuration")?;
    staged
        .as_file()
        .sync_all()
        .context("sync Antigravity MCP configuration")?;
    if let Some(permissions) = prior_permissions {
        staged
            .as_file()
            .set_permissions(permissions)
            .context("preserve Antigravity MCP configuration permissions")?;
    }
    staged
        .persist(&path)
        .map_err(|error| error.error)
        .context("replace Antigravity MCP configuration")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn entry(command: &str) -> Value {
        native_server_entry(Path::new(command), Path::new("/project")).unwrap()
    }

    #[test]
    fn merges_unique_server_and_preserves_unrelated_settings() {
        let original = r#"{"extension":"preserve","mcpServers":{"docs":{"serverUrl":"https://example.test/mcp"}}}"#;
        let merged: Value =
            serde_json::from_str(&merge_config(Some(original), entry("/app/discodex")).unwrap())
                .unwrap();
        assert_eq!(merged["extension"], "preserve");
        assert_eq!(
            merged["mcpServers"]["docs"]["serverUrl"],
            "https://example.test/mcp"
        );
        assert_eq!(
            merged["mcpServers"][SERVER_NAME]["args"][0],
            "harness-tools"
        );
    }

    #[test]
    fn refreshes_only_an_owned_entry_and_conflicts_with_unowned_name() {
        let first = merge_config(None, entry("/app/old/discodex")).unwrap();
        let refreshed = merge_config(Some(&first), entry("/app/new/discodex")).unwrap();
        let value: Value = serde_json::from_str(&refreshed).unwrap();
        assert_eq!(
            value["mcpServers"][SERVER_NAME]["command"],
            "/app/new/discodex"
        );
        assert_eq!(
            value["mcpServers"][SERVER_NAME]["args"],
            json!(["harness-tools", "--native"])
        );

        let conflict = r#"{"mcpServers":{"discodex_tasks":{"command":"custom-tool","args":[]}}}"#;
        assert!(merge_config(Some(conflict), entry("/app/new/discodex")).is_err());
    }

    #[test]
    fn rejects_malformed_or_wrongly_marked_configuration() {
        assert!(merge_config(Some("not json"), entry("/app/discodex")).is_err());
        assert!(merge_config(Some(r#"{"mcpServers":[]}"#), entry("/app/discodex")).is_err());
        assert!(merge_config(None, json!({"command":"agy","args":[]})).is_err());
    }

    #[test]
    fn native_entry_is_stable_and_uses_inherited_session_context() {
        let entry = native_server_entry(Path::new("/app/discodex"), Path::new("/project")).unwrap();
        assert!(is_owned_entry(&entry));
        assert_eq!(entry["args"][0], "harness-tools");
        assert_eq!(entry["args"][1], "--native");
        assert!(entry.get("endpoint").is_none());
    }
}
