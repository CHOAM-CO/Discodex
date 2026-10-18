//! Thin Discord-facing adapters for the native Codex plugin APIs.
//!
//! This module deliberately delegates catalog, install, auth, and uninstall behavior to the
//! app-server. Callers must obtain explicit user confirmation before `install_confirmed`.
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::i18n::Language;
use crate::native::Native;

const APPROVAL_KIND: &str = "tool_suggestion";
const PERSIST_KEY: &str = "persist";
const PERSIST_ALWAYS: &str = "always";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCatalog {
    pub marketplaces: Vec<PluginMarketplace>,
    #[serde(default)]
    pub marketplace_load_errors: Vec<Value>,
    #[serde(default)]
    pub featured_plugin_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketplace {
    pub name: String,
    pub path: Option<String>,
    #[serde(default)]
    pub plugins: Vec<PluginSummary>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSummary {
    pub id: String,
    pub remote_plugin_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub install_policy: Option<String>,
    #[serde(default)]
    pub availability: Option<String>,
}

/// A selected catalog row with the exact selector required by `plugin/read` and `plugin/install`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallTarget {
    pub marketplace_path: Option<String>,
    pub remote_marketplace_name: Option<String>,
    pub plugin_name: String,
    pub display_name: String,
}

impl PluginInstallTarget {
    pub fn from_catalog(marketplace: &PluginMarketplace, plugin: &PluginSummary) -> Result<Self> {
        ensure!(!plugin.installed, "this plugin is already installed");
        ensure!(
            !matches!(
                plugin.availability.as_deref(),
                Some("NOT_AVAILABLE" | "DISABLED_BY_ADMIN")
            ),
            "Codex reports this plugin is unavailable"
        );
        if let Some(path) = marketplace.path.as_ref() {
            Ok(Self {
                marketplace_path: Some(path.clone()),
                remote_marketplace_name: None,
                plugin_name: plugin.name.clone(),
                display_name: plugin.name.clone(),
            })
        } else {
            let remote_id = plugin
                .remote_plugin_id
                .as_ref()
                .ok_or_else(|| anyhow!("remote plugin row has no remotePluginId"))?;
            Ok(Self {
                marketplace_path: None,
                remote_marketplace_name: Some(marketplace.name.clone()),
                plugin_name: remote_id.clone(),
                display_name: plugin.name.clone(),
            })
        }
    }

    fn selector_params(&self) -> Value {
        match (&self.marketplace_path, &self.remote_marketplace_name) {
            (Some(path), None) => json!({"marketplacePath": path}),
            (None, Some(name)) => json!({"remoteMarketplaceName": name}),
            _ => Value::Null,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDetailResponse {
    pub plugin: PluginDetail,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDetail {
    pub marketplace_name: String,
    pub marketplace_path: Option<String>,
    pub summary: PluginSummary,
    pub description: Option<String>,
    #[serde(default)]
    pub apps: Vec<PluginApp>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginApp {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub install_url: Option<String>,
    pub category: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallOutcome {
    pub auth_policy: String,
    #[serde(default)]
    pub apps_needing_auth: Vec<PluginApp>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthLogin {
    pub authorization_url: String,
    #[serde(default)]
    pub login_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthCompletion {
    pub name: String,
    pub thread_id: Option<String>,
    pub login_id: Option<String>,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginSuggestion {
    pub message: String,
    pub tool_id: Option<String>,
    pub tool_type: Option<String>,
    pub tool_name: Option<String>,
    pub install_url: Option<String>,
    pub suggestion_id: Option<String>,
    pub remote_plugin_id: Option<String>,
    pub app_connector_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionDecision {
    Accept,
    Decline,
    DeclineAndRemember,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuggestionLabels {
    pub accept: &'static str,
    pub decline: &'static str,
    pub decline_and_remember: &'static str,
    pub cancel: &'static str,
}

pub const fn suggestion_labels(language: Language) -> SuggestionLabels {
    match language {
        Language::English => SuggestionLabels {
            accept: "Install",
            decline: "Not now",
            decline_and_remember: "Don't suggest again",
            cancel: "Cancel",
        },
        Language::Korean => SuggestionLabels {
            accept: "설치",
            decline: "지금은 안 함",
            decline_and_remember: "다시 제안하지 않기",
            cancel: "취소",
        },
        Language::Chinese => SuggestionLabels {
            accept: "安装",
            decline: "暂不",
            decline_and_remember: "不再建议",
            cancel: "取消",
        },
        Language::Japanese => SuggestionLabels {
            accept: "インストール",
            decline: "今はしない",
            decline_and_remember: "今後提案しない",
            cancel: "キャンセル",
        },
    }
}

pub async fn list_catalog(native: &Native, force_refetch: bool) -> Result<PluginCatalog> {
    let response = native
        .request("plugin/list", json!({"forceRefetch": force_refetch}))
        .await?;
    Ok(serde_json::from_value(response)?)
}

pub async fn list_installed(native: &Native) -> Result<PluginCatalog> {
    let response = native.request("plugin/installed", json!({})).await?;
    Ok(serde_json::from_value(response)?)
}

pub async fn read_plugin(native: &Native, target: &PluginInstallTarget) -> Result<PluginDetail> {
    let mut params = target.selector_params();
    ensure!(
        !params.is_null(),
        "plugin selection has invalid marketplace selector"
    );
    params["pluginName"] = Value::String(target.plugin_name.clone());
    let response = native.request("plugin/read", params).await?;
    let detail: PluginDetailResponse = serde_json::from_value(response)?;
    Ok(detail.plugin)
}

/// Sends the official install request only after the caller has an affirmative Discord action.
pub async fn install_confirmed(
    native: &Native,
    target: &PluginInstallTarget,
    confirmed: bool,
) -> Result<PluginInstallOutcome> {
    ensure!(
        confirmed,
        "plugin installation requires explicit user confirmation"
    );
    let mut params = target.selector_params();
    ensure!(
        !params.is_null(),
        "plugin selection has invalid marketplace selector"
    );
    params["pluginName"] = Value::String(target.plugin_name.clone());
    params["installAttemptId"] = Value::String(Uuid::new_v4().to_string());
    let response = native.request("plugin/install", params).await?;
    Ok(serde_json::from_value(response)?)
}

pub async fn uninstall_confirmed(native: &Native, plugin_id: &str, confirmed: bool) -> Result<()> {
    ensure!(
        confirmed,
        "plugin removal requires explicit user confirmation"
    );
    native
        .request("plugin/uninstall", json!({"pluginId": plugin_id}))
        .await?;
    Ok(())
}

pub async fn begin_oauth_login(
    native: &Native,
    server_name: &str,
    thread_id: Option<&str>,
) -> Result<OAuthLogin> {
    let response = native
        .request(
            "mcpServer/oauth/login",
            json!({"name": server_name, "threadId": thread_id}),
        )
        .await?;
    Ok(serde_json::from_value(response)?)
}

pub fn oauth_completion(event: &Value) -> Option<OAuthCompletion> {
    if event.get("method")?.as_str()? != "mcpServer/oauthLogin/completed" {
        return None;
    }
    serde_json::from_value(event.get("params")?.clone()).ok()
}

pub fn plugin_suggestion_request_id(event: &Value) -> Option<Value> {
    plugin_suggestion(event)?;
    event.get("id").cloned()
}

pub fn plugin_suggestion(event: &Value) -> Option<PluginSuggestion> {
    if event.get("method")?.as_str()? != "mcpServer/elicitation/request" {
        return None;
    }
    let params = event.get("params")?;
    if !matches!(params.get("mode")?.as_str()?, "form" | "openai/form")
        || params.pointer("/_meta/codex_approval_kind")?.as_str()? != APPROVAL_KIND
    {
        return None;
    }
    let meta = params.get("_meta")?;
    Some(PluginSuggestion {
        message: params.get("message")?.as_str()?.to_owned(),
        tool_id: meta
            .get("tool_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        tool_type: meta
            .get("tool_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        tool_name: meta
            .get("tool_name")
            .and_then(Value::as_str)
            .map(str::to_owned),
        install_url: meta
            .get("install_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        suggestion_id: meta
            .get("suggestion_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        remote_plugin_id: meta
            .get("remote_plugin_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        app_connector_ids: meta
            .get("app_connector_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    })
}

fn suggested_plugin_matches(plugin: &PluginSummary, suggestion: &PluginSuggestion) -> bool {
    let Some(tool_id) = suggestion.tool_id.as_deref() else {
        return false;
    };
    plugin.id == tool_id
        && suggestion
            .remote_plugin_id
            .as_deref()
            .is_none_or(|remote_id| plugin.remote_plugin_id.as_deref() == Some(remote_id))
}

pub fn is_plugin_suggestion(suggestion: &PluginSuggestion) -> bool {
    match suggestion.tool_type.as_deref() {
        Some("plugin") => true,
        Some("connector") => false,
        _ => suggestion.remote_plugin_id.is_some(),
    }
}

pub fn suggested_plugin_is_installed(
    catalog: &PluginCatalog,
    suggestion: &PluginSuggestion,
) -> bool {
    catalog
        .marketplaces
        .iter()
        .flat_map(|marketplace| &marketplace.plugins)
        .any(|plugin| plugin.installed && suggested_plugin_matches(plugin, suggestion))
}

/// Resolves a native plugin suggestion to one exact installed catalog selector.
pub fn suggested_plugin_target(
    catalog: &PluginCatalog,
    suggestion: &PluginSuggestion,
) -> Result<PluginInstallTarget> {
    ensure!(
        is_plugin_suggestion(suggestion),
        "suggestion is not a plugin install"
    );
    let mut matches = catalog.marketplaces.iter().flat_map(|marketplace| {
        marketplace.plugins.iter().filter_map(move |plugin| {
            suggested_plugin_matches(plugin, suggestion).then_some((marketplace, plugin))
        })
    });
    let (marketplace, plugin) = matches.next().ok_or_else(|| {
        anyhow!("suggested plugin is no longer available in Codex's native catalog")
    })?;
    ensure!(
        matches.next().is_none(),
        "suggestion matches more than one native catalog entry"
    );
    PluginInstallTarget::from_catalog(marketplace, plugin)
}

/// Matches the official post-install verification inputs before the elicitation is accepted.
pub fn suggested_plugin_install_verified(
    suggestion: &PluginSuggestion,
    installed: &PluginCatalog,
    accessible_connector_ids: &std::collections::HashSet<String>,
) -> bool {
    let installed_plugin = suggested_plugin_is_installed(installed, suggestion);
    installed_plugin
        && suggestion
            .app_connector_ids
            .iter()
            .all(|id| accessible_connector_ids.contains(id))
}

/// Acceptance is sent only after native plugin/app state satisfies the selection.
pub fn verified_suggestion_accept(verified: bool) -> Option<Value> {
    verified.then(|| plugin_suggestion_response(SuggestionDecision::Accept))
}

/// Builds the MCP elicitation result for a plugin suggestion. Use `DeclineAndRemember` only
/// when the user explicitly chooses the persistent opt-out control.
pub fn plugin_suggestion_response(decision: SuggestionDecision) -> Value {
    match decision {
        SuggestionDecision::Accept => json!({"action": "accept", "content": {}}),
        SuggestionDecision::Decline => json!({"action": "decline"}),
        SuggestionDecision::DeclineAndRemember => {
            json!({"action": "decline", "_meta": {PERSIST_KEY: PERSIST_ALWAYS}})
        }
        SuggestionDecision::Cancel => json!({"action": "cancel"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_and_local_catalog_rows_keep_native_install_selectors() {
        let remote: PluginMarketplace = serde_json::from_value(json!({
            "name": "openai-curated-remote",
            "path": null,
            "plugins": [{
                "id": "calendar@openai-curated-remote",
                "remotePluginId": "remote-calendar-id",
                "name": "Calendar",
                "installed": false,
                "enabled": false,
                "installPolicy": "AVAILABLE",
                "availability": "AVAILABLE"
            }]
        }))
        .unwrap();
        let target = PluginInstallTarget::from_catalog(&remote, &remote.plugins[0]).unwrap();
        assert_eq!(
            target.remote_marketplace_name.as_deref(),
            Some("openai-curated-remote")
        );
        assert_eq!(target.marketplace_path, None);
        assert_eq!(target.plugin_name, "remote-calendar-id");
        assert_eq!(
            target.selector_params(),
            json!({"remoteMarketplaceName": "openai-curated-remote"})
        );

        let local = PluginMarketplace {
            name: "local-marketplace".to_owned(),
            path: Some("/workspace/marketplace.json".to_owned()),
            plugins: vec![remote.plugins[0].clone()],
        };
        let target = PluginInstallTarget::from_catalog(&local, &local.plugins[0]).unwrap();
        assert_eq!(
            target.marketplace_path.as_deref(),
            Some("/workspace/marketplace.json")
        );
        assert_eq!(target.remote_marketplace_name, None);
        assert_eq!(target.plugin_name, "Calendar");
    }

    #[test]
    fn suggestion_detection_and_response_preserve_official_metadata_contract() {
        let event = json!({
            "method": "mcpServer/elicitation/request",
            "id": "suggestion-1",
            "params": {
                "mode": "form",
                "message": "This helps with the task.",
                "requestedSchema": {"type": "object", "properties": {}},
                "_meta": {
                    "codex_approval_kind": "tool_suggestion",
                    "tool_id": "calendar@openai-curated-remote",
                    "tool_type": "plugin",
                    "tool_name": "Calendar",
                    "install_url": "https://example.invalid/install",
                    "suggestion_id": "suggestion-1",
                    "remote_plugin_id": "remote-calendar-id",
                    "app_connector_ids": ["calendar"]
                }
            }
        });
        let suggestion = plugin_suggestion(&event).unwrap();
        assert_eq!(suggestion.tool_name.as_deref(), Some("Calendar"));
        assert_eq!(suggestion.app_connector_ids, vec!["calendar".to_owned()]);
        assert_eq!(
            plugin_suggestion_request_id(&event),
            Some(json!("suggestion-1"))
        );
        assert!(plugin_suggestion(&json!({"method": "mcpServer/elicitation/request", "params": {"mode": "form", "message": "x", "_meta": {"codex_approval_kind": "other"}}})).is_none());
        assert_eq!(
            plugin_suggestion_response(SuggestionDecision::Accept),
            json!({"action": "accept", "content": {}})
        );
        assert_eq!(
            plugin_suggestion_response(SuggestionDecision::Decline),
            json!({"action": "decline"})
        );
        assert_eq!(
            plugin_suggestion_response(SuggestionDecision::DeclineAndRemember),
            json!({"action": "decline", "_meta": {"persist": "always"}})
        );
        assert_eq!(
            plugin_suggestion_response(SuggestionDecision::Cancel),
            json!({"action": "cancel"})
        );
    }

    #[test]
    fn suggested_plugin_uses_exact_catalog_selector_and_only_accepts_after_native_verification() {
        let catalog: PluginCatalog = serde_json::from_value(json!({
            "marketplaces": [{"name":"openai-curated-remote","path":null,"plugins":[{
                "id":"calendar@openai-curated-remote","remotePluginId":"remote-calendar-id",
                "name":"Calendar","installed":false,"enabled":false,"installPolicy":"AVAILABLE","availability":"AVAILABLE"
            }]}]
        })).unwrap();
        let suggestion = PluginSuggestion {
            message: "connect a calendar".to_owned(),
            tool_id: Some("calendar@openai-curated-remote".to_owned()),
            tool_type: Some("plugin".to_owned()),
            tool_name: Some("Calendar".to_owned()),
            install_url: None,
            suggestion_id: Some("suggestion-1".to_owned()),
            remote_plugin_id: Some("remote-calendar-id".to_owned()),
            app_connector_ids: vec!["calendar".to_owned()],
        };
        let target = suggested_plugin_target(&catalog, &suggestion).unwrap();
        assert_eq!(
            target.remote_marketplace_name.as_deref(),
            Some("openai-curated-remote")
        );
        assert_eq!(target.plugin_name, "remote-calendar-id");
        assert_eq!(verified_suggestion_accept(false), None);
        assert!(
            suggested_plugin_install_verified(
                &suggestion,
                &catalog,
                &std::collections::HashSet::from(["calendar".to_owned()])
            ) == false
        );

        let mut installed = catalog.clone();
        installed.marketplaces[0].plugins[0].installed = true;
        let accessible = std::collections::HashSet::from(["calendar".to_owned()]);
        assert!(suggested_plugin_install_verified(
            &suggestion,
            &installed,
            &accessible
        ));
        assert_eq!(
            verified_suggestion_accept(true),
            Some(json!({"action":"accept","content":{}}))
        );
        assert!(
            suggested_plugin_target(
                &catalog,
                &PluginSuggestion {
                    tool_type: Some("connector".to_owned()),
                    ..suggestion
                }
            )
            .is_err()
        );
    }

    #[test]
    fn oauth_completion_reports_native_success_and_failure_without_inference() {
        assert_eq!(
            oauth_completion(&json!({
                "method": "mcpServer/oauthLogin/completed",
                "params": {"name": "calendar", "threadId": "thread", "loginId": "login", "success": true}
            })),
            Some(OAuthCompletion {
                name: "calendar".to_owned(),
                thread_id: Some("thread".to_owned()),
                login_id: Some("login".to_owned()),
                success: true,
                error: None
            })
        );
        assert_eq!(
            oauth_completion(&json!({
                "method": "mcpServer/oauthLogin/completed",
                "params": {"name": "calendar", "success": false, "error": "denied"}
            }))
            .unwrap()
            .error
            .as_deref(),
            Some("denied")
        );
        assert!(oauth_completion(&json!({"method": "mcpServer/status/list"})).is_none());
    }

    #[test]
    fn labels_cover_all_supported_languages_and_install_requires_confirmation() {
        for language in [
            Language::English,
            Language::Korean,
            Language::Chinese,
            Language::Japanese,
        ] {
            let labels = suggestion_labels(language);
            assert!(!labels.accept.is_empty());
            assert!(!labels.decline.is_empty());
            assert!(!labels.decline_and_remember.is_empty());
            assert!(!labels.cancel.is_empty());
        }
        assert!(
            PluginInstallTarget::from_catalog(
                &PluginMarketplace {
                    name: "remote".to_owned(),
                    path: None,
                    plugins: vec![]
                },
                &PluginSummary {
                    id: "x".to_owned(),
                    remote_plugin_id: None,
                    name: "x".to_owned(),
                    installed: false,
                    enabled: false,
                    install_policy: None,
                    availability: None
                }
            )
            .is_err()
        );
        assert!(
            PluginInstallTarget::from_catalog(
                &PluginMarketplace {
                    name: "local".to_owned(),
                    path: Some("/m.json".to_owned()),
                    plugins: vec![]
                },
                &PluginSummary {
                    id: "x".to_owned(),
                    remote_plugin_id: None,
                    name: "x".to_owned(),
                    installed: true,
                    enabled: false,
                    install_policy: None,
                    availability: None
                }
            )
            .is_err()
        );
    }
}
