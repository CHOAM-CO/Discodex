use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model_catalog::ModelSelection;

fn enabled_by_default() -> Option<bool> {
    Some(true)
}

/// Per-Discodex native feature overrides. An unset value inherits Codex configuration.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeFeaturePreferences {
    #[serde(default = "enabled_by_default")]
    pub code_mode: Option<bool>,
    #[serde(default = "enabled_by_default")]
    pub code_mode_host: Option<bool>,
    #[serde(default = "enabled_by_default")]
    pub multi_agent_v2: Option<bool>,
    #[serde(default = "enabled_by_default")]
    pub wait_agent: Option<bool>,
    #[serde(default = "enabled_by_default")]
    pub async_notifications: Option<bool>,
    #[serde(default = "enabled_by_default")]
    pub request_user_input: Option<bool>,
    #[serde(default = "enabled_by_default")]
    pub clock: Option<bool>,
    #[serde(default = "enabled_by_default")]
    pub token_budget: Option<bool>,
    #[serde(default = "enabled_by_default")]
    pub history_notes: Option<bool>,
    /// Optional private profile default. None inherits the native config.
    #[serde(default)]
    pub subagent_model: Option<String>,
    /// Provider to which the saved child-model preference belongs.
    #[serde(default)]
    pub subagent_model_provider: Option<String>,
}

impl Default for NativeFeaturePreferences {
    fn default() -> Self {
        Self {
            code_mode: Some(true),
            code_mode_host: Some(true),
            multi_agent_v2: Some(true),
            wait_agent: Some(true),
            async_notifications: Some(true),
            request_user_input: Some(true),
            clock: Some(true),
            token_budget: Some(true),
            history_notes: Some(true),
            subagent_model: None,
            subagent_model_provider: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeFeatureDescriptor {
    pub key: &'static str,
    pub label: &'static str,
    pub restart_required: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NativeFeatureSnapshot {
    pub preferences: NativeFeaturePreferences,
    /// Effective config/read response, including origins when provided by Codex.
    pub native_config: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeFeatureApplyResult {
    SavedRequiresRestart,
}

/// Stores Discodex-owned feature choices under its state directory. It never writes
/// to CODEX_HOME or the native app's shared config.toml.
#[derive(Clone, Debug)]
pub struct NativeFeatureStore {
    path: PathBuf,
}

impl NativeFeatureStore {
    pub fn new(state_dir: impl AsRef<Path>) -> Self {
        Self {
            path: state_dir.as_ref().join("native-features.json"),
        }
    }

    pub fn read(&self) -> Result<NativeFeaturePreferences> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| {
                format!("parse native feature preferences {}", self.path.display())
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(NativeFeaturePreferences::default())
            }
            Err(error) => Err(error).with_context(|| {
                format!("read native feature preferences {}", self.path.display())
            }),
        }
    }

    pub fn list(&self) -> Result<Vec<(NativeFeatureDescriptor, Option<Value>)>> {
        let preferences = self.read()?;
        Ok(vec![
            (
                descriptor("codeMode", "Code Mode"),
                preferences.code_mode.map(Value::Bool),
            ),
            (
                descriptor("codeModeHost", "Code Mode host"),
                preferences.code_mode_host.map(Value::Bool),
            ),
            (
                descriptor("multiAgentV2", "Multi-agent v2"),
                preferences.multi_agent_v2.map(Value::Bool),
            ),
            (
                descriptor("waitAgent", "Wait for agents"),
                preferences.wait_agent.map(Value::Bool),
            ),
            (
                descriptor("asyncNotifications", "Async notifications"),
                preferences.async_notifications.map(Value::Bool),
            ),
            (
                descriptor("requestUserInput", "Native questions"),
                preferences.request_user_input.map(Value::Bool),
            ),
            (
                descriptor("clock", "Clock and time reminders"),
                preferences.clock.map(Value::Bool),
            ),
            (
                descriptor("tokenBudget", "Token budget"),
                preferences.token_budget.map(Value::Bool),
            ),
            (
                descriptor("historyNotes", "History and notes"),
                preferences.history_notes.map(Value::Bool),
            ),
            (
                descriptor("subagentModel", "Default subagent model"),
                preferences.subagent_model.map(Value::String),
            ),
        ])
    }

    pub fn apply(
        &self,
        preferences: &NativeFeaturePreferences,
    ) -> Result<NativeFeatureApplyResult> {
        let parent = self
            .path
            .parent()
            .context("native feature preferences path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create state directory {}", parent.display()))?;
        let bytes = serde_json::to_vec_pretty(preferences)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
            format!(
                "create temporary native feature preferences in {}",
                parent.display()
            )
        })?;
        temporary.write_all(&bytes).with_context(|| {
            format!(
                "write temporary native feature preferences in {}",
                parent.display()
            )
        })?;
        temporary
            .persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| {
                format!("replace native feature preferences {}", self.path.display())
            })?;
        Ok(NativeFeatureApplyResult::SavedRequiresRestart)
    }

    /// Return exact Codex CLI config overrides for the next owned app-server process.
    /// Unset options are omitted so native configuration remains inherited.
    pub fn startup_overrides(&self) -> Result<Vec<(String, String)>> {
        startup_overrides(&self.read()?)
    }
}

fn descriptor(key: &'static str, label: &'static str) -> NativeFeatureDescriptor {
    NativeFeatureDescriptor {
        key,
        label,
        restart_required: true,
    }
}

fn startup_overrides(preferences: &NativeFeaturePreferences) -> Result<Vec<(String, String)>> {
    let mut result = Vec::new();
    let mut push_bool = |key: &str, value: Option<bool>| {
        if let Some(value) = value {
            result.push((key.to_owned(), value.to_string()));
        }
    };
    push_bool("features.code_mode.enabled", preferences.code_mode);
    push_bool(
        "features.code_mode_host.enabled",
        preferences.code_mode_host,
    );
    push_bool(
        "features.multi_agent_v2.enabled",
        preferences.multi_agent_v2,
    );
    push_bool(
        "features.multi_agent_v2.wait_agent_enabled",
        preferences.wait_agent,
    );
    push_bool(
        "features.send_message_to_user_async",
        preferences.async_notifications,
    );
    push_bool(
        "tools.experimental_request_user_input.enabled",
        preferences.request_user_input,
    );
    push_bool(
        "features.default_mode_request_user_input",
        preferences.request_user_input,
    );
    push_bool("features.current_time_reminder.enabled", preferences.clock);
    push_bool("features.token_budget.enabled", preferences.token_budget);
    push_bool(
        "features.token_budget.use_history_notes_extension",
        preferences.history_notes,
    );
    Ok(result)
}

/// Resolve the child default for a parent thread without applying cross-provider slugs.
pub fn subagent_model_for_provider(
    preferences: &NativeFeaturePreferences,
    target: &ModelSelection,
    configured_provider: &str,
    configured_default: Option<&str>,
) -> Option<String> {
    if let Some(saved_model) = preferences.subagent_model.as_deref() {
        let affinity = preferences
            .subagent_model_provider
            .as_deref()
            .unwrap_or(configured_provider);
        return Some(
            if affinity == target.provider {
                saved_model
            } else {
                &target.model
            }
            .to_owned(),
        );
    }
    configured_default.map(|model| {
        if configured_provider == target.provider {
            model
        } else {
            &target.model
        }
        .to_owned()
    })
}

pub(crate) fn subagent_model_without_parent_override(
    preferences: &NativeFeaturePreferences,
    configured_provider: &str,
    configured_default: Option<&str>,
) -> Option<String> {
    if preferences
        .subagent_model_provider
        .as_deref()
        .unwrap_or(configured_provider)
        == configured_provider
    {
        preferences
            .subagent_model
            .clone()
            .or_else(|| configured_default.map(str::to_owned))
    } else {
        configured_default.map(str::to_owned)
    }
}

pub fn feature_config_read_params(cwd: &Path) -> Value {
    json!({
        "includeLayers": true,
        "cwd": cwd.to_string_lossy(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_enables_requested_features_and_inherits_subagent_model() {
        let preferences = NativeFeaturePreferences::default();
        let overrides = startup_overrides(&preferences).expect("build native config overrides");

        assert!(
            overrides
                .iter()
                .any(|(key, value)| { key == "features.code_mode.enabled" && value == "true" })
        );
        assert!(
            overrides.iter().any(|(key, value)| {
                key == "features.multi_agent_v2.enabled" && value == "true"
            })
        );
        assert!(overrides.iter().any(|(key, value)| {
            key == "tools.experimental_request_user_input.enabled" && value == "true"
        }));
        assert!(overrides.iter().any(|(key, value)| {
            key == "features.default_mode_request_user_input" && value == "true"
        }));
        assert!(overrides.iter().any(|(key, value)| {
            key == "features.send_message_to_user_async" && value == "true"
        }));
        assert!(!overrides.iter().any(|(key, _)| {
            key == "features.async_compaction.enabled"
                || key == "features.current_time_reminder.sleep_tool"
        }));
        assert!(
            !overrides
                .iter()
                .any(|(key, _)| key == "agents.default_subagent_model")
        );
    }

    #[test]
    fn unset_preferences_produce_no_startup_overrides() {
        let preferences = NativeFeaturePreferences {
            code_mode: None,
            code_mode_host: None,
            multi_agent_v2: None,
            wait_agent: None,
            async_notifications: None,
            request_user_input: None,
            clock: None,
            token_budget: None,
            history_notes: None,
            subagent_model: Some("model-example".to_owned()),
            subagent_model_provider: Some("provider-example".to_owned()),
        };
        let overrides = startup_overrides(&preferences).expect("build native config overrides");
        assert!(overrides.is_empty());
    }

    #[test]
    fn child_model_uses_saved_affinity_and_inherits_parent_on_provider_mismatch() {
        let target = ModelSelection {
            provider: "provider-local".into(),
            model: "parent-local".into(),
        };
        let mut preferences = NativeFeaturePreferences::default();
        preferences.subagent_model = Some("child-openai".into());
        preferences.subagent_model_provider = Some("openai".into());
        assert_eq!(
            subagent_model_for_provider(&preferences, &target, "openai", None),
            Some("parent-local".into())
        );

        preferences.subagent_model_provider = Some("provider-local".into());
        assert_eq!(
            subagent_model_for_provider(&preferences, &target, "openai", None),
            Some("child-openai".into())
        );
    }

    #[test]
    fn legacy_child_model_binds_to_configured_provider_and_native_default_falls_back_safely() {
        let target = ModelSelection {
            provider: "provider-local".into(),
            model: "parent-local".into(),
        };
        let mut preferences = NativeFeaturePreferences::default();
        preferences.subagent_model = Some("legacy-child".into());
        assert_eq!(
            subagent_model_for_provider(&preferences, &target, "openai", None),
            Some("parent-local".into())
        );
        preferences.subagent_model = None;
        assert_eq!(
            subagent_model_for_provider(&preferences, &target, "openai", Some("global-child")),
            Some("parent-local".into())
        );
        let openai_target = ModelSelection {
            provider: "openai".into(),
            model: "gpt-example".into(),
        };
        assert_eq!(
            subagent_model_for_provider(
                &preferences,
                &openai_target,
                "openai",
                Some("global-child")
            ),
            Some("global-child".into())
        );
        assert_eq!(
            subagent_model_for_provider(&preferences, &target, "openai", None),
            None
        );
    }

    #[test]
    fn preferences_round_trip_under_the_owned_state_directory() {
        let directory = tempfile::tempdir().expect("create state directory");
        let store = NativeFeatureStore::new(directory.path());
        let mut preferences = NativeFeaturePreferences::default();
        preferences.subagent_model = Some("model-example".to_owned());
        preferences.subagent_model_provider = Some("provider-example".to_owned());

        assert_eq!(
            store.apply(&preferences).expect("save preferences"),
            NativeFeatureApplyResult::SavedRequiresRestart
        );
        assert_eq!(store.read().expect("read preferences"), preferences);
        assert!(store.path.starts_with(directory.path()));
    }
}

#[cfg(test)]
mod unspecified_parent_tests {
    use super::*;
    #[test]
    fn default_parent_does_not_receive_a_foreign_saved_child() {
        let preferences = NativeFeaturePreferences {
            subagent_model: Some("local-child".into()),
            subagent_model_provider: Some("local".into()),
            ..Default::default()
        };
        assert_eq!(
            subagent_model_without_parent_override(&preferences, "cloud", None),
            None
        );
        assert_eq!(
            subagent_model_without_parent_override(&preferences, "cloud", Some("cloud-child")),
            Some("cloud-child".into())
        );
        assert_eq!(
            subagent_model_without_parent_override(&preferences, "local", None),
            Some("local-child".into())
        );
    }
}
