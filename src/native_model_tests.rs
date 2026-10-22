use super::*;
use std::future;

#[test]
fn superseded_applied_revision_cannot_cross_conditional_persistence_guard() {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let key = ThreadSettingKey {
        generation: 4,
        thread_id: "thread-a".to_owned(),
        field: "model".to_owned(),
    };
    let revision_flag = Arc::new(AtomicBool::new(true));
    let revision = ThreadSettingRevision {
        key: key.clone(),
        current: revision_flag.clone(),
        inner: Weak::new(),
    };
    let mut pending = HashMap::new();
    let mut current = HashMap::from([(key.clone(), Arc::downgrade(&revision_flag))]);

    // A has already applied. B supersedes A before A reaches persistence.
    assert!(!supersede_thread_setting_revisions(
        &mut pending,
        &mut current,
        &key,
    ));
    let replacement = Arc::new(AtomicBool::new(true));
    current.insert(key, Arc::downgrade(&replacement));
    let pending = Mutex::new(pending);
    let current = Mutex::new(current);

    let writes = AtomicUsize::new(0);
    let result = with_current_thread_setting_revision(&pending, &current, &revision, || {
        writes.fetch_add(1, Ordering::SeqCst);
        Ok("saved")
    })
    .unwrap();

    assert_eq!(result, None);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
}

#[test]
fn current_applied_revision_runs_conditional_persistence_once() {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let key = ThreadSettingKey {
        generation: 4,
        thread_id: "thread-a".to_owned(),
        field: "model".to_owned(),
    };
    let current_flag = Arc::new(AtomicBool::new(true));
    let revision = ThreadSettingRevision {
        key: key.clone(),
        current: current_flag.clone(),
        inner: Weak::new(),
    };
    let pending = Mutex::new(HashMap::new());
    let current = Mutex::new(HashMap::from([(key, Arc::downgrade(&current_flag))]));
    let writes = AtomicUsize::new(0);

    let result = with_current_thread_setting_revision(&pending, &current, &revision, || {
        writes.fetch_add(1, Ordering::SeqCst);
        Ok("saved")
    })
    .unwrap();

    assert_eq!(result, Some("saved"));
    assert_eq!(writes.load(Ordering::SeqCst), 1);
}

#[test]
fn thread_settings_cache_changes_only_for_current_thread_and_generation_event() {
    let mut cache = json!({
        "id":"thread-current",
        "effort":"medium",
        "serviceTier":"standard",
        "model":"old-model"
    });
    let unchanged = cache.clone();
    for event in [
        json!({"method":"error","params":{"error":{"code":-32000,"message":"unrelated"}}}),
        json!({"method":"thread/settings/updated","params":{
            "_discodex_connection_generation":8,
            "threadId":"thread-other",
            "threadSettings":{"effort":"high","model":"new-model"}
        }}),
        json!({"method":"thread/settings/updated","params":{
            "_discodex_connection_generation":6,
            "threadId":"thread-current",
            "threadSettings":{"effort":"high","model":"new-model"}
        }}),
    ] {
        assert!(!apply_thread_settings_event_to_cache(
            &mut cache,
            "thread-current",
            7,
            &event,
        ));
        assert_eq!(cache, unchanged);
    }

    let applied = json!({"method":"thread/settings/updated","params":{
        "_discodex_connection_generation":7,
        "threadId":"thread-current",
        "threadSettings":{"reasoningEffort":"high","model":"new-model"}
    }});
    assert!(apply_thread_settings_event_to_cache(
        &mut cache,
        "thread-current",
        7,
        &applied,
    ));
    assert_eq!(cache["effort"], "high");
    assert_eq!(cache["model"], "new-model");
    assert_eq!(cache["serviceTier"], "standard");
}

#[tokio::test]
async fn delayed_thread_a_readback_cannot_overwrite_thread_b_cache() {
    let reconnect = AsyncMutex::new(());
    let current_thread_id = Mutex::new("thread-A".to_owned());
    let current_generation = AtomicU64::new(7);
    let thread_state = Mutex::new(json!({"id":"thread-A","effort":"medium"}));
    let (started_sender, started_receiver) = oneshot::channel();
    let (response_sender, response_receiver) = oneshot::channel();

    let refresh = refresh_thread_state_with(
        &reconnect,
        &current_thread_id,
        &current_generation,
        &thread_state,
        move |thread_id| async move {
            started_sender.send(thread_id).unwrap();
            response_receiver
                .await
                .map_err(|_| anyhow!("synthetic thread/read response was canceled"))
        },
    );
    tokio::pin!(refresh);
    let requested_thread = tokio::select! {
        result = &mut refresh => panic!("refresh completed before its response: {result:?}"),
        requested_thread = started_receiver => requested_thread.unwrap(),
    };
    assert_eq!(requested_thread, "thread-A");

    *current_thread_id.lock().unwrap() = "thread-B".to_owned();
    *thread_state.lock().unwrap() = json!({
        "id":"thread-B","effort":"low","model":"model-B"
    });
    response_sender
        .send(json!({
            "id":"thread-A","effort":"high","model":"model-A",
            "_discodex_connection_generation":7
        }))
        .unwrap();

    let returned = refresh.await.unwrap();
    assert_eq!(returned["id"], "thread-B");
    assert_eq!(returned["effort"], "low");
    assert_eq!(returned["model"], "model-B");

    *current_thread_id.lock().unwrap() = "thread-A".to_owned();
    *thread_state.lock().unwrap() = json!({"id":"thread-A","effort":"medium"});
    let returned = refresh_thread_state_with(
        &reconnect,
        &current_thread_id,
        &current_generation,
        &thread_state,
        |_| {
            future::ready(Ok(json!({
                "id":"thread-A","effort":"high","model":"model-A",
                "_discodex_connection_generation":7
            })))
        },
    )
    .await
    .unwrap();
    assert_eq!(returned["id"], "thread-A");
    assert_eq!(returned["effort"], "high");
    assert_eq!(returned["model"], "model-A");
}

#[tokio::test]
async fn native_responses_stay_bound_to_their_pending_request_ids() {
    let (sender_a, receiver_a) = oneshot::channel();
    let (sender_b, receiver_b) = oneshot::channel();
    let pending = Mutex::new(HashMap::from([
        (
            "id-A".to_owned(),
            PendingRequest {
                sender: sender_a,
                method: "thread/read".to_owned(),
                observation_fence: 0,
            },
        ),
        (
            "id-B".to_owned(),
            PendingRequest {
                sender: sender_b,
                method: "thread/settings/update".to_owned(),
                observation_fence: 0,
            },
        ),
    ]));

    let response_b = json!({"error":{"code":-32001,"message":"status 503; provider rejected api_key=fixture-secret; retry later"}});
    let (request_b, _) = take_pending_response(&pending, "id-B");
    let PendingRequest {
        sender: sender_b,
        method: method_b,
        observation_fence: fence_b,
    } = request_b.unwrap();
    sender_b
        .send(decode_pending_response(
            &method_b, response_b, 7, 41, fence_b,
        ))
        .unwrap();

    let success_a = json!({"result":{"id":"thread-a","status":"ready"}});
    let (request_a, _) = take_pending_response(&pending, "id-A");
    let PendingRequest {
        sender: sender_a,
        method: method_a,
        observation_fence: fence_a,
    } = request_a.unwrap();
    sender_a
        .send(decode_pending_response(
            &method_a, success_a, 7, 42, fence_a,
        ))
        .unwrap();

    let error_b = receiver_b.await.unwrap().unwrap_err();
    assert_eq!(error_b.method, "thread/settings/update");
    assert_eq!(error_b.code, Some(-32001));
    assert_eq!(
        error_b.message,
        "status 503; provider rejected api_key=fixture-secret; retry later"
    );
    assert_eq!(
        receiver_a.await.unwrap().unwrap(),
        json!({
            "id":"thread-a",
            "status":"ready",
            "_discodex_connection_generation":7,
            "_discodex_observation_order":42,
            "_discodex_observation_fence":0
        })
    );
    assert!(pending.lock().unwrap().is_empty());
}

#[tokio::test]
async fn matching_thread_setting_readback_confirms_without_queueing_update() {
    let calls = std::cell::RefCell::new(Vec::new());
    let outcome = readback_then_apply_thread_setting_update(
        |method, _| {
            calls.borrow_mut().push(method);
            future::ready(Ok(json!({
                "id":"thread-current",
                "effort":"high"
            })))
        },
        "thread-current",
        json!({"threadId":"thread-current","effort":"high"}),
        "effort",
        &json!("high"),
        true,
        || true,
        Language::English,
    )
    .await
    .unwrap();

    let ThreadSettingReadbackOutcome::Confirmed(snapshot) = outcome else {
        panic!("matching readback should be confirmed: {outcome:?}");
    };
    assert_eq!(snapshot["effort"], "high");
    assert_eq!(*calls.borrow(), ["thread/read"]);
}

#[tokio::test]
async fn mismatching_thread_setting_readback_uses_completed_update_snapshot() {
    let calls = std::cell::RefCell::new(Vec::new());
    let outcome = readback_then_apply_thread_setting_update(
        |method, _| {
            calls.borrow_mut().push(method);
            future::ready(Ok(if method == "thread/read" {
                json!({"id":"thread-current","effort":"medium"})
            } else {
                json!({"threadId":"thread-current","threadSettings":{"effort":"high"}})
            }))
        },
        "thread-current",
        json!({"threadId":"thread-current","effort":"high"}),
        "effort",
        &json!("high"),
        true,
        || true,
        Language::English,
    )
    .await
    .unwrap();

    assert!(matches!(
        outcome,
        ThreadSettingReadbackOutcome::Confirmed(_)
    ));
    assert_eq!(*calls.borrow(), ["thread/read", "thread/settings/update"]);
}

#[tokio::test]
async fn superseded_thread_setting_readback_does_not_queue_update() {
    let calls = std::cell::RefCell::new(Vec::new());
    let outcome = readback_then_apply_thread_setting_update(
        |method, _| {
            calls.borrow_mut().push(method);
            future::ready(Ok(json!({
                "id":"thread-current",
                "effort":"high"
            })))
        },
        "thread-current",
        json!({"threadId":"thread-current","effort":"high"}),
        "effort",
        &json!("high"),
        true,
        || false,
        Language::English,
    )
    .await
    .unwrap();

    assert!(matches!(outcome, ThreadSettingReadbackOutcome::Superseded));
    assert_eq!(*calls.borrow(), ["thread/read"]);
}

#[tokio::test]
async fn prior_pending_supersession_disables_readback_shortcut() {
    let calls = std::cell::RefCell::new(Vec::new());
    let outcome = readback_then_apply_thread_setting_update(
        |method, _| {
            calls.borrow_mut().push(method);
            future::ready(Ok(if method == "thread/read" {
                json!({"id":"thread-current","effort":"high"})
            } else {
                json!({"threadId":"thread-current","threadSettings":{"effort":"high"}})
            }))
        },
        "thread-current",
        json!({"threadId":"thread-current","effort":"high"}),
        "effort",
        &json!("high"),
        false,
        || true,
        Language::English,
    )
    .await
    .unwrap();

    assert!(matches!(
        outcome,
        ThreadSettingReadbackOutcome::Confirmed(_)
    ));
    assert_eq!(*calls.borrow(), ["thread/read", "thread/settings/update"]);
}

#[tokio::test]
async fn new_thread_settings_cache_uses_matching_applied_snapshot_not_start_snapshot() {
    let requested = json!({"threadId":"thread-new","effort":"high","serviceTier":null});
    let calls = std::cell::RefCell::new(Vec::new());
    let stale_start_snapshot =
        json!({"id":"thread-new","effort":"medium","serviceTier":"standard"});
    let mut cache = thread_state_summary(&stale_start_snapshot);
    apply_and_merge_configured_thread_settings(
        |method, params| {
            calls.borrow_mut().push((method.to_owned(), params));
            assert_eq!(method, "thread/settings/update");
            std::future::ready(Ok(json!({
                "threadId":"thread-new",
                "_discodex_connection_generation":4,
                "threadSettings":{"reasoningEffort":"high","serviceTier":null,"model":"example-model"}
            })))
        },
        "thread-new",
        4,
        requested.clone(),
        &mut cache,
        Language::English,
    )
    .await
    .unwrap();

    assert_eq!(cache["effort"], "high");
    assert_eq!(cache["serviceTier"], Value::Null);
    assert_eq!(cache["model"], "example-model");
    assert_eq!(
        *calls.borrow(),
        vec![("thread/settings/update".to_owned(), requested)]
    );
}

#[tokio::test]
async fn new_thread_settings_noop_is_confirmed_by_completed_snapshot_without_notification() {
    let calls = std::cell::RefCell::new(Vec::new());
    let applied = apply_configured_thread_settings(
        |method, params| {
            calls.borrow_mut().push((method.to_owned(), params));
            assert_eq!(method, "thread/settings/update");
            std::future::ready(Ok(json!({
                "threadId":"thread-new",
                "threadSettings":{"effort":"high"},
                "_discodex_connection_generation":2
            })))
        },
        "thread-new",
        2,
        json!({"threadId":"thread-new","effort":"high"}),
        Language::English,
    )
    .await
    .unwrap();

    assert_eq!(applied["effort"], "high");
    assert_eq!(calls.borrow().len(), 1);
}

#[tokio::test]
async fn new_thread_settings_update_failure_preserves_native_error() {
    let error = apply_configured_thread_settings(
        |_, _| std::future::ready(Err(anyhow!("native rejected configured settings"))),
        "thread-new",
        2,
        json!({"threadId":"thread-new","effort":"high"}),
        Language::English,
    )
    .await
    .unwrap_err();

    assert!(format!("{error:#}").contains("native rejected configured settings"));
}

#[tokio::test]
async fn new_thread_settings_connection_close_is_not_reported_as_applied() {
    let old_cache = json!({"id":"thread-new","effort":"medium"});
    let mut cache = thread_state_summary(&old_cache);
    let error = apply_and_merge_configured_thread_settings(
        |_, _| {
            std::future::ready(Err(anyhow!(
                "Codex app-server connection ended before response"
            )))
        },
        "thread-new",
        2,
        json!({"threadId":"thread-new","effort":"high"}),
        &mut cache,
        Language::English,
    )
    .await
    .unwrap_err();

    assert!(format!("{error:#}").contains("Codex app-server connection ended before response"));
    assert_eq!(cache["effort"], "medium");
}

#[test]
fn selection_persistence_requires_the_confirmed_thread_and_generation() {
    let current_thread = Mutex::new("thread-current".to_owned());
    let generation = AtomicU64::new(5);
    let previous = selection("provider-old", "model-old");
    let next = selection("provider-next", "model-next");
    let selected = std::cell::RefCell::new(previous.clone());
    let mut saves = 0;

    let saved = persist_model_selection_for_target(
        &current_thread,
        &generation,
        "thread-current",
        5,
        || {
            assert!(current_thread.try_lock().is_err());
            saves += 1;
            *selected.borrow_mut() = next.clone();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(saved, Some(()));
    assert_eq!(selected.borrow().model, "model-next");

    *current_thread.lock().unwrap() = "thread-forked".to_owned();
    let stale_thread = persist_model_selection_for_target(
        &current_thread,
        &generation,
        "thread-current",
        5,
        || {
            saves += 1;
            *selected.borrow_mut() = previous.clone();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(stale_thread, None);

    generation.store(6, Ordering::Release);
    let stale_generation = persist_model_selection_for_target(
        &current_thread,
        &generation,
        "thread-forked",
        5,
        || {
            saves += 1;
            *selected.borrow_mut() = previous.clone();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(stale_generation, None);
    assert_eq!(saves, 1);
    assert_eq!(selected.borrow().model, "model-next");
}

fn selection(provider: &str, model: &str) -> ModelSelection {
    ModelSelection {
        executor: crate::model_catalog::ModelExecutor::Codex,
        provider: provider.to_owned(),
        model: model.to_owned(),
    }
}

#[test]
fn saved_external_selection_requires_profile_opt_in_without_mutating_request() {
    let saved: ModelSelection = serde_json::from_value(json!({
        "executor":"antigravity", "provider":"antigravity", "model":"example-agy"
    }))
    .unwrap();
    let mut preferences = NativeFeaturePreferences {
        subagent_model: Some("example-child".into()),
        subagent_model_provider: Some("example-provider".into()),
        ..Default::default()
    };
    let original = json!({"threadId":"example-thread","config":{"existingSetting":true}});
    let mut params = original.clone();
    let error =
        prepare_model_params_with_preferences(&mut params, Some(&saved), &preferences).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Antigravity is disabled in this profile; enable it in settings to use this selection"
    );
    assert_eq!(params, original);
    preferences.antigravity_enabled = true;
    let overrides =
        prepare_model_params_with_preferences(&mut params, Some(&saved), &preferences).unwrap();
    assert_eq!(params["model"], "example-agy");
    assert_eq!(params["config"]["model_executor"], "antigravity");
    assert_eq!(params["config"]["existingSetting"], true);
    assert_eq!(
        params["config"]["agents.default_subagent_model"],
        "example-child"
    );
    assert_eq!(
        overrides["agents.default_subagent_model_provider"],
        "example-provider"
    );
    assert!(params.get("modelProvider").is_none());
}

#[test]
fn disabled_external_executor_does_not_block_native_or_inherited_selection() {
    let preferences = NativeFeaturePreferences::default();
    let mut params = json!({"threadId":"example-thread"});
    let selected = selection("example-provider", "example-model");
    prepare_model_params_with_preferences(&mut params, Some(&selected), &preferences).unwrap();
    assert_eq!(params["model"], "example-model");
    assert_eq!(params["modelProvider"], "example-provider");
    assert_eq!(params["config"]["model_executor"], "codex");
    let mut inherited = json!({"threadId":"example-thread"});
    prepare_model_params_with_preferences(&mut inherited, None, &preferences).unwrap();
    assert_eq!(inherited, json!({"threadId":"example-thread"}));
}

#[test]
fn external_selection_preserves_native_provider_and_combines_child_overrides() {
    let selected = ModelSelection {
        executor: ModelExecutor::Antigravity,
        provider: "antigravity".into(),
        model: "example-agy".into(),
    };
    let mut params = json!({"threadId":"thread-1"});
    apply_model_selection(&mut params, &selected);
    merge_config_overrides(
        &mut params,
        &json!({"agents.default_subagent_model":"example-child", "agents.default_subagent_model_provider":"local"}),
    );
    assert!(params.get("modelProvider").is_none());
    assert_eq!(params["config"]["model_executor"], "antigravity");
    assert_eq!(
        params["config"]["agents.default_subagent_model_provider"],
        "local"
    );
    assert!(
        validate_model_selection_response(
            &json!({"model":"example-agy","modelProvider":"openai","modelExecutor":"antigravity"}),
            &selected
        )
        .is_ok()
    );
    assert!(
        validate_model_selection_response(
            &json!({"model":"example-agy","modelProvider":"openai","modelExecutor":"codex"}),
            &selected
        )
        .is_err()
    );
}

fn response(thread_id: &str, provider: &str, model: &str, nested: bool) -> Value {
    if nested {
        json!({"thread":{"id":thread_id,"modelProvider":provider,"model":model,"turns":[]}})
    } else {
        json!({"id":thread_id,"modelProvider":provider,"model":model})
    }
}

fn closing_error() -> anyhow::Error {
    anyhow::Error::new(NativeRequestError::from_response(
        "thread/resume".to_owned(),
        &json!({"code":-32600,"message":"thread thread-1 is closing; retry thread/resume after the thread is closed"}),
    ))
}

#[test]
fn model_selection_params_and_response_validation_support_top_level_and_nested_thread() {
    let selected = selection("provider-local", "model-local");
    let mut params = json!({"threadId":"thread-1"});
    apply_model_selection(&mut params, &selected);
    assert_eq!(
        params,
        json!({"threadId":"thread-1","modelProvider":"provider-local","model":"model-local","config":{"model_executor":"codex"}})
    );
    assert!(
        validate_model_selection_response(
            &response("thread-1", "provider-local", "model-local", false),
            &selected
        )
        .is_ok()
    );
    assert!(
        validate_model_selection_response(
            &response("thread-1", "provider-local", "model-local", true),
            &selected
        )
        .is_ok()
    );
    assert!(
        validate_model_selection_response(
            &response("thread-1", "provider-other", "model-local", false),
            &selected
        )
        .is_err()
    );
    assert!(
        validate_model_selection_response(
            &response("thread-other", "provider-local", "model-local", false),
            &selected
        )
        .is_ok()
    ); // ID is checked by resume_model_thread, before selection validation.
}

#[test]
fn response_with_nested_history_and_top_level_model_settings_is_valid() {
    let selected = selection("provider-local", "model-local");
    let result = json!({
        "thread":{"id":"thread-1","turns":[{"id":"turn-1"}]},
        "modelProvider":"provider-local",
        "model":"model-local"
    });
    assert!(validate_model_selection_response(&result, &selected).is_ok());
    assert_eq!(response_thread_id(&result).unwrap(), "thread-1");
}

#[tokio::test]
async fn wrong_returned_thread_id_rolls_back_to_the_original_thread() {
    let selected = selection("provider-local", "model-local");
    let previous = selection("openai", "model-openai");
    let params =
        json!({"threadId":"thread-1","model":"model-local","modelProvider":"provider-local"});
    let rollback = json!({"threadId":"thread-1","model":"model-openai","modelProvider":"openai"});
    let (_sender, mut events) = broadcast::channel(8);
    let mut resumes = 0usize;
    let mut calls = Vec::new();
    let err = switch_model_thread(
        |method, request_params| {
            calls.push((method.to_owned(), request_params));
            let result = match method {
                "thread/unsubscribe" => Ok(json!({})),
                "thread/resume" => {
                    resumes += 1;
                    if resumes == 1 {
                        Ok(response(
                            "thread-other",
                            "provider-local",
                            "model-local",
                            false,
                        ))
                    } else {
                        Ok(response("thread-1", "openai", "model-openai", false))
                    }
                }
                _ => panic!("unexpected request: {method}"),
            };
            future::ready(result)
        },
        &mut events,
        "thread-1",
        &params,
        &selected,
        &rollback,
        &previous,
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("restored the previous conversation")
    );
    assert_eq!(
        calls
            .iter()
            .map(|(method, _)| method.as_str())
            .collect::<Vec<_>>(),
        vec![
            "thread/unsubscribe",
            "thread/resume",
            "thread/unsubscribe",
            "thread/resume"
        ]
    );
    assert_eq!(calls[3].1["threadId"], "thread-1");
}

#[tokio::test]
async fn switch_keeps_the_thread_id_and_resumes_with_provider_qualified_selection() {
    let selected = selection("provider-local", "model-local");
    let params =
        json!({"threadId":"thread-1","model":"model-local","modelProvider":"provider-local"});
    let rollback = json!({"threadId":"thread-1","model":"model-openai","modelProvider":"openai"});
    let previous = selection("openai", "model-openai");
    let (sender, mut events) = broadcast::channel(8);
    let _keep_sender = sender;
    let mut calls = Vec::new();

    let result = switch_model_thread(
        |method, params| {
            calls.push((method.to_owned(), params.clone()));
            let result = match method {
                "thread/unsubscribe" => Ok(json!({})),
                "thread/resume" => Ok(response("thread-1", "provider-local", "model-local", false)),
                _ => panic!("unexpected request: {method}"),
            };
            future::ready(result)
        },
        &mut events,
        "thread-1",
        &params,
        &selected,
        &rollback,
        &previous,
    )
    .await
    .unwrap();

    assert_eq!(response_thread_id(&result).unwrap(), "thread-1");
    assert_eq!(
        calls
            .iter()
            .map(|(method, _)| method.as_str())
            .collect::<Vec<_>>(),
        vec!["thread/unsubscribe", "thread/resume"]
    );
    assert_eq!(calls[1].1["modelProvider"], "provider-local");
    assert_eq!(calls[1].1["model"], "model-local");
}

#[tokio::test]
async fn mismatched_provider_rolls_back_same_thread_and_reports_restoration() {
    let selected = selection("provider-local", "model-local");
    let previous = selection("openai", "model-openai");
    let params =
        json!({"threadId":"thread-1","model":"model-local","modelProvider":"provider-local"});
    let rollback = json!({"threadId":"thread-1","model":"model-openai","modelProvider":"openai"});
    let (_sender, mut events) = broadcast::channel(8);
    let mut calls = Vec::new();

    let err = switch_model_thread(
        |method, params| {
            calls.push((method.to_owned(), params.clone()));
            let result = match (
                method,
                calls.iter().filter(|(m, _)| m == "thread/resume").count(),
            ) {
                ("thread/unsubscribe", _) => Ok(json!({})),
                ("thread/resume", 1) => {
                    Ok(response("thread-1", "provider-wrong", "model-local", false))
                }
                ("thread/resume", 2) => Ok(response("thread-1", "openai", "model-openai", true)),
                _ => panic!("unexpected request: {method}"),
            };
            future::ready(result)
        },
        &mut events,
        "thread-1",
        &params,
        &selected,
        &rollback,
        &previous,
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("restored the previous conversation")
    );
    assert_eq!(
        calls.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(),
        vec![
            "thread/unsubscribe",
            "thread/resume",
            "thread/unsubscribe",
            "thread/resume"
        ]
    );
    assert_eq!(calls[3].1["modelProvider"], "openai");
    assert_eq!(calls[3].1["model"], "model-openai");
}

#[tokio::test]
async fn selected_provider_errors_preserve_typed_cause_and_selection_across_rollback() {
    for (code, message) in [
        (-32602, "unknown model provider: provider-b"),
        (-32602, "provider-b does not support reasoning_effort=ultra"),
        (-32001, "provider-b authentication failed"),
    ] {
        for rollback_succeeds in [true, false] {
            let selected = selection("provider-b", "model-b");
            let previous = selection("provider-a", "model-a");
            let params = json!({
                "threadId":"thread-example", "model":"model-b", "modelProvider":"provider-b",
                "config":{"model_executor":"codex","model_reasoning_effort":"ultra"}
            });
            let rollback = json!({
                "threadId":"thread-example", "model":"model-a", "modelProvider":"provider-a",
                "config":{"model_executor":"codex","model_reasoning_effort":"high"}
            });
            let before = (
                selected.clone(),
                previous.clone(),
                params.clone(),
                rollback.clone(),
            );
            let (_sender, mut events) = broadcast::channel(8);
            let mut calls = Vec::new();
            let mut resumes = 0;
            let error = switch_model_thread(
                |method, request_params| {
                    calls.push((method.to_owned(), request_params));
                    future::ready(match method {
                        "thread/unsubscribe" => Ok(json!({})),
                        "thread/resume" => {
                            resumes += 1;
                            if resumes == 1 {
                                Err(anyhow::Error::new(NativeRequestError::from_response(
                                    "thread/resume".to_owned(),
                                    &json!({"code":code,"message":message}),
                                )))
                            } else if rollback_succeeds {
                                Ok(response("thread-example", "provider-a", "model-a", false))
                            } else {
                                Err(anyhow::Error::new(NativeRequestError::from_response(
                                    "thread/resume".to_owned(),
                                    &json!({"code":-32002,"message":"provider-a restoration failed"}),
                                )))
                            }
                        }
                        _ => panic!("unexpected request: {method}"),
                    })
                },
                &mut events,
                "thread-example",
                &params,
                &selected,
                &rollback,
                &previous,
            )
            .await
            .unwrap_err();

            let native = error
                .downcast_ref::<NativeRequestError>()
                .expect("restoration context must retain the original typed provider error");
            assert_eq!(native.method, "thread/resume");
            assert_eq!(native.code, Some(code));
            assert_eq!(native.message, message);
            if rollback_succeeds {
                assert_eq!(
                    error.to_string(),
                    "could not switch model/provider; restored the previous conversation"
                );
            } else {
                assert!(
                    error
                        .to_string()
                        .contains("restoring the previous conversation also failed")
                );
            }
            // These are the complete requests: no turn/start or other generation
            // request is introduced when restoring the previous configuration.
            assert_eq!(
                calls,
                vec![
                    (
                        "thread/unsubscribe".to_owned(),
                        json!({"threadId":"thread-example"})
                    ),
                    ("thread/resume".to_owned(), params.clone()),
                    (
                        "thread/unsubscribe".to_owned(),
                        json!({"threadId":"thread-example"})
                    ),
                    ("thread/resume".to_owned(), rollback.clone()),
                ]
            );
            assert_eq!((selected, previous, params, rollback), before);
        }
    }
}

#[tokio::test]
async fn closing_race_skips_unrelated_closed_event_then_retries_after_matching_close() {
    let selected = selection("provider-local", "model-local");
    let previous = selection("openai", "model-openai");
    let params =
        json!({"threadId":"thread-1","model":"model-local","modelProvider":"provider-local"});
    let rollback = json!({"threadId":"thread-1","model":"model-openai","modelProvider":"openai"});
    let (sender, mut events) = broadcast::channel(8);
    sender
        .send(json!({"method":"thread/closed","params":{"threadId":"unrelated"}}))
        .unwrap();
    sender
        .send(json!({"method":"thread/closed","params":{"threadId":"thread-1"}}))
        .unwrap();
    let mut resumes = 0usize;
    let mut calls = Vec::new();
    switch_model_thread(
        |method, params| {
            calls.push((method.to_owned(), params));
            let result = match method {
                "thread/unsubscribe" => Ok(json!({})),
                "thread/resume" => {
                    resumes += 1;
                    if resumes == 1 {
                        Err(closing_error())
                    } else {
                        Ok(response("thread-1", "provider-local", "model-local", false))
                    }
                }
                _ => panic!("unexpected request"),
            };
            future::ready(result)
        },
        &mut events,
        "thread-1",
        &params,
        &selected,
        &rollback,
        &previous,
    )
    .await
    .unwrap();
    assert_eq!(resumes, 2);
    assert_eq!(
        calls.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(),
        vec!["thread/unsubscribe", "thread/resume", "thread/resume"]
    );
}

#[tokio::test]
async fn connection_close_while_waiting_for_unload_is_reported() {
    let selected = selection("provider-local", "model-local");
    let (sender, mut events) = broadcast::channel(8);
    sender
        .send(json!({"method":"discodex/connection-closed","params":{}}))
        .unwrap();
    let mut calls = 0;
    let err = resume_model_thread(
        &mut |_, _| {
            calls += 1;
            future::ready(Err(closing_error()))
        },
        &mut events,
        "thread-1",
        &json!({"threadId":"thread-1"}),
        &selected,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("connection closed while waiting"));
    assert_eq!(calls, 1);
}
