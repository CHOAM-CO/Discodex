use super::*;
use std::future;

fn selection(provider: &str, model: &str) -> ModelSelection {
    ModelSelection {
        provider: provider.to_owned(),
        model: model.to_owned(),
    }
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
        json!({"threadId":"thread-1","modelProvider":"provider-local","model":"model-local"})
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
async fn failed_switch_request_rolls_back_and_reports_failed_rollback() {
    let selected = selection("provider-local", "model-local");
    let previous = selection("openai", "model-openai");
    let params =
        json!({"threadId":"thread-1","model":"model-local","modelProvider":"provider-local"});
    let rollback = json!({"threadId":"thread-1","model":"model-openai","modelProvider":"openai"});
    let (_sender, mut events) = broadcast::channel(8);
    let mut resumes = 0usize;
    let mut calls = Vec::new();
    let err = switch_model_thread(
        |method, params| {
            calls.push((method.to_owned(), params));
            let result = match method {
                "thread/unsubscribe" => Ok(json!({})),
                "thread/resume" => {
                    resumes += 1;
                    if resumes == 1 {
                        Err(anyhow!("requested provider unavailable"))
                    } else {
                        Err(anyhow!("rollback unavailable"))
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
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("restoring the previous conversation also failed")
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
