//! Model identities and the active Codex provider's selectable catalog.
//! Only names explicitly returned/configured by Codex are added. Provider auth,
//! inference, and capability metadata remain owned by Codex.
use std::collections::HashSet;

use anyhow::{Result, bail};
use serde_json::{Value, json};

pub(crate) fn model_id(model: &Value) -> Option<&str> {
    ["model", "id", "slug"]
        .into_iter()
        .filter_map(|key| model.get(key).and_then(Value::as_str))
        .find(|value| !value.trim().is_empty())
}

pub(crate) fn model_matches(model: &Value, requested: &str) -> bool {
    model_id(model) == Some(requested)
        || ["id", "slug"]
            .into_iter()
            .any(|key| model.get(key).and_then(Value::as_str) == Some(requested))
}

pub(crate) fn catalog_has_model(catalog: &Value, requested: &str) -> bool {
    catalog
        .get("data")
        .and_then(Value::as_array)
        .is_some_and(|models| models.iter().any(|model| model_matches(model, requested)))
}

pub(crate) fn model_choices(catalog: &Value) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    catalog
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model_id(model)?;
            if !seen.insert(id.to_owned()) {
                return None;
            }
            let display = ["displayName", "name", "display_name"]
                .into_iter()
                .filter_map(|key| model.get(key).and_then(Value::as_str))
                .find(|value| !value.trim().is_empty())
                .unwrap_or(id);
            let label = if display == id {
                id.to_owned()
            } else {
                format!("{display} ({id})")
            };
            Some((id.to_owned(), label))
        })
        .collect()
}

pub(crate) async fn read_pages<F, Fut>(mut request: F) -> Result<Vec<Value>>
where
    F: FnMut(Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let mut params = json!({"includeHidden":false});
    let mut rows = Vec::new();
    let mut cursors = HashSet::new();
    loop {
        let page = request(params.clone()).await?;
        let data = page
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("native model catalog omitted its data list"))?;
        rows.extend(data.iter().cloned());
        match page.get("nextCursor").and_then(Value::as_str) {
            Some(cursor) => {
                if !cursors.insert(cursor.to_owned()) {
                    bail!("native model catalog repeated a pagination cursor");
                }
                params["cursor"] = json!(cursor);
            }
            None => return Ok(rows),
        }
    }
}

fn active_profile(config: &Value) -> Option<&Value> {
    let name = config.get("profile")?.as_str()?;
    config.get("profiles")?.get(name)
}

fn active_value<'a>(config: &'a Value, key: &str) -> Option<&'a Value> {
    active_profile(config)
        .and_then(|profile| profile.get(key))
        .filter(|value| !value.is_null())
        .or_else(|| config.get(key).filter(|value| !value.is_null()))
}

pub(crate) fn configured_provider(config: &Value) -> &str {
    active_value(config, "model_provider")
        .and_then(Value::as_str)
        .unwrap_or("openai")
}

/// A standard provider /v1/models response is not a Codex catalog. Local and
/// custom model names explicitly configured by the user still must be selectable.
pub(crate) fn assemble_catalog(
    mut models: Vec<Value>,
    config_response: &Value,
    thread: &Value,
    installation_model: Option<&str>,
) -> Result<Value> {
    let config = config_response.get("config").unwrap_or(config_response);
    let provider = configured_provider(config);
    if let Some(thread_provider) = thread.get("modelProvider").and_then(Value::as_str)
        && thread_provider != provider
    {
        bail!(
            "Codex's configured provider differs from the active thread provider; align the native thread and provider configuration before changing models"
        );
    }
    let mut add = |name: Option<&str>| {
        let Some(name) = name.filter(|name| !name.trim().is_empty()) else {
            return;
        };
        if !models.iter().any(|model| model_matches(model, name)) {
            // Do not manufacture supported reasoning, input, or service capabilities.
            models.push(json!({"id":name,"model":name,"displayName":name}));
        }
    };
    add(active_value(config, "model").and_then(Value::as_str));
    add(config
        .pointer("/agents/default_subagent_model")
        .and_then(Value::as_str));
    if let Some(profiles) = config.get("profiles").and_then(Value::as_object) {
        for profile in profiles.values() {
            let profile_provider = profile
                .get("model_provider")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    config
                        .get("model_provider")
                        .and_then(Value::as_str)
                        .unwrap_or("openai")
                });
            if profile_provider == provider {
                add(profile.get("model").and_then(Value::as_str));
            }
        }
    }
    add(installation_model);
    add(thread.get("model").and_then(Value::as_str));
    Ok(json!({"data":models,"nextCursor":null,"provider":provider}))
}

/// Respect native provider authentication rather than imposing ChatGPT sign-in
/// on an API-key or unauthenticated local provider.
pub(crate) fn needs_chatgpt_login(account: &Value) -> bool {
    account.get("requiresOpenaiAuth").and_then(Value::as_bool) != Some(false)
        && account
            .get("account")
            .filter(|value| !value.is_null())
            .is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_prefers_inference_name_and_accepts_catalog_id_alias() {
        let model = json!({"id":"ui-model-key","model":"provider-model","slug":"obsolete"});
        assert_eq!(model_id(&model), Some("provider-model"));
        assert!(model_matches(&model, "ui-model-key"));
        assert!(model_matches(&model, "provider-model"));
        assert!(model_matches(&model, "obsolete"));
        assert_eq!(model_id(&json!({"id":"id-only"})), Some("id-only"));
        assert_eq!(model_id(&json!({"slug":"legacy"})), Some("legacy"));
        assert_eq!(
            model_id(&json!({"model":"","id":"fallback"})),
            Some("fallback")
        );
        assert_eq!(model_id(&json!({"id":" "})), None);
    }

    #[test]
    fn configured_local_models_and_subagents_remain_selectable_without_native_rows() {
        let catalog = assemble_catalog(
            vec![],
            &json!({"config":{
                "model_provider":"ollama","model":"local-main",
                "agents":{"default_subagent_model":"local-small"}
            }}),
            &json!({"model":"local-current","modelProvider":"ollama"}),
            None,
        )
        .unwrap();
        assert_eq!(
            model_choices(&catalog),
            vec![
                ("local-main".into(), "local-main".into()),
                ("local-small".into(), "local-small".into()),
                ("local-current".into(), "local-current".into())
            ]
        );
        assert_eq!(catalog["provider"], "ollama");
        assert!(
            catalog["data"][0]
                .get("supportedReasoningEfforts")
                .is_none()
        );
        assert!(catalog["data"][0].get("serviceTiers").is_none());
    }

    #[test]
    fn merges_same_provider_profile_models_without_credentials_or_other_providers() {
        let catalog = assemble_catalog(vec![], &json!({"config":{
            "model_provider":"gateway","model":"primary",
            "model_providers":{"gateway":{"env_key":"SECRET_ENV","http_headers":{"Authorization":"private"}}},
            "profiles":{
                "small":{"model":"small"},
                "large":{"model_provider":"gateway","model":"large"},
                "elsewhere":{"model_provider":"other","model":"foreign"}
            }
        }}), &Value::Null, None).unwrap();
        assert!(catalog_has_model(&catalog, "primary"));
        assert!(catalog_has_model(&catalog, "small"));
        assert!(catalog_has_model(&catalog, "large"));
        assert!(!catalog_has_model(&catalog, "foreign"));
        assert_eq!(
            catalog
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["data", "nextCursor", "provider"]
        );
    }

    #[test]
    fn catalog_rows_keep_native_capabilities_and_deduplicate_aliases() {
        let row = json!({"id":"catalog-key","model":"actual","displayName":"Friendly",
            "supportedReasoningEfforts":[{"reasoningEffort":"low"}]});
        let catalog = assemble_catalog(
            vec![row.clone()],
            &json!({"config":{"model":"actual"}}),
            &json!({"model":"catalog-key"}),
            Some("actual"),
        )
        .unwrap();
        assert_eq!(catalog["data"], json!([row]));
        assert_eq!(
            model_choices(&catalog),
            vec![("actual".into(), "Friendly (actual)".into())]
        );
    }

    #[test]
    fn configured_active_profile_selects_its_provider_and_model() {
        let catalog = assemble_catalog(vec![], &json!({"config":{
            "model_provider":"openai","model":"root","profile":"local",
            "profiles":{"local":{"model_provider":"lmstudio","model":"local-model"},"inherited":{"model":"cloud-model"}}
        }}), &json!({"modelProvider":"lmstudio"}), None).unwrap();
        assert_eq!(catalog["provider"], "lmstudio");
        assert!(!catalog_has_model(&catalog, "root"));
        assert!(!catalog_has_model(&catalog, "cloud-model"));
        assert!(catalog_has_model(&catalog, "local-model"));
    }

    #[test]
    fn refuses_catalog_for_a_different_live_thread_provider() {
        assert!(
            assemble_catalog(
                vec![json!({"model":"wrong-provider"})],
                &json!({"config":{"model_provider":"gateway"}}),
                &json!({"modelProvider":"ollama"}),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn every_catalog_model_is_kept_including_large_and_legacy_catalogs() {
        let rows = (0..81)
            .map(|n| json!({"id":format!("model-{n}")}))
            .collect();
        let catalog = assemble_catalog(rows, &json!({"config":{}}), &Value::Null, None).unwrap();
        assert_eq!(model_choices(&catalog).len(), 81);
        assert!(catalog_has_model(&catalog, "model-80"));
        assert!(!catalog_has_model(&catalog, "missing"));
    }

    #[test]
    fn auth_uses_native_requirement_and_existing_accounts() {
        assert!(!needs_chatgpt_login(
            &json!({"requiresOpenaiAuth":false,"account":null})
        ));
        assert!(!needs_chatgpt_login(
            &json!({"requiresOpenaiAuth":true,"account":{"type":"apiKey"}})
        ));
        assert!(!needs_chatgpt_login(
            &json!({"requiresOpenaiAuth":true,"account":{"type":"chatgpt"}})
        ));
        assert!(needs_chatgpt_login(
            &json!({"requiresOpenaiAuth":true,"account":null})
        ));
        assert!(needs_chatgpt_login(&json!({"account":null})));
    }
    #[tokio::test]
    async fn catalog_reads_all_native_pages_with_exact_cursor() {
        let mut calls = Vec::new();
        let rows = read_pages(|params| {
            calls.push(params.clone());
            let page = match params.get("cursor").and_then(Value::as_str) {
                None => json!({"data":[{"model":"first"}],"nextCursor":"page-two"}),
                Some("page-two") => json!({"data":[{"model":"last"}],"nextCursor":null}),
                _ => panic!("unexpected cursor"),
            };
            async move { Ok(page) }
        })
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![json!({"model":"first"}), json!({"model":"last"})]
        );
        assert_eq!(
            calls,
            vec![
                json!({"includeHidden":false}),
                json!({"includeHidden":false,"cursor":"page-two"})
            ]
        );
    }

    #[tokio::test]
    async fn malformed_or_looping_native_catalog_reports_failure() {
        assert!(
            read_pages(|_| async { Ok(json!({"models":[]})) })
                .await
                .is_err()
        );
        assert!(
            read_pages(|_| async { Ok(json!({"data":[],"nextCursor":"repeat"})) })
                .await
                .is_err()
        );
        assert!(
            read_pages(|_| async { anyhow::bail!("native error") })
                .await
                .is_err()
        );
    }
}
