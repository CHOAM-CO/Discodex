//! Profile-private AGY settings. Personal CLI preferences are read-only inputs.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use crate::{
    config::Config,
    i18n::{Language, MessageKey},
};

pub const MANAGED_MCP_ALLOW_RULE: &str = "mcp(discodex_tasks/*)";
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

pub fn root(state_dir: &Path) -> Result<PathBuf> {
    let state = std::path::absolute(state_dir)?;
    Ok(state
        .parent()
        .context(Language::English.message(MessageKey::AgySettingsStateParent))?
        .join("agy-settings"))
}

fn cli_path(config: &Config) -> Result<PathBuf> {
    Ok(root(&config.state_dir)?
        .join("antigravity-cli")
        .join("settings.json"))
}

fn overrides_path(config: &Config) -> Result<PathBuf> {
    Ok(root(&config.state_dir)?.join("discodex-overrides.json"))
}

fn personal_cli_path(language: Language) -> Result<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    Ok(
        PathBuf::from(home.context(language.message(MessageKey::AgySettingsUserHome))?)
            .join(".gemini")
            .join("antigravity-cli")
            .join("settings.json"),
    )
}

fn context(language: Language, key: MessageKey, path: &Path) -> String {
    language
        .message(key)
        .replace("{path}", &path.display().to_string())
}

fn load(path: &Path, language: Language) -> Result<Value> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => {
            return Err(error).context(context(language, MessageKey::AgySettingsRead, path));
        }
    };
    let value: Value = serde_json::from_str(&text)
        .with_context(|| context(language, MessageKey::AgySettingsParse, path))?;
    anyhow::ensure!(
        value.is_object(),
        "{}",
        context(language, MessageKey::AgySettingsObject, path)
    );
    Ok(value)
}

fn overlay(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                overlay(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, patch) => *base = patch,
    }
}

fn ensure_mcp_allow(value: &mut Value, language: Language) -> Result<()> {
    let permissions = value
        .as_object_mut()
        .context(language.message(MessageKey::AgySettingsRootObject))?
        .entry("permissions")
        .or_insert_with(|| json!({}));
    let permissions = permissions
        .as_object_mut()
        .context(language.message(MessageKey::AgySettingsPermissionsObject))?;
    let allow = permissions
        .entry("allow")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context(language.message(MessageKey::AgySettingsAllowArray))?;
    if !allow
        .iter()
        .any(|rule| rule.as_str() == Some(MANAGED_MCP_ALLOW_RULE))
    {
        allow.push(json!(MANAGED_MCP_ALLOW_RULE));
    }
    Ok(())
}

fn ensure_managed_hook(settings: &mut Value, config: &Config) -> Result<()> {
    let language = config.language;
    let worker = config
        .worker_path
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)?;
    let hooks = settings
        .as_object_mut()
        .context(language.message(MessageKey::AgySettingsRootObject))?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context(language.message(MessageKey::AgySettingsHooksObject))?;
    if let Some(existing) = hooks.get(crate::agy_hook::MANAGED_HOOK) {
        // Refresh only our reserved hook. A conflicting personal hook is preserved.
        let owned = existing
            .pointer("/PreInvocation/0/command")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(" agy-hook --event PreInvocation --language "))
            && existing
                .pointer("/PreToolUse/0/hooks/0/command")
                .and_then(Value::as_str)
                .is_some_and(|command| {
                    command.contains(" agy-hook --event PreToolUse --language ")
                });
        anyhow::ensure!(
            owned,
            "{}",
            language.message(MessageKey::AgySettingsHookConflict)
        );
    }
    hooks.insert(crate::agy_hook::MANAGED_HOOK.to_owned(), json!({
        "PreInvocation":[{"type":"command", "command":crate::agy_hook::command(&worker, "PreInvocation", language)?}],
        "PreToolUse":[{"matcher":"^(invoke_subagent|define_subagent|send_message|manage_subagents)$", "hooks":[{
            "type":"command", "command":crate::agy_hook::command(&worker, "PreToolUse", language)?
        }]}]
    }));
    Ok(())
}

fn read_unlocked(config: &Config, share: bool) -> Result<Value> {
    let mut settings = if share {
        load(&personal_cli_path(config.language)?, config.language)?
    } else {
        load(&cli_path(config)?, config.language)?
    };
    overlay(
        &mut settings,
        load(&overrides_path(config)?, config.language)?,
    );
    ensure_mcp_allow(&mut settings, config.language)?;
    ensure_managed_hook(&mut settings, config)?;
    Ok(settings)
}

fn write(path: &Path, value: &Value, language: Language) -> Result<()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).context(context(language, MessageKey::AgySettingsMetadata, path));
        }
    };
    if metadata.is_some() && load(path, language)? == *value {
        return Ok(());
    }
    let parent = path
        .parent()
        .with_context(|| context(language, MessageKey::AgySettingsFileParent, path))?;
    fs::create_dir_all(parent)
        .with_context(|| context(language, MessageKey::AgySettingsWrite, path))?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| context(language, MessageKey::AgySettingsWrite, path))?;
    serde_json::to_writer_pretty(&mut staged, value)?;
    staged.write_all(b"\n")?;
    staged.as_file().sync_all()?;
    if let Some(metadata) = metadata {
        staged.as_file().set_permissions(metadata.permissions())?;
    }
    staged
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| context(language, MessageKey::AgySettingsWrite, path))?;
    Ok(())
}

/// Return configured values plus the managed MCP grant, without writing on view.
pub fn read(config: &Config, share: bool) -> Result<Value> {
    let _guard = SETTINGS_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!(config.language.message(MessageKey::AgySettingsLock)))?;
    read_unlocked(config, share)
}

/// Save an explicit choice in the same private overrides document for both bases.
pub fn set(config: &Config, share: bool, key: &str, value: Value) -> Result<()> {
    let _guard = SETTINGS_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!(config.language.message(MessageKey::AgySettingsLock)))?;
    let path = overrides_path(config)?;
    let mut settings = load(&path, config.language)?;
    let mut fields = key.trim_start_matches('/').split('/').peekable();
    let mut current = &mut settings;
    while let Some(field) = fields.next() {
        let object = current
            .as_object_mut()
            .context(config.language.message(MessageKey::AgySettingsFieldParent))?;
        if fields.peek().is_none() {
            if value.is_null() {
                object.remove(field);
            } else {
                object.insert(field.to_owned(), value);
            }
            break;
        }
        current = object.entry(field).or_insert_with(|| json!({}));
    }
    write(&path, &settings, config.language)?;
    write(
        &cli_path(config)?,
        &read_unlocked(config, share)?,
        config.language,
    )
}

/// Materialize private preferences and MCP configuration before any CLI launch.
pub fn prepare(config: &Config, share: bool) -> Result<()> {
    let _guard = SETTINGS_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!(config.language.message(MessageKey::AgySettingsLock)))?;
    write(
        &cli_path(config)?,
        &read_unlocked(config, share)?,
        config.language,
    )?;
    let worker = config
        .worker_path
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)?;
    crate::agy_mcp::write_managed_config(
        &root(&config.state_dir)?,
        crate::agy_mcp::native_server_entry(&worker)?,
    )?;
    crate::agy_mcp::remove_workspace_registration(&config.project_dir)?;
    Ok(())
}
