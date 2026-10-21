//! Provider-qualified model catalog identities.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

pub(crate) fn model_id(model: &Value) -> Option<&str> {
    ["model", "id", "slug"]
        .into_iter()
        .filter_map(|k| model.get(k).and_then(Value::as_str))
        .find(|v| !v.trim().is_empty())
}
pub(crate) fn selection_value(row: &Value) -> Option<&str> {
    row.get("selectionValue")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
}
pub(crate) fn model_matches(row: &Value, requested: &str) -> bool {
    selection_value(row) == Some(requested)
        || model_id(row) == Some(requested)
        || ["id", "slug"]
            .into_iter()
            .any(|k| row.get(k).and_then(Value::as_str) == Some(requested))
}
pub(crate) fn catalog_has_model(c: &Value, requested: &str) -> bool {
    c.get("data")
        .and_then(Value::as_array)
        .is_some_and(|rows| rows.iter().any(|r| model_matches(r, requested)))
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ModelSelection {
    #[serde(default)]
    pub executor: ModelExecutor,
    pub provider: String,
    pub model: String,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ModelExecutor {
    #[default]
    Codex,
    Antigravity,
}
impl ModelExecutor {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
        }
    }
}
impl ModelSelection {
    pub(crate) fn value(&self) -> String {
        if self.executor == ModelExecutor::Codex {
            identity(&self.provider, &self.model)
        } else {
            serde_json::to_string(&[self.executor.as_str(), &self.provider, &self.model])
                .expect("model identity serializes")
        }
    }
}
fn provider(row: &Value) -> Option<&str> {
    row.get("provider")
        .or_else(|| row.get("modelProvider"))
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
}
fn identity(p: &str, m: &str) -> String {
    serde_json::to_string(&[p, m]).expect("identity serializes")
}
fn row_selection(row: &Value, fallback: Option<&str>) -> Option<ModelSelection> {
    Some(ModelSelection {
        executor: row
            .get("executor")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .ok()?
            .unwrap_or_default(),
        provider: provider(row).or(fallback).unwrap_or("openai").to_owned(),
        model: model_id(row)?.to_owned(),
    })
}
/// AGY is an opt-in executor, not an inferred provider or model-name prefix.
/// Exact configured IDs supplement the installed CLI's catalog.
pub(crate) fn add_antigravity_models(catalog: &mut Value, enabled: bool, models: &[String]) {
    if !enabled {
        return;
    }
    let Some(rows) = catalog.get_mut("data").and_then(Value::as_array_mut) else {
        return;
    };
    for model in models {
        let selection = ModelSelection {
            executor: ModelExecutor::Antigravity,
            provider: "antigravity".into(),
            model: model.clone(),
        };
        if !rows
            .iter()
            .any(|row| selection_value(row) == Some(selection.value().as_str()))
        {
            rows.push(json!({"id":model,"model":model,"displayName":model,
                "executor":"antigravity","provider":"antigravity","modelProvider":"antigravity",
                "selectionValue":selection.value()}));
        }
    }
}
pub(crate) fn add_antigravity_catalog(catalog: &mut Value, discovered: &crate::agy::Catalog) {
    let ids = discovered
        .models
        .iter()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    add_antigravity_models(catalog, true, &ids);
    if let Some(rows) = catalog.get_mut("data").and_then(Value::as_array_mut) {
        for row in rows
            .iter_mut()
            .filter(|row| row.get("executor").and_then(Value::as_str) == Some("antigravity"))
        {
            if let Some(model) = discovered
                .models
                .iter()
                .find(|model| model_id(row) == Some(model.id.as_str()))
            {
                row["displayName"] = json!(model.label);
            }
            // These are CLI argument choices, not guessed model capabilities.
            // The CLI remains responsible for validating model-specific support.
            row["supportedReasoningEfforts"] = json!(discovered.efforts);
        }
    }
}
pub(crate) fn resolve_selection(c: &Value, requested: &str) -> Result<ModelSelection> {
    let rows = c
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("native model catalog omitted its data list"))?;
    let active = c.get("provider").and_then(Value::as_str);
    let mut matches = Vec::new();
    for row in rows {
        if model_matches(row, requested) {
            if let Some(sel) = row_selection(row, active) {
                if !matches.contains(&sel) {
                    matches.push(sel);
                }
            }
        }
    }
    if let Some(p) = active
        && let Some(sel) = matches.iter().find(|s| s.provider == p)
    {
        return Ok(sel.clone());
    }
    match matches.as_slice() {
        [s] => Ok(s.clone()),
        [] => bail!("requested model is absent from the native model catalog"),
        _ => bail!("requested model name is ambiguous across providers"),
    }
}
pub(crate) fn model_choices(c: &Value) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    c.get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let id = model_id(row)?;
            let p = provider(row);
            let value = selection_value(row)
                .map(str::to_owned)
                .or_else(|| p.map(|p| identity(p, id)))
                .unwrap_or_else(|| id.to_owned());
            if !seen.insert(value.clone()) {
                return None;
            }
            let display = ["displayName", "name", "display_name"]
                .into_iter()
                .filter_map(|k| row.get(k).and_then(Value::as_str))
                .find(|v| !v.trim().is_empty())
                .unwrap_or(id);
            let label = match p {
                Some(p) if display == id => format!("{id} — {p}"),
                Some(p) => format!("{display} ({id}) — {p}"),
                None if display == id => id.to_owned(),
                None => format!("{display} ({id})"),
            };
            Some((value, label))
        })
        .collect()
}
fn annotate(row: &mut Value, p: &str) {
    let id = model_id(row).map(str::to_owned);
    if let Some(obj) = row.as_object_mut() {
        obj.insert("provider".into(), json!(p));
        obj.insert("modelProvider".into(), json!(p));
        if let Some(id) = id {
            obj.insert("selectionValue".into(), json!(identity(p, &id)));
        }
    }
}
fn has_model(rows: &[Value], p: &str, name: &str) -> bool {
    rows.iter().any(|r| {
        provider(r) == Some(p)
            && (model_id(r) == Some(name)
                || ["id", "slug"]
                    .into_iter()
                    .any(|key| r.get(key).and_then(Value::as_str) == Some(name)))
    })
}
fn add(rows: &mut Vec<Value>, name: Option<&str>, p: &str) {
    let Some(name) = name.filter(|s| !s.trim().is_empty()) else {
        return;
    };
    if !has_model(rows, p, name) {
        rows.push(json!({"id":name,"model":name,"displayName":name,"provider":p,"modelProvider":p,"selectionValue":identity(p,name)}));
    }
}
fn active_profile(c: &Value) -> Option<&Value> {
    c.get("profile")?
        .as_str()
        .and_then(|n| c.get("profiles")?.get(n))
}
fn active_value<'a>(c: &'a Value, k: &str) -> Option<&'a Value> {
    active_profile(c)
        .and_then(|p| p.get(k))
        .filter(|v| !v.is_null())
        .or_else(|| c.get(k).filter(|v| !v.is_null()))
}
pub(crate) fn configured_provider(c: &Value) -> &str {
    active_value(c, "model_provider")
        .and_then(Value::as_str)
        .unwrap_or("openai")
}
pub(crate) fn assemble_catalog(
    mut rows: Vec<Value>,
    response: &Value,
    thread: &Value,
    installation: Option<&str>,
    subagent_provider: Option<&str>,
) -> Result<Value> {
    let c = response.get("config").unwrap_or(response);
    let active = configured_provider(c);
    for row in &mut rows {
        annotate(row, active);
    }
    let root = c
        .get("model_provider")
        .and_then(Value::as_str)
        .unwrap_or("openai");
    let root_executor = c.get("model_executor").and_then(Value::as_str);
    if root_executor != Some("antigravity") {
        add(&mut rows, c.get("model").and_then(Value::as_str), root);
    }
    if let Some(profiles) = c.get("profiles").and_then(Value::as_object) {
        for profile in profiles.values() {
            if profile
                .get("model_executor")
                .and_then(Value::as_str)
                .or(root_executor)
                == Some("antigravity")
            {
                continue;
            }
            let p = profile
                .get("model_provider")
                .and_then(Value::as_str)
                .unwrap_or(root);
            add(&mut rows, profile.get("model").and_then(Value::as_str), p);
        }
    }
    let subagent_p = subagent_provider.unwrap_or(active);
    if c.pointer("/agents/default_subagent_executor")
        .and_then(Value::as_str)
        .or(root_executor)
        != Some("antigravity")
    {
        add(
            &mut rows,
            c.pointer("/agents/default_subagent_model")
                .and_then(Value::as_str),
            c.pointer("/agents/default_subagent_model_provider")
                .and_then(Value::as_str)
                .unwrap_or(subagent_p),
        );
    }
    add(&mut rows, installation, active);
    let thread_p = thread
        .get("modelProvider")
        .and_then(Value::as_str)
        .unwrap_or(active);
    if thread.get("modelExecutor").and_then(Value::as_str) != Some("antigravity") {
        add(
            &mut rows,
            thread.get("model").and_then(Value::as_str),
            thread_p,
        );
    }
    Ok(json!({"data":rows,"nextCursor":null,"provider":thread_p,"catalogProvider":active}))
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
pub(crate) fn needs_chatgpt_login(a: &Value) -> bool {
    a.get("requiresOpenaiAuth").and_then(Value::as_bool) != Some(false)
        && a.get("account").filter(|v| !v.is_null()).is_none()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_catalog_adds_exact_names_and_efforts_without_replacing_configured_choices() {
        let mut catalog = json!({"data":[]});
        add_antigravity_models(&mut catalog, true, &["manual".into(), "listed".into()]);
        add_antigravity_catalog(
            &mut catalog,
            &crate::agy::Catalog {
                models: vec![crate::agy::ModelOption {
                    id: "listed".into(),
                    label: "CLI model".into(),
                }],
                efforts: vec!["medium".into(), "high".into()],
            },
        );
        assert_eq!(catalog["data"].as_array().unwrap().len(), 2);
        assert_eq!(catalog["data"][0]["model"], "manual");
        assert_eq!(catalog["data"][1]["displayName"], "CLI model");
        for row in catalog["data"].as_array().unwrap() {
            assert_eq!(row["supportedReasoningEfforts"], json!(["medium", "high"]));
            assert_eq!(row["executor"], "antigravity");
        }
    }
    #[test]
    fn agy_catalog_is_opt_in_and_keeps_executor_identity_distinct() {
        let mut catalog = json!({"provider":"openai","data":[{"model":"same","provider":"openai","selectionValue":"[\"openai\",\"same\"]"}]});
        add_antigravity_models(&mut catalog, false, &["same".into()]);
        assert_eq!(model_choices(&catalog).len(), 1);
        add_antigravity_models(&mut catalog, true, &["same".into(), "same".into()]);
        assert_eq!(model_choices(&catalog).len(), 2);
        let selected =
            resolve_selection(&catalog, "[\"antigravity\",\"antigravity\",\"same\"]").unwrap();
        assert_eq!(selected.executor, ModelExecutor::Antigravity);
        assert_eq!(selected.model, "same");
        assert_eq!(
            resolve_selection(&catalog, "[\"openai\",\"same\"]")
                .unwrap()
                .executor,
            ModelExecutor::Codex
        );
    }

    #[test]
    fn legacy_selection_defaults_to_codex_and_external_thread_is_not_a_native_model() {
        let selection: ModelSelection =
            serde_json::from_value(json!({"provider":"example","model":"example-model"})).unwrap();
        assert_eq!(selection.executor, ModelExecutor::Codex);
        let catalog = assemble_catalog(vec![], &json!({"config":{}}),
            &json!({"model":"external-only","modelProvider":"openai","modelExecutor":"antigravity"}),None,None).unwrap();
        assert!(!catalog_has_model(&catalog, "external-only"));
    }
    #[test]
    fn external_configuration_does_not_fabricate_native_provider_model_rows() {
        let config = json!({"model_executor":"antigravity", "model":"external-main",
            "profiles":{"external":{"model":"external-profile"},"native":{"model_executor":"codex","model":"native-profile"}},
            "agents":{"default_subagent_model":"external-child","default_subagent_executor":"antigravity"}});
        let catalog = assemble_catalog(vec![], &config, &Value::Null, None, None).unwrap();
        assert_eq!(catalog["data"].as_array().unwrap().len(), 1);
        assert_eq!(catalog["data"][0]["model"], "native-profile");
    }
    #[test]
    fn identities_and_legacy_aliases() {
        let r = json!({"id":"key","model":"slug","slug":"old"});
        assert_eq!(model_id(&r), Some("slug"));
        assert!(model_matches(&r, "key"));
        assert!(model_matches(&r, "old"));
    }
    #[test]
    fn configured_profiles_and_different_live_provider_are_included_without_credentials() {
        let c=assemble_catalog(vec![json!({"id":"list","model":"listed","displayName":"Listed"})],
   &json!({"config":{"model_provider":"openai","model":"shared","profile":"local","profiles":{
    "local":{"model_provider":"ollama","model":"shared"},"other":{"model_provider":"gateway","model":"remote"}},
    "agents":{"default_subagent_model":"sub"},"model_providers":{"gateway":{"env_key":"SECRET","http_headers":{"Authorization":"secret"}}}}}),
   &json!({"model":"thread","modelProvider":"different"}),Some("install"),None).unwrap();
        for n in ["shared", "remote", "sub", "thread", "install", "listed"] {
            assert!(catalog_has_model(&c, n), "{n}");
        }
        let shared: Vec<_> = c["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| model_id(r) == Some("shared"))
            .collect();
        assert_eq!(shared.len(), 2);
        assert_ne!(selection_value(shared[0]), selection_value(shared[1]));
        assert_eq!(c["provider"], "different");
        assert_eq!(c["catalogProvider"], "ollama");
        assert_eq!(shared[0]["provider"], "openai");
        assert_eq!(shared[1]["provider"], "ollama");
        assert_eq!(
            c["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| model_id(r) == Some("thread"))
                .unwrap()["provider"],
            "different"
        );
        let serialized = serde_json::to_string(&c).unwrap();
        assert!(!serialized.contains("SECRET"));
        assert!(!serialized.contains("secret"));
    }
    #[test]
    fn choices_qualify_duplicate_models_and_resolve_raw_by_active_provider() {
        let c = json!({"provider":"ollama","data":[
   {"model":"same","displayName":"Cloud","provider":"openai","selectionValue":"[\"openai\",\"same\"]"},
   {"model":"same","displayName":"Local","provider":"ollama","selectionValue":"[\"ollama\",\"same\"]"}]});
        let choices = model_choices(&c);
        assert_eq!(choices.len(), 2);
        assert_ne!(choices[0].0, choices[1].0);
        assert!(choices[0].1.contains("openai"));
        assert!(choices[1].1.contains("ollama"));
        assert_eq!(
            resolve_selection(&c, "same").unwrap(),
            ModelSelection {
                executor: crate::model_catalog::ModelExecutor::Codex,
                provider: "ollama".into(),
                model: "same".into()
            }
        );
        assert_eq!(
            resolve_selection(&c, &choices[0].0).unwrap(),
            ModelSelection {
                executor: crate::model_catalog::ModelExecutor::Codex,
                provider: "openai".into(),
                model: "same".into()
            }
        );
        assert!(resolve_selection(&json!({"data":c["data"]}), "same").is_err());
    }
    #[test]
    fn legacy_rows_keep_old_choice_values() {
        let c = json!({"provider":"legacy","data":[{"id":"slug","displayName":"Friendly"}]});
        assert_eq!(
            model_choices(&c),
            vec![("slug".into(), "Friendly (slug)".into())]
        );
        assert_eq!(
            resolve_selection(&c, "slug").unwrap(),
            ModelSelection {
                executor: crate::model_catalog::ModelExecutor::Codex,
                provider: "legacy".into(),
                model: "slug".into()
            }
        );
    }
    #[test]
    fn current_configuration_provider_and_native_capabilities_are_retained() {
        let c=assemble_catalog(vec![json!({"model":"provider-model","supportedReasoningEfforts":[{"reasoningEffort":"low"}]})],
   &json!({"config":{"model_provider":"openai","profile":"local","profiles":{"local":{"model_provider":"local","model":"local"}}}}),
   &json!({"modelProvider":"old-thread"}),None,None).unwrap();
        assert_eq!(c["provider"], "old-thread");
        assert_eq!(c["catalogProvider"], "local");
        assert_eq!(c["data"][0]["provider"], "local");
        assert_eq!(
            c["data"][0]["supportedReasoningEfforts"],
            json!([{"reasoningEffort":"low"}])
        );
        assert!(catalog_has_model(&c, "local"));
        assert_eq!(resolve_selection(&c, "local").unwrap().provider, "local");
    }
    #[test]
    fn auth_respects_native_requirement() {
        assert!(!needs_chatgpt_login(
            &json!({"requiresOpenaiAuth":false,"account":null})
        ));
        assert!(!needs_chatgpt_login(
            &json!({"requiresOpenaiAuth":true,"account":{"type":"apiKey"}})
        ));
        assert!(needs_chatgpt_login(&json!({"account":null})));
    }
    #[test]
    fn subagent_provider_affinity_overrides_parent_catalog_provider() {
        let c = assemble_catalog(
            vec![],
            &json!({"config":{"model_provider":"cloud","agents":{"default_subagent_model":"small"}}}),
            &json!({"modelProvider":"cloud"}),
            None,
            Some("local"),
        ).unwrap();
        let row = c["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| model_id(row) == Some("small"))
            .unwrap();
        assert_eq!(row["provider"], "local");
        assert_eq!(c["catalogProvider"], "cloud");
        assert_eq!(c["provider"], "cloud");
    }

    #[test]
    fn configured_native_alias_does_not_create_a_duplicate_row() {
        let c = assemble_catalog(
            vec![json!({"id":"catalog-key","model":"inference-name"})],
            &json!({"config":{"model_provider":"openai","model":"catalog-key"}}),
            &Value::Null,
            None,
            None,
        )
        .unwrap();
        let rows = c["data"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            serde_json::from_str::<[String; 2]>(selection_value(&rows[0]).unwrap()).unwrap(),
            ["openai", "inference-name"]
        );
    }

    #[tokio::test]
    async fn pagination_uses_cursors_and_reports_malformed_or_repeated_pages() {
        let mut calls = Vec::new();
        let rows = read_pages(|params| {
            calls.push(params.clone());
            let page = match params.get("cursor").and_then(Value::as_str) {
                None => json!({"data":[{"model":"first"}],"nextCursor":"next"}),
                Some("next") => json!({"data":[{"model":"last"}],"nextCursor":null}),
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
                json!({"includeHidden":false,"cursor":"next"})
            ]
        );
        assert!(
            read_pages(|_| async { Ok(json!({"models":[]})) })
                .await
                .is_err()
        );
        assert!(
            read_pages(|_| async { Ok(json!({"data":[],"nextCursor":"same"})) })
                .await
                .is_err()
        );
    }

    #[test]
    fn model_selection_round_trips_for_persistence() {
        let selection = ModelSelection {
            executor: crate::model_catalog::ModelExecutor::Codex,
            provider: "local-provider".into(),
            model: "local-model".into(),
        };
        let encoded = serde_json::to_value(&selection).unwrap();
        assert_eq!(
            serde_json::from_value::<ModelSelection>(encoded).unwrap(),
            selection
        );
    }

    #[test]
    fn large_native_catalog_rows_are_preserved() {
        let rows = (0..81)
            .map(|i| json!({"id":format!("model-{i}")}))
            .collect();
        let c = assemble_catalog(rows, &json!({"config":{}}), &Value::Null, None, None).unwrap();
        assert_eq!(model_choices(&c).len(), 81);
        assert!(catalog_has_model(&c, "model-80"));
        assert!(!catalog_has_model(&c, "missing"));
    }

    #[test]
    fn raw_duplicate_slug_prefers_live_thread_provider_over_config_provider() {
        let c = assemble_catalog(
            vec![json!({"model":"shared","displayName":"Cloud shared"})],
            &json!({"config":{"model_provider":"cloud","model":"shared"}}),
            &json!({"model":"shared","modelProvider":"local"}),
            None,
            None,
        )
        .unwrap();
        assert_eq!(c["catalogProvider"], "cloud");
        assert_eq!(c["provider"], "local");
        assert_eq!(
            resolve_selection(&c, "shared").unwrap(),
            ModelSelection {
                executor: crate::model_catalog::ModelExecutor::Codex,
                provider: "local".into(),
                model: "shared".into()
            }
        );
        let rows = c["data"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["provider"], "cloud");
        assert_eq!(rows[1]["provider"], "local");
    }
}
