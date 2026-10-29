//! Register Discodex MCP inside its private AGY configuration root.
//! Legacy workspace cleanup removes only our marked entry.

use anyhow::{Context, Result, bail};
use jsonc_parser::{ParseOptions, cst::CstRootNode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use crate::i18n::{Language, MessageKey};

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
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// Location of the documented Antigravity CLI project-level MCP config.
pub fn workspace_config_path(project_dir: &Path) -> std::path::PathBuf {
    project_dir.join(".agents").join("mcp_config.json")
}

pub fn managed_config_path(settings_root: &Path) -> PathBuf {
    settings_root.join("config").join("mcp_config.json")
}

/// Stable MCP entry. The AGY process passes its private endpoint and bound
/// thread identity through inherited environment variables, avoiding shared
/// project-config races between concurrent main and child AGY sessions.
pub fn native_server_entry(command: &Path) -> Result<Value> {
    let command = command
        .to_str()
        .context(Language::English.message(MessageKey::AgyMcpExecutableUtf8))?;
    Ok(json!({
        "command": command,
        "args": ["harness-tools", "--native"],
        // Native tools can wait for a user's approval or another task. Let the
        // owning run cancel them instead of AGY's default three-minute deadline.
        "timeoutSeconds": -1,
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
        bail!(Language::English.message(MessageKey::AgyMcpOwnershipMarker));
    }
    let mut config = match existing {
        Some(text) => {
            serde_json::from_str::<Value>(text)
                .context(Language::English.message(MessageKey::AgyMcpParse))?
        }
        None => json!({}),
    };
    let root = config
        .as_object_mut()
        .context(Language::English.message(MessageKey::AgyMcpRootObject))?;
    if !root.contains_key("mcpServers") {
        root.insert("mcpServers".into(), Value::Object(Map::new()));
    }
    let servers = root
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .context(Language::English.message(MessageKey::AgyMcpServersObject))?;
    if servers
        .get(SERVER_NAME)
        .is_some_and(|current| !is_owned_entry(current))
    {
        bail!(
            Language::English
                .message(MessageKey::AgyMcpServerConflict)
                .replace("{server}", SERVER_NAME)
        )
    }
    servers.insert(SERVER_NAME.into(), desired_entry);
    Ok(serde_json::to_string_pretty(&config)?)
}

/// Atomically update the server in the profile-private global MCP config.
pub fn write_managed_config(settings_root: &Path, desired_entry: Value) -> Result<PathBuf> {
    let _guard = CONFIG_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!(Language::English.message(MessageKey::AgyMcpLock)))?;
    let path = managed_config_path(settings_root);
    let original = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).context(Language::English.message(MessageKey::AgyMcpRead));
        }
    };
    let merged = merge_config(original.as_deref(), desired_entry)?;
    if original.as_deref() != Some(merged.as_str()) {
        write_config(&path, &merged)?;
    }
    Ok(path)
}

/// Remove a registration written by older Discodex versions, preserving users' MCP entries.
pub fn remove_workspace_registration(project_dir: &Path) -> Result<()> {
    let _guard = CONFIG_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!(Language::English.message(MessageKey::AgyMcpLock)))?;
    remove_registration(&workspace_config_path(project_dir))?;
    Ok(())
}

/// Include legacy workspace configuration in an update snapshot only when it
/// contains the registration that managed preparation is allowed to remove.
pub(crate) fn owned_workspace_config_path(project_dir: &Path) -> Result<Option<PathBuf>> {
    let _guard = CONFIG_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!(Language::English.message(MessageKey::AgyMcpLock)))?;
    let path = workspace_config_path(project_dir);
    Ok(read_owned_registration(&path)?.map(|_| path))
}

fn read_owned_registration(path: &Path) -> Result<Option<(String, CstRootNode)>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context(Language::English.message(MessageKey::AgyMcpReadLegacy));
        }
    };
    let document = CstRootNode::parse(
        text.strip_prefix('\u{feff}').unwrap_or(&text),
        &ParseOptions::default(),
    )
    .context(Language::English.message(MessageKey::AgyMcpParse))?;
    let Some(entry) = document
        .object_value()
        .and_then(|root| root.object_value("mcpServers"))
        .and_then(|servers| servers.get(SERVER_NAME))
    else {
        return Ok(None);
    };
    if !entry.to_serde_value().as_ref().is_some_and(is_owned_entry) {
        return Ok(None);
    }
    Ok(Some((text, document)))
}

fn remove_registration(path: &Path) -> Result<bool> {
    let Some((text, document)) = read_owned_registration(path)? else {
        return Ok(false);
    };
    if let Some(entry) = document
        .object_value()
        .and_then(|root| root.object_value("mcpServers"))
        .and_then(|servers| servers.get(SERVER_NAME))
    {
        entry.remove();
    }
    let mut updated = document.to_string();
    if text.starts_with('\u{feff}') {
        updated.insert(0, '\u{feff}');
    }
    write_config(path, &updated)?;
    Ok(true)
}

/// Unregister the owned server. Configuration and conversation data remain intact.
pub fn remove_owned_registration(settings_root: &Path) -> Result<()> {
    let _guard = CONFIG_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!(Language::English.message(MessageKey::AgyMcpLock)))?;
    remove_registration(&managed_config_path(settings_root))?;
    Ok(())
}

fn write_config(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .context(Language::English.message(MessageKey::AgyMcpFileParent))?;
    fs::create_dir_all(parent)
        .context(Language::English.message(MessageKey::AgyMcpCreateDirectory))?;
    let prior_permissions = match fs::metadata(&path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).context(Language::English.message(MessageKey::AgyMcpMetadata));
        }
    };
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .context(Language::English.message(MessageKey::AgyMcpStage))?;
    staged
        .write_all(content.as_bytes())
        .context(Language::English.message(MessageKey::AgyMcpWrite))?;
    staged
        .as_file()
        .sync_all()
        .context(Language::English.message(MessageKey::AgyMcpSync))?;
    if let Some(permissions) = prior_permissions {
        staged
            .as_file()
            .set_permissions(permissions)
            .context(Language::English.message(MessageKey::AgyMcpPermissions))?;
    }
    staged
        .persist(&path)
        .map_err(|error| error.error)
        .context(Language::English.message(MessageKey::AgyMcpReplace))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn entry(command: &str) -> Value {
        native_server_entry(Path::new(command)).unwrap()
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
        let entry = native_server_entry(Path::new("/app/discodex")).unwrap();
        assert!(is_owned_entry(&entry));
        assert_eq!(entry["args"][0], "harness-tools");
        assert_eq!(entry["args"][1], "--native");
        assert!(entry.get("endpoint").is_none());
    }
}
