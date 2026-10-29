//! Adapter for native external turns. Codex owns history, tools, and child tasks;
//! this module only runs the selected CLI and forwards its actual events.
use super::*;
use crate::agy_mcp::NativeRunBinding;
use crate::state::{AgyContextRange, AgyContextReference, AgyRunAttempt, StateStore};
use base64::Engine;
use std::io::Write;

pub(super) struct ActiveTurn {
    pub binding: NativeRunBinding,
    pub cancel: watch::Sender<bool>,
    pub task: Option<JoinHandle<()>>,
    recorded_calls: Vec<Value>,
}

pub(super) fn record_native_call(inner: &Inner, binding: &NativeRunBinding, receipt: Value) {
    if let Ok(mut turns) = inner.external_turns.lock()
        && let Some(turn) = turns.get_mut(&binding.thread_id)
        && turn.binding == *binding
    {
        turn.recorded_calls.push(receipt);
    }
}

fn consume_recorded_call(receipts: &mut Vec<Value>, event: &Value) -> bool {
    if !event.get("output").is_some() || event.get("error").is_some_and(|error| !error.is_null()) {
        return false;
    }
    let wrapped = event
        .get("arguments")
        .filter(|arguments| {
            event.get("name").and_then(Value::as_str) == Some("call_mcp_tool")
                && arguments.get("ServerName").and_then(Value::as_str)
                    == Some(crate::agy_mcp::SERVER_NAME)
        })
        .and_then(|arguments| {
            let name = arguments.get("ToolName")?.as_str()?;
            let args = arguments.get("Arguments")?;
            let args = match args.as_str() {
                Some(text) => serde_json::from_str::<Value>(text).ok()?,
                None => args.clone(),
            };
            Some(json!({"name":name,"arguments":args}))
        });
    let Some(event) = wrapped.as_ref() else {
        return false;
    };
    let matching = receipts.iter().position(|receipt| {
        receipt.get("name") == event.get("name")
            && receipt.get("arguments") == event.get("arguments")
    });
    if let Some(index) = matching {
        receipts.remove(index);
        true
    } else {
        false
    }
}

fn is_native_call_echo(inner: &Inner, binding: &NativeRunBinding, event: &Value) -> bool {
    inner
        .external_turns
        .lock()
        .ok()
        .and_then(|mut turns| {
            turns
                .get_mut(&binding.thread_id)
                .filter(|turn| turn.binding == *binding)
                .map(|turn| consume_recorded_call(&mut turn.recorded_calls, event))
        })
        .unwrap_or(false)
}

pub(super) fn active_turn_id(inner: &Inner, thread_id: &str) -> Result<String> {
    inner
        .external_turns
        .lock()
        .map_err(|_| anyhow!("external turn lock poisoned"))?
        .get(thread_id)
        .map(|turn| turn.binding.turn_id.clone())
        .context("thread has no active external turn")
}

pub(super) fn cancel_all(inner: &Inner, generation: Option<u64>) {
    if let Ok(turns) = inner.external_turns.lock() {
        for turn in turns.values() {
            if generation.is_none_or(|generation| generation == turn.binding.generation) {
                let _ = turn.cancel.send(true);
            }
        }
    }
}

pub(super) fn observe_completion(inner: &Inner, event: &Value) {
    if event.get("method").and_then(Value::as_str) != Some("turn/completed") {
        return;
    }
    let Some(thread) = event.pointer("/params/threadId").and_then(Value::as_str) else {
        return;
    };
    let turn_id = event.pointer("/params/turn/id").and_then(Value::as_str);
    if let Ok(turns) = inner.external_turns.lock()
        && let Some(turn) = turns.get(thread)
        && turn_id == Some(turn.binding.turn_id.as_str())
    {
        let _ = turn.cancel.send(true);
    }
}

pub(super) fn spawn(inner: Arc<Inner>, request: Value, generation: u64) -> Result<()> {
    let params = request
        .get("params")
        .context("external request omitted params")?;
    let thread_id = required_string(params, "threadId")?.to_owned();
    let binding = NativeRunBinding {
        thread_id: thread_id.clone(),
        turn_id: required_string(params, "turnId")?.to_owned(),
        run_id: required_string(params, "runId")?.to_owned(),
        generation,
    };
    let (cancel, receiver) = watch::channel(false);
    {
        let mut turns = inner
            .external_turns
            .lock()
            .map_err(|_| anyhow!("external turn lock poisoned"))?;
        if turns.contains_key(&thread_id) {
            bail!(
                "{}",
                inner
                    .config
                    .language
                    .message(crate::i18n::MessageKey::NativeExternalRequestAlreadyActive)
            );
        }
        turns.insert(
            thread_id.clone(),
            ActiveTurn {
                binding: binding.clone(),
                cancel,
                task: None,
                recorded_calls: Vec::new(),
            },
        );
    }
    let task_inner = inner.clone();
    let task_binding = binding.clone();
    let task = tokio::spawn(async move {
        let native = Native {
            inner: task_inner.clone(),
        };
        let mut result = run(&native, &request["params"], &task_binding, receiver).await;
        let store = StateStore::new(&native.inner.config.state_dir);
        // Keep this completed run's references locally until its response write
        // finishes. A successor may replace the persisted slot after we remove
        // the active binding, including while an old-generation write fails.
        let completed_state = match store.agy_thread(&task_binding.thread_id) {
            Ok(saved) => Some(saved),
            Err(error) => {
                result = combine_turn_and_cleanup(result, Err(error), native.inner.config.language);
                None
            }
        };
        // Drop the active binding before completing the native request. The core
        // can immediately issue another request for queued input in the same turn.
        if let Ok(mut turns) = task_inner.external_turns.lock() {
            remove_run(&mut turns, &task_binding);
        }
        let response = run_response(result, native.inner.config.language);
        let delivery = native
            .respond_for_generation(generation, request["id"].clone(), response)
            .await;
        let persisted = store.update_agy_thread(&task_binding.thread_id, |saved| {
            if let Some(attempt) = saved
                .current_run
                .as_mut()
                .filter(|attempt| attempt.binding == task_binding)
            {
                match &delivery {
                    Ok(()) => saved.current_run = None,
                    Err(error) => {
                        let error = crate::diagnostics::sanitize_diagnostic(&format!("{error:#}"));
                        let previous = attempt.diagnostic.take().unwrap_or_default();
                        attempt.diagnostic = Some(if previous.is_empty() {
                            error
                        } else {
                            format!("{previous}\n{error}")
                        });
                    }
                }
            }
            Ok(())
        });
        if let Err(error) = &delivery {
            tracing::warn!(
                %error,
                thread_id = %task_binding.thread_id,
                turn_id = %task_binding.turn_id,
                run_id = %task_binding.run_id,
                generation = task_binding.generation,
                recovery = %json!(completed_state),
                "could not finish native external request"
            );
        }
        if let Err(error) = persisted {
            tracing::warn!(%error, "could not update external request notification state");
        }
    });
    if let Ok(mut turns) = inner.external_turns.lock()
        && let Some(turn) = turns.get_mut(&thread_id)
        && turn.binding == binding
    {
        turn.task = Some(task);
    }
    Ok(())
}

fn validate_run(
    turns: &HashMap<String, ActiveTurn>,
    binding: &NativeRunBinding,
    generation: u64,
) -> Result<()> {
    let active = turns
        .get(&binding.thread_id)
        .context("external response is no longer active")?;
    if active.binding != *binding || binding.generation != generation || *active.cancel.borrow() {
        bail!("external response is no longer active");
    }
    Ok(())
}

pub(super) fn validate_binding(inner: &Inner, binding: &NativeRunBinding) -> Result<()> {
    let turns = inner
        .external_turns
        .lock()
        .map_err(|_| anyhow!("external turn lock poisoned"))?;
    validate_run(
        &turns,
        binding,
        inner.connection_generation.load(Ordering::Acquire),
    )
    .map_err(|error| localized_response_validation_error(inner.config.language, error))
}

fn localized_response_validation_error(
    language: crate::i18n::Language,
    error: impl std::fmt::Display,
) -> anyhow::Error {
    anyhow!(
        "{}: {error:#}",
        language.message(crate::i18n::MessageKey::NativeExternalResponseInactive)
    )
}

pub(super) fn take_binding(params: &mut Value) -> Result<Option<NativeRunBinding>> {
    params
        .as_object_mut()
        .and_then(|params| params.remove("discodexExternalRun"))
        .map(serde_json::from_value)
        .transpose()
        .context("parse external response binding")
}

fn remove_run(turns: &mut HashMap<String, ActiveTurn>, binding: &NativeRunBinding) {
    if turns
        .get(&binding.thread_id)
        .is_some_and(|turn| turn.binding == *binding)
    {
        turns.remove(&binding.thread_id);
    }
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("external request omitted {field}"))
}

fn run_response(result: Result<()>, language: crate::i18n::Language) -> Value {
    match result {
        Ok(()) => json!({"success":true,"error":null}),
        Err(error) => {
            let detail = crate::diagnostics::sanitize_diagnostic(&format!("{error:#}"));
            let message = language
                .message(crate::i18n::MessageKey::NativeExternalExecutionFailed)
                .replace("{detail}", &detail);
            json!({"success":false,"error":message})
        }
    }
}

fn combine_turn_and_cleanup(
    turn: Result<()>,
    cleanup: Result<()>,
    language: crate::i18n::Language,
) -> Result<()> {
    match (turn, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(turn_error), Ok(())) => Err(turn_error),
        (Ok(()), Err(cleanup_error)) => Err(cleanup_error)
            .context(language.message(crate::i18n::MessageKey::DiagnosticsAgyCleanupStopFailed)),
        (Err(turn_error), Err(cleanup_error)) => {
            let cleanup_detail =
                crate::diagnostics::sanitize_diagnostic(&format!("{cleanup_error:#}"));
            let summary = language
                .message(crate::i18n::MessageKey::DiagnosticsAgyCleanupAlsoFailed)
                .replace("{error}", &cleanup_detail);
            Err(turn_error).context(CleanupFailureContext {
                cleanup: cleanup_error,
                summary,
            })
        }
    }
}

struct CleanupFailureContext {
    cleanup: anyhow::Error,
    summary: String,
}

impl std::fmt::Debug for CleanupFailureContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CleanupFailureContext")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for CleanupFailureContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.summary)
    }
}

fn validate_admission(
    preferences: &NativeFeaturePreferences,
    language: crate::i18n::Language,
) -> Result<()> {
    if !preferences.antigravity_enabled {
        bail!(
            "{}",
            language.message(crate::i18n::MessageKey::NativeExternalProfileDisabled)
        );
    }
    Ok(())
}

fn terminal_response_fallback<'a>(streamed: &str, response: Option<&'a str>) -> Option<&'a str> {
    let response = response?;
    let remaining = response.strip_prefix(streamed).unwrap_or(response);
    (!remaining.is_empty()).then_some(remaining)
}

fn terminal_result(
    status: Option<&str>,
    error: Option<&str>,
    language: crate::i18n::Language,
) -> Result<()> {
    if status == Some("SUCCESS") {
        return Ok(());
    }
    let message = language
        .message(crate::i18n::MessageKey::NativeExternalTerminalFailure)
        .replace("{status}", status.unwrap_or("missing status"))
        .replace("{detail}", error.unwrap_or("no error detail"));
    bail!("{message}");
}

struct Prompt {
    text: String,
    // AGY's documented stream protocol accepts text, while its local tools can
    // inspect files. Keep downloaded inline images alive for the entire turn.
    _files: Vec<tempfile::NamedTempFile>,
}

fn decode_inline_image(url: &str) -> Result<(&'static str, Vec<u8>)> {
    let (header, data) = url
        .split_once(',')
        .context("inline image has no data separator")?;
    let bytes = if header.ends_with(";base64") {
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .context("decode inline image")?
    } else {
        percent_encoding::percent_decode_str(data).collect()
    };
    let suffix = match header.split(';').next() {
        Some("data:image/jpeg") => ".jpg",
        Some("data:image/png") => ".png",
        Some("data:image/webp") => ".webp",
        Some("data:image/gif") => ".gif",
        _ => ".img",
    };
    Ok((suffix, bytes))
}

fn prepare_prompt(params: &Value, language: crate::i18n::Language) -> Result<Prompt> {
    let mut files = Vec::new();
    let text = prepare_prompt_with_stager(params, language, |suffix, bytes| {
        let mut file = tempfile::Builder::new()
            .prefix("discodex-attachment-")
            .suffix(suffix)
            .tempfile()?;
        file.write_all(bytes)?;
        let path = file.path().display().to_string();
        files.push(file);
        Ok(path)
    })?;
    Ok(Prompt {
        text,
        _files: files,
    })
}

fn prepare_prompt_with_stager(
    params: &Value,
    language: crate::i18n::Language,
    mut stage_image: impl FnMut(&str, &[u8]) -> Result<String>,
) -> Result<String> {
    let mut input = params
        .get("input")
        .cloned()
        .context("external request omitted input")?;
    let items = input
        .as_array_mut()
        .context("external input is not an array")?;
    for item in items.iter_mut() {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(contents) = item.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for content in contents {
            if content.get("type").and_then(Value::as_str) != Some("input_image") {
                continue;
            }
            let Some(url) = content.get("image_url").and_then(Value::as_str) else {
                continue;
            };
            if !url.starts_with("data:") {
                continue;
            }
            let (suffix, bytes) = decode_inline_image(url)?;
            let path = stage_image(suffix, &bytes)?;
            let label = language
                .message(crate::i18n::MessageKey::NativeExternalAttachedImageFile)
                .replace("{path}", &path);
            *content = json!({"type":"input_text", "text":label});
        }
    }
    let updates = params.get("contextUpdates").and_then(Value::as_array);
    let has_updates = updates.is_some_and(|updates| !updates.is_empty());
    if !has_updates
        && items.len() == 1
        && items[0].get("type").and_then(Value::as_str) == Some("message")
        && items[0].get("role").and_then(Value::as_str) == Some("user")
        && let Some(content) = items[0].get("content").and_then(Value::as_array)
        && content.iter().all(|block| {
            block.get("type").and_then(Value::as_str) == Some("input_text")
                && block.get("text").is_some_and(Value::is_string)
        })
    {
        return Ok(content
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let mut parts = Vec::new();
    if let Some(updates) = updates.filter(|updates| !updates.is_empty()) {
        parts.push(format!(
            "{}\n{}",
            language.message(crate::i18n::MessageKey::NativeExternalContextUpdates),
            serde_json::to_string(updates)?
        ));
    }
    parts.push(format!(
        "{}\n{}",
        language.message(crate::i18n::MessageKey::NativeExternalNewInput),
        serde_json::to_string(&input)?
    ));
    Ok(parts.join("\n\n"))
}

fn run_instructions(
    params: &Value,
    state: &crate::state::NativeState,
    language: crate::i18n::Language,
) -> Result<String> {
    let instructions = required_string(params, "instructions")?;
    let references = json!({
        "currentThreadId":required_string(params, "threadId")?,
        "parentThreadId":params.get("parentThreadId").and_then(Value::as_str),
        "previousThreadIds":state.previous_thread_ids,
    });
    Ok(format!(
        "{instructions}\n\n{}",
        language
            .message(crate::i18n::MessageKey::NativeExternalThreadReferences)
            .replace("{references}", &references.to_string())
    ))
}

fn pending_context(params: &Value, thread_id: &str, frontier: Option<u64>) -> Result<Vec<Value>> {
    let updates = match params.get("contextUpdates") {
        None => return Ok(Vec::new()),
        Some(updates) => updates
            .as_array()
            .context("external contextUpdates is not an array")?,
    };
    let mut selected = Vec::new();
    for update in updates {
        let reference: AgyContextReference = serde_json::from_value(update.clone())
            .context("parse typed external context update reference")?;
        if reference.thread_id == thread_id && frontier.is_none_or(|order| reference.order > order)
        {
            anyhow::ensure!(
                update.get("item").is_some(),
                "external context update omitted item"
            );
            selected.push(update.clone());
        }
    }
    selected.sort_by_key(|update| update.get("order").and_then(Value::as_u64));
    Ok(selected)
}

fn agy_invocation(
    params: &Value,
    catalog: Option<&crate::agy::Catalog>,
) -> Result<crate::agy_model_catalog::Invocation> {
    let selected_model = required_string(params, "model")?;
    let selected_effort = params.get("effort").and_then(Value::as_str);
    Ok(crate::agy_model_catalog::invocation(
        catalog,
        selected_model,
        selected_effort,
    ))
}

fn completed_tool_event(event: &crate::agy::Event) -> Option<Value> {
    let crate::agy::Event::ToolStep {
        conversation_id,
        step_index,
        state,
        tool_name,
        tool_info,
        ..
    } = event
    else {
        return None;
    };
    if state.as_deref() != Some("DONE") {
        return None;
    }
    let name = tool_name
        .as_deref()
        .or_else(|| tool_info.as_ref()?.get("name")?.as_str())?;
    let info = tool_info.as_ref();
    let call_id = info
        .and_then(|info| info.get("call_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "agy-{}-{}",
                conversation_id.as_deref().unwrap_or("turn"),
                step_index.unwrap_or_default()
            )
        });
    let mut event = json!({"type":"tool", "callId":call_id, "name":name,
        "arguments":info.and_then(|info| info.get("parameters")).cloned().unwrap_or_else(|| json!({}))});
    for key in ["output", "error"] {
        if let Some(value) = info.and_then(|info| info.get(key)) {
            event[key] = value.clone();
        }
    }
    if let Some(state) = state {
        event["status"] = json!(state);
    }
    Some(event)
}

fn publish_diagnostic(inner: &Inner, binding: &NativeRunBinding, message: String) -> Result<()> {
    let event = json!({"method":"warning","params":{
        "threadId":binding.thread_id,"turnId":binding.turn_id,"runId":binding.run_id,
        "_discodex_connection_generation":inner.connection_generation.load(Ordering::Acquire),"message":message
    }});
    // Preserve the event in the existing bridge buffer before clearing recovery
    // state. Discord delivery and its bounded retries consume this same buffer.
    inner
        .bridge_events
        .lock()
        .map_err(|_| anyhow!("native bridge event buffer lock poisoned"))?
        .publish(event.clone());
    if inner.live_events.load(Ordering::Acquire) {
        let _ = inner.events.send(event);
    } else {
        inner
            .startup_events
            .lock()
            .map_err(|_| anyhow!("native startup event lock poisoned"))?
            .push(event);
    }
    Ok(())
}

pub(super) fn recover_pending(inner: &Inner) -> Result<()> {
    let store = StateStore::new(&inner.config.state_dir);
    let state = store.load()?;
    for (thread_id, saved) in state.agy_threads {
        let Some(attempt) = saved.current_run.as_ref() else {
            continue;
        };
        let active = inner
            .external_turns
            .lock()
            .map_err(|_| anyhow!("external turn lock poisoned"))?
            .get(&thread_id)
            .is_some_and(|active| active.binding == attempt.binding);
        if active {
            continue;
        }
        recover_attempt(inner, &store, &saved, attempt, false)?;
    }
    Ok(())
}

fn recover_attempt(
    inner: &Inner,
    store: &StateStore,
    saved: &crate::state::AgyThreadState,
    attempt: &AgyRunAttempt,
    completed_successor: bool,
) -> Result<()> {
    if !completed_successor {
        let detail = json!({"threadId":attempt.binding.thread_id,"conversationId":saved.conversation_id,
            "turnId":attempt.binding.turn_id,"runId":attempt.binding.run_id,
            "generation":attempt.binding.generation,"contextRange":attempt.context_range,"outcome":attempt.outcome,
            "terminalStatus":attempt.terminal_status,"exitStatus":attempt.exit_status,"error":attempt.diagnostic});
        let message = inner
            .config
            .language
            .message(crate::i18n::MessageKey::NativeExternalRecoveredAttempt)
            .replace("{detail}", &detail.to_string());
        tracing::warn!(%message, "recovering previous AGY run diagnostic");
        // If buffering fails, leave the old attempt intact and return the real
        // error; the new run must not silently overwrite its recovery references.
        publish_diagnostic(inner, &attempt.binding, message)?;
    }
    store.update_agy_thread(&attempt.binding.thread_id, |saved| {
        if saved
            .current_run
            .as_ref()
            .is_some_and(|current| current.binding == attempt.binding)
        {
            saved.current_run = None;
        }
        Ok(())
    })
}

fn recover_before_successor(
    inner: &Inner,
    store: &StateStore,
    binding: &NativeRunBinding,
) -> Result<()> {
    let saved = store.agy_thread(&binding.thread_id)?;
    let Some(attempt) = saved.current_run.as_ref() else {
        return Ok(());
    };
    if attempt.binding == *binding {
        return Ok(());
    }
    // spawn admitted this binding only after the previous one became inactive.
    // In the same connection, a serialized successor also proves that core has
    // received the preceding completion. Do not report its confirmed terminal
    // result and process exit as an uncertain interruption during a write race.
    let completed_successor = attempt.binding.generation == binding.generation
        && matches!(attempt.outcome.as_str(), "success" | "failed")
        && attempt.terminal_status.is_some()
        && attempt.exit_status.is_some();
    recover_attempt(inner, store, &saved, attempt, completed_successor)
}

async fn run(
    native: &Native,
    params: &Value,
    binding: &NativeRunBinding,
    mut canceled: watch::Receiver<bool>,
) -> Result<()> {
    let language = native.inner.config.language;
    if required_string(params, "executor")?
        != crate::model_catalog::ModelExecutor::Antigravity.as_str()
    {
        bail!("unsupported external executor");
    }
    let preferences = native.read_native_feature_preferences()?;
    validate_admission(&preferences, language)?;
    let thread_id = required_string(params, "threadId")?;
    let turn_id = required_string(params, "turnId")?;
    let run_id = required_string(params, "runId")?;
    let cwd = Path::new(required_string(params, "cwd")?);
    let share = preferences
        .antigravity_share_settings
        .context("choose whether to share existing AGY settings in Discodex settings")?;
    crate::agy_settings::prepare(&native.inner.config, share)?;
    crate::agy_mcp::remove_workspace_registration(cwd)?;
    let store = StateStore::new(&native.inner.config.state_dir);
    recover_before_successor(&native.inner, &store, binding)?;
    let state = store.load()?;
    let saved = state
        .agy_threads
        .get(thread_id)
        .cloned()
        .unwrap_or_default();
    let updates = pending_context(params, thread_id, saved.attempted_tail_order)?;
    let references = updates
        .iter()
        .cloned()
        .map(serde_json::from_value::<AgyContextReference>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let context_range = references
        .first()
        .zip(references.last())
        .map(|(first, last)| AgyContextRange {
            first: first.clone(),
            last: last.clone(),
        });
    let mut prompt_params = params.clone();
    prompt_params["contextUpdates"] = json!(updates);
    let prompt = prepare_prompt(&prompt_params, language)?;
    let instructions =
        crate::agy_hook::stage_instructions(binding, &run_instructions(params, &state, language)?)?;
    let catalog = match native.cached_agy_catalog().await {
        Ok(lookup) => {
            if let Some(warning) = lookup.warning {
                tracing::warn!(%warning, "AGY model catalog unavailable or stale for external turn");
            }
            lookup.catalog
        }
        Err(error) => {
            tracing::warn!(%error, "could not read AGY model catalog for external turn");
            None
        }
    };
    let invocation = agy_invocation(params, catalog.as_ref())?;
    let context = crate::agy::McpSessionContext {
        endpoint: native.voice_tool_endpoint(),
        binding: binding.clone(),
    };
    let runtime = crate::agy::Runtime::for_preferences(&native.inner.config, &preferences)?;
    let mapping_store = store.clone();
    let mapping_thread = thread_id.to_owned();
    let mut session = crate::agy::Session::spawn_with_run_context(
        &runtime,
        cwd,
        Some(&invocation.model),
        invocation.effort.as_deref(),
        &context,
        saved.conversation_id.as_deref().filter(|id| !id.is_empty()),
        instructions,
        Box::new(move |conversation_id| {
            // Official startup failures can report an empty ID when no
            // conversation was created. Keep any existing actual mapping.
            if conversation_id.is_empty() {
                return Ok(());
            }
            mapping_store
                .update_agy_thread(&mapping_thread, |saved| {
                    saved.conversation_id = Some(conversation_id.to_owned());
                    Ok(())
                })
                .context(language.message(crate::i18n::MessageKey::AgyProtocolInitIdPersist))
        }),
    )
    .await?;
    let mut terminal_observed = false;
    let mut terminal_status: Option<String> = None;
    let mut public_output = false;
    let outcome = async {
        if *canceled.borrow() { bail!("{}", language.message(crate::i18n::MessageKey::NativeExternalTurnInterrupted)); }
        // Advance only the attempted frontier, before the first write can partly
        // reach AGY. Later success never confirms an older uncertain range.
        store.update_agy_thread(thread_id, |saved| {
            if let Some(order) = references.iter().map(|reference| reference.order).max() {
                saved.attempted_tail_order = Some(saved.attempted_tail_order.map_or(order, |frontier| frontier.max(order)));
            }
            saved.current_run = Some(AgyRunAttempt {
                binding:binding.clone(), context_range:context_range.clone(), outcome:"attempted".to_owned(),
                terminal_status:None, exit_status:None, diagnostic:None,
            });
            Ok(())
        })?;
        tokio::select! {
            _ = canceled.changed() => bail!("{}", language.message(crate::i18n::MessageKey::NativeExternalTurnInterrupted)),
            result = session.send_text(&prompt.text) => result?,
        }
        let mut streamed = String::new();
        loop {
            let event = tokio::select! {
                _ = canceled.changed() => bail!("{}", language.message(crate::i18n::MessageKey::NativeExternalTurnInterrupted)),
                event = session.next_event() => event.context(language.message(crate::i18n::MessageKey::NativeExternalCliExitedEarly))??,
            };
            if let Some(mut tool) = completed_tool_event(&event) {
                if let crate::agy::Event::ToolStep { raw, .. } = &event { tool["source"] = raw.clone(); }
                if !is_native_call_echo(&native.inner, binding, &tool) {
                    native.request_raw("turn/external/event", json!({"threadId":thread_id,"turnId":turn_id,"runId":run_id,"event":tool})).await?;
                }
            } else if let crate::agy::Event::ToolStep { conversation_id, step_index, state, .. } = &event
                && state.as_deref() == Some("DONE") {
                let detail = json!({"conversationId":conversation_id,"stepIndex":step_index,"state":state});
                publish_diagnostic(&native.inner, binding, language.message(crate::i18n::MessageKey::NativeExternalUnnamedTool)
                    .replace("{detail}", &detail.to_string()))?;
            }
            if let Some(text) = event.text_delta().filter(|text| !text.is_empty()) {
                public_output = true;
                streamed.push_str(text);
                native.request_raw("turn/external/event", json!({"threadId":thread_id,"turnId":turn_id,"runId":run_id,"event":{"type":"textDelta","delta":text}})).await?;
            }
            match event {
                crate::agy::Event::Init { conversation_id:Some(conversation_id), .. } => {
                    if !conversation_id.is_empty() {
                        store.update_agy_thread(thread_id, |saved| { saved.conversation_id = Some(conversation_id); Ok(()) })?;
                    }
                }
                crate::agy::Event::Init { conversation_id:None, .. } => {
                    bail!("{}", language.message(crate::i18n::MessageKey::NativeExternalInitIdMissing));
                }
                crate::agy::Event::Result { conversation_id, status, response, error, result } => {
                    terminal_observed = true;
                    terminal_status = status.clone();
                    if let Some(conversation_id) = conversation_id.filter(|id| !id.is_empty()) {
                        store.update_agy_thread(thread_id, |saved| { saved.conversation_id = Some(conversation_id); Ok(()) })?;
                    }
                    tracing::info!(status = status.as_deref(), response_bytes = response.as_deref().map(str::len), error_present = result.get("error").is_some(), "AGY turn completed");
                    if let Some(text) = terminal_response_fallback(&streamed, response.as_deref()) {
                        public_output = true;
                        native.request_raw("turn/external/event", json!({"threadId":thread_id,"turnId":turn_id,"runId":run_id,"event":{"type":"textDelta","delta":text}})).await?;
                    }
                    let detail = error.or_else(|| result.get("error").map(Value::to_string));
                    return terminal_result(status.as_deref(), detail.as_deref(), language);
                }
                _ => {}
            }
        }
    }.await;
    // ERROR is a terminal result too. Close stdin and collect its real exit and
    // stderr; only cancellation or protocol/connection failure terminates it.
    let mut exit_status = None;
    let cleanup = async {
        let status = if terminal_observed {
            tokio::select! {
                result = session.finish() => result?,
                _ = canceled.changed() => {
                    let status = session.terminate().await?;
                    exit_status = Some(status.to_string());
                    bail!("{}", language.message(crate::i18n::MessageKey::NativeExternalTurnInterrupted));
                }
            }
        } else {
            session.terminate().await?
        };
        exit_status = Some(status.to_string());
        if terminal_observed && !status.success() {
            bail!("{}", language.message(crate::i18n::MessageKey::NativeExternalCliExitFailure)
                .replace("{status}", &status.to_string()));
        }
        Ok(())
    }.await;
    if exit_status.is_none() {
        exit_status = session
            .observed_exit_status()
            .map(|status| status.to_string());
    }
    let diagnostics = session.take_diagnostics().await;
    let mut result = combine_turn_and_cleanup(outcome, cleanup, language);
    if !diagnostics.trim().is_empty() {
        let message = language
            .message(crate::i18n::MessageKey::NativeExternalCliDiagnostics)
            .replace("{detail}", diagnostics.trim_end());
        if let Err(error) = publish_diagnostic(&native.inner, binding, message.clone()) {
            result = combine_turn_and_cleanup(result, Err(error), language);
        }
        if result.is_err() {
            result = result.context(message);
        }
    }
    if !public_output {
        let detail =
            json!({"status":terminal_status,"exitStatus":exit_status,"publicOutput":false});
        if let Err(error) = publish_diagnostic(
            &native.inner,
            binding,
            language
                .message(crate::i18n::MessageKey::NativeExternalNoPublicOutput)
                .replace("{detail}", &detail.to_string()),
        ) {
            result = combine_turn_and_cleanup(result, Err(error), language);
        }
    }
    let diagnostic = result
        .as_ref()
        .err()
        .map(|error| crate::diagnostics::sanitize_diagnostic(&format!("{error:#}")));
    let persisted = store.update_agy_thread(thread_id, |saved| {
        if let Some(attempt) = saved
            .current_run
            .as_mut()
            .filter(|attempt| attempt.binding == *binding)
        {
            attempt.outcome = if result.is_ok() {
                "success"
            } else if terminal_observed {
                "failed"
            } else {
                "uncertain"
            }
            .to_owned();
            attempt.terminal_status = terminal_status;
            attempt.exit_status = exit_status;
            attempt.diagnostic = diagnostic;
        }
        Ok(())
    });
    result = combine_turn_and_cleanup(result, persisted, language);
    if result.is_err() {
        let conversation_id = match store.agy_thread(thread_id) {
            Ok(saved) => saved.conversation_id,
            Err(error) => {
                result = combine_turn_and_cleanup(result, Err(error), language);
                None
            }
        };
        let refs = json!({"threadId":thread_id,"turnId":turn_id,"runId":run_id,
            "conversationId":conversation_id,"contextRange":context_range});
        result = result.context(
            language
                .message(crate::i18n::MessageKey::NativeExternalAttemptReferences)
                .replace("{references}", &refs.to_string()),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn localized_inactive_response_wrapper_preserves_full_cause_and_code() {
        let cause = anyhow::anyhow!("native rejection code=-32041")
            .context("external response validation failed");
        let wrapped = localized_response_validation_error(crate::i18n::Language::English, cause);
        assert_eq!(
            format!("{wrapped:#}"),
            "The external response is no longer active.: external response validation failed: native rejection code=-32041"
        );
    }

    fn variant_catalog() -> crate::agy::Catalog {
        crate::agy::Catalog {
            models: vec![
                crate::agy::ModelOption {
                    id: "example-orbit-high".into(),
                    label: "Example Orbit (High)".into(),
                },
                crate::agy::ModelOption {
                    id: "example-quartz-low".into(),
                    label: "Example Quartz (Low)".into(),
                },
            ],
            efforts: vec!["low".into(), "high".into()],
        }
    }

    #[test]
    fn external_params_resolve_model_variant_and_codex_ultra_at_cli_boundary() {
        let catalog = variant_catalog();
        let runtime = crate::agy::Runtime {
            executable: "fixture-agy".into(),
            arguments: vec!["--existing-arg".into()],
            environment: Default::default(),
        };
        let context = crate::agy::McpSessionContext {
            endpoint: "fixture-endpoint".into(),
            binding: NativeRunBinding {
                thread_id: "thread".into(),
                turn_id: "turn".into(),
                run_id: "run".into(),
                generation: 1,
            },
        };

        for (selected_effort, expected_model, expected_effort) in [
            ("high", "example-orbit", Some("high")),
            ("ultra", "example-orbit", Some("high")),
        ] {
            let params = json!({"model":"example-orbit-high", "effort":selected_effort});
            let invocation = agy_invocation(&params, Some(&catalog)).unwrap();
            assert_eq!(invocation.model, expected_model);
            assert_eq!(invocation.effort.as_deref(), expected_effort);
            let command = crate::agy::session_command(
                &runtime,
                Path::new("."),
                Some(&invocation.model),
                invocation.effort.as_deref(),
                Some(&context),
            );
            let args = command
                .as_std()
                .get_args()
                .map(OsStr::to_owned)
                .collect::<Vec<_>>();
            assert!(
                args.windows(2)
                    .any(|pair| pair == [OsStr::new("--model"), OsStr::new(expected_model)])
            );
            if let Some(effort) = expected_effort {
                assert!(
                    args.windows(2)
                        .any(|pair| pair == [OsStr::new("--effort"), OsStr::new(effort)])
                );
            } else {
                assert!(!args.iter().any(|arg| arg == OsStr::new("--effort")));
            }
            let envs = command
                .as_std()
                .get_envs()
                .collect::<std::collections::BTreeMap<_, _>>();
            assert!(envs.contains_key(OsStr::new(crate::agy_mcp::NATIVE_ENDPOINT_ENV)));
            assert!(envs.contains_key(OsStr::new(crate::agy_mcp::NATIVE_RUN_ENV)));
        }

        let base_ultra = agy_invocation(
            &json!({"model":"example-orbit", "effort":"ultra"}),
            Some(&catalog),
        )
        .unwrap();
        assert_eq!(base_ultra.model, "example-orbit");
        assert_eq!(base_ultra.effort.as_deref(), Some("high"));

        let no_catalog = agy_invocation(
            &json!({"model":"example-quartz-low", "effort":"high"}),
            None,
        )
        .unwrap();
        assert_eq!(no_catalog.model, "example-quartz-low");
        assert_eq!(no_catalog.effort.as_deref(), Some("high"));
        let no_catalog_command = crate::agy::session_command(
            &runtime,
            Path::new("."),
            Some(&no_catalog.model),
            no_catalog.effort.as_deref(),
            Some(&context),
        );
        let no_catalog_args = no_catalog_command
            .as_std()
            .get_args()
            .map(OsStr::to_owned)
            .collect::<Vec<_>>();
        assert!(
            no_catalog_args
                .windows(2)
                .any(|pair| { pair == [OsStr::new("--model"), OsStr::new("example-quartz-low")] })
        );
        assert!(
            no_catalog_args
                .windows(2)
                .any(|pair| pair == [OsStr::new("--effort"), OsStr::new("high")])
        );
        let no_catalog_ultra = agy_invocation(
            &json!({"model":"example-quartz-low", "effort":"ultra"}),
            None,
        )
        .unwrap();
        assert_eq!(no_catalog_ultra.model, "example-quartz-low");
        assert_eq!(no_catalog_ultra.effort, None);
        let unknown = agy_invocation(
            &json!({"model":"custom-highly-specialized", "effort":"high"}),
            None,
        )
        .unwrap();
        assert_eq!(unknown.model, "custom-highly-specialized");
        assert_eq!(unknown.effort.as_deref(), Some("high"));
    }

    fn active_run(binding: NativeRunBinding) -> ActiveTurn {
        let (cancel, _) = watch::channel(false);
        ActiveTurn {
            binding,
            cancel,
            task: None,
            recorded_calls: vec![],
        }
    }

    #[tokio::test]
    async fn late_calls_cannot_move_to_a_new_response_even_within_the_same_turn() {
        for next_turn in ["turn-1", "turn-2"] {
            let first = NativeRunBinding {
                thread_id: "thread-a".into(),
                turn_id: "turn-1".into(),
                run_id: "response-1".into(),
                generation: 3,
            };
            let second = NativeRunBinding {
                turn_id: next_turn.into(),
                run_id: "response-2".into(),
                ..first.clone()
            };
            let other = NativeRunBinding {
                thread_id: "thread-b".into(),
                turn_id: "other-turn".into(),
                run_id: "other-response".into(),
                generation: 3,
            };
            let mut turns = HashMap::from([
                (first.thread_id.clone(), active_run(first.clone())),
                (other.thread_id.clone(), active_run(other.clone())),
            ]);
            let untouched = json!({"name":"other-call","arguments":{}});
            turns
                .get_mut(&other.thread_id)
                .unwrap()
                .recorded_calls
                .push(untouched.clone());
            validate_run(&turns, &first, 3).unwrap();
            // The frame was prepared during S1, but its send boundary runs after S2 starts.
            turns.insert(second.thread_id.clone(), active_run(second.clone()));
            let sent = Arc::new(Mutex::new(Vec::<Message>::new()));
            let sink = futures_util::sink::unfold(sent.clone(), |sent, message| async move {
                sent.lock().unwrap().push(message);
                Ok::<_, std::io::Error>(sent)
            });
            let writer = AsyncMutex::new(Some(Box::pin(sink)));
            let fence = AtomicU64::new(0);
            let stale =
                json!({"threadId":first.thread_id,"turnId":first.turn_id,"runId":first.run_id});
            assert!(
                send_json_with_commit(&writer, stale, &fence, |_| validate_run(&turns, &first, 3))
                    .await
                    .is_err()
            );
            assert!(sent.lock().unwrap().is_empty());
            remove_run(&mut turns, &first);
            validate_run(&turns, &second, 3).unwrap();
            let current =
                json!({"threadId":second.thread_id,"turnId":second.turn_id,"runId":second.run_id});
            send_json_with_commit(&writer, current.clone(), &fence, |_| {
                validate_run(&turns, &second, 3)
            })
            .await
            .unwrap();
            assert_eq!(
                *sent.lock().unwrap(),
                vec![Message::Text(current.to_string().into())]
            );
            assert!(validate_run(&turns, &second, 4).is_err());
            turns[&second.thread_id].cancel.send_replace(true);
            assert!(validate_run(&turns, &second, 3).is_err());
            assert_eq!(turns[&other.thread_id].binding, other);
            assert_eq!(turns[&other.thread_id].recorded_calls, vec![untouched]);
            assert!(!*turns[&other.thread_id].cancel.borrow());
            remove_run(&mut turns, &second);
            assert!(!turns.contains_key(&second.thread_id));
            validate_run(&turns, &other, 3).unwrap();
        }
    }

    #[test]
    fn external_execution_requires_current_profile_opt_in() {
        let mut preferences = NativeFeaturePreferences::default();
        assert_eq!(
            run_response(
                validate_admission(&preferences, crate::i18n::Language::English),
                crate::i18n::Language::English
            ),
            json!({"success":false,"error":"Antigravity execution failed: Antigravity is disabled in this profile."})
        );
        preferences.antigravity_enabled = true;
        assert_eq!(
            run_response(
                validate_admission(&preferences, crate::i18n::Language::English),
                crate::i18n::Language::English
            ),
            json!({"success":true,"error":null})
        );
        preferences.antigravity_enabled = false;
        assert!(validate_admission(&preferences, crate::i18n::Language::English).is_err());
        assert_eq!(
            run_response(
                validate_admission(&preferences, crate::i18n::Language::Japanese),
                crate::i18n::Language::Japanese
            ),
            json!({"success":false,"error":"Antigravityの実行に失敗しました: このプロファイルではAntigravityが無効になっています。"})
        );
    }

    #[test]
    fn external_run_response_preserves_startup_error_chain() {
        let startup = Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "example executable was not found",
        ))
        .context("start Antigravity CLI at example-agy");
        assert_eq!(
            run_response(startup, crate::i18n::Language::English),
            json!({"success":false,"error":"Antigravity execution failed: start Antigravity CLI at example-agy: example executable was not found"})
        );
    }

    #[test]
    fn external_run_response_localizes_and_redacts_error_details() {
        let error = anyhow::anyhow!("provider rejected api_key=fixture-secret");
        assert_eq!(
            run_response(Err(error), crate::i18n::Language::Korean),
            json!({"success":false,"error":"Antigravity 실행에 실패했습니다: provider rejected api_key=[secret redacted]"})
        );
    }

    #[test]
    fn external_run_response_preserves_cli_auth_error_and_success() {
        for (result, expected) in [
            (
                json!({"status":"ERROR","error":"Authentication failed: subscription login expired"}),
                json!({"success":false,"error":"Antigravity execution failed: Antigravity returned ERROR: Authentication failed: subscription login expired"}),
            ),
            (
                json!({"status":"SUCCESS","response":"completed"}),
                json!({"success":true,"error":null}),
            ),
        ] {
            let crate::agy::Event::Result { status, error, .. } =
                crate::agy::parse_event(json!({"event":"result","result":result})).unwrap()
            else {
                panic!("expected the CLI terminal result");
            };
            assert_eq!(
                run_response(
                    terminal_result(
                        status.as_deref(),
                        error.as_deref(),
                        crate::i18n::Language::English,
                    ),
                    crate::i18n::Language::English
                ),
                expected
            );
        }
    }

    #[test]
    fn external_turn_cleanup_preserves_both_successes() {
        assert!(combine_turn_and_cleanup(Ok(()), Ok(()), crate::i18n::Language::English).is_ok());
    }

    #[test]
    fn external_turn_cleanup_preserves_primary_error_when_cleanup_succeeds() {
        let primary = std::io::Error::from_raw_os_error(23);
        let result = combine_turn_and_cleanup(
            Err(anyhow::Error::new(primary)),
            Ok(()),
            crate::i18n::Language::English,
        )
        .expect_err("the turn failed");
        assert_eq!(
            result
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(23)
        );
    }

    #[test]
    fn external_turn_cleanup_reports_cleanup_error_after_turn_success() {
        let result = combine_turn_and_cleanup(
            Ok(()),
            Err(anyhow::anyhow!("cleanup diagnostic code -32042")),
            crate::i18n::Language::English,
        )
        .expect_err("cleanup failed");
        assert_eq!(
            format!("{result:#}"),
            "Could not stop the completed AGY process tree.: cleanup diagnostic code -32042"
        );
    }

    #[test]
    fn external_turn_cleanup_keeps_primary_chain_and_cleanup_diagnostic() {
        let primary =
            anyhow::Error::new(std::io::Error::from_raw_os_error(24)).context("primary turn cause");
        let cleanup =
            anyhow::Error::new(std::io::Error::from_raw_os_error(25)).context("cleanup cause");
        let result =
            combine_turn_and_cleanup(Err(primary), Err(cleanup), crate::i18n::Language::English)
                .expect_err("both turn and cleanup failed");
        let context = result
            .downcast_ref::<CleanupFailureContext>()
            .expect("cleanup error chain remains available in the context");
        assert_eq!(
            result
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(24)
        );
        assert_eq!(
            context
                .cleanup
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(25)
        );
        let rendered = format!("{result:#}");
        assert!(rendered.contains("AGY cleanup also failed: cleanup cause:"));
        assert!(rendered.contains("primary turn cause:"));
    }

    #[test]
    fn successful_empty_terminal_remains_success_for_tool_only_completions() {
        let result = terminal_result(Some("SUCCESS"), None, crate::i18n::Language::English);
        assert_eq!(
            run_response(result, crate::i18n::Language::English),
            json!({"success":true,"error":null})
        );
    }

    #[test]
    fn terminal_failure_text_uses_selected_locale_and_keeps_status_and_detail() {
        for (language, expected) in [
            (
                crate::i18n::Language::English,
                "Antigravity returned ERROR: example detail",
            ),
            (
                crate::i18n::Language::Korean,
                "Antigravity가 ERROR 상태를 반환했습니다: example detail",
            ),
            (
                crate::i18n::Language::Chinese,
                "Antigravity 返回了 ERROR 状态：example detail",
            ),
            (
                crate::i18n::Language::Japanese,
                "AntigravityはステータスERRORを返しました: example detail",
            ),
        ] {
            let result = terminal_result(Some("ERROR"), Some("example detail"), language);
            assert_eq!(format!("{:#}", result.unwrap_err()), expected);
        }
    }

    #[test]
    fn inline_image_decoding_preserves_bytes_and_reports_invalid_data() {
        assert_eq!(
            decode_inline_image("data:image/png;base64,AQID").unwrap(),
            (".png", vec![1, 2, 3])
        );
        assert_eq!(
            decode_inline_image("data:image/jpeg,%01%02%03").unwrap(),
            (".jpg", vec![1, 2, 3])
        );
        assert!(decode_inline_image("data:image/png;base64,not base64!").is_err());
    }

    #[test]
    fn only_completed_external_tools_become_native_tool_history() {
        let event = |state| {
            crate::agy::parse_event(json!({"event":"step_update","step_update":{
            "conversation_id":"example","step_index":4,"step_type":"tool","state":state,
            "tool_name":"read_file","tool_info":{"parameters":{"path":"example.txt"},"output":"contents"}}})).unwrap()
        };
        assert_eq!(completed_tool_event(&event("ACTIVE")), None);
        let output = completed_tool_event(&event("DONE")).unwrap();
        assert_eq!(output["name"], "read_file");
        assert_eq!(
            output,
            json!({
                "type":"tool",
                "callId":"agy-example-4",
                "name":"read_file",
                "arguments":{"path":"example.txt"},
                "output":"contents",
                "status":"DONE"
            })
        );
    }
}
