//! Shared settings use cases and UI-neutral snapshots for Discord and desktop adapters.
use std::{path::PathBuf, sync::Arc};
use anyhow::{Context as _, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};
use crate::{config::{Config, InputMode}, i18n::MessageKey, message_access::{MessageAccess, MessagePolicy}, native::Native, voice::VoiceBridge};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SettingsKind {
    Hub,
    Model,
    SubagentModel,
    Effort,
    Fast,
    Permissions,
    ApprovalPolicy,
    Input,
    Voice,
    Updates,
    MessageAccess,
    PairedAccounts,
    Whitelist,
    Blacklist,
    RegisteredChannels,
    NativeFeatures,
    Antigravity,
    AntigravityAutoApprove,
    AntigravitySetting(AgySettingKey),
    NativeFeature(NativeFeatureKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgySettingKey {
    ToolPermission,
    ArtifactReview,
    TerminalSandbox,
    NonWorkspaceAccess,
    Allow,
    Ask,
    Deny,
}

impl AgySettingKey {
    pub(crate) const ALL: [Self; 7] = [
        Self::ToolPermission,
        Self::ArtifactReview,
        Self::TerminalSandbox,
        Self::NonWorkspaceAccess,
        Self::Allow,
        Self::Ask,
        Self::Deny,
    ];

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::ToolPermission => "tool-permission",
            Self::ArtifactReview => "artifact-review",
            Self::TerminalSandbox => "sandbox",
            Self::NonWorkspaceAccess => "non-workspace",
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::ToolPermission => "toolPermission",
            Self::ArtifactReview => "artifactReviewPolicy",
            Self::TerminalSandbox => "enableTerminalSandbox",
            Self::NonWorkspaceAccess => "allowNonWorkspaceAccess",
            Self::Allow => "/permissions/allow",
            Self::Ask => "/permissions/ask",
            Self::Deny => "/permissions/deny",
        }
    }

    pub(crate) fn label(self, language: crate::i18n::Language) -> &'static str {
        language.message(match self {
            Self::ToolPermission => MessageKey::CommandsAgyToolPermission,
            Self::ArtifactReview => MessageKey::CommandsAgyArtifactReview,
            Self::TerminalSandbox => MessageKey::CommandsAgyTerminalSandbox,
            Self::NonWorkspaceAccess => MessageKey::CommandsAgyNonWorkspaceAccess,
            Self::Allow => MessageKey::CommandsAgyAllowRules,
            Self::Ask => MessageKey::CommandsAgyAskRules,
            Self::Deny => MessageKey::CommandsAgyDenyRules,
        })
    }

    pub(crate) fn is_rule_list(self) -> bool {
        matches!(self, Self::Allow | Self::Ask | Self::Deny)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NativeFeatureKey {
    CodeMode,
    CodeModeHost,
    MultiAgentV2,
    WaitAgent,
    AsyncNotifications,
    RequestUserInput,
    Clock,
    TokenBudget,
    HistoryNotes,
    SubagentModel,
}

impl NativeFeatureKey {
    pub(crate) const ALL: [Self; 9] = [
        Self::CodeMode,
        Self::CodeModeHost,
        Self::MultiAgentV2,
        Self::WaitAgent,
        Self::AsyncNotifications,
        Self::RequestUserInput,
        Self::Clock,
        Self::TokenBudget,
        Self::HistoryNotes,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::CodeMode => "codeMode",
            Self::CodeModeHost => "codeModeHost",
            Self::MultiAgentV2 => "multiAgentV2",
            Self::WaitAgent => "waitAgent",
            Self::AsyncNotifications => "asyncNotifications",
            Self::RequestUserInput => "requestUserInput",
            Self::Clock => "clock",
            Self::TokenBudget => "tokenBudget",
            Self::HistoryNotes => "historyNotes",
            Self::SubagentModel => "subagentModel",
        }
    }

    pub(crate) fn from_key(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.key() == value)
    }

    pub(crate) fn label(self, language: crate::i18n::Language) -> &'static str {
        match self {
            Self::CodeMode => language.message(MessageKey::CommandsCodeMode004c354e),
            Self::CodeModeHost => language.message(MessageKey::CommandsCodeModeHost4cb2e89d),
            Self::MultiAgentV2 => language.message(MessageKey::CommandsMultiAgentV26dd758e9),
            Self::WaitAgent => language.message(MessageKey::CommandsWaitForAgents032f9048),
            Self::AsyncNotifications => {
                language.message(MessageKey::CommandsAsyncNotifications0c673297)
            }
            Self::RequestUserInput => language.message(MessageKey::CommandsNativeQuestionsD61baa04),
            Self::Clock => language.message(MessageKey::CommandsClockAndTimeReminders4246b655),
            Self::TokenBudget => language.message(MessageKey::CommandsTokenBudget6a6282bf),
            Self::HistoryNotes => language.message(MessageKey::CommandsHistoryNotes234724c3),
            Self::SubagentModel => {
                language.message(MessageKey::CommandsDefaultSubagentModel9811e108)
            }
        }
    }

    pub(crate) fn config_paths(self) -> &'static [&'static str] {
        match self {
            Self::CodeMode => &["features.code_mode.enabled"],
            Self::CodeModeHost => &["features.code_mode_host.enabled"],
            Self::MultiAgentV2 => &["features.multi_agent_v2.enabled"],
            Self::WaitAgent => &["features.multi_agent_v2.wait_agent_enabled"],
            Self::AsyncNotifications => &["features.send_message_to_user_async"],
            Self::RequestUserInput => &[
                "tools.experimental_request_user_input.enabled",
                "features.default_mode_request_user_input",
            ],
            Self::Clock => &["features.current_time_reminder.enabled"],
            Self::TokenBudget => &["features.token_budget.enabled"],
            Self::HistoryNotes => &["features.token_budget.use_history_notes_extension"],
            Self::SubagentModel => &[
                "agents.default_subagent_model",
                "agents.default_subagent_model_provider",
                "agents.default_subagent_executor",
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SettingsChoice {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsChoiceFreshness {
    Current,
    ModelChanged,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct SettingsModelScopeChanged;

impl std::fmt::Display for SettingsModelScopeChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("settings model changed before the choice was applied")
    }
}

impl std::error::Error for SettingsModelScopeChanged {}

pub(crate) fn settings_kind_uses_model_catalog(kind: SettingsKind) -> bool {
    matches!(
        kind,
        SettingsKind::Model
            | SettingsKind::SubagentModel
            | SettingsKind::Effort
            | SettingsKind::Fast
            | SettingsKind::NativeFeature(NativeFeatureKey::SubagentModel)
    )
}

pub(crate) fn effort_settings_choice(item: &Value, language: crate::i18n::Language) -> Option<SettingsChoice> {
    let value = item
        .get("reasoningEffort")
        .and_then(Value::as_str)
        .or_else(|| item.as_str())?;
    let label = match value {
        "none" => language.message(MessageKey::CommandsNone45e9a015),
        "minimal" => language.message(MessageKey::CommandsMinimalB4fec1ec),
        "low" => language.message(MessageKey::CommandsLowCbc3febe),
        "medium" => language.message(MessageKey::CommandsMediumEd9c6849),
        "high" => language.message(MessageKey::CommandsHigh454ac2f1),
        "xhigh" => language.message(MessageKey::CommandsExtraHighFd13615e),
        "max" => language.message(MessageKey::CommandsMaximumAe40202b),
        "ultra" => language.message(MessageKey::CommandsUltra0f57cfc1),
        _ => value,
    };
    Some(SettingsChoice {
        value: value.to_owned(),
        label: label.to_owned(),
    })
}

pub(crate) fn model_effort_settings_choice(
    item: &Value,
    language: crate::i18n::Language,
) -> Option<SettingsChoice> {
    let mut choice = effort_settings_choice(item, language)?;
    if item.get("agyOrchestration").and_then(Value::as_bool) == Some(true) {
        choice.label = if let Some(actual) = item.get("agyEffort").and_then(Value::as_str) {
            language
                .message(MessageKey::CommandsAgyUltraEffortAliasLabel)
                .replace(
                    "{actual}",
                    &effort_settings_choice(&json!(actual), language)?.label,
                )
        } else {
            language
                .message(MessageKey::CommandsUltraCodexProactiveOrchestrationAgyDefault5b74f52a)
                .to_owned()
        };
    } else if let Some(actual) = item.get("agyEffort").and_then(Value::as_str) {
        choice.label = language
            .message(MessageKey::CommandsAgyEffortAliasLabel)
            .replace(
                "{actual}",
                &effort_settings_choice(&json!(actual), language)?.label,
            );
    }
    Some(choice)
}

pub(crate) fn model_effort_settings_choices(
    model: &Value,
    language: crate::i18n::Language,
) -> Vec<SettingsChoice> {
    let mut items = model
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let aliases = model
        .get("reasoningEffortAliases")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for alias in aliases {
        let (Some(requested), Some(actual)) = (
            alias.get("requested").and_then(Value::as_str),
            alias.get("actual").and_then(Value::as_str),
        ) else {
            continue;
        };
        if items.iter().any(|item| {
            item.get("reasoningEffort")
                .and_then(Value::as_str)
                .or_else(|| item.as_str())
                == Some(requested)
        }) {
            continue;
        }
        let alias_item = json!({
            "reasoningEffort": requested,
            "agyEffort": actual,
            "source": "agy-effort-alias"
        });
        let insert_at = items
            .iter()
            .position(|item| {
                item.get("reasoningEffort")
                    .and_then(Value::as_str)
                    .or_else(|| item.as_str())
                    == Some(actual)
            })
            .map(|index| index + 1)
            .unwrap_or(items.len());
        items.insert(insert_at, alias_item);
    }
    items
        .iter()
        .filter_map(|item| model_effort_settings_choice(item, language))
        .collect()
}

pub(crate) fn agy_variant_effort(model: &Value, model_id: &str) -> Option<String> {
    model
        .get("agyVariants")
        .and_then(Value::as_array)?
        .iter()
        .find(|variant| variant.get("id").and_then(Value::as_str) == Some(model_id))?
        .get("effort")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(crate) fn service_tier_settings_choice(
    item: &Value,
    language: crate::i18n::Language,
) -> Option<SettingsChoice> {
    let value = item
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| item.as_str())?;
    let label = match value {
        "priority" => language.message(MessageKey::CommandsPriorityE9c80c39),
        "standard" => language.message(MessageKey::CommandsStandard0dd9d07b),
        "flex" => language.message(MessageKey::CommandsFlex0f24853f),
        _ => item.get("name").and_then(Value::as_str).unwrap_or(value),
    };
    Some(SettingsChoice {
        value: value.to_owned(),
        label: label.to_owned(),
    })
}

pub(crate) fn effort_current_label(
    choices: &[SettingsChoice],
    effort: Option<&Value>,
    language: crate::i18n::Language,
) -> String {
    // A null native value can be unset or unavailable; do not invent a default.
    choice_current_label(choices, effort.and_then(Value::as_str), language)
}

pub(crate) fn choice_current_label(
    choices: &[SettingsChoice],
    current: Option<&str>,
    language: crate::i18n::Language,
) -> String {
    current
        .and_then(|value| choices.iter().find(|choice| choice.value == value))
        .map(|choice| choice.label.clone())
        .or_else(|| current.map(str::to_owned))
        .unwrap_or_else(|| {
            language
                .message(MessageKey::CommandsNotReported0a1fa855)
                .to_owned()
        })
}

pub(crate) fn model_catalog_warnings(mut current: String, catalog: &Value) -> String {
    for warning in catalog
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        current.push('\n');
        current.push_str(warning);
    }
    current
}

pub(crate) fn settings_home_values(
    thread: &Value,
    mode: crate::config::InputMode,
    selected_voice: Option<String>,
    language: crate::i18n::Language,
) -> (Option<String>, String, Vec<SettingsChoice>) {
    let mut current = format_settings_with_language(&settings_snapshot(thread), language);
    current.push_str(&format!(
        "\n{}: {}",
        language.message(MessageKey::CommandsInputDelivery47ae354d),
        input_mode(mode, language)
    ));
    if let Some(provider) = thread_model_provider(thread) {
        current.push_str(&format!(
            "\n{}: {provider}",
            language.message(MessageKey::CommandsActiveModelProviderEa54b5ed)
        ));
    }
    let voice = selected_voice.unwrap_or_else(|| {
        language
            .message(MessageKey::CommandsCodexDefault3029befd)
            .to_owned()
    });
    current.push_str(&format!(
        "\n{}: {voice}",
        language.message(MessageKey::CommandsVoiceProfile2f25af45)
    ));
    let choices = [
        ("model", MessageKey::CommandsSettingsHomeModel),
        (
            "subagent-model",
            MessageKey::CommandsSettingsHomeSubagentModel,
        ),
        ("effort", MessageKey::CommandsSettingsHomeEffort),
        ("fast", MessageKey::CommandsSettingsHomeFast),
        ("updates", MessageKey::CommandsSettingsHomeUpdates),
        ("permissions", MessageKey::CommandsSettingsHomePermissions),
        (
            "approval-policy",
            MessageKey::CommandsSettingsHomeApprovalPolicy,
        ),
        ("input", MessageKey::CommandsSettingsHomeInput),
        (
            "message-access",
            MessageKey::CommandsSettingsHomeMessageAccess,
        ),
        ("voice", MessageKey::CommandsSettingsHomeVoice),
        ("antigravity", MessageKey::CommandsAgyTitle),
        (
            "native-features",
            MessageKey::CommandsSettingsHomeNativeFeatures,
        ),
    ]
    .into_iter()
    .map(|(value, label)| SettingsChoice {
        value: value.to_owned(),
        label: language.message(label).to_owned(),
    })
    .collect();
    (None, current, choices)
}

pub(crate) fn subagent_model_settings_values(
    thread: &Value,
    snapshot: &crate::native::NativeFeatureSnapshot,
    catalog: &Value,
    language: crate::i18n::Language,
) -> (Option<String>, String, Vec<SettingsChoice>) {
    let saved = snapshot.preferences.subagent_model.clone();
    let effective = effective_native_feature_text(
        &snapshot.native_config,
        NativeFeatureKey::SubagentModel,
        language,
    );
    let selected = subagent_preference_scope(&snapshot.preferences, catalog)
        .and_then(|value| normalized_model_value_with_language(catalog, &value, language));
    let saved_label = saved.as_ref().map(|model| {
        format!(
            "{} — {} / {}",
            model,
            snapshot
                .preferences
                .subagent_model_executor
                .as_deref()
                .unwrap_or("codex"),
            snapshot
                .preferences
                .subagent_model_provider
                .as_deref()
                .unwrap_or_else(|| language.message(MessageKey::CommandsNativeDefault308250fc)),
        )
    });
    let current = format!(
        "{}: {}\n{}: {}\n{}: {}\n{}",
        language.message(MessageKey::CommandsSavedPreference47ce2cf1),
        saved_label
            .as_deref()
            .unwrap_or_else(|| language.message(MessageKey::CommandsNativeDefault308250fc)),
        language.message(MessageKey::CommandsActiveModelProviderEa54b5ed),
        thread_model_provider(thread)
            .unwrap_or_else(|| language.message(MessageKey::CommandsModelProviderUnknown)),
        language.message(MessageKey::CommandsEffectiveCodexConfigEeb899f3),
        effective,
        child_default_timing(language)
    );
    let current_value = selected
        .or(saved)
        .unwrap_or_else(|| "native-default".to_owned());
    let choices = subagent_model_choices(catalog, language);
    (
        Some(current_value),
        model_catalog_warnings(current, catalog),
        choices,
    )
}

pub(crate) fn child_default_timing(language: crate::i18n::Language) -> &'static str {
    language.message(MessageKey::CommandsTheSavedChildDefaultAppliesOnTheNextThre8f9e7706)
}

pub(crate) fn child_default_saved_notice(language: crate::i18n::Language) -> String {
    format!(
        "{}\n{}",
        language.message(MessageKey::CommandsSaved7b6b2c9c),
        child_default_timing(language)
    )
}

pub(crate) fn advanced_settings_values(
    preferences: &crate::native::NativeFeaturePreferences,
    language: crate::i18n::Language,
) -> (Option<String>, String, Vec<SettingsChoice>) {
    let descriptors = preferences.settings();
    let mut current = language
        .message(MessageKey::CommandsNativeFeatureSwitchesApplyAfterTheDiscodex1ea6deb2)
        .to_owned();
    for key in NativeFeatureKey::ALL {
        let value = native_feature_saved_value(preferences, key);
        current.push_str(&format!(
            "\n• {}: {}",
            key.label(language),
            native_preference_label(value.as_ref(), language)
        ));
    }
    let choices = descriptors
        .into_iter()
        .filter_map(|(descriptor, _)| {
            NativeFeatureKey::from_key(descriptor.key).map(|key| SettingsChoice {
                value: format!("native-feature:{}", key.key()),
                label: key.label(language).to_owned(),
            })
        })
        .collect::<Vec<_>>();
    (None, current, choices)
}

pub(crate) fn message_policy_key(policy: MessagePolicy) -> &'static str {
    match policy {
        MessagePolicy::Everyone => "everyone",
        MessagePolicy::RegisteredServer => "registered-server",
        MessagePolicy::RegisteredChannels => "registered-channels",
        MessagePolicy::Paired => "paired",
    }
}

pub(crate) fn message_policy_label(policy: MessagePolicy, language: crate::i18n::Language) -> &'static str {
    match policy {
        MessagePolicy::Everyone => {
            language.message(MessageKey::CommandsEveryoneIncludingDms3d00363e)
        }
        MessagePolicy::RegisteredServer => {
            language.message(MessageKey::CommandsAllMembersInRegisteredServerE81e3e9f)
        }
        MessagePolicy::RegisteredChannels => {
            language.message(MessageKey::CommandsRegisteredChannels5a818eab)
        }
        MessagePolicy::Paired => language.message(MessageKey::CommandsPairedAccounts263399a8),
    }
}

pub(crate) fn is_access_list(kind: SettingsKind) -> bool {
    matches!(
        kind,
        SettingsKind::PairedAccounts
            | SettingsKind::Whitelist
            | SettingsKind::Blacklist
            | SettingsKind::RegisteredChannels
    )
}

pub(crate) fn access_list(access: &MessageAccess, kind: SettingsKind) -> &[u64] {
    match kind {
        SettingsKind::PairedAccounts => &access.paired_user_ids,
        SettingsKind::Whitelist => &access.whitelist,
        SettingsKind::Blacklist => &access.blacklist,
        SettingsKind::RegisteredChannels => &access.channel_ids,
        _ => &[],
    }
}

pub(crate) fn apply_message_access_list_edit(
    access: &mut MessageAccess,
    kind: SettingsKind,
    add: bool,
    id: u64,
) -> anyhow::Result<()> {
    let list = match kind {
        SettingsKind::PairedAccounts => &mut access.paired_user_ids,
        SettingsKind::Whitelist => &mut access.whitelist,
        SettingsKind::Blacklist => &mut access.blacklist,
        SettingsKind::RegisteredChannels => &mut access.channel_ids,
        _ => bail!("not a message access list"),
    };
    if add {
        if list.contains(&id) {
            bail!("ID {id} is already registered");
        }
        list.push(id);
    } else {
        let Some(index) = list.iter().position(|value| *value == id) else {
            bail!("ID {id} is not registered in this list");
        };
        list.remove(index);
    }
    Ok(())
}

pub(crate) fn native_feature_saved_value(
    preferences: &crate::native::NativeFeaturePreferences,
    key: NativeFeatureKey,
) -> Option<String> {
    match key {
        NativeFeatureKey::CodeMode => preferences.code_mode.map(|value| value.to_string()),
        NativeFeatureKey::CodeModeHost => preferences.code_mode_host.map(|value| value.to_string()),
        NativeFeatureKey::MultiAgentV2 => preferences.multi_agent_v2.map(|value| value.to_string()),
        NativeFeatureKey::WaitAgent => preferences.wait_agent.map(|value| value.to_string()),
        NativeFeatureKey::AsyncNotifications => preferences
            .async_notifications
            .map(|value| value.to_string()),
        NativeFeatureKey::RequestUserInput => preferences
            .request_user_input
            .map(|value| value.to_string()),
        NativeFeatureKey::Clock => preferences.clock.map(|value| value.to_string()),
        NativeFeatureKey::TokenBudget => preferences.token_budget.map(|value| value.to_string()),
        NativeFeatureKey::HistoryNotes => preferences.history_notes.map(|value| value.to_string()),
        NativeFeatureKey::SubagentModel => preferences.subagent_model.clone(),
    }
}

pub(crate) fn native_preference_label(value: Option<&String>, language: crate::i18n::Language) -> String {
    match value.map(String::as_str) {
        Some("true") => language.message(MessageKey::CommandsOn65845774).to_owned(),
        Some("false") => language.message(MessageKey::CommandsOff5677591d).to_owned(),
        Some(value) => value.to_owned(),
        None => language
            .message(MessageKey::CommandsNativeDefault308250fc)
            .to_owned(),
    }
}

pub(crate) fn effective_native_feature_text(
    native_config: &Value,
    key: NativeFeatureKey,
    language: crate::i18n::Language,
) -> String {
    key.config_paths()
        .iter()
        .map(|path| {
            let value = effective_config_value(native_config, path)
                .map(Value::to_string)
                .unwrap_or_else(|| {
                    language
                        .message(MessageKey::CommandsNotReportedEb1c723f)
                        .to_owned()
                });
            let origin = native_config
                .pointer("/origins")
                .and_then(|origins| origins.get(*path));
            match origin {
                Some(origin) => format!("{path} = {value} ({})", origin),
                None => format!("{path} = {value}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn effective_config_value<'a>(native_config: &'a Value, path: &str) -> Option<&'a Value> {
    let mut value = native_config.get("config")?;
    for segment in path.split('.') {
        let camel = snake_to_camel(segment);
        value = value.get(segment).or_else(|| value.get(camel))?;
    }
    Some(value)
}

pub(crate) fn snake_to_camel(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut uppercase = false;
    for character in value.chars() {
        if character == '_' {
            uppercase = true;
        } else if uppercase {
            output.extend(character.to_uppercase());
            uppercase = false;
        } else {
            output.push(character);
        }
    }
    output
}

pub(crate) fn set_native_feature_preference(
    preferences: &mut crate::native::NativeFeaturePreferences,
    key: NativeFeatureKey,
    selected: Option<&str>,
) -> anyhow::Result<()> {
    if key == NativeFeatureKey::SubagentModel {
        preferences.subagent_model = selected.map(str::to_owned);
        return Ok(());
    }
    let value = match selected {
        Some("true") | Some("on") => Some(true),
        Some("false") | Some("off") => Some(false),
        None => None,
        _ => bail!("unknown native feature preference"),
    };
    match key {
        NativeFeatureKey::CodeMode => preferences.code_mode = value,
        NativeFeatureKey::CodeModeHost => preferences.code_mode_host = value,
        NativeFeatureKey::MultiAgentV2 => preferences.multi_agent_v2 = value,
        NativeFeatureKey::WaitAgent => preferences.wait_agent = value,
        NativeFeatureKey::AsyncNotifications => preferences.async_notifications = value,
        NativeFeatureKey::RequestUserInput => preferences.request_user_input = value,
        NativeFeatureKey::Clock => preferences.clock = value,
        NativeFeatureKey::TokenBudget => preferences.token_budget = value,
        NativeFeatureKey::HistoryNotes => preferences.history_notes = value,
        NativeFeatureKey::SubagentModel => unreachable!(),
    }
    Ok(())
}

pub(crate) fn set_subagent_model_selection(
    preferences: &mut crate::native::NativeFeaturePreferences,
    selection: Option<&crate::model_catalog::ModelSelection>,
) {
    preferences.subagent_model = selection.map(|value| value.model.clone());
    preferences.subagent_model_provider = selection.map(|value| value.provider.clone());
    preferences.subagent_model_executor = selection.map(|value| value.executor.as_str().to_owned());
}

pub(crate) fn antigravity_settings_values(
    preferences: &crate::native::NativeFeaturePreferences,
    language: crate::i18n::Language,
) -> (Option<String>, String, Vec<SettingsChoice>) {
    let enabled = if preferences.antigravity_enabled {
        "on"
    } else {
        "off"
    };
    let status = if preferences.antigravity_enabled {
        language.message(MessageKey::CommandsOn2666c335)
    } else {
        language.message(MessageKey::CommandsOff38c695fb)
    };
    let models = if preferences.antigravity_models.is_empty() {
        language
            .message(MessageKey::CommandsNoModelIdsConfiguredA4c9e21b)
            .to_owned()
    } else {
        preferences.antigravity_models.join("\n")
    };
    let sharing = match preferences.antigravity_share_settings {
        Some(true) => language.message(MessageKey::CommandsSharedExistingSettingsAfa9effe),
        Some(false) => language.message(MessageKey::CommandsSeparateSettings016de9e5),
        None => language.message(MessageKey::CommandsChooseDuringSetup9558c853),
    };
    let current = format!(
        "{status}\n{sharing}\n{}:\n{models}\n{}",
        language.message(MessageKey::CommandsModelIdsF17651aa),
        language.message(MessageKey::CommandsDiscodexManagesItsOwnAgyCliEnablingItSta9ab1e631),
    );
    let mut choices = [
        ("on", language.message(MessageKey::CommandsOn65845774)),
        ("off", language.message(MessageKey::CommandsOff5677591d)),
        (
            "sharing",
            language.message(MessageKey::CommandsSettingsSharing2e85b807),
        ),
        (
            "models",
            language.message(MessageKey::CommandsModelIdsF17651aa),
        ),
        (
            "refresh-models",
            language.message(MessageKey::CommandsUpdateModelList3e0facf2),
        ),
    ]
    .into_iter()
    .map(|(value, label)| SettingsChoice {
        value: value.to_owned(),
        label: label.to_owned(),
    })
    .collect::<Vec<_>>();
    choices.push(SettingsChoice {
        value: "auto-approve".to_owned(),
        label: language
            .message(MessageKey::CommandsAgyAutoApprove)
            .to_owned(),
    });
    choices.extend(AgySettingKey::ALL.into_iter().map(|key| SettingsChoice {
        value: format!("agy-setting:{}", key.id()),
        label: key.label(language).to_owned(),
    }));
    (Some(enabled.to_owned()), current, choices)
}

pub(crate) fn agy_setting_values(
    key: AgySettingKey,
    settings: &Value,
    language: crate::i18n::Language,
) -> anyhow::Result<(Option<String>, String, Vec<SettingsChoice>)> {
    let choices = match key {
        AgySettingKey::ToolPermission => vec![
            ("request-review", MessageKey::CommandsAgyRequestReview),
            (
                "proceed-in-sandbox",
                MessageKey::CommandsAgyProceedInSandbox,
            ),
            ("strict", MessageKey::CommandsAgyStrict),
            ("always-proceed", MessageKey::CommandsAgyAlwaysProceed),
        ],
        AgySettingKey::ArtifactReview => vec![
            ("asks-for-review", MessageKey::CommandsAgyAsksForReview),
            ("agent-decides", MessageKey::CommandsAgyAgentDecides),
            ("always-proceed", MessageKey::CommandsAgyAlwaysProceed),
        ],
        AgySettingKey::TerminalSandbox | AgySettingKey::NonWorkspaceAccess => vec![
            ("true", MessageKey::CommandsOn65845774),
            ("false", MessageKey::CommandsOff5677591d),
        ],
        AgySettingKey::Allow | AgySettingKey::Ask | AgySettingKey::Deny => vec![
            ("add", MessageKey::CommandsAgyAddRule),
            ("remove", MessageKey::CommandsAgyRemoveRule),
        ],
    }
    .into_iter()
    .map(|(value, label)| SettingsChoice {
        value: value.to_owned(),
        label: language.message(label).to_owned(),
    })
    .collect::<Vec<_>>();
    if key.is_rule_list() {
        let rules = agy_permission_rules(settings, key, language)?;
        let mut current = if rules.is_empty() {
            language.message(MessageKey::CommandsAgyNoRules).to_owned()
        } else {
            rules
                .iter()
                .map(|rule| {
                    format!(
                        "• {}",
                        rule.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| rule.to_string())
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        current.push_str(&format!(
            "\n{}",
            language.message(MessageKey::CommandsAgyRulePrecedence)
        ));
        if key == AgySettingKey::Allow {
            current.push_str(&format!(
                "\n{} `{}`",
                language.message(MessageKey::CommandsAgyManagedRule),
                crate::agy_settings::MANAGED_MCP_ALLOW_RULE,
            ));
        }
        return Ok((None, current, choices));
    }
    let default = match key {
        AgySettingKey::ToolPermission => json!("request-review"),
        AgySettingKey::ArtifactReview => json!("asks-for-review"),
        _ => json!(false),
    };
    let saved = settings.get(key.key()).filter(|value| !value.is_null());
    let effective = saved.unwrap_or(&default);
    let value = effective
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| effective.to_string());
    let label = choices
        .iter()
        .find(|choice| choice.value == value)
        .map(|choice| choice.label.as_str())
        .unwrap_or(&value);
    let current = format!(
        "{}: {label}\n{}\n{}",
        if saved.is_some() {
            language.message(MessageKey::CommandsAgyEffectivePreference)
        } else {
            language.message(MessageKey::CommandsAgyInheritedDefault)
        },
        language.message(MessageKey::CommandsAgyNativePolicyNotice),
        language.message(MessageKey::CommandsAgyAppliesNextSession),
    );
    Ok((Some(value), current, choices))
}

pub(crate) fn agy_permission_rules(
    settings: &Value,
    key: AgySettingKey,
    language: crate::i18n::Language,
) -> anyhow::Result<Vec<Value>> {
    match settings.pointer(key.key()) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(rules)) => Ok(rules.clone()),
        Some(_) => bail!(
            "{}: {}",
            language.message(MessageKey::CommandsAgyInvalidRuleList),
            key.key()
        ),
    }
}

pub(crate) fn set_antigravity_text_preference(
    preferences: &mut crate::native::NativeFeaturePreferences,
    field: &str,
    value: &str,
) -> anyhow::Result<()> {
    match field {
        "models" => {
            preferences.antigravity_models = value
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        }
        _ => bail!("unknown Antigravity text setting"),
    }
    Ok(())
}

pub(crate) fn ensure_settings_model_scope(
    kind: SettingsKind,
    expected_model_scope: &Option<String>,
    current_model_scope: Option<String>,
) -> anyhow::Result<()> {
    if matches!(kind, SettingsKind::Effort | SettingsKind::Fast)
        && *expected_model_scope != current_model_scope
    {
        return Err(SettingsModelScopeChanged.into());
    }
    Ok(())
}

pub(crate) fn ensure_settings_apply_target(
    expected_generation: Option<u64>,
    current_generation: u64,
    expected_thread_id: Option<&str>,
    current_thread_id: &str,
) -> anyhow::Result<()> {
    if expected_generation.is_some_and(|generation| generation != current_generation) {
        bail!("Codex connection changed before the setting could be applied");
    }
    if expected_thread_id.is_some_and(|thread| thread != current_thread_id) {
        bail!("Codex conversation changed before the setting could be applied");
    }
    Ok(())
}

pub(crate) fn settings_choice_freshness(
    kind: SettingsKind,
    previous_model_scope: &Option<String>,
    current_model_scope: &Option<String>,
    choices: &[SettingsChoice],
    requested: &str,
) -> SettingsChoiceFreshness {
    if matches!(kind, SettingsKind::Effort | SettingsKind::Fast)
        && previous_model_scope != current_model_scope
    {
        SettingsChoiceFreshness::ModelChanged
    } else if kind == SettingsKind::Model || choices.iter().any(|choice| choice.value == requested)
    {
        SettingsChoiceFreshness::Current
    } else {
        SettingsChoiceFreshness::Unavailable
    }
}

pub(crate) fn thread_model_executor(thread: &Value) -> Option<crate::model_catalog::ModelExecutor> {
    thread
        .get("modelExecutor")
        .or_else(|| thread.get("model_executor"))
        .cloned()
        .map(serde_json::from_value::<crate::model_catalog::ModelExecutor>)
        .transpose()
        .ok()
        .map(Option::unwrap_or_default)
}

pub(crate) fn thread_model_provider(thread: &Value) -> Option<&str> {
    if thread_model_executor(thread)? == crate::model_catalog::ModelExecutor::Antigravity {
        Some("antigravity")
    } else {
        thread.get("modelProvider").and_then(Value::as_str)
    }
}

pub(crate) fn thread_model_scope(thread: &Value) -> Option<String> {
    crate::settings_operation::model_scope(thread)
}

pub(crate) fn normalized_model_value(catalog: &Value, value: &str) -> Option<String> {
    normalized_model_value_with_language(catalog, value, crate::i18n::Language::English)
}

pub(crate) fn normalized_model_value_with_language(
    catalog: &Value,
    value: &str,
    language: crate::i18n::Language,
) -> Option<String> {
    let model = selected_model_with_language(catalog, &Some(value.to_owned()), language).ok()?;
    crate::model_catalog::selection_value(model)
        .or_else(|| crate::model_catalog::model_id(model))
        .map(str::to_owned)
}

pub(crate) fn subagent_preference_scope(
    preferences: &crate::native::NativeFeaturePreferences,
    catalog: &Value,
) -> Option<String> {
    let model = preferences.subagent_model.as_deref()?;
    let executor = preferences
        .subagent_model_executor
        .as_deref()
        .map(|executor| {
            serde_json::from_value::<crate::model_catalog::ModelExecutor>(json!(executor))
        })
        .transpose()
        .ok()?
        .unwrap_or_default();
    let provider = preferences
        .subagent_model_provider
        .as_deref()
        .or_else(|| catalog.get("catalogProvider").and_then(Value::as_str))
        .or_else(|| catalog.get("provider").and_then(Value::as_str));
    Some(match provider {
        Some(provider) => crate::model_catalog::ModelSelection {
            executor,
            provider: provider.to_owned(),
            model: model.to_owned(),
        }
        .value(),
        None => model.to_owned(),
    })
}

pub(crate) fn subagent_model_selection(
    catalog: &Value,
    requested: &str,
    _language: crate::i18n::Language,
) -> anyhow::Result<crate::model_catalog::ModelSelection> {
    crate::model_catalog::resolve_selection(catalog, requested)
}

pub(crate) fn model_change_requires_resume(
    active_executor: crate::model_catalog::ModelExecutor,
    active_provider: &str,
    selection: &crate::model_catalog::ModelSelection,
) -> bool {
    selection.executor != active_executor || selection.provider != active_provider
}

pub(crate) fn model_selection_for_choice(
    catalog: &Value,
    requested: &str,
    current_thread: &Value,
    language: crate::i18n::Language,
) -> anyhow::Result<crate::model_catalog::ModelSelection> {
    if let Ok(selection) = crate::model_catalog::resolve_selection(catalog, requested) {
        return Ok(selection);
    }

    // Stale settings buttons can carry the full provider-qualified selection
    // created from the earlier catalog. Preserve it and let Native validate it.
    if let Ok(parts) = serde_json::from_str::<Vec<String>>(requested) {
        let selection = match parts.as_slice() {
            [provider, model] if !provider.is_empty() && !model.is_empty() => {
                Some(crate::model_catalog::ModelSelection {
                    executor: crate::model_catalog::ModelExecutor::Codex,
                    provider: provider.clone(),
                    model: model.clone(),
                })
            }
            [executor, provider, model] if !provider.is_empty() && !model.is_empty() => {
                let executor = match executor.as_str() {
                    "codex" => Some(crate::model_catalog::ModelExecutor::Codex),
                    "antigravity" => Some(crate::model_catalog::ModelExecutor::Antigravity),
                    _ => None,
                };
                executor.map(|executor| crate::model_catalog::ModelSelection {
                    executor,
                    provider: provider.clone(),
                    model: model.clone(),
                })
            }
            _ => None,
        };
        if let Some(selection) = selection {
            return Ok(selection);
        }
    }

    // Legacy catalog entries can contain only a model ID. Reuse the active
    // executor/provider for that explicitly selected ID; Native remains the
    // authority on whether the model can actually be applied.
    let provider = thread_model_provider(current_thread)
        .or_else(|| catalog.get("provider").and_then(Value::as_str));
    let Some(provider) = provider else {
        bail!(
            "{}",
            language.message(MessageKey::CommandsModelProviderUnavailable)
        );
    };
    let executor = current_thread
        .get("modelExecutor")
        .cloned()
        .map(serde_json::from_value::<crate::model_catalog::ModelExecutor>)
        .transpose()
        .context("read current model executor")?
        .unwrap_or_default();
    Ok(crate::model_catalog::ModelSelection {
        executor,
        provider: provider.to_owned(),
        model: requested.to_owned(),
    })
}

pub(crate) fn catalog_model_choices(catalog: &Value) -> Vec<SettingsChoice> {
    crate::model_catalog::model_choices(catalog)
        .into_iter()
        .map(|(value, label)| SettingsChoice { value, label })
        .collect()
}

pub(crate) fn subagent_model_choices(catalog: &Value, language: crate::i18n::Language) -> Vec<SettingsChoice> {
    let mut choices = vec![SettingsChoice {
        value: "native-default".to_owned(),
        label: language
            .message(MessageKey::CommandsNativeDefault308250fc)
            .to_owned(),
    }];
    choices.extend(catalog_model_choices(catalog));
    choices
}

pub(crate) async fn validate_setting_choice(
    sub: &str,
    requested: &str,
    catalog: &Value,
    current_model: &Option<String>,
) -> anyhow::Result<()> {
    validate_setting_choice_with_language(
        sub,
        requested,
        catalog,
        current_model,
        crate::i18n::Language::English,
    )
    .await
}

pub(crate) async fn validate_setting_choice_with_language(
    sub: &str,
    requested: &str,
    catalog: &Value,
    current_model: &Option<String>,
    language: crate::i18n::Language,
) -> anyhow::Result<()> {
    match sub {
        // Model choices are presentation data. Native is the authority for
        // whether an explicitly selected exact ID can be applied.
        "model" => {}
        "effort" => {
            let model = selected_model_with_language(catalog, current_model, language)?;
            let valid = model
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .is_some_and(|xs| {
                    xs.iter().any(|x| {
                        x.get("reasoningEffort")
                            .and_then(Value::as_str)
                            .or_else(|| x.as_str())
                            == Some(requested)
                    })
                })
                || model
                    .get("reasoningEffortAliases")
                    .and_then(Value::as_array)
                    .is_some_and(|aliases| {
                        aliases.iter().any(|alias| {
                            alias.get("requested").and_then(Value::as_str) == Some(requested)
                        })
                    });
            if !valid {
                bail!(
                    "{}",
                    language
                        .message(MessageKey::CommandsEffortUnavailableForModel)
                        .replace("{value}", requested)
                );
            }
        }
        "fast" => {
            if requested.eq_ignore_ascii_case("off") {
                return Ok(());
            }
            let model = selected_model_with_language(catalog, current_model, language)?;
            let valid = model
                .get("serviceTiers")
                .and_then(Value::as_array)
                .is_some_and(|xs| {
                    xs.iter().any(|x| {
                        x.get("id").and_then(Value::as_str).or_else(|| x.as_str())
                            == Some(requested)
                    })
                });
            if !valid {
                bail!(
                    "{}",
                    language
                        .message(MessageKey::CommandsTierUnavailableForModel)
                        .replace("{value}", requested)
                );
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn selected_model<'a>(
    catalog: &'a Value,
    current_model: &Option<String>,
) -> anyhow::Result<&'a Value> {
    selected_model_with_language(catalog, current_model, crate::i18n::Language::English)
}

pub(crate) fn selected_model_with_language<'a>(
    catalog: &'a Value,
    current_model: &Option<String>,
    language: crate::i18n::Language,
) -> anyhow::Result<&'a Value> {
    let data = catalog
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!(language.message(MessageKey::CommandsModelListUnreadable)))?;
    let current_model = current_model
        .as_deref()
        .ok_or_else(|| anyhow!(language.message(MessageKey::CommandsCurrentModelUnavailable)))?;
    let selected =
        crate::model_catalog::resolve_selection(catalog, current_model).map_err(|_| {
            anyhow!(language.message(MessageKey::CommandsCurrentModelMissingFromCatalog))
        })?;
    data.iter()
        .find(|model| {
            crate::model_catalog::model_id(model) == Some(selected.model.as_str())
                && model
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or(selected.provider.as_str())
                    == selected.provider
        })
        .ok_or_else(|| {
            anyhow!(language.message(MessageKey::CommandsCurrentModelMissingFromCatalog))
        })
}

pub(crate) fn current_turn_settings_result_with_language(
    result: Option<&anyhow::Result<Value>>,
    language: crate::i18n::Language,
) -> &'static str {
    match result {
        Some(Ok(value)) => match value.get("status").and_then(Value::as_str) {
            Some("applied") => language
                .message(MessageKey::CommandsUpdatedSettingsForLaterWorkInTheActiveReC44c334f),
            Some("targetUnavailable") => language
                .message(MessageKey::CommandsCouldNotApplySettingsToTheActiveResponseCeec6b12),
            _ => language
                .message(MessageKey::CommandsCouldNotConfirmWhetherSettingsWereAppliedAce88396),
        },
        Some(Err(_)) => {
            language.message(MessageKey::CommandsCouldNotSendTheSettingsUpdateToTheActiveF50c2a86)
        }
        None => language.message(MessageKey::CommandsNoResponseIsInProgress16302856),
    }
}

pub(crate) fn format_settings_update_result_with_language(
    requested: &str,
    defaults: Option<&Value>,
    live_result: Option<&anyhow::Result<Value>>,
    language: crate::i18n::Language,
) -> String {
    format_settings_update_result_with_config(requested, defaults, live_result, language, None)
}

pub(crate) fn format_settings_update_result_with_config(
    requested: &str,
    defaults: Option<&Value>,
    live_result: Option<&anyhow::Result<Value>>,
    language: crate::i18n::Language,
    config: Option<&crate::config::Config>,
) -> String {
    let defaults = defaults
        .map(|value| format_settings_with_language(&settings_snapshot(value), language))
        .unwrap_or_else(|| {
            language
                .message(MessageKey::CommandsCouldNotReadSettings23412011)
                .to_owned()
        });
    let mut result = format!(
        "{}: `{requested}`\n{}\n{}:\n{defaults}\n{}: {}",
        language.message(MessageKey::CommandsRequestedValue9eb15821),
        language.message(MessageKey::CommandsSentTheConversationDefaultUpdateRequest7097b23b),
        language.message(MessageKey::CommandsConversationDefaultsAfterUpdate4806be50),
        language.message(MessageKey::CommandsCurrentResponse5c4a065c),
        current_turn_settings_result_with_language(live_result, language),
    );
    if let Some(Err(error)) = live_result {
        result.push('\n');
        result.push_str(&settings_error_detail(error, language, config));
    }
    result
}

pub(crate) fn format_permission_settings_result_with_language(
    display_name: &str,
    settings: &Value,
    active_turn: bool,
    language: crate::i18n::Language,
) -> String {
    let current = if active_turn {
        language.message(MessageKey::CommandsPermissionUseInTheActiveResponseWasNotCo485a99f0)
    } else {
        language.message(MessageKey::CommandsNoResponseIsInProgress16302856)
    };
    let prefix = language.message(MessageKey::CommandsSetPermissionProfileTo1589e2c0);
    let suffix = language.message(MessageKey::CommandsMessage6c09cf1f);
    format!(
        "{prefix} `{display_name}`{suffix}\n{}:\n{}\n{}: {current}",
        language.message(MessageKey::CommandsConversationDefaultsAfterUpdate4806be50),
        format_settings_with_language(settings, language),
        language.message(MessageKey::CommandsCurrentResponse5c4a065c),
    )
}

pub(crate) fn settings_snapshot(thread: &Value) -> Value {
    let keys = [
        "model",
        "effort",
        "serviceTier",
        "activePermissionProfile",
        "approvalPolicy",
        "sandboxPolicy",
        "approvalsReviewer",
    ];
    let mut result = serde_json::Map::new();
    for key in keys {
        result.insert(
            key.to_owned(),
            thread
                .get(key)
                .cloned()
                .unwrap_or_else(|| json!("native snapshot did not report this field")),
        );
    }
    Value::Object(result)
}

pub(crate) fn permission_profile_display_name(profile: &Value, index: usize) -> String {
    permission_profile_display_name_with_language(profile, index, crate::i18n::Language::English)
}

pub(crate) fn permission_profile_display_name_with_language(
    profile: &Value,
    index: usize,
    language: crate::i18n::Language,
) -> String {
    profile
        .get("displayName")
        .and_then(Value::as_str)
        .or_else(|| profile.get("name").and_then(Value::as_str))
        .or_else(|| profile.get("label").and_then(Value::as_str))
        .map(|name| humanize_permission_profile_name_with_language(name, language))
        .or_else(|| match profile.get("id").and_then(Value::as_str) {
            Some(":read-only") => Some(
                language
                    .message(MessageKey::CommandsReadOnlyBcb1d1ea)
                    .to_owned(),
            ),
            Some(":danger-full-access") => Some(
                language
                    .message(MessageKey::CommandsFullAccess6238ce12)
                    .to_owned(),
            ),
            Some(":workspace") => Some(
                language
                    .message(MessageKey::CommandsWorkspace8128b964)
                    .to_owned(),
            ),
            _ => None,
        })
        .unwrap_or_else(|| {
            format!(
                "{} {}",
                language.message(MessageKey::CommandsPermissionProfileC7e4bb80),
                index + 1
            )
        })
}

pub(crate) fn permission_profile_aliases(display_name: &str, id: Option<&str>) -> Vec<String> {
    let mut aliases = vec![display_name.to_owned()];
    match id {
        Some(":read-only") => aliases.extend(["Read Only".to_owned(), "읽기 전용".to_owned()]),
        Some(":danger-full-access") => {
            aliases.extend(["Full Access".to_owned(), "전체 접근".to_owned()])
        }
        Some(":workspace") => aliases.extend(["Workspace".to_owned(), "작업 공간".to_owned()]),
        _ => {}
    }
    aliases
}

pub(crate) fn humanize_permission_profile_name(value: &str) -> String {
    humanize_permission_profile_name_with_language(value, crate::i18n::Language::English)
}

pub(crate) fn humanize_permission_profile_name_with_language(
    value: &str,
    language: crate::i18n::Language,
) -> String {
    match value {
        ":read-only" => language
            .message(MessageKey::CommandsReadOnlyBcb1d1ea)
            .to_owned(),
        ":danger-full-access" => language
            .message(MessageKey::CommandsFullAccess6238ce12)
            .to_owned(),
        ":workspace" => language
            .message(MessageKey::CommandsWorkspace8128b964)
            .to_owned(),
        _ => value.to_owned(),
    }
}

pub(crate) fn resolve_permission_profile(
    catalog: &Value,
    requested: &str,
) -> anyhow::Result<(String, String)> {
    resolve_permission_profile_with_language(catalog, requested, crate::i18n::Language::English)
}

pub(crate) fn resolve_permission_profile_with_language(
    catalog: &Value,
    requested: &str,
    language: crate::i18n::Language,
) -> anyhow::Result<(String, String)> {
    let profiles = catalog
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("권한 프로필 목록을 읽을 수 없습니다"))?;
    let matches = profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| profile.get("allowed").and_then(Value::as_bool) == Some(true))
        .filter_map(|(index, profile)| {
            let id = profile.get("id").and_then(Value::as_str)?;
            let display_name =
                permission_profile_display_name_with_language(profile, index, language);
            let aliases = permission_profile_aliases(&display_name, Some(id));
            let is_match = id == requested
                || aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(requested));
            is_match.then(|| (id.to_owned(), display_name))
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [(id, display_name)] => Ok((id.clone(), display_name.clone())),
        [] => bail!(
            "사용할 수 있는 권한 프로필 이름을 찾지 못했습니다. `/discodex permission-profiles`에서 목록을 확인해 주세요."
        ),
        _ => bail!(
            "권한 프로필 표시 이름이 중복됩니다. 목록에서 더 구체적인 이름으로 선택해 주세요."
        ),
    }
}

pub(crate) fn format_settings_with_language(settings: &Value, language: crate::i18n::Language) -> String {
    let labels = [
        ("model", language.message(MessageKey::CommandsModel704caa01)),
        (
            "effort",
            language.message(MessageKey::CommandsReasoningEffortCeade6e1),
        ),
        (
            "serviceTier",
            language.message(MessageKey::CommandsServiceTierB86474e3),
        ),
        (
            "activePermissionProfile",
            language.message(MessageKey::CommandsPermissionProfileC7e4bb80),
        ),
        (
            "approvalPolicy",
            language.message(MessageKey::CommandsApprovalPolicyBe6ee113),
        ),
        (
            "sandboxPolicy",
            language.message(MessageKey::CommandsFileAccess65b1775e),
        ),
        (
            "approvalsReviewer",
            language.message(MessageKey::CommandsApprovalReviewer1bb5dad9),
        ),
    ];
    labels
        .iter()
        .filter_map(|(key, label)| {
            settings.get(*key).and_then(|value| {
                if value.as_str() == Some("native snapshot did not report this field") {
                    return None;
                }
                let display = if *key == "activePermissionProfile" {
                    format_active_permission_profile_with_language(value, language)
                } else if *key == "effort" {
                    let choices = effort_settings_choice(value, language)
                        .into_iter()
                        .collect::<Vec<_>>();
                    Some(effort_current_label(&choices, Some(value), language))
                } else if *key == "sandboxPolicy" {
                    Some(format_sandbox_policy_with_language(value, language))
                } else if *key == "approvalPolicy" {
                    Some(format_approval_policy_with_language(value, language))
                } else if *key == "approvalsReviewer" {
                    Some(format_approvals_reviewer_with_language(value, language))
                } else {
                    setting_value_with_language(value, language)
                };
                display.map(|value| format!("{label}: {value}"))
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn format_sandbox_policy_with_language(value: &Value, language: crate::i18n::Language) -> String {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| value.as_str());
    let access = match kind {
        Some("readOnly") => language.message(MessageKey::CommandsReadOnlyAabd721a),
        Some("workspaceWrite") => {
            language.message(MessageKey::CommandsReadAndWriteInTheWorkspaceCd2cf07f)
        }
        Some("dangerFullAccess") => {
            language.message(MessageKey::CommandsUnrestrictedFileAccess9720c6c4)
        }
        _ => language.message(MessageKey::CommandsCurrentNativeSetting32c855ae),
    };
    let mut details = vec![access.to_owned()];
    for (key, label) in [
        (
            "readableRoots",
            language.message(MessageKey::CommandsReadableLocations99fdcca7),
        ),
        (
            "writableRoots",
            language.message(MessageKey::CommandsWritableLocationsDd96deb1),
        ),
    ] {
        if let Some(roots) = value.get(key).and_then(Value::as_array) {
            let paths = roots.iter().filter_map(Value::as_str).collect::<Vec<_>>();
            if !paths.is_empty() {
                details.push(format!("{label}: {}", paths.join(", ")));
            }
        }
    }
    if let Some(network) = value.get("networkAccess").and_then(Value::as_bool) {
        details.push(format!(
            "{}: {}",
            language.message(MessageKey::CommandsNetworkAccessEec1571b),
            if network {
                language.message(MessageKey::CommandsAllowed0d02047a)
            } else {
                language.message(MessageKey::CommandsBlocked1b94f0d3)
            }
        ));
    }
    details.join(" · ")
}

pub(crate) fn format_approval_policy(value: &Value) -> String {
    format_approval_policy_with_language(value, crate::i18n::Language::English)
}

pub(crate) fn format_approval_policy_with_language(value: &Value, language: crate::i18n::Language) -> String {
    let label = match value.as_str() {
        Some("on-request") => Some(language.message(MessageKey::CommandsAskWhenNeeded998a5eef)),
        Some("untrusted") => Some(language.message(MessageKey::CommandsAskUnlessTrustedD339f046)),
        Some("never") => Some(language.message(MessageKey::CommandsNeverAskE7783a8c)),
        _ => None,
    };
    if let Some(label) = label {
        format!("{label} ({})", value.as_str().unwrap())
    } else if value.is_null() || value.as_str() == Some("native snapshot did not report this field")
    {
        language
            .message(MessageKey::CommandsNotReportedByCodex9ba3ffef)
            .to_owned()
    } else {
        value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string())
    }
}

pub(crate) fn approval_policy_settings_values(
    thread: &Value,
    saved: &Value,
    requirements: &Value,
    language: crate::i18n::Language,
) -> (Option<String>, String, Vec<SettingsChoice>) {
    let actual = thread.get("approvalPolicy");
    let current = format!(
        "{}: {}\n{}: {}",
        language.message(MessageKey::CommandsEffectivePolicyB2e092d3),
        format_approval_policy_with_language(actual.unwrap_or(&Value::Null), language),
        language.message(MessageKey::CommandsProfilePolicyAdaebde1),
        format_approval_policy_with_language(saved, language),
    );
    (
        actual.map(Value::to_string),
        current,
        approval_policy_choices(requirements, language),
    )
}

pub(crate) fn approval_policy_choices(
    requirements: &Value,
    language: crate::i18n::Language,
) -> Vec<SettingsChoice> {
    let policies = requirements
        .pointer("/requirements/allowedApprovalPolicies")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![json!("on-request"), json!("untrusted"), json!("never")]);
    policies
        .into_iter()
        .map(|policy| SettingsChoice {
            value: policy.to_string(),
            label: format_approval_policy_with_language(&policy, language),
        })
        .collect()
}

pub(crate) fn format_approvals_reviewer_with_language(
    value: &Value,
    language: crate::i18n::Language,
) -> String {
    match value.as_str() {
        Some("user") => language
            .message(MessageKey::CommandsUser8116d237)
            .to_owned(),
        Some("auto_review") => language
            .message(MessageKey::CommandsAutomaticReviewAutoReview25c10795)
            .to_owned(),
        _ if value.is_null()
            || value.as_str() == Some("native snapshot did not report this field") =>
        {
            language
                .message(MessageKey::CommandsNotReportedByCodex9ba3ffef)
                .to_owned()
        }
        _ => value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
    }
}

pub(crate) fn format_active_permission_profile_with_language(
    value: &Value,
    language: crate::i18n::Language,
) -> Option<String> {
    if value.as_str() == Some("native snapshot did not report this field") {
        return None;
    }
    if value.is_null() {
        return Some(
            language
                .message(MessageKey::CommandsNoProfileSelected25f65a7d)
                .to_owned(),
        );
    }
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| value.as_str());
    let display_name = value
        .get("displayName")
        .and_then(Value::as_str)
        .or_else(|| value.get("name").and_then(Value::as_str))
        .or_else(|| value.get("label").and_then(Value::as_str));
    if let Some(name) = display_name {
        return Some(humanize_permission_profile_name(name));
    }
    match id {
        Some(":read-only") => Some(
            language
                .message(MessageKey::CommandsReadOnlyBcb1d1ea)
                .to_owned(),
        ),
        Some(":danger-full-access") => Some(
            language
                .message(MessageKey::CommandsFullAccess6238ce12)
                .to_owned(),
        ),
        Some(":workspace") => Some(
            language
                .message(MessageKey::CommandsWorkspace8128b964)
                .to_owned(),
        ),
        Some(_) => Some(
            language
                .message(MessageKey::CommandsCurrentPermissionProfile9e74fa66)
                .to_owned(),
        ),
        None => Some(
            setting_value_with_language(value, language).unwrap_or_else(|| {
                language
                    .message(MessageKey::CommandsCurrentPermissionProfile9e74fa66)
                    .to_owned()
            }),
        ),
    }
}

pub(crate) fn setting_value_with_language(value: &Value, language: crate::i18n::Language) -> Option<String> {
    match value {
        Value::Null => Some(language.message(MessageKey::CommandsOff9616c89c).to_owned()),
        Value::String(text) if text == "native snapshot did not report this field" => None,
        Value::String(text) => Some(match text.as_str() {
            ":danger-full-access" => language
                .message(MessageKey::CommandsFullAccess6238ce12)
                .to_owned(),
            ":workspace" => language
                .message(MessageKey::CommandsWorkspace8128b964)
                .to_owned(),
            _ => text.clone(),
        }),
        Value::Bool(value) => Some(
            if *value {
                language.message(MessageKey::CommandsEnabled222f5de9)
            } else {
                language.message(MessageKey::CommandsDisabledA5d631b5)
            }
            .to_owned(),
        ),
        Value::Number(value) => Some(value.to_string()),
        Value::Array(values) => Some(
            values
                .iter()
                .filter_map(|value| setting_value_with_language(value, language))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Value::Object(values) => Some(
            values
                .iter()
                .filter(|(key, _)| {
                    !matches!(key.as_str(), "id" | "threadId" | "turnId" | "requestId")
                })
                .filter_map(|(key, value)| {
                    setting_value_with_language(value, language).map(|value| {
                        format!("{}: {value}", field_label_with_language(key, language))
                    })
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

pub(crate) fn field_label_with_language(value: &str, language: crate::i18n::Language) -> &str {
    match value {
        "read" => language.message(MessageKey::CommandsRead0c3244ec),
        "write" => language.message(MessageKey::CommandsWriteD2cfe0e4),
        "network" => language.message(MessageKey::CommandsNetwork0e84aac9),
        "roots" => language.message(MessageKey::CommandsAllowedPaths8c5120c2),
        "mode" | "scope" => language.message(MessageKey::CommandsScope60298d24),
        _ => value,
    }
}

pub(crate) fn input_mode(mode: crate::config::InputMode, language: crate::i18n::Language) -> &'static str {
    match mode {
        crate::config::InputMode::Steering => {
            language.message(MessageKey::CommandsSendToTheActiveResponse32141a0b)
        }
        crate::config::InputMode::NextTurn => {
            language.message(MessageKey::CommandsSendBeforeTheNextResponse9c3b5081)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingsMenuSnapshot {
    pub connection_generation: u64,
    pub thread_id: String,
    pub kind: SettingsKind,
    pub choices: Vec<SettingsChoice>,
    pub current: String,
    pub current_value: Option<String>,
    pub model_scope: Option<String>,
    pub effective_thread_state: Value,
    pub saved_preferences: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingChangeRequest {
    pub kind: SettingsKind,
    pub value: String,
    pub expected_thread_id: Option<String>,
    pub expected_generation: Option<u64>,
    pub expected_model_scope: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteErrorProjection {
    pub message: String,
    pub code: Option<Value>,
    pub status: Option<Value>,
}

impl RemoteErrorProjection {
    pub(crate) fn from_error(error: &anyhow::Error, config: &Config) -> Self {
        let native = error.chain().find_map(|cause| cause.downcast_ref::<crate::native::NativeRequestError>());
        let token = config.token().ok();
        let message = native.map(|error| error.original_message().to_owned()).unwrap_or_else(|| format!("{error:#}"));
        Self {
            message: crate::diagnostics::sanitize_diagnostic_with_secret(&message, token.as_deref()),
            code: native.and_then(|error| error.original_code().cloned()),
            status: native.and_then(|error| error.original_status().cloned()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ApplicationOutcomeProjection { Applied(Value), Superseded, StaleTarget, Failed(RemoteErrorProjection) }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PersistenceOutcomeProjection { NotNeeded, Saved, Failed(RemoteErrorProjection) }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CurrentTurnOutcomeProjection { NotRequested, Applied(Value), Unconfirmed(Value), Failed(RemoteErrorProjection) }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingOutcomeProjection {
    pub application: ApplicationOutcomeProjection,
    pub persistence: PersistenceOutcomeProjection,
    pub current_turn: CurrentTurnOutcomeProjection,
    pub target_current: bool,
    pub superseded: bool,
}
impl SettingOutcomeProjection {
    fn from_outcome(outcome: &crate::settings_operation::SettingOutcome, config: &Config) -> Self {
        use crate::settings_operation::{ApplicationOutcome as A, PersistenceOutcome as P, CurrentTurnOutcome as C};
        Self {
            application: match &outcome.application {
                A::Applied(value) => ApplicationOutcomeProjection::Applied(value.clone()),
                A::Superseded => ApplicationOutcomeProjection::Superseded,
                A::StaleTarget => ApplicationOutcomeProjection::StaleTarget,
                A::Failed(error) => ApplicationOutcomeProjection::Failed(RemoteErrorProjection::from_error(error, config)),
            },
            persistence: match &outcome.persistence {
                P::NotNeeded => PersistenceOutcomeProjection::NotNeeded,
                P::Saved => PersistenceOutcomeProjection::Saved,
                P::Failed(error) => PersistenceOutcomeProjection::Failed(RemoteErrorProjection::from_error(error, config)),
            },
            current_turn: match &outcome.current_turn {
                C::NotRequested => CurrentTurnOutcomeProjection::NotRequested,
                C::Applied(value) => CurrentTurnOutcomeProjection::Applied(value.clone()),
                C::Unconfirmed(value) => CurrentTurnOutcomeProjection::Unconfirmed(value.clone()),
                C::Failed(error) => CurrentTurnOutcomeProjection::Failed(RemoteErrorProjection::from_error(error, config)),
            },
            target_current: outcome.target_current, superseded: outcome.superseded,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingApplyResult {
    pub kind: SettingsKind,
    pub connection_generation: u64,
    pub primary_thread_id: String,
    pub voice_bound_thread_id: Option<String>,
    pub outcome: SettingOutcomeProjection,
    pub requires_service_restart: bool,
}

/// Shared update state and its existing notification wake-up; no Discord UI dependency.
#[derive(Clone)]
pub(crate) struct UpdateSettings {
    pub(crate) store: Arc<Mutex<crate::install::updates::UpdateStore>>,
    pub(crate) wake: Arc<tokio::sync::Notify>,
    pub(crate) initialization_error: Option<String>,
}
impl UpdateSettings {
    pub(crate) async fn muted(&self) -> bool { self.store.lock().await.snapshot().muted }
    pub(crate) async fn set_muted(&self, muted: bool, language: crate::i18n::Language) -> anyhow::Result<()> {
        if let Some(error) = &self.initialization_error {
            bail!("{}", language.message(MessageKey::DiscordUpdatesStateUnavailable).replace("{error}", error));
        }
        self.store.lock().await.set_muted(muted)?;
        self.wake.notify_one();
        Ok(())
    }
}

pub struct SettingsAppService {
    pub(crate) config: Arc<Config>,
    pub(crate) native: Native,
    pub(crate) voice: Arc<VoiceBridge>,
    pub(crate) input_mode: Arc<RwLock<InputMode>>,
    pub(crate) input_mode_path: PathBuf,
    pub(crate) message_access: Arc<RwLock<MessageAccess>>,
    pub(crate) message_access_path: PathBuf,
    pub(crate) current_turn: Arc<Mutex<Option<String>>>,
    pub(crate) installation_checks: Option<Arc<crate::installation_check::Checks>>,
    pub(crate) updates: UpdateSettings,
    pub(crate) settings_change_lock: Mutex<()>,
    operations: crate::settings_operation::SettingsOperations,
}

pub(crate) enum SettingChoiceOutcome {
    Applied(String),
    OperationStarted { request: PendingSettingOperation, requested: String },
}
enum PreparedSettingChoice {
    Applied(Value, String),
    OperationStarted { request: crate::settings_operation::SettingRequest, requested: String },
}

pub(crate) struct PendingSettingOperation {
    pub(crate) field: &'static str,
    pub(crate) expected: Value,
    receiver: tokio::sync::oneshot::Receiver<anyhow::Result<Arc<crate::settings_operation::SettingOutcome>>>,
    language: crate::i18n::Language,
}
impl PendingSettingOperation {
    pub(crate) async fn wait(self) -> anyhow::Result<Arc<crate::settings_operation::SettingOutcome>> {
        self.receiver.await.context(self.language.message(MessageKey::SettingsOperationResultMissing))?
    }
}

impl SettingsAppService {
    pub(crate) fn new(
        config: Arc<Config>, native: Native, voice: Arc<VoiceBridge>,
        input_mode: Arc<RwLock<InputMode>>, input_mode_path: PathBuf,
        message_access: Arc<RwLock<MessageAccess>>, message_access_path: PathBuf,
        current_turn: Arc<Mutex<Option<String>>>,
        installation_checks: Option<Arc<crate::installation_check::Checks>>, updates: UpdateSettings,
    ) -> Self {
        let operations = crate::settings_operation::SettingsOperations::new(config.language);
        Self { config, native, voice, input_mode, input_mode_path, message_access, message_access_path,
            current_turn, installation_checks, updates, settings_change_lock: Mutex::new(()), operations }
    }

    pub async fn open_settings(&self, kind: SettingsKind) -> Result<SettingsMenuSnapshot, RemoteErrorProjection> {
        self.open_settings_with_catalog(kind, self.native.current_generation(), None).await
            .map_err(|error| RemoteErrorProjection::from_error(&error, &self.config))
    }

    /// Admission starts a worker-owned task. Dropping this observer never cancels that task.
    pub async fn apply_setting(self: &Arc<Self>, request: SettingChangeRequest) -> Result<SettingApplyResult, RemoteErrorProjection> {
        let (response, receiver) = tokio::sync::oneshot::channel();
        self.spawn_change(request, None, None, None, Some(response));
        receiver.await.map_err(|error| RemoteErrorProjection::from_error(&error.into(), &self.config))?
    }

    pub(crate) async fn start_choice(
        self: &Arc<Self>, request: SettingChangeRequest, observation_id: u64, catalog: Option<Value>,
    ) -> anyhow::Result<SettingChoiceOutcome> {
        let (response, receiver) = tokio::sync::oneshot::channel();
        self.spawn_change(request, Some(observation_id), catalog, Some(response), None);
        receiver.await.context(self.config.language.message(MessageKey::SettingsOperationResultMissing))?
    }

    fn spawn_change(
        self: &Arc<Self>, change: SettingChangeRequest, observation_id: Option<u64>, catalog: Option<Value>,
        admitted: Option<tokio::sync::oneshot::Sender<anyhow::Result<SettingChoiceOutcome>>>,
        completed: Option<tokio::sync::oneshot::Sender<Result<SettingApplyResult, RemoteErrorProjection>>>,
    ) {
        let service = self.clone();
        tokio::spawn(async move {
            let kind = change.kind;
            let prepared = service.prepare_setting_choice(observation_id, kind, &change.value,
                change.expected_thread_id.as_deref(), change.expected_generation,
                change.expected_model_scope, catalog.as_ref()).await;
            let result = match prepared {
                Err(error) => {
                    if let Some(sender) = admitted { let _ = sender.send(Err(error)); return; }
                    Err(RemoteErrorProjection::from_error(&error, &service.config))
                },
                Ok(PreparedSettingChoice::Applied(payload, notice)) => {
                    if let Some(sender) = admitted { let _ = sender.send(Ok(SettingChoiceOutcome::Applied(notice))); }
                    let saved = matches!(kind, SettingsKind::Model | SettingsKind::SubagentModel | SettingsKind::ApprovalPolicy |
                        SettingsKind::Input | SettingsKind::Voice | SettingsKind::Updates | SettingsKind::MessageAccess | SettingsKind::Antigravity |
                        SettingsKind::AntigravityAutoApprove | SettingsKind::AntigravitySetting(_) | SettingsKind::NativeFeature(_));
                    let outcome = SettingOutcomeProjection {
                        application: ApplicationOutcomeProjection::Applied(payload),
                        persistence: if saved { PersistenceOutcomeProjection::Saved } else { PersistenceOutcomeProjection::NotNeeded },
                        current_turn: CurrentTurnOutcomeProjection::NotRequested,
                        target_current: true, superseded: false,
                    };
                    service.result(kind, outcome)
                },
                Ok(PreparedSettingChoice::OperationStarted { request, requested }) => {
                    let (sender, receiver) = tokio::sync::oneshot::channel();
                    if let Some(admitted) = admitted {
                        let pending = PendingSettingOperation { field: request.field, expected: request.expected.clone(), receiver, language: service.config.language };
                        let _ = admitted.send(Ok(SettingChoiceOutcome::OperationStarted { request: pending, requested }));
                    }
                    let outcome = service.confirm_operation(kind, request).await;
                    let projection = outcome.as_ref().map(|outcome| SettingOutcomeProjection::from_outcome(outcome, &service.config));
                    let result = match projection {
                        Ok(outcome) => service.result(kind, outcome),
                        Err(error) => Err(RemoteErrorProjection::from_error(error, &service.config)),
                    };
                    let _ = sender.send(outcome);
                    result
                },
            };
            if let Some(sender) = completed { let _ = sender.send(result); }
        });
    }

    fn result(&self, kind: SettingsKind, outcome: SettingOutcomeProjection) -> Result<SettingApplyResult, RemoteErrorProjection> {
        Ok(SettingApplyResult {
            kind, connection_generation: self.native.current_generation(),
            primary_thread_id: self.native.thread_id(), voice_bound_thread_id: self.voice.input_gate.call_thread_id().map_err(|error| RemoteErrorProjection::from_error(&error, &self.config))?,
            outcome, requires_service_restart: matches!(kind, SettingsKind::NativeFeature(key) if key != NativeFeatureKey::SubagentModel),
        })
    }

    async fn confirm_operation(&self, kind: SettingsKind, request: crate::settings_operation::SettingRequest)
        -> anyhow::Result<Arc<crate::settings_operation::SettingOutcome>> {
        let input_phase = if kind == SettingsKind::Model { Some(self.voice.input_gate.state.lock().await) } else { None };
        let settings_change = self.settings_change_lock.lock().await;
        let (dispatched_tx, dispatched_rx) = tokio::sync::oneshot::channel();
        let apply = self.operations.apply_setting_with_dispatch(&self.native, request, Some(dispatched_tx));
        tokio::pin!(apply);
        tokio::select! {
            outcome = &mut apply => { drop(input_phase); drop(settings_change); outcome },
            _ = dispatched_rx => { drop(input_phase); drop(settings_change); apply.await },
        }
    }
pub(crate) async fn open_settings_with_catalog(
    &self, kind: SettingsKind, connection_generation: u64, model_catalog: Option<&Value>,
) -> anyhow::Result<SettingsMenuSnapshot> {
    let state = self;
    let language = state.config.language;
    if state.native.current_generation() != connection_generation {
        bail!("Codex connection changed while settings were opening");
    }
    let target_thread_id = state.native.thread_id();
    let thread = if kind == SettingsKind::MessageAccess
        || is_access_list(kind)
        || matches!(
            kind,
            SettingsKind::Antigravity
                | SettingsKind::AntigravityAutoApprove
                | SettingsKind::AntigravitySetting(_)
        ) {
        json!({})
    } else {
        state.native.refresh_thread_state().await?
    };
    let model_scope = thread_model_scope(&thread);
    let model_catalog = match model_catalog {
        Some(catalog) => Some(catalog.clone()),
        None if settings_kind_uses_model_catalog(kind) => Some(state.native.model_catalog().await?),
        None => None,
    };
    let (current_value, current, choices) = match kind {
        SettingsKind::Hub => settings_home_values(
            &thread,
            *state.input_mode.read().await,
            state.voice.selected_voice()?,
            language,
        ),
        SettingsKind::SubagentModel => {
            let snapshot = state.native.read_native_feature_snapshot().await?;
            let catalog = model_catalog.as_ref().ok_or_else(|| {
                anyhow!(language.message(MessageKey::CommandsModelCatalogUnavailable))
            })?;
            subagent_model_settings_values(&thread, &snapshot, &catalog, language)
        }
        SettingsKind::NativeFeatures => {
            advanced_settings_values(&state.native.read_native_feature_preferences()?, language)
        }
        SettingsKind::Antigravity => {
            let preferences = state.native.read_native_feature_preferences()?;
            antigravity_settings_values(&preferences, language)
        }
        SettingsKind::AntigravityAutoApprove => {
            let preferences = state.native.read_native_feature_preferences()?;
            (
                Some(
                    if preferences.antigravity_auto_approve {
                        "on"
                    } else {
                        "off"
                    }
                    .to_owned(),
                ),
                language
                    .message(MessageKey::CommandsAgyAutoApproveDescription)
                    .to_owned(),
                vec![
                    SettingsChoice {
                        value: "on".to_owned(),
                        label: language.message(MessageKey::CommandsOn65845774).to_owned(),
                    },
                    SettingsChoice {
                        value: "off".to_owned(),
                        label: language.message(MessageKey::CommandsOff5677591d).to_owned(),
                    },
                ],
            )
        }
        SettingsKind::AntigravitySetting(key) => {
            let preferences = state.native.read_native_feature_preferences()?;
            let settings = crate::agy_settings::read(
                &state.config,
                preferences.antigravity_share_settings.unwrap_or(false),
            )?;
            agy_setting_values(key, &settings, language)?
        }
        SettingsKind::NativeFeature(key) => {
            let snapshot = state.native.read_native_feature_snapshot().await?;
            let saved = native_feature_saved_value(&snapshot.preferences, key);
            let effective = effective_native_feature_text(&snapshot.native_config, key, language);
            let subagent_catalog = if key == NativeFeatureKey::SubagentModel {
                model_catalog.as_ref()
            } else {
                None
            };
            let choices = if let Some(catalog) = subagent_catalog {
                subagent_model_choices(catalog, language)
            } else {
                vec![
                    SettingsChoice {
                        value: "on".to_owned(),
                        label: language.message(MessageKey::CommandsOn65845774).to_owned(),
                    },
                    SettingsChoice {
                        value: "off".to_owned(),
                        label: language.message(MessageKey::CommandsOff5677591d).to_owned(),
                    },
                    SettingsChoice {
                        value: "native-default".to_owned(),
                        label: language
                            .message(MessageKey::CommandsNativeDefault308250fc)
                            .to_owned(),
                    },
                ]
            };
            let current_value = if key == NativeFeatureKey::SubagentModel {
                subagent_catalog
                    .and_then(|catalog| {
                        subagent_preference_scope(&snapshot.preferences, catalog).and_then(
                            |value| normalized_model_value_with_language(catalog, &value, language),
                        )
                    })
                    .or_else(|| saved.clone())
                    .unwrap_or_else(|| "native-default".to_owned())
            } else {
                match saved.as_deref() {
                    Some("true") => "on".to_owned(),
                    Some("false") => "off".to_owned(),
                    _ => "native-default".to_owned(),
                }
            };
            let selected = choices.iter().find(|choice| choice.value == current_value);
            let desired = selected
                .map(|choice| choice.label.as_str())
                .unwrap_or(current_value.as_str());
            let current = format!(
                "{}: {}\n{}: {}\n{}",
                language.message(MessageKey::CommandsSavedPreference47ce2cf1),
                desired,
                language.message(MessageKey::CommandsEffectiveCodexConfigEeb899f3),
                effective,
                if key == NativeFeatureKey::SubagentModel {
                    child_default_timing(language)
                } else {
                    language
                        .message(MessageKey::CommandsRestartRequiredToApplyThisPreference4ac207b7)
                }
            );
            (Some(current_value), current, choices)
        }
        SettingsKind::MessageAccess => {
            let access = state.message_access.read().await.clone();
            let selected = format!("policy:{}", message_policy_key(access.policy));
            let current = format!(
                "{}: {}\n{}: {} · {}: {} · {}: {} · {}: {}",
                language.message(MessageKey::CommandsCurrentPolicy7ed3f9e2),
                message_policy_label(access.policy, language),
                language.message(MessageKey::CommandsPairedDb2c2b0b),
                access.paired_user_ids.len(),
                language.message(MessageKey::CommandsWhitelist4df98d9d),
                access.whitelist.len(),
                language.message(MessageKey::CommandsBlacklistD5332ba7),
                access.blacklist.len(),
                language.message(MessageKey::CommandsRegisteredChannels5a818eab),
                access.channel_ids.len()
            );
            let choices = [
                (
                    "policy:everyone",
                    message_policy_label(MessagePolicy::Everyone, language),
                ),
                (
                    "policy:registered-server",
                    message_policy_label(MessagePolicy::RegisteredServer, language),
                ),
                (
                    "policy:registered-channels",
                    message_policy_label(MessagePolicy::RegisteredChannels, language),
                ),
                (
                    "policy:paired",
                    message_policy_label(MessagePolicy::Paired, language),
                ),
                (
                    "list:paired",
                    language.message(MessageKey::CommandsEditPairedAccountsAceb36c6),
                ),
                (
                    "list:whitelist",
                    language.message(MessageKey::CommandsEditWhitelist82750902),
                ),
                (
                    "list:blacklist",
                    language.message(MessageKey::CommandsEditBlocklistF37f514b),
                ),
                (
                    "list:channels",
                    language.message(MessageKey::CommandsEditRegisteredChannelsCca7da5c),
                ),
            ]
            .into_iter()
            .map(|(value, label)| SettingsChoice {
                value: value.to_owned(),
                label: label.to_owned(),
            })
            .collect();
            (Some(selected.to_owned()), current, choices)
        }
        SettingsKind::PairedAccounts
        | SettingsKind::Whitelist
        | SettingsKind::Blacklist
        | SettingsKind::RegisteredChannels => {
            let access = state.message_access.read().await.clone();
            let ids = access_list(&access, kind).to_vec();
            let choices = ids.iter().map(|id| SettingsChoice {
                value: id.to_string(), label: id.to_string(),
            }).collect::<Vec<_>>();
            let current = format!(
                "{}: {}",
                language.message(MessageKey::CommandsEntries9281e703),
                ids.len()
            );
            (None, current, choices)
        }
        SettingsKind::Updates => {
            let muted = state.updates.muted().await;
            let choices = [
                ("on", language.message(MessageKey::CommandsOn65845774)),
                ("off", language.message(MessageKey::CommandsOff5677591d)),
            ]
            .into_iter()
            .map(|(value, label)| SettingsChoice {
                value: value.to_owned(),
                label: label.to_owned(),
            })
            .collect::<Vec<_>>();
            let value = if muted { "off" } else { "on" };
            let current = choice_current_label(&choices, Some(value), language);
            (Some(value.to_owned()), current, choices)
        }
        SettingsKind::Model => {
            let catalog = model_catalog.as_ref().ok_or_else(|| {
                anyhow!(language.message(MessageKey::CommandsModelCatalogUnavailable))
            })?;
            let choices = catalog_model_choices(&catalog);
            let selected = model_scope
                .as_deref()
                .and_then(|value| normalized_model_value_with_language(&catalog, value, language));
            let provider = thread_model_provider(&thread)
                .unwrap_or_else(|| language.message(MessageKey::CommandsModelProviderUnknown));
            let current = format!(
                "{}\n{}: {provider}",
                choice_current_label(&choices, selected.as_deref(), language),
                language.message(MessageKey::CommandsActiveModelProviderEa54b5ed)
            );
            (selected, model_catalog_warnings(current, &catalog), choices)
        }
        SettingsKind::Effort | SettingsKind::Fast => {
            let catalog = model_catalog.as_ref().ok_or_else(|| {
                anyhow!(language.message(MessageKey::CommandsModelCatalogUnavailable))
            })?;
            let model = selected_model_with_language(&catalog, &model_scope, language)?;
            if kind == SettingsKind::Effort {
                let choices = model_effort_settings_choices(model, language);
                let selected = thread
                    .get("effort")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        thread
                            .get("model")
                            .and_then(Value::as_str)
                            .and_then(|id| agy_variant_effort(model, id))
                    });
                let effort_value = selected.as_ref().map(|effort| json!(effort));
                let current = effort_current_label(&choices, effort_value.as_ref(), language);
                (selected, model_catalog_warnings(current, &catalog), choices)
            } else {
                let mut choices = vec![SettingsChoice {
                    value: "off".to_owned(),
                    label: language.message(MessageKey::CommandsOff4d374cce).to_owned(),
                }];
                choices.extend(
                    model
                        .get("serviceTiers")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|item| service_tier_settings_choice(item, language)),
                );
                let selected = thread
                    .get("serviceTier")
                    .filter(|value| !value.is_null())
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| "off".to_owned());
                let current = choice_current_label(&choices, Some(&selected), language);
                (
                    Some(selected),
                    model_catalog_warnings(current, &catalog),
                    choices,
                )
            }
        }
        SettingsKind::Permissions => {
            let catalog = full_catalog(
                state,
                "permissionProfile/list",
                json!({"cwd":state.config.project_dir.to_string_lossy()}),
            )
            .await?;
            let choices = catalog
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .filter(|(_, profile)| {
                    profile.get("allowed").and_then(Value::as_bool) == Some(true)
                })
                .filter_map(|(index, profile)| {
                    Some(SettingsChoice {
                        value: profile.get("id")?.as_str()?.to_owned(),
                        label: permission_profile_display_name_with_language(
                            profile, index, language,
                        ),
                    })
                })
                .collect::<Vec<_>>();
            let selected = thread
                .pointer("/activePermissionProfile/id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let current = choice_current_label(&choices, selected.as_deref(), language);
            (selected, current, choices)
        }
        SettingsKind::ApprovalPolicy => {
            let requirements = state
                .native
                .request("configRequirements/read", json!({}))
                .await?;
            let saved = state
                .native
                .read_native_feature_preferences()?
                .approval_policy;
            approval_policy_settings_values(&thread, &saved, &requirements, language)
        }
        SettingsKind::Input => {
            let mode = *state.input_mode.read().await;
            let (selected, label) = match mode {
                crate::config::InputMode::Steering => ("steering", input_mode(mode, language)),
                crate::config::InputMode::NextTurn => ("next-turn", input_mode(mode, language)),
            };
            let choices = [
                crate::config::InputMode::Steering,
                crate::config::InputMode::NextTurn,
            ]
            .into_iter()
            .map(|mode| SettingsChoice {
                value: match mode {
                    crate::config::InputMode::Steering => "steering",
                    crate::config::InputMode::NextTurn => "next-turn",
                }
                .to_owned(),
                label: input_mode(mode, language).to_owned(),
            })
            .collect();
            (Some(selected.to_owned()), label.to_owned(), choices)
        }
        SettingsKind::Voice => {
            let voices = state
                .native
                .request("thread/realtime/listVoices", json!({}))
                .await
                .context("load thread/realtime/listVoices catalog")?;
            let default_label = language
                .message(MessageKey::CommandsCodexDefault3029befd)
                .to_owned();
            let mut choices = vec![SettingsChoice {
                value: String::new(),
                label: default_label,
            }];
            choices.extend(
                voices
                    .pointer("/voices/v1")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|voice| SettingsChoice {
                        value: voice.to_owned(),
                        label: voice.to_owned(),
                    }),
            );
            let selected = state.voice.selected_voice()?;
            let current_value = selected.clone().unwrap_or_default();
            let current = if let Some(voice) = selected {
                choice_current_label(&choices, Some(&voice), language)
            } else {
                choices[0].label.clone()
            };
            (Some(current_value), current, choices)
        }
    };
    if state.native.current_generation() != connection_generation
        || state.native.thread_id() != target_thread_id
    {
        bail!("Codex connection changed while settings were loading");
    }
    Ok(SettingsMenuSnapshot {
        connection_generation,
        thread_id: target_thread_id,
        kind,
        choices,
        current,
        current_value,
        model_scope,
        effective_thread_state: thread,
        saved_preferences: serde_json::to_value(state.native.read_native_feature_preferences()?)?,
    })
}
async fn prepare_setting_choice(
    &self,
    interaction_id: Option<u64>,
    kind: SettingsKind,
    requested: &str,
    expected_thread_id: Option<&str>,
    expected_generation: Option<u64>,
    expected_model_scope: Option<String>,
    model_catalog: Option<&Value>,
) -> anyhow::Result<PreparedSettingChoice> {
    let state = self;
    let mut payload = json!({"value": requested});
    // Match new-session admission order: input gate, settings, then active turn.
    // Keep incoming Discord input outside a provider detach/resume transition.
    let _input_phase = if kind == SettingsKind::Model {
        Some(state.voice.input_gate.state.lock().await)
    } else {
        None
    };
    let _settings_change = state.settings_change_lock.lock().await;
    ensure_settings_apply_target(
        expected_generation,
        state.native.current_generation(),
        expected_thread_id,
        &state.native.thread_id(),
    )?;
    let thread_id = expected_thread_id
        .map(str::to_owned)
        .unwrap_or_else(|| state.native.thread_id());
    let language = state.config.language;
    match kind {
        SettingsKind::AntigravityAutoApprove => {
            let mut preferences = state.native.read_native_feature_preferences()?;
            preferences.antigravity_auto_approve = match requested {
                "on" => true,
                "off" => false,
                _ => bail!(
                    "{}",
                    language.message(MessageKey::CommandsAgyUnknownSetting)
                ),
            };
            state
                .native
                .apply_native_feature_preferences(&preferences)
                .await?;
            Ok(PreparedSettingChoice::Applied(payload, 
                language
                    .message(MessageKey::CommandsAgyAppliesNextSession)
                    .to_owned(),
            ))
        }
        SettingsKind::AntigravitySetting(key) => {
            let preferences = state.native.read_native_feature_preferences()?;
            let value = match key {
                AgySettingKey::TerminalSandbox | AgySettingKey::NonWorkspaceAccess => {
                    Value::Bool(requested.parse::<bool>()?)
                }
                AgySettingKey::ToolPermission | AgySettingKey::ArtifactReview => {
                    Value::String(requested.to_owned())
                }
                _ => bail!(
                    "{}",
                    language.message(MessageKey::CommandsAgyUseRuleButtons)
                ),
            };
            crate::agy_settings::set(
                &state.config,
                preferences.antigravity_share_settings.unwrap_or(false),
                key.key(),
                value,
            )?;
            Ok(PreparedSettingChoice::Applied(payload, 
                language
                    .message(MessageKey::CommandsAgyAppliesNextSession)
                    .to_owned(),
            ))
        }
        SettingsKind::Antigravity => {
            let mut preferences = state.native.read_native_feature_preferences()?;
            preferences.antigravity_enabled = match requested {
                "off" => false,
                _ => bail!("unknown Antigravity switch"),
            };
            state
                .native
                .apply_native_feature_preferences(&preferences)
                .await?;
            Ok(PreparedSettingChoice::Applied(payload, 
                language
                    .message(MessageKey::CommandsAntigravityPreferenceSavedItAppliesOnTheN85aacfc8)
                    .to_owned(),
            ))
        }
        SettingsKind::Updates => {
            let muted = match requested {
                "on" => false,
                "off" => true,
                _ => bail!("unknown update notification choice"),
            };
            state.updates.set_muted(muted, language).await?;
            Ok(PreparedSettingChoice::Applied(payload, 
                language
                    .message(
                        MessageKey::CommandsUpdateNotificationPreferenceSavedManualUpda807290ea,
                    )
                    .to_owned(),
            ))
        }
        SettingsKind::Permissions => {
            let catalog = full_catalog(
                state,
                "permissionProfile/list",
                json!({"cwd":state.config.project_dir.to_string_lossy()}),
            )
            .await?;
            let (profile_id, display_name) =
                resolve_permission_profile_with_language(&catalog, requested, language)?;
            let settings = state
                .native
                .set_permissions_and_confirm(&profile_id)
                .await?;
            payload = settings.clone();
            let active_turn = state.current_turn.lock().await.is_some();
            if let (Some(checks), Some(interaction_id)) = (&state.installation_checks, interaction_id) {
                checks.settings_confirmed(
                    interaction_id,
                    &state.native.thread_id(),
                    settings
                        .pointer("/activePermissionProfile/id")
                        .and_then(Value::as_str)
                        == Some(profile_id.as_str()),
                    !active_turn,
                    None,
                );
            }
            Ok(PreparedSettingChoice::Applied(payload, 
                format_permission_settings_result_with_language(
                    &display_name,
                    &settings,
                    active_turn,
                    language,
                ),
            ))
        }
        SettingsKind::ApprovalPolicy => {
            let policy: Value =
                serde_json::from_str(requested).context("parse native approval policy choice")?;
            let settings = state.native.set_approval_policy_and_confirm(policy).await?;
            payload = settings.clone();
            Ok(PreparedSettingChoice::Applied(payload, format!(
                "{}: {}",
                language.message(MessageKey::CommandsApprovalPolicyAppliedAndSavedA6d52828),
                format_approval_policy_with_language(&settings["approvalPolicy"], language),
            )))
        }
        SettingsKind::Input => {
            let mode = match requested {
                "steering" => crate::config::InputMode::Steering,
                "next-turn" => crate::config::InputMode::NextTurn,
                _ => bail!("unsupported input mode"),
            };
            let mut current_mode = state.input_mode.write().await;
            save_input_mode(&state.input_mode_path, mode)?;
            *current_mode = mode;
            Ok(PreparedSettingChoice::Applied(payload, format!(
                "{} `{}`",
                language.message(MessageKey::CommandsInputDeliverySetToDd4696ee),
                input_mode(mode, language)
            )))
        }
        SettingsKind::Voice => {
            let voices = state
                .native
                .request("thread/realtime/listVoices", json!({}))
                .await?;
            let allowed = voices
                .pointer("/voices/v1")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|voice| voice == requested);
            if !requested.is_empty() && !allowed {
                bail!(
                    "{}",
                    language.message(MessageKey::CommandsVoiceNoLongerAvailable)
                );
            }
            let selected = if requested.is_empty() {
                None
            } else {
                Some(requested)
            };
            state.voice.set_voice(selected)?;
            Ok(PreparedSettingChoice::Applied(payload, 
                language
                    .message(MessageKey::CommandsVoiceProfileSavedForTheNextCallTheActive468592de)
                    .to_owned(),
            ))
        }
        SettingsKind::Model => {
            let thread = state.native.refresh_thread_state().await?;
            ensure_settings_apply_target(
                expected_generation,
                state.native.current_generation(),
                expected_thread_id,
                &state.native.thread_id(),
            )?;
            let catalog = match model_catalog {
                Some(catalog) => catalog.clone(),
                None => state.native.model_catalog().await?,
            };
            let selection = model_selection_for_choice(&catalog, requested, &thread, language)?;
            let active_provider = thread_model_provider(&thread)
                .or_else(|| catalog.get("provider").and_then(Value::as_str));
            let active_executor = thread
                .get("modelExecutor")
                .cloned()
                .map(serde_json::from_value::<crate::model_catalog::ModelExecutor>)
                .transpose()
                .context("read current model executor")?
                .unwrap_or_default();
            if active_provider.is_none_or(|provider| {
                model_change_requires_resume(active_executor, provider, &selection)
            }) {
                let mut current_turn = state.current_turn.lock().await;
                let snapshot = state.native.set_model_selection(&selection).await?;
                payload = snapshot.clone();
                *current_turn = None;
                let mut message = format_settings_update_result_with_language(
                    &format!("{} — {}", selection.model, selection.provider),
                    Some(&snapshot),
                    None,
                    language,
                );
                if let Some(call_thread) = state.voice.input_gate.call_thread_id()? {
                    let targets = language
                        .message(MessageKey::CommandsModelSwitchTargets)
                        .replace("{voice_thread}", &call_thread)
                        .replace("{primary_thread}", &state.native.thread_id());
                    message.push_str("\n\n");
                    message.push_str(&targets);
                }
                drop(current_turn);
                drop(_input_phase);
                state.voice.input_gate.ready.notify_one();
                return Ok(PreparedSettingChoice::Applied(payload, message));
            }
            let requested = format!("{} — {}", selection.model, selection.provider);
            let expected = Value::String(selection.model.clone());
            let current_turn_params = state.current_turn.lock().await.clone().map(|turn_id| {
                json!({
                    "threadId":thread_id,
                    "turnId":turn_id,
                    "model":selection.model
                })
            });
            Ok(PreparedSettingChoice::OperationStarted {
                request: crate::settings_operation::SettingRequest {
                    generation: expected_generation
                        .unwrap_or_else(|| state.native.current_generation()),
                    thread_id: thread_id.clone(),
                    field: "model",
                    params: json!({"threadId":thread_id,"model":selection.model}),
                    expected,
                    expected_model_scope: None,
                    model_selection: Some(selection),
                    current_turn_params,
                },
                requested,
            })
        }
        SettingsKind::SubagentModel | SettingsKind::Effort | SettingsKind::Fast => {
            let sub = match kind {
                SettingsKind::Model => "model",
                SettingsKind::SubagentModel => "model",
                SettingsKind::Effort => "effort",
                SettingsKind::Fast => "fast",
                _ => unreachable!(),
            };
            let thread = state.native.refresh_thread_state().await?;
            ensure_settings_apply_target(
                expected_generation,
                state.native.current_generation(),
                expected_thread_id,
                &state.native.thread_id(),
            )?;
            ensure_settings_model_scope(kind, &expected_model_scope, thread_model_scope(&thread))?;
            let current_model = thread
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let catalog = match model_catalog {
                Some(catalog) => catalog.clone(),
                None => state.native.model_catalog().await?,
            };
            if kind == SettingsKind::SubagentModel {
                let selection = if requested == "native-default" {
                    None
                } else {
                    Some(subagent_model_selection(&catalog, requested, language)?)
                };
                let mut preferences = state.native.read_native_feature_preferences()?;
                set_subagent_model_selection(&mut preferences, selection.as_ref());
                state
                    .native
                    .apply_native_feature_preferences_with_catalog(&preferences, Some(&catalog))
                    .await?;
                return Ok(PreparedSettingChoice::Applied(payload, child_default_saved_notice(
                    language,
                )));
            }
            validate_setting_choice_with_language(
                sub,
                requested,
                &catalog,
                &current_model,
                language,
            )
            .await?;
            let (field, value) = match kind {
                SettingsKind::Model => ("model", Value::String(requested.to_owned())),
                SettingsKind::SubagentModel => unreachable!(),
                SettingsKind::Effort => ("effort", Value::String(requested.to_owned())),
                SettingsKind::Fast => (
                    "serviceTier",
                    if requested.eq_ignore_ascii_case("off") {
                        Value::Null
                    } else {
                        Value::String(requested.to_owned())
                    },
                ),
                _ => unreachable!(),
            };
            let mut params = json!({"threadId":thread_id});
            params[field] = value.clone();
            let live_value = if kind == SettingsKind::Fast && requested.eq_ignore_ascii_case("off")
            {
                Value::Null
            } else {
                Value::String(requested.to_owned())
            };
            let current_turn_params = state.current_turn.lock().await.clone().map(|turn_id| {
                let mut live = json!({"threadId":thread_id,"turnId":turn_id});
                live[field] = live_value;
                live
            });
            Ok(PreparedSettingChoice::OperationStarted {
                request: crate::settings_operation::SettingRequest {
                    generation: expected_generation
                        .unwrap_or_else(|| state.native.current_generation()),
                    thread_id: thread_id.clone(),
                    field,
                    params,
                    expected: value,
                    expected_model_scope: matches!(kind, SettingsKind::Effort | SettingsKind::Fast)
                        .then_some(expected_model_scope.clone()),
                    model_selection: None,
                    current_turn_params,
                },
                requested: requested.to_owned(),
            })
        }
        SettingsKind::NativeFeatures => bail!(
            "{}",
            language.message(MessageKey::CommandsChooseNativeFeatureFirst)
        ),
        SettingsKind::NativeFeature(key) => {
            let mut preferences = state.native.read_native_feature_preferences()?;
            let selection =
                if key == NativeFeatureKey::SubagentModel && requested != "native-default" {
                    Some(subagent_model_selection(
                        &match model_catalog {
                            Some(catalog) => catalog.clone(),
                            None => state.native.model_catalog().await?,
                        },
                        requested,
                        language,
                    )?)
                } else {
                    None
                };
            let selected = if requested == "native-default" {
                None
            } else {
                Some(
                    selection
                        .as_ref()
                        .map_or(requested, |value| value.model.as_str()),
                )
            };
            set_native_feature_preference(&mut preferences, key, selected)?;
            if key == NativeFeatureKey::SubagentModel {
                set_subagent_model_selection(&mut preferences, selection.as_ref());
            }
            state
                .native
                .apply_native_feature_preferences_with_catalog(
                    &preferences,
                    (key == NativeFeatureKey::SubagentModel)
                        .then_some(model_catalog)
                        .flatten(),
                )
                .await?;
            if key == NativeFeatureKey::SubagentModel {
                return Ok(PreparedSettingChoice::Applied(payload, child_default_saved_notice(
                    language,
                )));
            }
            Ok(PreparedSettingChoice::Applied(payload, 
                language
                    .message(MessageKey::CommandsSavedForTheNextDiscodexWorkerStartRestart0f175de2)
                    .to_owned(),
            ))
        }
        SettingsKind::Hub => bail!(
            "{}",
            language.message(MessageKey::CommandsChooseSettingFirst)
        ),
        SettingsKind::MessageAccess => {
            let key = requested.strip_prefix("policy:").unwrap_or(requested);
            let policy = match key {
                "everyone" => MessagePolicy::Everyone,
                "registered-server" => MessagePolicy::RegisteredServer,
                "registered-channels" => MessagePolicy::RegisteredChannels,
                "paired" => MessagePolicy::Paired,
                _ => bail!("unknown message policy"),
            };
            let mut current = state.message_access.write().await;
            let mut staged = current.clone();
            staged.policy = policy;
            staged.save(&state.message_access_path)?;
            payload = serde_json::to_value(&staged)?;
            *current = staged;
            Ok(PreparedSettingChoice::Applied(payload, message_policy_label(policy, language).to_owned()))
        }
        SettingsKind::PairedAccounts
        | SettingsKind::Whitelist
        | SettingsKind::Blacklist
        | SettingsKind::RegisteredChannels => {
            bail!(
                "{}",
                language.message(MessageKey::CommandsMessageAccessSettingsHaveASeparateMenu)
            )
        }
    }
}
}

async fn full_catalog(state: &SettingsAppService, method: &str, base: Value) -> anyhow::Result<Value> {
    let mut page_params = base;
    let mut all = Vec::new();
    loop {
        let page = state.native.request(method, page_params.clone()).await.with_context(|| format!("load {method} catalog"))?;
        if let Some(data) = page.get("data").and_then(Value::as_array) { all.extend(data.iter().cloned()); }
        match page.get("nextCursor").and_then(Value::as_str) {
            Some(cursor) => page_params["cursor"] = Value::String(cursor.to_owned()),
            None => return Ok(json!({"data": all, "nextCursor": null})),
        }
    }
}
fn settings_error_detail(error: &anyhow::Error, _language: crate::i18n::Language, config: Option<&Config>) -> String {
    let token = config.and_then(|config| config.token().ok());
    crate::diagnostics::sanitize_diagnostic_with_secret(&format!("{error:#}"), token.as_deref())
}
pub(crate) fn save_input_mode(path: &std::path::Path, mode: InputMode) -> anyhow::Result<()> {
    let parent = path.parent().context("Discord settings path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec(&mode)?)?;
    std::fs::rename(&temp, path).with_context(|| format!("replace Discord settings {}", path.display()))
}
