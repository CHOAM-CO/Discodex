use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn enabled_by_default() -> Option<bool> {
    Some(true)
}

fn default_approval_policy() -> Value {
    json!("on-request")
}

/// Per-Discodex native settings. Unset feature switches inherit Codex configuration;
/// the profile's approval policy defaults to native on-request.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeFeaturePreferences {
    /// Native per-thread policy; deliberately separate from shared Codex config.
    #[serde(default = "default_approval_policy")]
    pub approval_policy: Value,
    /// Opt-in to the locally installed subscription-authenticated AGY CLI.
    #[serde(default)]
    pub antigravity_enabled: bool,
    /// None uses the `agy` executable on PATH.
    #[serde(default)]
    pub antigravity_path: Option<String>,
    /// Exact IDs obtained from AGY or entered by the user.
    #[serde(default)]
    pub antigravity_models: Vec<String>,
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
    #[serde(default)]
    pub subagent_model_executor: Option<String>,
}

impl Default for NativeFeaturePreferences {
    fn default() -> Self {
        Self {
            approval_policy: default_approval_policy(),
            antigravity_enabled: false,
            antigravity_path: None,
            antigravity_models: Vec::new(),
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
            subagent_model_executor: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeFeatureDescriptor {
    pub key: &'static str,
    pub label: &'static str,
    pub restart_required: bool,
}

impl NativeFeaturePreferences {
    pub fn settings(&self) -> Vec<(NativeFeatureDescriptor, Option<Value>)> {
        vec![
            (
                descriptor("codeMode", "Code Mode"),
                self.code_mode.map(Value::Bool),
            ),
            (
                descriptor("codeModeHost", "Code Mode host"),
                self.code_mode_host.map(Value::Bool),
            ),
            (
                descriptor("multiAgentV2", "Multi-agent v2"),
                self.multi_agent_v2.map(Value::Bool),
            ),
            (
                descriptor("waitAgent", "Wait for agents"),
                self.wait_agent.map(Value::Bool),
            ),
            (
                descriptor("asyncNotifications", "Async notifications"),
                self.async_notifications.map(Value::Bool),
            ),
            (
                descriptor("requestUserInput", "Native questions"),
                self.request_user_input.map(Value::Bool),
            ),
            (
                descriptor("clock", "Clock and time reminders"),
                self.clock.map(Value::Bool),
            ),
            (
                descriptor("tokenBudget", "Token budget"),
                self.token_budget.map(Value::Bool),
            ),
            (
                descriptor("historyNotes", "History and notes"),
                self.history_notes.map(Value::Bool),
            ),
        ]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NativeFeatureSnapshot {
    pub preferences: NativeFeaturePreferences,
    /// Effective config/read response, including origins when provided by Codex.
    pub native_config: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeFeatureApplyResult {
    Saved,
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
        Ok(self.read()?.settings())
    }

    pub fn apply(
        &self,
        preferences: &NativeFeaturePreferences,
    ) -> Result<NativeFeatureApplyResult> {
        self.apply_with(preferences, |path, bytes| {
            let parent = path
                .parent()
                .context("native feature preferences path has no parent")?;
            fs::create_dir_all(parent)
                .with_context(|| format!("create state directory {}", parent.display()))?;
            let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
                format!(
                    "create temporary native feature preferences in {}",
                    parent.display()
                )
            })?;
            temporary.write_all(bytes).with_context(|| {
                format!(
                    "write temporary native feature preferences in {}",
                    parent.display()
                )
            })?;
            temporary
                .persist(path)
                .map_err(|error| error.error)
                .with_context(|| {
                    format!("replace native feature preferences {}", path.display())
                })?;
            Ok(())
        })
    }

    fn apply_with<F>(
        &self,
        preferences: &NativeFeaturePreferences,
        write: F,
    ) -> Result<NativeFeatureApplyResult>
    where
        F: FnOnce(&Path, &[u8]) -> Result<()>,
    {
        write(&self.path, &serde_json::to_vec_pretty(preferences)?)?;
        Ok(NativeFeatureApplyResult::Saved)
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

/// Explicit profile child defaults. Unset fields stay under native Codex inheritance.
/// Provider/model/executor travel together; the bridge does not substitute the
/// parent model when a child uses a different provider.
pub(crate) fn subagent_model_overrides(preferences: &NativeFeaturePreferences) -> Value {
    let mut overrides = serde_json::Map::new();
    if let Some(model) = &preferences.subagent_model {
        overrides.insert("agents.default_subagent_model".into(), json!(model));
        let executor = preferences
            .subagent_model_executor
            .as_deref()
            .unwrap_or("codex");
        overrides.insert("agents.default_subagent_executor".into(), json!(executor));
        if executor == "codex" {
            if let Some(provider) = &preferences.subagent_model_provider {
                overrides.insert(
                    "agents.default_subagent_model_provider".into(),
                    json!(provider),
                );
            }
        }
    }
    Value::Object(overrides)
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
            approval_policy: default_approval_policy(),
            antigravity_enabled: false,
            antigravity_path: None,
            antigravity_models: Vec::new(),
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
            subagent_model_executor: None,
        };
        let overrides = startup_overrides(&preferences).expect("build native config overrides");
        assert!(overrides.is_empty());
    }

    #[test]
    fn child_defaults_preserve_cross_provider_selection() {
        let preferences = NativeFeaturePreferences {
            subagent_model: Some("child-cloud".into()),
            subagent_model_provider: Some("cloud-provider".into()),
            ..Default::default()
        };
        assert_eq!(
            subagent_model_overrides(&preferences),
            json!({
                "agents.default_subagent_model":"child-cloud",
                "agents.default_subagent_model_provider":"cloud-provider",
                "agents.default_subagent_executor":"codex"
            })
        );
    }

    #[test]
    fn agy_child_selects_executor_without_fabricating_native_provider() {
        let preferences = NativeFeaturePreferences {
            subagent_model: Some("model-example".into()),
            subagent_model_provider: Some("antigravity".into()),
            subagent_model_executor: Some("antigravity".into()),
            ..Default::default()
        };
        assert_eq!(
            subagent_model_overrides(&preferences),
            json!({
                "agents.default_subagent_model":"model-example",
                "agents.default_subagent_executor":"antigravity"
            })
        );
        assert_eq!(
            subagent_model_overrides(&NativeFeaturePreferences::default()),
            json!({})
        );
    }

    #[test]
    fn preferences_round_trip_preserves_all_profile_choices() {
        let mut preferences = NativeFeaturePreferences::default();
        preferences.subagent_model = Some("model-example".to_owned());
        preferences.subagent_model_provider = Some("provider-example".to_owned());
        preferences.subagent_model_executor = Some("antigravity".to_owned());
        preferences.approval_policy = json!("never");
        preferences.antigravity_enabled = true;
        preferences.antigravity_path = Some("agy-preview".to_owned());
        preferences.antigravity_models = vec!["model-exact-a".into(), "model-exact-b".into()];

        let encoded = serde_json::to_vec_pretty(&preferences).unwrap();
        let restored: NativeFeaturePreferences = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored, preferences);
    }

    #[test]
    fn profile_save_boundary_writes_only_selected_profile_and_reports_write_failure() {
        let p_path = PathBuf::from("profiles/p/native-features.json");
        let q_path = PathBuf::from("profiles/q/native-features.json");
        let shared_path = PathBuf::from("shared/config.toml");
        let store = NativeFeatureStore::new("profiles/p");
        let original = std::collections::HashMap::from([
            (
                p_path.clone(),
                json!({"subagentModel":"child-before","subagentModelProvider":"provider-before","subagentModelExecutor":"codex","clock":false,"approvalPolicy":"on-request"}),
            ),
            (
                q_path.clone(),
                json!({"subagentModel":"other-child","subagentModelProvider":"other-provider","subagentModelExecutor":"codex","codeMode":false}),
            ),
            (
                shared_path.clone(),
                json!({"model":"shared-model","approval_policy":"never"}),
            ),
        ]);
        for write_succeeds in [true, false] {
            let mut stored = original.clone();
            let mut preferences: NativeFeaturePreferences =
                serde_json::from_value(stored[&p_path].clone()).unwrap();
            preferences.subagent_model = Some("child-after".into());
            preferences.subagent_model_provider = Some("provider-after".into());
            preferences.subagent_model_executor = Some("codex".into());
            let expected = serde_json::to_value(&preferences).unwrap();
            let mut writes = Vec::new();
            let result = store.apply_with(&preferences, |path, bytes| {
                let payload: Value = serde_json::from_slice(bytes)?;
                writes.push((path.to_path_buf(), payload.clone()));
                if !write_succeeds {
                    return Err(std::io::Error::other("profile write rejected").into());
                }
                stored.insert(path.to_path_buf(), payload);
                Ok(())
            });
            assert_eq!(writes, vec![(p_path.clone(), expected.clone())]);
            if write_succeeds {
                assert_eq!(result.unwrap(), NativeFeatureApplyResult::Saved);
                assert_eq!(stored[&p_path], expected);
                let restored: NativeFeaturePreferences =
                    serde_json::from_value(stored[&p_path].clone()).unwrap();
                assert_eq!(restored, preferences);
                assert_eq!(
                    subagent_model_overrides(&restored),
                    json!({
                        "agents.default_subagent_model":"child-after",
                        "agents.default_subagent_model_provider":"provider-after",
                        "agents.default_subagent_executor":"codex"
                    })
                );
                assert_eq!(restored.clock, Some(false));
                assert_eq!(restored.approval_policy, json!("on-request"));
            } else {
                let error = result.unwrap_err();
                assert_eq!(
                    error.downcast_ref::<std::io::Error>().unwrap().to_string(),
                    "profile write rejected"
                );
                assert_eq!(stored, original);
            }
            assert_eq!(stored.len(), original.len());
            assert_eq!(stored[&q_path], original[&q_path]);
            assert_eq!(stored[&shared_path], original[&shared_path]);
        }
    }

    #[test]
    fn missing_approval_preference_defaults_to_on_request_and_explicit_values_survive() {
        let legacy: NativeFeaturePreferences = serde_json::from_value(json!({
            "subagentModel": "child-example", "codeMode": false
        }))
        .unwrap();
        assert_eq!(legacy.approval_policy, json!("on-request"));
        assert_eq!(legacy.subagent_model.as_deref(), Some("child-example"));
        assert_eq!(legacy.code_mode, Some(false));
        assert!(!legacy.antigravity_enabled);
        assert_eq!(legacy.antigravity_path, None);
        assert!(legacy.antigravity_models.is_empty());
        assert_eq!(legacy.subagent_model_executor, None);
        for policy in [
            json!("never"),
            json!("untrusted"),
            json!({"granular": {
                "sandbox_approval": true, "rules": false, "mcp_elicitations": true,
                "skill_approval": false, "request_permissions": true
            }}),
        ] {
            let preferences: NativeFeaturePreferences =
                serde_json::from_value(json!({"approvalPolicy": policy})).unwrap();
            assert_eq!(preferences.approval_policy, policy);
        }
    }

    #[test]
    fn advanced_feature_catalog_omits_subagent_model_without_erasing_its_preference() {
        let preferences = NativeFeaturePreferences {
            subagent_model: Some("child-example".into()),
            subagent_model_provider: Some("provider-example".into()),
            code_mode: Some(false),
            ..Default::default()
        };
        let before = preferences.clone();
        let settings = preferences.settings();
        assert_eq!(settings.len(), 9);
        assert!(
            settings
                .iter()
                .all(|(entry, _)| entry.key != "subagentModel")
        );
        assert_eq!(settings[0].1, Some(json!(false)));
        assert_eq!(preferences, before);
    }
}
