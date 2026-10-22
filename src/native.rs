use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use codex_uds::{UnixListener, UnixStream, prepare_private_socket_directory};
use futures_util::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex as AsyncMutex, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{WebSocketStream, client_async_with_config};
use uuid::Uuid;

use crate::config::Config;
use crate::model_catalog::{ModelExecutor, ModelSelection};
use crate::state::StateStore;

const NATIVE_ERROR_CODE_FIELD: &str = "code";
const NATIVE_ERROR_MESSAGE_FIELD: &str = "message";
const MISSING_NATIVE_ERROR_MESSAGE: &str = "native request failed";
const TURN_STEER_METHOD: &str = "turn/steer";
const THREAD_RESUME_METHOD: &str = "thread/resume";

#[path = "native_external.rs"]
mod external;

#[path = "native_features.rs"]
mod native_features;
pub use native_features::{
    NativeFeatureApplyResult, NativeFeatureDescriptor, NativeFeaturePreferences,
    NativeFeatureSnapshot, NativeFeatureStore, feature_config_read_params,
};

#[cfg(windows)]
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows_sys::Win32::Foundation::CloseHandle;
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
};

type Socket = WebSocketStream<UnixStream>;

#[path = "agy_catalog_cache.rs"]
pub(crate) mod agy_catalog_cache;

pub type VoiceLeaveHandler =
    Arc<dyn Fn(String) -> BoxFuture<'static, Result<Value>> + Send + Sync + 'static>;
pub type DiscordToolHandler =
    Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<Value>> + Send + Sync + 'static>;

struct ServerRequestState {
    generation: u64,
    responded: bool,
}

struct PendingRequest {
    sender: oneshot::Sender<std::result::Result<Value, NativeRequestError>>,
    method: String,
    observation_fence: u64,
}

/// Recover an in-flight native response from a poisoned map so its original
/// caller receives the actual response instead of waiting forever.
fn take_pending_response(
    pending: &Mutex<HashMap<String, PendingRequest>>,
    key: &str,
) -> (Option<PendingRequest>, bool) {
    match pending.lock() {
        Ok(mut pending) => (pending.remove(key), false),
        Err(poisoned) => (poisoned.into_inner().remove(key), true),
    }
}

fn drain_pending_responses(
    pending: &Mutex<HashMap<String, PendingRequest>>,
) -> (Vec<PendingRequest>, bool) {
    match pending.lock() {
        Ok(mut pending) => (pending.drain().map(|(_, request)| request).collect(), false),
        Err(poisoned) => (
            poisoned
                .into_inner()
                .drain()
                .map(|(_, request)| request)
                .collect(),
            true,
        ),
    }
}

fn publish_native_failure(inner: &Inner, detail: &str) {
    let detail = crate::discord::sanitize_discord_error(&inner.config, detail);
    tracing::error!(%detail, "native bridge failure");
    publish_event(
        inner,
        json!({"method":"error","params":{"error":{"message":detail}}}),
    );
}

#[cfg(test)]
mod pending_response_lock_tests {
    use super::*;

    fn pending(method: &str) -> PendingRequest {
        let (sender, _receiver) = oneshot::channel();
        PendingRequest {
            sender,
            method: method.to_owned(),
            observation_fence: 0,
        }
    }

    fn poison<T: Send + 'static>(mutex: Arc<Mutex<T>>) {
        let poisoned = mutex.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("synthetic pending-map poison");
        })
        .join();
    }

    #[test]
    fn response_entry_remains_available_to_its_caller_after_lock_poison() {
        let pending = Arc::new(Mutex::new(HashMap::from([(
            "response-1".to_owned(),
            pending("thread/read"),
        )])));
        poison(pending.clone());

        let (request, poisoned) = take_pending_response(&pending, "response-1");
        assert!(poisoned);
        assert_eq!(request.expect("response is routed").method, "thread/read");
        assert!(take_pending_response(&pending, "response-1").0.is_none());
    }

    #[test]
    fn disconnect_drains_waiters_even_when_pending_map_is_poisoned() {
        let pending = Arc::new(Mutex::new(HashMap::from([
            ("response-1".to_owned(), pending("thread/read")),
            ("response-2".to_owned(), pending("turn/start")),
        ])));
        poison(pending.clone());

        let (mut requests, poisoned) = drain_pending_responses(&pending);
        assert!(poisoned);
        requests.sort_by(|left, right| left.method.cmp(&right.method));
        assert_eq!(
            requests
                .into_iter()
                .map(|request| request.method)
                .collect::<Vec<_>>(),
            ["thread/read", "turn/start"],
        );
        assert!(drain_pending_responses(&pending).0.is_empty());
    }
}

struct BridgeEventBuffer {
    sender: mpsc::UnboundedSender<Value>,
    receiver: Option<mpsc::UnboundedReceiver<Value>>,
    startup_events: Vec<Value>,
    live: bool,
    enabled: bool,
}

impl BridgeEventBuffer {
    fn new(enabled: bool) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            sender,
            receiver: enabled.then_some(receiver),
            startup_events: Vec::new(),
            live: false,
            enabled,
        }
    }

    fn publish(&mut self, event: Value) {
        if !self.enabled || !is_bridge_control_event(&event) {
            return;
        }
        if self.live {
            let _ = self.sender.send(event);
        } else {
            self.startup_events.push(event);
        }
    }

    fn subscribe(&mut self) -> Result<mpsc::UnboundedReceiver<Value>> {
        let receiver = self
            .receiver
            .take()
            .context("native bridge event receiver was already claimed")?;
        for event in self.startup_events.drain(..) {
            let _ = self.sender.send(event);
        }
        self.live = true;
        Ok(receiver)
    }
}

fn is_bridge_control_event(event: &Value) -> bool {
    let Some(method) = event.get("method").and_then(Value::as_str) else {
        return false;
    };
    !method.starts_with("thread/realtime/")
        && !method.ends_with("/delta")
        && !method.ends_with("Delta")
}

#[derive(Debug)]
pub(crate) struct NativeRequestError {
    method: String,
    code: Option<i64>,
    message: String,
}

impl NativeRequestError {
    pub(crate) fn from_response(method: String, error: &Value) -> Self {
        Self {
            method,
            code: error.get(NATIVE_ERROR_CODE_FIELD).and_then(Value::as_i64),
            message: error
                .get(NATIVE_ERROR_MESSAGE_FIELD)
                .and_then(Value::as_str)
                .filter(|message| !message.trim().is_empty())
                .unwrap_or(MISSING_NATIVE_ERROR_MESSAGE)
                .to_owned(),
        }
    }

    pub(crate) fn is_expected_turn_mismatch(&self) -> bool {
        self.method == TURN_STEER_METHOD
            && self.code == Some(-32600)
            && (self.message == "no active turn to steer"
                || self.message.starts_with("expected active turn id"))
    }

    fn is_thread_closing(&self) -> bool {
        self.method == THREAD_RESUME_METHOD
            && self.code == Some(-32600)
            && self
                .message
                .contains("is closing; retry thread/resume after the thread is closed")
    }
}

// Matches Codex TUI app_server_session.rs history compatibility fallback.
pub(crate) fn is_history_pagination_unsupported(error: &anyhow::Error) -> bool {
    let Some(source) = error.downcast_ref::<NativeRequestError>() else {
        return false;
    };
    if source.code == Some(-32601) {
        return true;
    }
    if !matches!(source.code, Some(-32600 | -32602)) {
        return false;
    }
    let message = source.message.to_ascii_lowercase();
    [
        "historymode",
        "history mode",
        "excludeturns",
        "exclude turns",
        "thread/turns/list",
        "thread/items/list",
    ]
    .into_iter()
    .any(|field| message.contains(field))
        || (message.contains("paginated")
            && ["unknown variant", "unsupported variant", "invalid enum"]
                .into_iter()
                .any(|error| message.contains(error)))
}

impl std::fmt::Display for NativeRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.message == MISSING_NATIVE_ERROR_MESSAGE {
            // Preserve the established fallback when the server omitted a
            // usable error message.
            write!(
                formatter,
                "{} ({})",
                crate::i18n::Language::English
                    .message(crate::i18n::MessageKey::NativeRequestFailed),
                self.method
            )
        } else if let Some(code) = self.code {
            write!(
                formatter,
                "{} ({}; code {code}): {}",
                crate::i18n::Language::English
                    .message(crate::i18n::MessageKey::NativeRequestFailed),
                self.method,
                self.message
            )
        } else {
            write!(
                formatter,
                "{} ({}): {}",
                crate::i18n::Language::English
                    .message(crate::i18n::MessageKey::NativeRequestFailed),
                self.method,
                self.message
            )
        }
    }
}

impl std::error::Error for NativeRequestError {}

fn record_native_request_error(error: &NativeRequestError) {
    tracing::warn!(
        native_method = %error.method,
        native_error_code = ?error.code,
        native_error_message = %error.message,
        "Codex app-server request returned a structured error"
    );
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ServerRequestKey {
    generation: u64,
    id: String,
}

struct Inner {
    writer: AsyncMutex<Option<futures_util::stream::SplitSink<Socket, Message>>>,
    pending: Mutex<HashMap<String, PendingRequest>>,
    server_requests: AsyncMutex<HashMap<ServerRequestKey, ServerRequestState>>,
    events: broadcast::Sender<Value>,
    pending_thread_settings: Mutex<HashMap<ThreadSettingKey, PendingThreadSetting>>,
    thread_setting_current: Mutex<HashMap<ThreadSettingKey, Weak<AtomicBool>>>,
    next_thread_setting_revision: AtomicU64,
    bridge_events: Mutex<BridgeEventBuffer>,
    harness_worktrees: tokio::sync::OnceCell<crate::worktree_tools::WorktreeTools>,
    startup_events: Mutex<Vec<Value>>,
    live_events: AtomicBool,
    connection_state: AtomicU8,
    connection_generation: AtomicU64,
    next_id: AtomicU64,
    next_observation_order: AtomicU64,
    thread_id: Mutex<String>,
    thread_state: Mutex<Value>,
    applied_subagent_model: Mutex<Value>,
    external_turns: Mutex<HashMap<String, external::ActiveTurn>>,
    child: AsyncMutex<Option<Child>>,
    reconnect: AsyncMutex<()>,
    agy_catalog_refresh: AsyncMutex<()>,
    endpoint: PathBuf,
    endpoint_dir: PathBuf,
    voice_tool_endpoint: PathBuf,
    voice_tool_endpoint_dir: PathBuf,
    voice_leave_handler: std::sync::RwLock<Option<VoiceLeaveHandler>>,
    discord_tool_handler: std::sync::RwLock<Option<DiscordToolHandler>>,
    voice_ipc_shutdown: watch::Sender<bool>,
    voice_ipc_task: AsyncMutex<Option<JoinHandle<()>>>,
    config: Config,
    #[cfg(windows)]
    child_job: WindowsChildJob,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ThreadSettingKey {
    generation: u64,
    thread_id: String,
    field: String,
}

struct PendingThreadSetting {
    revision: u64,
    expected: Value,
    status: watch::Sender<ThreadSettingUpdateStatus>,
    current: Arc<AtomicBool>,
}

#[derive(Clone, Debug)]
enum ThreadSettingUpdateStatus {
    Pending,
    Applied(Value),
    Superseded,
    Failed(String),
}

pub enum ThreadSettingUpdate {
    Confirmed(Value),
    Pending(ThreadSettingUpdateTicket),
}

pub enum ThreadSettingUpdateOutcome {
    Applied(Value),
    Superseded,
}

pub struct ThreadSettingUpdateTicket {
    key: ThreadSettingKey,
    revision: u64,
    inner: Weak<Inner>,
    status: watch::Receiver<ThreadSettingUpdateStatus>,
    current: Arc<AtomicBool>,
}

impl ThreadSettingUpdateTicket {
    pub fn is_current(&self) -> bool {
        self.current.load(Ordering::Acquire)
    }

    pub async fn wait(&mut self) -> Result<ThreadSettingUpdateOutcome> {
        loop {
            match self.status.borrow().clone() {
                ThreadSettingUpdateStatus::Pending => {}
                ThreadSettingUpdateStatus::Applied(snapshot) => {
                    return Ok(ThreadSettingUpdateOutcome::Applied(snapshot));
                }
                ThreadSettingUpdateStatus::Superseded => {
                    return Ok(ThreadSettingUpdateOutcome::Superseded);
                }
                ThreadSettingUpdateStatus::Failed(reason) => {
                    bail!("thread settings update was not confirmed: {reason}");
                }
            }
            self.status
                .changed()
                .await
                .context("thread settings update status channel closed")?;
        }
    }
}

impl Drop for ThreadSettingUpdateTicket {
    fn drop(&mut self) {
        let terminal = !matches!(&*self.status.borrow(), ThreadSettingUpdateStatus::Pending);
        if !terminal {
            return;
        }
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        if let Ok(mut current) = inner.thread_setting_current.lock()
            && current
                .get(&self.key)
                .and_then(Weak::upgrade)
                .is_some_and(|value| Arc::ptr_eq(&value, &self.current))
        {
            current.remove(&self.key);
        }
    }
}

fn setting_value<'a>(settings: &'a Value, field: &str) -> Option<&'a Value> {
    match field {
        "effort" | "reasoningEffort" => settings
            .get("effort")
            .or_else(|| settings.get("reasoningEffort")),
        other => settings.get(other),
    }
}

fn confirmed_thread_setting_event(
    event: &Value,
    key: &ThreadSettingKey,
    expected: &Value,
) -> Option<Value> {
    if event.get("method").and_then(Value::as_str) != Some("thread/settings/updated")
        || event
            .pointer("/params/_discodex_connection_generation")
            .and_then(Value::as_u64)
            != Some(key.generation)
        || event.pointer("/params/threadId").and_then(Value::as_str) != Some(key.thread_id.as_str())
    {
        return None;
    }
    let settings = event.pointer("/params/threadSettings")?;
    (setting_value(settings, &key.field) == Some(expected))
        .then(|| thread_state_summary(&json!({ "thread": settings })))
}

#[cfg(test)]
mod thread_setting_confirmation_tests {
    use super::*;

    #[test]
    fn confirmation_requires_matching_thread_generation_and_requested_value() {
        let key = ThreadSettingKey {
            generation: 7,
            thread_id: "thread-synthetic".to_owned(),
            field: "model".to_owned(),
        };
        let expected = json!("synthetic-model-b");
        let event = json!({
            "method": "thread/settings/updated",
            "params": {
                "_discodex_connection_generation": 7,
                "threadId": "thread-synthetic",
                "threadSettings": { "model": "synthetic-model-b", "effort": "high" }
            }
        });
        let confirmed = confirmed_thread_setting_event(&event, &key, &expected)
            .expect("matching applied event confirms the requested value");
        assert_eq!(confirmed.get("model"), Some(&expected));

        let wrong_thread = json!({
            "method": "thread/settings/updated",
            "params": {
                "_discodex_connection_generation": 7,
                "threadId": "other-thread",
                "threadSettings": { "model": "synthetic-model-b" }
            }
        });
        assert_eq!(
            confirmed_thread_setting_event(&wrong_thread, &key, &expected),
            None
        );

        let wrong_generation = json!({
            "method": "thread/settings/updated",
            "params": {
                "_discodex_connection_generation": 6,
                "threadId": "thread-synthetic",
                "threadSettings": { "model": "synthetic-model-b" }
            }
        });
        assert_eq!(
            confirmed_thread_setting_event(&wrong_generation, &key, &expected),
            None
        );

        let mismatched_value = json!({
            "method": "thread/settings/updated",
            "params": {
                "_discodex_connection_generation": 7,
                "threadId": "thread-synthetic",
                "threadSettings": { "model": "synthetic-model-a" }
            }
        });
        assert_eq!(
            confirmed_thread_setting_event(&mismatched_value, &key, &expected),
            None
        );
    }

    #[test]
    fn effort_confirmation_accepts_native_reasoning_effort_alias() {
        let key = ThreadSettingKey {
            generation: 2,
            thread_id: "thread-synthetic".to_owned(),
            field: "effort".to_owned(),
        };
        let event = json!({
            "method": "thread/settings/updated",
            "params": {
                "_discodex_connection_generation": 2,
                "threadId": "thread-synthetic",
                "threadSettings": { "reasoningEffort": "medium" }
            }
        });
        assert!(confirmed_thread_setting_event(&event, &key, &json!("medium")).is_some());
    }
}

#[cfg(windows)]
struct WindowsChildJob {
    _handle: OwnedHandle,
}

#[cfg(windows)]
impl WindowsChildJob {
    fn assign(child: &Child) -> Result<Self> {
        use std::mem::{size_of, zeroed};
        use std::ptr::null;

        let pid = child
            .id()
            .context("Codex app-server child has no process ID")?;
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return Err(std::io::Error::last_os_error())
                    .context("create owned Codex app-server job object");
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set_result = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set_result == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(error).context("configure owned Codex child job lifetime");
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(error).context("open owned Codex app-server process");
            }
            let assign_result = AssignProcessToJobObject(job, process);
            CloseHandle(process);
            if assign_result == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(error).context("assign Codex app-server to owned process job");
            }
            Ok(Self {
                _handle: OwnedHandle::from_raw_handle(job as RawHandle),
            })
        }
    }
}

fn configured_thread_settings_update(config: &Config, thread_id: &str) -> Option<Value> {
    let mut params = json!({"threadId": thread_id});
    if let Some(effort) = &config.effort {
        params["effort"] = Value::String(effort.clone());
    }
    if let Some(service_tier) = &config.service_tier {
        params["serviceTier"] = if service_tier.eq_ignore_ascii_case("off") {
            Value::Null
        } else {
            Value::String(service_tier.clone())
        };
    }
    (config.effort.is_some() || config.service_tier.is_some()).then_some(params)
}

fn apply_memory_config(command: &mut Command, enabled: Option<bool>) {
    if let Some(enabled) = enabled {
        for key in [
            "features.memories",
            "memories.generate_memories",
            "memories.use_memories",
            "memories.dedicated_tools",
        ] {
            command.arg("--config").arg(format!("{key}={enabled}"));
        }
    }
}

#[cfg(test)]
mod memory_config_tests {
    use super::*;

    fn arguments(enabled: Option<bool>) -> Vec<String> {
        let mut command = Command::new("codex");
        apply_memory_config(&mut command, enabled);
        command
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn enabled_memory_includes_native_retrieval_and_note_tools() {
        assert_eq!(
            arguments(Some(true)),
            [
                "--config",
                "features.memories=true",
                "--config",
                "memories.generate_memories=true",
                "--config",
                "memories.use_memories=true",
                "--config",
                "memories.dedicated_tools=true",
            ]
        );
    }

    #[test]
    fn disabled_memory_also_disables_dedicated_tools() {
        assert_eq!(
            arguments(Some(false)),
            [
                "--config",
                "features.memories=false",
                "--config",
                "memories.generate_memories=false",
                "--config",
                "memories.use_memories=false",
                "--config",
                "memories.dedicated_tools=false",
            ]
        );
    }

    #[test]
    fn unspecified_memory_preserves_native_configuration() {
        assert!(arguments(None).is_empty());
    }
}

#[derive(Clone)]
pub struct Native {
    inner: Arc<Inner>,
}

async fn apply_native_feature_preferences_resolved<F, Fut>(
    store: &NativeFeatureStore,
    preferences: &NativeFeaturePreferences,
    previous: &NativeFeaturePreferences,
    model_catalog: Option<&Value>,
    fetch_catalog: F,
) -> Result<NativeFeatureApplyResult>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let mut preferences = preferences.clone();
    if preferences.subagent_model != previous.subagent_model
        || preferences.subagent_model_provider != previous.subagent_model_provider
        || preferences.subagent_model_executor != previous.subagent_model_executor
    {
        if let Some(model) = preferences.subagent_model.as_deref() {
            let catalog = match model_catalog {
                Some(catalog) => catalog.clone(),
                None => fetch_catalog().await?,
            };
            let requested = if let Some(provider) = preferences.subagent_model_provider.as_ref() {
                ModelSelection {
                    executor: match preferences.subagent_model_executor.as_deref() {
                        Some(executor) if executor == ModelExecutor::Antigravity.as_str() => {
                            ModelExecutor::Antigravity
                        }
                        _ => ModelExecutor::Codex,
                    },
                    provider: provider.clone(),
                    model: model.to_owned(),
                }
                .value()
            } else {
                model.to_owned()
            };
            let selection = crate::model_catalog::resolve_selection(&catalog, &requested)?;
            preferences.subagent_model = Some(selection.model);
            preferences.subagent_model_provider = Some(selection.provider);
            preferences.subagent_model_executor = Some(selection.executor.as_str().into());
        } else {
            preferences.subagent_model_provider = None;
            preferences.subagent_model_executor = None;
        }
    }
    store.apply(&preferences)
}

#[cfg(test)]
mod native_feature_preferences_apply_tests {
    use super::*;

    fn agy_preferences(model: &str) -> NativeFeaturePreferences {
        NativeFeaturePreferences {
            subagent_model: Some(model.to_owned()),
            subagent_model_provider: Some("antigravity".to_owned()),
            subagent_model_executor: Some("antigravity".to_owned()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn supplied_catalog_is_reused_at_the_preferences_apply_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let store = NativeFeatureStore::new(directory.path());
        let previous = store.read().unwrap();
        let requested = agy_preferences("agy-exact-id");
        let mut catalog = json!({"data": []});
        crate::model_catalog::add_antigravity_models(
            &mut catalog,
            true,
            &["agy-exact-id".to_owned()],
        );
        let fetches = std::cell::Cell::new(0);

        let result = apply_native_feature_preferences_resolved(
            &store,
            &requested,
            &previous,
            Some(&catalog),
            || {
                fetches.set(fetches.get() + 1);
                std::future::ready(Ok(json!({"data": []})))
            },
        )
        .await
        .unwrap();

        assert_eq!(result, NativeFeatureApplyResult::Saved);
        assert_eq!(fetches.get(), 0);
        let saved = store.read().unwrap();
        assert_eq!(saved.subagent_model.as_deref(), Some("agy-exact-id"));
        assert_eq!(
            saved.subagent_model_provider.as_deref(),
            Some("antigravity")
        );
        assert_eq!(
            saved.subagent_model_executor.as_deref(),
            Some("antigravity")
        );
    }

    #[tokio::test]
    async fn missing_catalog_is_fetched_once_and_resolution_failure_does_not_save() {
        let directory = tempfile::tempdir().unwrap();
        let store = NativeFeatureStore::new(directory.path());
        let previous = store.read().unwrap();
        let requested = agy_preferences("agy-exact-id");
        let mut fetches = 0;
        let result =
            apply_native_feature_preferences_resolved(&store, &requested, &previous, None, || {
                fetches += 1;
                let mut catalog = json!({"data": []});
                crate::model_catalog::add_antigravity_models(
                    &mut catalog,
                    true,
                    &["agy-exact-id".to_owned()],
                );
                std::future::ready(Ok(catalog))
            })
            .await
            .unwrap();
        assert_eq!(result, NativeFeatureApplyResult::Saved);
        assert_eq!(fetches, 1);
        let saved = store.read().unwrap();
        assert_eq!(saved.subagent_model.as_deref(), Some("agy-exact-id"));

        let before_failure = saved.clone();
        let unresolved = agy_preferences("unlisted-model");
        let mut catalog = json!({"data": []});
        crate::model_catalog::add_antigravity_models(
            &mut catalog,
            true,
            &["agy-exact-id".to_owned()],
        );
        let failed = apply_native_feature_preferences_resolved(
            &store,
            &unresolved,
            &saved,
            Some(&catalog),
            || std::future::ready(Ok(json!({"data": []}))),
        )
        .await;
        assert!(failed.is_err());
        assert_eq!(store.read().unwrap(), before_failure);
    }

    #[tokio::test]
    async fn preferences_apply_returns_storage_failure_without_reporting_saved() {
        let directory = tempfile::tempdir().unwrap();
        let state_file = directory.path().join("not-a-directory");
        std::fs::write(&state_file, b"occupied").unwrap();
        let store = NativeFeatureStore::new(&state_file);
        let previous = NativeFeaturePreferences::default();
        let requested = agy_preferences("agy-exact-id");
        let mut catalog = json!({"data": []});
        crate::model_catalog::add_antigravity_models(
            &mut catalog,
            true,
            &["agy-exact-id".to_owned()],
        );

        let result = apply_native_feature_preferences_resolved(
            &store,
            &requested,
            &previous,
            Some(&catalog),
            || std::future::ready(Ok(json!({"data": []}))),
        )
        .await;

        assert!(result.is_err());
    }
}

impl Native {
    pub async fn start(config: &Config) -> Result<Self> {
        Self::start_internal(config, false).await
    }

    /// Starts with lossless buffering for the Discord bridge event consumer.
    /// One-shot probes and setup connections should use start or connect.
    pub async fn start_for_bridge(config: &Config) -> Result<Self> {
        Self::start_internal(config, true).await
    }

    async fn start_internal(config: &Config, bridge_events_enabled: bool) -> Result<Self> {
        let native = Self::connect_internal(config, bridge_events_enabled).await?;
        let initialized = async {
            let saved = StateStore::new(&config.state_dir).load()?;
            if saved.pending_new_thread {
                native.start_new_raw().await?;
            } else if let Some(thread_id) = saved.thread_id.as_ref() {
                let mut params = json!({
                    "threadId": thread_id,
                    "baseInstructions": crate::instructions::agent_base_instructions(&config.name, config.language)
                });
                let approval_policy = native.read_native_feature_preferences()?.approval_policy;
                params["approvalPolicy"] = approval_policy.clone();
                let child_model = native.prepare_model_params(&mut params, saved.model_selection.as_ref()).await?;
                let thread = native.request("thread/resume", params).await
                    .with_context(|| format!("resume saved Codex thread {thread_id}"))?;
                if let Some(selection) = saved.model_selection.as_ref() {
                    validate_model_selection_response(&thread, selection)?;
                }
                native.remember_subagent_model(child_model)?;
                let actual_thread_id = response_thread_id(&thread)?;
                if let Some(enabled) = config.memory_enabled {
                    native
                        .request_raw(
                            "thread/memoryMode/set",
                            json!({
                                "threadId": actual_thread_id,
                                "mode": if enabled { "enabled" } else { "disabled" }
                            }),
                        )
                        .await
                        .context("apply configured memory mode to resumed Codex thread")?;
                }
                *native
                    .inner
                    .thread_id
                    .lock()
                    .map_err(|_| anyhow!("thread ID lock poisoned"))? = actual_thread_id;
                *native
                    .inner
                    .thread_state
                    .lock()
                    .map_err(|_| anyhow!("thread state lock poisoned"))? =
                    thread_state_summary(&thread);
                // An already-loaded native thread can ignore resume overrides.
                if thread_state_summary(&thread).get("approvalPolicy") != Some(&approval_policy) {
                    native.set_approval_policy_and_confirm(approval_policy).await?;
                }
            } else {
                native.start_new_raw().await?;
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = initialized {
            let cleanup = native.shutdown().await;
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(error.context(format!(
                    "also failed to clean up Codex app-server: {cleanup_error:#}"
                ))),
            };
        }
        Ok(native)
    }

    /// Starts and initializes the owned app-server without loading or creating
    /// a thread. Setup can inspect account and permission catalogs without
    /// leaving a thread ID that has no durable rollout behind it.
    pub async fn connect(config: &Config) -> Result<Self> {
        Self::connect_internal(config, false).await
    }

    async fn connect_internal(config: &Config, bridge_events_enabled: bool) -> Result<Self> {
        let codex_path = resolve_codex_path(&config.codex_path)?;
        crate::bundled_skill::sync_for_profile(config);
        tokio::fs::create_dir_all(&config.state_dir)
            .await
            .with_context(|| format!("create state directory {}", config.state_dir.display()))?;

        let native_feature_store = NativeFeatureStore::new(&config.state_dir);
        let native_feature_overrides = native_feature_store.startup_overrides()?;

        let endpoint_dir = private_socket_endpoint_dir(&std::env::temp_dir());
        let endpoint = endpoint_dir.join("app-server.sock");
        let listen = format!("unix://{}", endpoint.display());

        let voice_tool_endpoint_dir = private_socket_endpoint_dir(&std::env::temp_dir());
        let voice_tool_endpoint = voice_tool_endpoint_dir.join("voice-tools.sock");
        if let Err(error) = prepare_private_socket_directory(&voice_tool_endpoint_dir).await {
            let cleanup = cleanup_endpoint(&voice_tool_endpoint, &voice_tool_endpoint_dir);
            return match cleanup {
                Ok(()) => Err(error).context("prepare private voice tool IPC directory"),
                Err(cleanup_error) => Err(error).context(format!(
                    "prepare private voice tool IPC directory; cleanup also failed: {cleanup_error:#}"
                )),
            };
        }
        let voice_listener = match UnixListener::bind(&voice_tool_endpoint).await {
            Ok(listener) => listener,
            Err(error) => {
                let cleanup = cleanup_endpoint(&voice_tool_endpoint, &voice_tool_endpoint_dir);
                return match cleanup {
                    Ok(()) => Err(error).context("bind private voice tool IPC socket"),
                    Err(cleanup_error) => Err(error).context(format!(
                        "bind private voice tool IPC socket; cleanup also failed: {cleanup_error:#}"
                    )),
                };
            }
        };

        let worker_path = match &config.worker_path {
            Some(path) => path.clone(),
            None => match std::env::current_exe() {
                Ok(path) => path,
                Err(error) => {
                    drop(voice_listener);
                    let cleanup = cleanup_endpoint(&voice_tool_endpoint, &voice_tool_endpoint_dir);
                    let context = match cleanup {
                        Ok(()) => "resolve discodex executable for MCP".to_owned(),
                        Err(cleanup_error) => format!(
                            "resolve discodex executable for MCP; cleanup also failed: {cleanup_error:#}"
                        ),
                    };
                    return Err(error).context(context);
                }
            },
        };
        let worker_path = match worker_path.to_str() {
            Some(path) => path,
            None => {
                drop(voice_listener);
                let cleanup = cleanup_endpoint(&voice_tool_endpoint, &voice_tool_endpoint_dir);
                let context = match cleanup {
                    Ok(()) => "discodex MCP executable path is not valid UTF-8".to_owned(),
                    Err(cleanup_error) => format!(
                        "discodex MCP executable path is not valid UTF-8; cleanup also failed: {cleanup_error:#}"
                    ),
                };
                bail!("{context}");
            }
        };
        let voice_endpoint_arg = match voice_tool_endpoint.to_str() {
            Some(path) => path,
            None => {
                drop(voice_listener);
                let cleanup = cleanup_endpoint(&voice_tool_endpoint, &voice_tool_endpoint_dir);
                let context = match cleanup {
                    Ok(()) => "voice tool IPC socket path is not valid UTF-8".to_owned(),
                    Err(cleanup_error) => format!(
                        "voice tool IPC socket path is not valid UTF-8; cleanup also failed: {cleanup_error:#}"
                    ),
                };
                bail!("{context}");
            }
        };
        let worker_args = serde_json::to_string(&[
            "voice-tools",
            "--endpoint",
            voice_endpoint_arg,
            "--language",
            config.language.code(),
        ])?;
        let worker_path_value = serde_json::to_string(worker_path)?;
        let harness_args =
            serde_json::to_string(&["harness-tools", "--endpoint", voice_endpoint_arg])?;

        let mut command = Command::new(&codex_path);
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        command
            .arg("--config")
            .arg(format!(
                "mcp_servers.discodex_voice.command={worker_path_value}"
            ))
            .arg("--config")
            .arg(format!("mcp_servers.discodex_voice.args={worker_args}"))
            .arg("--config")
            .arg("mcp_servers.discodex_voice.enabled=true")
            .arg("--config")
            .arg("tools.update_plan.enabled=true")
            .arg("--config")
            .arg(format!(
                "mcp_servers.discodex_tasks.command={worker_path_value}"
            ))
            .arg("--config")
            .arg(format!("mcp_servers.discodex_tasks.args={harness_args}"))
            .arg("--config")
            .arg("mcp_servers.discodex_tasks.enabled=true")
            .arg("--config")
            .arg("mcp_servers.discodex_tasks.tool_timeout_sec=180")
            .arg("--config")
            .arg("mcp_servers.discodex_voice.tools.voice_leave.approval_mode=\"approve\"")
            .arg("--config")
            .arg("mcp_servers.discodex_voice.tools.voice_join.approval_mode=\"approve\"")
            .arg("--config")
            .arg("features.realtime_conversation=true")
            .arg("-c")
            .arg("experimental_realtime_model_provider=\"openai\"")
            .arg("--config")
            .arg(format!(
                "experimental_realtime_ws_backend_prompt={}",
                serde_json::to_string(&crate::realtime_prompt::for_agent(
                    &config.name,
                    config.language
                ))?
            ));
        apply_memory_config(&mut command, config.memory_enabled);
        for (key, value) in native_feature_overrides {
            command.arg("--config").arg(format!("{key}={value}"));
        }
        command
            .arg("app-server")
            .arg("--listen")
            .arg(listen)
            .current_dir(&config.project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(if bridge_events_enabled {
                Stdio::from(crate::logging::native_stderr_file(&config.state_dir)?)
            } else {
                Stdio::inherit()
            })
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        if let Some(codex_home) = &config.codex_home {
            command.env("CODEX_HOME", codex_home);
        }
        let mut child = match command
            .spawn()
            .with_context(|| format!("start Codex app-server at {}", codex_path.display()))
        {
            Ok(child) => child,
            Err(error) => {
                drop(voice_listener);
                let cleanup = cleanup_endpoint(&voice_tool_endpoint, &voice_tool_endpoint_dir);
                return match cleanup {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => Err(error.context(format!(
                        "also failed to remove voice tool IPC endpoint: {cleanup_error:#}"
                    ))),
                };
            }
        };
        #[cfg(windows)]
        let child_job = match WindowsChildJob::assign(&child) {
            Ok(job) => job,
            Err(err) => {
                let stop_error = stop_owned_child(&mut child).await.err();
                drop(voice_listener);
                let cleanup = cleanup_owned_endpoints(
                    &endpoint,
                    &endpoint_dir,
                    &voice_tool_endpoint,
                    &voice_tool_endpoint_dir,
                );
                return match cleanup {
                    Ok(()) => Err(with_stop_error(err, stop_error)),
                    Err(cleanup_err) => Err(with_stop_error(err, stop_error).context(format!(
                        "also failed to remove owned Codex endpoint: {cleanup_err:#}"
                    ))),
                };
            }
        };

        let stream = match connect_when_ready(&endpoint, &mut child).await {
            Ok(stream) => stream,
            Err(err) => {
                let stop_error = stop_owned_child(&mut child).await.err();
                drop(voice_listener);
                let cleanup = cleanup_owned_endpoints(
                    &endpoint,
                    &endpoint_dir,
                    &voice_tool_endpoint,
                    &voice_tool_endpoint_dir,
                );
                return match cleanup {
                    Ok(()) => Err(with_stop_error(err, stop_error)),
                    Err(cleanup_err) => Err(with_stop_error(err, stop_error).context(format!(
                        "also failed to remove owned Codex endpoint: {cleanup_err:#}"
                    ))),
                };
            }
        };

        let socket = match websocket(stream).await {
            Ok(socket) => socket,
            Err(err) => {
                let stop_error = stop_owned_child(&mut child).await.err();
                drop(voice_listener);
                let cleanup = cleanup_owned_endpoints(
                    &endpoint,
                    &endpoint_dir,
                    &voice_tool_endpoint,
                    &voice_tool_endpoint_dir,
                );
                return match cleanup {
                    Ok(()) => Err(with_stop_error(err, stop_error)),
                    Err(cleanup_err) => Err(with_stop_error(err, stop_error).context(format!(
                        "also failed to remove owned Codex endpoint: {cleanup_err:#}"
                    ))),
                };
            }
        };
        let (writer, reader) = socket.split();
        let (events, _) = broadcast::channel(256);
        let (voice_ipc_shutdown, voice_ipc_shutdown_rx) = watch::channel(false);

        let initial_thread_id = String::new();
        let inner = Arc::new(Inner {
            writer: AsyncMutex::new(Some(writer)),
            pending: Mutex::new(HashMap::new()),
            server_requests: AsyncMutex::new(HashMap::new()),
            events,
            pending_thread_settings: Mutex::new(HashMap::new()),
            thread_setting_current: Mutex::new(HashMap::new()),
            next_thread_setting_revision: AtomicU64::new(1),
            bridge_events: Mutex::new(BridgeEventBuffer::new(bridge_events_enabled)),
            harness_worktrees: tokio::sync::OnceCell::new(),
            startup_events: Mutex::new(Vec::new()),
            live_events: AtomicBool::new(false),
            connection_state: AtomicU8::new(2),
            connection_generation: AtomicU64::new(1),
            next_id: AtomicU64::new(1),
            next_observation_order: AtomicU64::new(1),
            thread_id: Mutex::new(initial_thread_id),
            thread_state: Mutex::new(Value::Null),
            applied_subagent_model: Mutex::new(json!({})),
            external_turns: Mutex::new(HashMap::new()),
            child: AsyncMutex::new(Some(child)),
            reconnect: AsyncMutex::new(()),
            agy_catalog_refresh: AsyncMutex::new(()),
            endpoint,
            endpoint_dir,
            voice_tool_endpoint,
            voice_tool_endpoint_dir,
            voice_leave_handler: std::sync::RwLock::new(None),
            discord_tool_handler: std::sync::RwLock::new(None),
            voice_ipc_shutdown,
            voice_ipc_task: AsyncMutex::new(None),
            config: config.clone(),
            #[cfg(windows)]
            child_job,
        });
        let voice_ipc_task = tokio::spawn(serve_voice_tool_ipc(
            voice_listener,
            Arc::downgrade(&inner),
            voice_ipc_shutdown_rx,
        ));
        *inner.voice_ipc_task.lock().await = Some(voice_ipc_task);
        spawn_reader(reader, Arc::downgrade(&inner), 1);
        let native = Self { inner };

        let initialized = async {
            native
                .request(
                    "initialize",
                    json!({
                        "clientInfo": {
                            "name": "discodex",
                            "title": "Discodex",
                            "version": env!("CARGO_PKG_VERSION")
                        },
                        "capabilities": {
                            "experimentalApi": true,
                            "requestAttestation": false
                        }
                    }),
                )
                .await
                .context("initialize Codex app-server client")?;
            native.notify("initialized", json!({})).await?;

            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(err) = initialized {
            let cleanup = native.shutdown().await;
            return match cleanup {
                Ok(()) => Err(err),
                Err(cleanup_err) => Err(err.context(format!(
                    "also failed to clean up Codex app-server: {cleanup_err:#}"
                ))),
            };
        }
        Ok(native)
    }

    pub async fn start_new(&self) -> Result<String> {
        self.ensure_connected().await?;
        let thread_id = self.start_new_raw().await?;
        StateStore::new(&self.inner.config.state_dir).mark_pending_new_thread()?;
        Ok(thread_id)
    }

    async fn start_new_raw(&self) -> Result<String> {
        let saved = StateStore::new(&self.inner.config.state_dir).load()?;
        self.start_new_raw_with(saved.model_selection.as_ref())
            .await
    }

    async fn start_new_raw_with(&self, selection: Option<&ModelSelection>) -> Result<String> {
        let mut params = json!({
            "cwd": self.inner.config.project_dir,
            "ephemeral": false,
            "baseInstructions": crate::instructions::agent_base_instructions(&self.inner.config.name, self.inner.config.language)
        });
        let approval_policy = self.read_native_feature_preferences()?.approval_policy;
        params["approvalPolicy"] = approval_policy.clone();
        if let Some(model) = &self.inner.config.model {
            params["model"] = Value::String(model.clone());
        }
        let child_model = self.prepare_model_params(&mut params, selection).await?;
        if let Some(profile_id) = &self.inner.config.permissions {
            self.validate_permission_profile(profile_id).await?;
            params["permissions"] = Value::String(profile_id.clone());
        }
        let response = self
            .request_raw("thread/start", params)
            .await
            .context("start a Codex thread")?;
        if let Some(selection) = selection {
            validate_model_selection_response(&response, selection)?;
        }
        let thread_id = response_thread_id(&response)?;
        if response
            .pointer("/thread/ephemeral")
            .and_then(Value::as_bool)
            == Some(true)
        {
            bail!(
                "Codex returned ephemeral thread `{thread_id}` despite requesting a persistent thread"
            );
        }
        if let Some(expected) = &self.inner.config.permissions {
            let actual = response
                .pointer("/activePermissionProfile/id")
                .and_then(Value::as_str);
            if actual != Some(expected.as_str()) {
                bail!(
                    "Codex started thread `{thread_id}` with active permission profile {:?}, expected `{expected}`",
                    actual
                );
            }
        }
        if let Some(settings) = configured_thread_settings_update(&self.inner.config, &thread_id) {
            self.request_raw("thread/settings/update", settings)
                .await
                .context("apply configured settings to newly started Codex thread")?;
        }
        self.remember_subagent_model(child_model)?;
        *self
            .inner
            .thread_id
            .lock()
            .map_err(|_| anyhow!("thread ID lock poisoned"))? = thread_id.clone();
        *self
            .inner
            .thread_state
            .lock()
            .map_err(|_| anyhow!("thread state lock poisoned"))? = thread_state_summary(&response);
        if thread_state_summary(&response).get("approvalPolicy") != Some(&approval_policy) {
            self.set_approval_policy_and_confirm(approval_policy)
                .await?;
        }
        Ok(thread_id)
    }

    /// Native provider definitions and credentials remain owned by Codex.
    async fn prepare_model_params(
        &self,
        params: &mut Value,
        selection: Option<&ModelSelection>,
    ) -> Result<Value> {
        let preferences = self.read_native_feature_preferences()?;
        prepare_model_params_with_preferences(params, selection, &preferences)
    }

    fn remember_subagent_model(&self, overrides: Value) -> Result<()> {
        *self
            .inner
            .applied_subagent_model
            .lock()
            .map_err(|_| anyhow!("subagent model lock poisoned"))? = overrides;
        Ok(())
    }

    /// Callers hold input admission, settings, and active-turn locks for this transition.
    pub(crate) async fn set_model_selection(&self, selection: &ModelSelection) -> Result<Value> {
        self.ensure_connected().await?;
        let store = StateStore::new(&self.inner.config.state_dir);
        let saved = store.load()?;
        let thread_id = self.thread_id();
        if thread_id.is_empty() {
            bail!("there is no active Codex thread to change models on");
        }
        if saved.thread_id.as_deref() != Some(thread_id.as_str()) {
            let old = self.thread_state();
            let old_child = self
                .inner
                .applied_subagent_model
                .lock()
                .map_err(|_| anyhow!("subagent model lock poisoned"))?
                .clone();
            let new_id = self.start_new_raw_with(Some(selection)).await?;
            if let Err(error) = store.save_model_selection(selection) {
                *self
                    .inner
                    .thread_id
                    .lock()
                    .map_err(|_| anyhow!("thread ID lock poisoned"))? = thread_id.clone();
                *self
                    .inner
                    .thread_state
                    .lock()
                    .map_err(|_| anyhow!("thread state lock poisoned"))? = old;
                self.remember_subagent_model(old_child)?;
                let cleanup = self
                    .request_raw("thread/unsubscribe", json!({"threadId":new_id}))
                    .await;
                return Err(error).context(format!("could not save the model selection; kept the previous empty conversation (replacement cleanup: {})", if cleanup.is_ok(){"done"}else{"failed"}));
            }
            // The old empty thread has no history. An unsubscribe cleanup error
            // must not misreport a successfully applied and persisted selection.
            if let Err(error) = self
                .request_raw("thread/unsubscribe", json!({"threadId": thread_id}))
                .await
            {
                tracing::warn!(%error,"could not detach previous empty thread after selecting model");
            }
            return Ok(self.thread_state());
        }
        let old = self
            .inner
            .thread_state
            .lock()
            .map_err(|_| anyhow!("thread state lock poisoned"))?
            .clone();
        let mut params = json!({
            "threadId": thread_id,
            "baseInstructions": crate::instructions::agent_base_instructions(
                &self.inner.config.name,
                self.inner.config.language
            )
        });
        let child_model = self
            .prepare_model_params(&mut params, Some(selection))
            .await?;
        // A loaded thread ignores executor/provider overrides on resume. Forking
        // preserves its history while applying the requested configuration.
        preserve_thread_fork_settings(&mut params, &old, self.inner.config.permissions.as_deref());

        let response = self
            .request_raw("thread/fork", params.clone())
            .await
            .context("fork the current Codex conversation with the selected model")?;
        let new_thread_id = match (|| {
            validate_model_selection_response(&response, selection)?;
            let id = response_thread_id(&response)?;
            if id == thread_id {
                bail!("Codex returned the existing thread ID for a history-preserving model fork");
            }
            if response
                .pointer("/thread/ephemeral")
                .and_then(Value::as_bool)
                == Some(true)
            {
                bail!("Codex returned an ephemeral thread for a persistent model switch");
            }
            Ok(id)
        })() {
            Ok(id) => id,
            Err(error) => {
                if let Ok(id) = response_thread_id(&response) {
                    if id != thread_id {
                        if let Err(cleanup_error) = self
                            .request_raw("thread/unsubscribe", json!({"threadId":id}))
                            .await
                        {
                            tracing::warn!(%cleanup_error, "could not detach rejected model fork");
                        }
                    }
                }
                return Err(error);
            }
        };
        let next_thread_state = thread_state_after_fork(&response, &old, &params);
        let commit = (|| -> Result<()> {
            let mut active_thread = self
                .inner
                .thread_id
                .lock()
                .map_err(|_| anyhow!("thread ID lock poisoned"))?;
            if *active_thread != thread_id {
                bail!("active Codex thread changed before the model fork was committed");
            }
            let mut active_state = self
                .inner
                .thread_state
                .lock()
                .map_err(|_| anyhow!("thread state lock poisoned"))?;
            let mut active_child_model = self
                .inner
                .applied_subagent_model
                .lock()
                .map_err(|_| anyhow!("subagent model lock poisoned"))?;

            store.save_thread_and_model_selection(&new_thread_id, selection)?;
            *active_thread = new_thread_id.clone();
            *active_state = next_thread_state.clone();
            *active_child_model = child_model;
            Ok(())
        })();
        if let Err(error) = commit {
            if let Err(cleanup_error) = self
                .request_raw("thread/unsubscribe", json!({"threadId":new_thread_id}))
                .await
            {
                tracing::warn!(%cleanup_error, "could not detach uncommitted model fork");
            }
            return Err(error).context("could not commit the selected model fork");
        }

        if let Err(error) = self
            .request_raw("thread/unsubscribe", json!({"threadId":thread_id}))
            .await
        {
            tracing::warn!(%error, "could not detach previous thread after model fork");
        }
        Ok(next_thread_state)
    }

    pub(crate) fn save_model_selection(&self, selection: &ModelSelection) -> Result<()> {
        StateStore::new(&self.inner.config.state_dir).save_model_selection(selection)
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.ensure_connected().await?;
        self.request_raw(method, params).await
    }

    /// Queue a per-thread setting update and distinguish the RPC acknowledgement
    /// from the later settings-applied notification. A ticket remains current only
    /// until a newer request for the same thread and field supersedes it.
    pub async fn request_thread_setting_update(
        &self,
        thread_id: &str,
        params: Value,
        field: &'static str,
        expected: Value,
    ) -> Result<ThreadSettingUpdate> {
        if params.get("threadId").and_then(Value::as_str) != Some(thread_id) {
            bail!("thread/settings/update params do not match the requested thread");
        }
        let generation = self.current_generation();
        let key = ThreadSettingKey {
            generation,
            thread_id: thread_id.to_owned(),
            field: field.to_owned(),
        };
        let mut events = self.subscribe();
        let (revision, status_rx, superseded_pending, current_flag) = {
            let mut pending = self
                .inner
                .pending_thread_settings
                .lock()
                .map_err(|_| anyhow!("pending thread settings lock poisoned"))?;
            if let Some(existing) = pending.get(&key) {
                if existing.expected == expected
                    && matches!(
                        &*existing.status.borrow(),
                        ThreadSettingUpdateStatus::Pending
                    )
                {
                    return Ok(ThreadSettingUpdate::Pending(ThreadSettingUpdateTicket {
                        key,
                        revision: existing.revision,
                        inner: Arc::downgrade(&self.inner),
                        status: existing.status.subscribe(),
                        current: existing.current.clone(),
                    }));
                }
            }
            let mut superseded = false;
            let mut current = self
                .inner
                .thread_setting_current
                .lock()
                .map_err(|_| anyhow!("thread setting current-state lock poisoned"))?;
            let obsolete = current
                .keys()
                .filter(|old_key| old_key.thread_id == key.thread_id && old_key.field == key.field)
                .cloned()
                .collect::<Vec<_>>();
            for old_key in obsolete {
                if let Some(flag) = current.remove(&old_key).and_then(|flag| flag.upgrade()) {
                    flag.store(false, Ordering::Release);
                }
                if let Some(old) = pending.remove(&old_key)
                    && matches!(&*old.status.borrow(), ThreadSettingUpdateStatus::Pending)
                {
                    superseded = true;
                    old.status
                        .send_replace(ThreadSettingUpdateStatus::Superseded);
                }
            }
            let revision = self
                .inner
                .next_thread_setting_revision
                .fetch_add(1, Ordering::AcqRel);
            let (status_tx, status_rx) = watch::channel(ThreadSettingUpdateStatus::Pending);
            let current_flag = Arc::new(AtomicBool::new(true));
            current.insert(key.clone(), Arc::downgrade(&current_flag));
            pending.insert(
                key.clone(),
                PendingThreadSetting {
                    revision,
                    expected: expected.clone(),
                    status: status_tx,
                    current: current_flag.clone(),
                },
            );
            (revision, status_rx, superseded, current_flag)
        };

        let snapshot = match self
            .request(
                "thread/read",
                json!({"threadId": thread_id, "includeTurns": false}),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.finish_thread_setting(
                    &key,
                    revision,
                    ThreadSettingUpdateStatus::Failed(format!("{error:#}")),
                );
                self.forget_thread_setting_current(&key, &current_flag);
                return Err(error).context("read current thread settings before update");
            }
        };
        if response_thread_id(&snapshot).ok().as_deref() != Some(thread_id) {
            let error = anyhow!("thread/read returned a different thread than requested");
            self.finish_thread_setting(
                &key,
                revision,
                ThreadSettingUpdateStatus::Failed(error.to_string()),
            );
            self.forget_thread_setting_current(&key, &current_flag);
            return Err(error);
        }
        let summary = thread_state_summary(&snapshot);
        if !superseded_pending && setting_value(&summary, field) == Some(&expected) {
            if self.thread_setting_is_current(&key, revision) {
                self.finish_thread_setting(
                    &key,
                    revision,
                    ThreadSettingUpdateStatus::Applied(summary.clone()),
                );
                self.forget_thread_setting_current(&key, &current_flag);
                return Ok(ThreadSettingUpdate::Confirmed(summary));
            }
            return Ok(ThreadSettingUpdate::Pending(ThreadSettingUpdateTicket {
                key,
                revision,
                inner: Arc::downgrade(&self.inner),
                status: status_rx,
                current: current_flag.clone(),
            }));
        }
        if !self.thread_setting_is_current(&key, revision) {
            return Ok(ThreadSettingUpdate::Pending(ThreadSettingUpdateTicket {
                key,
                revision,
                inner: Arc::downgrade(&self.inner),
                status: status_rx,
                current: current_flag.clone(),
            }));
        }
        if let Err(error) = self.request("thread/settings/update", params).await {
            self.finish_thread_setting(
                &key,
                revision,
                ThreadSettingUpdateStatus::Failed(format!("{error:#}")),
            );
            self.forget_thread_setting_current(&key, &current_flag);
            return Err(error).context("queue thread settings update");
        }
        let native = self.clone();
        let watch_key = key.clone();
        let mut watcher_status = status_rx.clone();
        tokio::spawn(async move {
            loop {
                if !native.thread_setting_is_current(&watch_key, revision) {
                    break;
                }
                tokio::select! {
                    changed = watcher_status.changed() => {
                        if changed.is_err()
                            || !matches!(&*watcher_status.borrow(), ThreadSettingUpdateStatus::Pending)
                        {
                            break;
                        }
                    }
                    received = events.recv() => match received {
                    Ok(event) => {
                        let method = event.get("method").and_then(Value::as_str);
                        let event_generation = event
                            .pointer("/params/_discodex_connection_generation")
                            .and_then(Value::as_u64);
                        if method == Some("discodex/connection-closed")
                            && event_generation == Some(watch_key.generation)
                        {
                            native.finish_thread_setting(
                                &watch_key,
                                revision,
                                ThreadSettingUpdateStatus::Failed(
                                    "native connection closed before settings were confirmed"
                                        .to_owned(),
                                ),
                            );
                            break;
                        }
                        if method == Some("thread/settings/updated")
                            && event_generation == Some(watch_key.generation)
                            && let Some(summary) =
                                confirmed_thread_setting_event(&event, &watch_key, &expected)
                        {
                            native.finish_thread_setting(
                                &watch_key,
                                revision,
                                ThreadSettingUpdateStatus::Applied(summary),
                            );
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        native.finish_thread_setting(
                            &watch_key,
                            revision,
                            ThreadSettingUpdateStatus::Failed(format!(
                                "missed {count} native events before settings confirmation"
                            )),
                        );
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        native.finish_thread_setting(
                            &watch_key,
                            revision,
                            ThreadSettingUpdateStatus::Failed(
                                "native event stream closed before settings were confirmed"
                                    .to_owned(),
                            ),
                        );
                        break;
                    }
                    }
                }
            }
        });
        Ok(ThreadSettingUpdate::Pending(ThreadSettingUpdateTicket {
            key,
            revision,
            inner: Arc::downgrade(&self.inner),
            status: status_rx,
            current: current_flag,
        }))
    }

    fn thread_setting_is_current(&self, key: &ThreadSettingKey, revision: u64) -> bool {
        self.inner
            .pending_thread_settings
            .lock()
            .ok()
            .and_then(|pending| pending.get(key).map(|state| state.revision == revision))
            .unwrap_or(false)
    }

    fn forget_thread_setting_current(&self, key: &ThreadSettingKey, flag: &Arc<AtomicBool>) {
        if let Ok(mut current) = self.inner.thread_setting_current.lock()
            && current
                .get(key)
                .and_then(Weak::upgrade)
                .is_some_and(|value| Arc::ptr_eq(&value, flag))
        {
            current.remove(key);
        }
    }

    fn finish_thread_setting(
        &self,
        key: &ThreadSettingKey,
        revision: u64,
        status: ThreadSettingUpdateStatus,
    ) {
        let flag = if let Ok(mut pending) = self.inner.pending_thread_settings.lock()
            && pending
                .get(key)
                .is_some_and(|current| current.revision == revision)
        {
            pending.remove(key).map(|current| {
                current.status.send_replace(status);
                current.current
            })
        } else {
            None
        };
        if let Some(flag) = flag
            && Arc::strong_count(&flag) == 1
            && let Ok(mut current) = self.inner.thread_setting_current.lock()
            && current
                .get(key)
                .and_then(Weak::upgrade)
                .is_some_and(|value| Arc::ptr_eq(&value, &flag))
        {
            current.remove(key);
        }
    }

    pub(crate) async fn request_prepared(&self, method: &str, params: Value) -> Result<Value> {
        self.request_raw(method, params).await
    }

    pub(crate) async fn prepare_turn_input(&self) -> Result<Option<Value>> {
        let reconnecting = self.inner.connection_state.load(Ordering::Acquire) != 2;
        self.ensure_connected().await?;
        Ok(reconnecting.then(|| self.thread_state()))
    }

    pub(crate) async fn request_with_send_commit<T, F>(
        &self,
        method: &str,
        params: Value,
        commit: F,
    ) -> Result<(Value, T)>
    where
        F: FnOnce(&AtomicU64) -> Result<T>,
    {
        self.ensure_connected().await?;
        self.request_raw_with_commit(method, params, commit).await
    }

    async fn request_raw(&self, method: &str, params: Value) -> Result<Value> {
        self.request_raw_with_commit(method, params, |_| Ok(()))
            .await
            .map(|(response, ())| response)
    }

    async fn request_raw_with_commit<T, F>(
        &self,
        method: &str,
        params: Value,
        commit: F,
    ) -> Result<(Value, T)>
    where
        F: FnOnce(&AtomicU64) -> Result<T>,
    {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let key = id.to_string();
        let (tx, rx) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .map_err(|_| anyhow!("pending request lock poisoned"))?
            .insert(
                key.clone(),
                PendingRequest {
                    sender: tx,
                    method: method.to_owned(),
                    observation_fence: self
                        .inner
                        .next_observation_order
                        .load(Ordering::Relaxed)
                        .saturating_sub(1),
                },
            );
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut commit_failed = false;
        let sent = send_json_with_commit(
            &self.inner.writer,
            message,
            &self.inner.next_observation_order,
            |fence| commit(fence).inspect_err(|_| commit_failed = true),
        )
        .await;
        let committed = match sent {
            Ok(committed) => committed,
            Err(err) => {
                self.inner
                    .pending
                    .lock()
                    .map_err(|_| anyhow!("pending request lock poisoned"))?
                    .remove(&key);
                // A rejected synchronous call boundary did not send a frame;
                // retain the connection rather than treating it as a socket failure.
                if !commit_failed {
                    mark_disconnected(&self.inner);
                    self.inner.writer.lock().await.take();
                }
                return Err(err).with_context(|| format!("send native request {method}"));
            }
        };
        match rx
            .await
            .context("Codex app-server connection ended before response")?
        {
            Ok(result) => Ok((result, committed)),
            Err(error) => {
                record_native_request_error(&error);
                Err(error.into())
            }
        }
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<()> {
        self.ensure_connected().await?;
        send_json(
            &self.inner,
            json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        )
        .await
    }

    pub(crate) fn current_generation(&self) -> u64 {
        self.inner.connection_generation.load(Ordering::Acquire)
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.inner.connection_state.load(Ordering::Acquire) == 2
    }

    pub(crate) async fn respond_for_generation(
        &self,
        expected_generation: u64,
        id: Value,
        result: Value,
    ) -> Result<()> {
        self.ensure_connected().await?;
        send_json_for_generation(
            &self.inner,
            expected_generation,
            json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        )
        .await
    }

    pub(crate) async fn is_server_request_active(
        &self,
        connection_generation: u64,
        id: &Value,
    ) -> bool {
        self.inner
            .server_requests
            .lock()
            .await
            .contains_key(&server_request_key(connection_generation, id))
    }

    /// Registers the single lossless bridge consumer. Startup control events are
    /// drained before the live stream begins, preserving receive order.
    pub fn subscribe_bridge_events(&self) -> Result<mpsc::UnboundedReceiver<Value>> {
        subscribe_bridge_events(
            &self.inner.bridge_events,
            &self.inner.events,
            &self.inner.startup_events,
            &self.inner.live_events,
        )
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        activate_broadcast_events(
            &self.inner.events,
            &self.inner.startup_events,
            &self.inner.live_events,
        )
    }

    /// Voice reads this existing receive counter while holding its call-boundary lock.
    pub(crate) fn received_observation_fence(&self) -> &AtomicU64 {
        &self.inner.next_observation_order
    }

    pub fn thread_id(&self) -> String {
        self.inner
            .thread_id
            .lock()
            .map(|id| id.clone())
            .unwrap_or_default()
    }

    /// Registers the operation exposed through the app-server's process-scoped
    /// `discodex_voice` MCP server.
    pub fn set_voice_leave_handler(&self, handler: VoiceLeaveHandler) {
        if let Ok(mut current) = self.inner.voice_leave_handler.write() {
            *current = Some(handler);
        }
    }

    pub fn set_discord_tool_handler(&self, handler: DiscordToolHandler) {
        if let Ok(mut current) = self.inner.discord_tool_handler.write() {
            *current = Some(handler);
        }
    }

    /// Filesystem endpoint passed to the owned `voice-tools` MCP subprocess.
    pub fn voice_tool_endpoint(&self) -> PathBuf {
        self.inner.voice_tool_endpoint.clone()
    }

    /// Returns metadata observed in actual thread/start, thread/resume, and
    /// thread/settings/updated messages. It is a cache of native responses.
    pub fn thread_state(&self) -> Value {
        self.inner
            .thread_state
            .lock()
            .map(|state| state.clone())
            .unwrap_or(Value::Null)
    }

    /// The official catalog is scoped to the active provider. Include configured
    /// model names because local/custom providers may omit them from model/list.
    pub async fn model_catalog(&self) -> Result<Value> {
        self.model_catalog_with_agy_refresh(false).await
    }

    /// Explicitly refresh AGY metadata and return the combined catalog. A
    /// successful return means the new AGY snapshot was discovered and saved.
    pub async fn refresh_agy_model_catalog(&self) -> Result<Value> {
        self.model_catalog_with_agy_refresh(true).await
    }

    pub(crate) async fn cached_agy_catalog(&self) -> Result<agy_catalog_cache::CatalogLookup> {
        self.read_agy_catalog(false).await
    }

    async fn read_agy_catalog(&self, force: bool) -> Result<agy_catalog_cache::CatalogLookup> {
        let _guard = self.inner.agy_catalog_refresh.lock().await;
        let preferences = self.read_native_feature_preferences()?;
        if !preferences.antigravity_enabled {
            anyhow::bail!("AGY is not enabled for this profile");
        }
        let runtime = crate::agy::Runtime::for_preferences(&self.inner.config, &preferences)?;
        let context = format!(
            "profile={}\nexecutable={}\narguments={}",
            std::path::absolute(&self.inner.config.state_dir)
                .context("resolve AGY catalog profile state directory")?
                .display(),
            runtime.executable.display(),
            runtime.arguments.join("\0")
        );
        let context_key = context;
        let cache = agy_catalog_cache::CatalogCache::new(&self.inner.config.state_dir, context_key);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_secs();
        cache
            .get_or_refresh(now, force, || async {
                crate::agy::discover_catalog(&runtime).await
            })
            .await
    }

    async fn model_catalog_with_agy_refresh(&self, force_agy_refresh: bool) -> Result<Value> {
        let native_config = self
            .request(
                "config/read",
                json!({"includeLayers":false,"cwd":self.inner.config.project_dir}),
            )
            .await
            .context("read native model/provider configuration")?;
        let models =
            crate::model_catalog::read_pages(|params| self.request("model/list", params)).await?;
        let preferences = self.read_native_feature_preferences()?;
        let mut catalog = crate::model_catalog::assemble_catalog(
            models,
            &native_config,
            &self.thread_state(),
            self.inner.config.model.as_deref(),
            None,
        )?;
        let mut configured_agy_models = preferences.antigravity_models.clone();
        if preferences.subagent_model_executor.as_deref()
            == Some(ModelExecutor::Antigravity.as_str())
            && let Some(model) = preferences.subagent_model.as_ref()
        {
            configured_agy_models.push(model.clone());
        }
        let thread = self.thread_state();
        if thread.get("modelExecutor").and_then(Value::as_str)
            == Some(ModelExecutor::Antigravity.as_str())
            && let Some(model) = thread.get("model").and_then(Value::as_str)
        {
            configured_agy_models.push(model.to_owned());
        }
        crate::model_catalog::add_antigravity_models(
            &mut catalog,
            preferences.antigravity_enabled,
            &configured_agy_models,
        );
        if preferences.antigravity_enabled {
            match self.read_agy_catalog(force_agy_refresh).await {
                Ok(lookup) => {
                    if let Some(discovered) = lookup.catalog {
                        crate::model_catalog::add_antigravity_catalog(&mut catalog, &discovered);
                    }
                    if let Some(warning) = lookup.warning {
                        let mut warnings = catalog
                            .get("warnings")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        warnings.push(Value::String(warning));
                        catalog["warnings"] = Value::Array(warnings);
                    }
                }
                Err(error) if !force_agy_refresh => {
                    catalog["warnings"] = json!([format!("AGY model discovery failed: {error:#}")]);
                }
                Err(error) => return Err(error),
            }
        } else if force_agy_refresh {
            anyhow::bail!("AGY is not enabled for this profile");
        }
        Ok(catalog)
    }

    pub fn list_native_feature_settings(
        &self,
    ) -> Result<Vec<(NativeFeatureDescriptor, Option<Value>)>> {
        NativeFeatureStore::new(&self.inner.config.state_dir).list()
    }

    pub fn read_native_feature_preferences(&self) -> Result<NativeFeaturePreferences> {
        NativeFeatureStore::new(&self.inner.config.state_dir).read()
    }

    /// Read the persisted profile preferences together with Codex's effective config and origins.
    pub async fn read_native_feature_snapshot(&self) -> Result<NativeFeatureSnapshot> {
        let preferences = self.read_native_feature_preferences()?;
        let mut native_config = self
            .request(
                "config/read",
                feature_config_read_params(&self.inner.config.project_dir),
            )
            .await
            .context("read effective native feature configuration")?;
        let overrides = self
            .inner
            .applied_subagent_model
            .lock()
            .map_err(|_| anyhow!("subagent model lock poisoned"))?
            .clone();
        if let Some(entries) = overrides.as_object() {
            for (key, value) in entries {
                if let Some(field) = key.strip_prefix("agents.") {
                    native_config["config"]["agents"][field] = value.clone();
                }
            }
        }
        Ok(NativeFeatureSnapshot {
            preferences,
            native_config,
        })
    }

    /// Save per-profile choices for the next owned app-server start. The child-model
    /// preference is applied through per-thread config, retaining its provider affinity.
    pub async fn apply_native_feature_preferences(
        &self,
        preferences: &NativeFeaturePreferences,
    ) -> Result<NativeFeatureApplyResult> {
        self.apply_native_feature_preferences_with_catalog(preferences, None)
            .await
    }

    /// Applies preferences using a model catalog already fetched by the caller.
    /// This keeps a single settings operation from discovering the same catalog
    /// again while resolving a changed subagent model.
    pub async fn apply_native_feature_preferences_with_catalog(
        &self,
        preferences: &NativeFeaturePreferences,
        model_catalog: Option<&Value>,
    ) -> Result<NativeFeatureApplyResult> {
        let previous = self.read_native_feature_preferences()?;
        apply_native_feature_preferences_resolved(
            &NativeFeatureStore::new(&self.inner.config.state_dir),
            preferences,
            &previous,
            model_catalog,
            || self.model_catalog(),
        )
        .await
    }

    pub async fn permission_profiles(&self) -> Result<Value> {
        self.request(
            "permissionProfile/list",
            json!({"cwd": self.inner.config.project_dir}),
        )
        .await
    }

    /// Applies the official per-thread policy and persists the profile choice only
    /// after native state confirms it. No shared Codex configuration is written.
    pub async fn set_approval_policy_and_confirm(&self, policy: Value) -> Result<Value> {
        let mut events = self.subscribe();
        let summary = update_approval_policy(
            |method, params| self.request(method, params),
            &mut events,
            &self.thread_id(),
            &policy,
            |confirmed| {
                let store = NativeFeatureStore::new(&self.inner.config.state_dir);
                let mut preferences = store.read()?;
                preferences.approval_policy = confirmed.clone();
                store
                    .apply(&preferences)
                    .context("save the confirmed profile approval policy")?;
                Ok(())
            },
        )
        .await?;
        {
            let mut state = self
                .inner
                .thread_state
                .lock()
                .map_err(|_| anyhow!("thread state lock poisoned"))?;
            merge_thread_state(&mut state, &summary);
        }
        Ok(summary)
    }

    pub async fn set_permissions(&self, profile_id: &str) -> Result<Value> {
        self.request(
            "thread/settings/update",
            json!({
                "threadId": self.thread_id(),
                "permissions": profile_id
            }),
        )
        .await
    }

    /// Applies a native permission profile and confirms it from a settings notification
    /// or a fresh native resume response when the server emits no change notification.
    pub async fn set_permissions_and_confirm(&self, profile_id: &str) -> Result<Value> {
        let mut events = self.subscribe();
        self.set_permissions(profile_id).await?;
        let thread_id = self.thread_id();
        let notified = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let event = events.recv().await.context("native event stream closed")?;
                if event.get("method").and_then(Value::as_str) != Some("thread/settings/updated") {
                    continue;
                }
                let params = event.get("params").unwrap_or(&Value::Null);
                if params.get("threadId").and_then(Value::as_str) != Some(thread_id.as_str()) {
                    continue;
                }
                let settings = params
                    .get("threadSettings")
                    .cloned()
                    .context("thread/settings/updated omitted threadSettings")?;
                let active_id = settings
                    .pointer("/activePermissionProfile/id")
                    .and_then(Value::as_str);
                if active_id != Some(profile_id) {
                    bail!(
                        "Codex reported active permission profile {:?}, expected `{profile_id}`",
                        active_id
                    );
                }
                return Ok::<Value, anyhow::Error>(settings);
            }
        })
        .await;
        let summary = match notified {
            Ok(Ok(settings)) => thread_state_summary(&json!({ "thread": settings })),
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                let response = self
                    .request("thread/resume", json!({ "threadId": thread_id }))
                    .await
                    .context(
                        "read actual native permission settings after thread/settings/update",
                    )?;
                let actual = response
                    .pointer("/activePermissionProfile/id")
                    .and_then(Value::as_str);
                if actual != Some(profile_id) {
                    bail!(
                        "Codex thread/resume reports active permission profile {:?}, expected `{profile_id}`",
                        actual
                    );
                }
                thread_state_summary(&response)
            }
        };
        let mut state = self
            .inner
            .thread_state
            .lock()
            .map_err(|_| anyhow!("thread state lock poisoned"))?;
        merge_thread_state(&mut state, &summary);
        Ok(summary)
    }

    /// Reads thread/read without loading turns and merges fields that endpoint
    /// actually reports into the metadata last returned by start/resume/settings.
    /// thread/read omits some settings, so it does not erase those native values.
    pub async fn refresh_thread_state(&self) -> Result<Value> {
        let response = self
            .request(
                "thread/read",
                json!({ "threadId": self.thread_id(), "includeTurns": false }),
            )
            .await
            .context("read current Codex thread settings")?;
        let update = thread_state_summary(&response);
        let mut state = self
            .inner
            .thread_state
            .lock()
            .map_err(|_| anyhow!("thread state lock poisoned"))?;
        merge_thread_state(&mut state, &update);
        Ok(state.clone())
    }

    async fn validate_permission_profile(&self, profile_id: &str) -> Result<()> {
        let catalog = self
            .request_raw(
                "permissionProfile/list",
                json!({ "cwd": self.inner.config.project_dir }),
            )
            .await
            .context("read native permission profile catalog")?;
        let allowed = catalog
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|entry| {
                entry.get("id").and_then(Value::as_str) == Some(profile_id)
                    && entry.get("allowed").and_then(Value::as_bool) == Some(true)
            });
        if !allowed {
            bail!(
                "configured native permission profile `{profile_id}` is absent or not allowed by permissionProfile/list"
            );
        }
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<()> {
        external::cancel_all(&self.inner, None);
        let external_tasks: Vec<_> = self
            .inner
            .external_turns
            .lock()
            .map_err(|_| anyhow!("external turn lock poisoned"))?
            .values_mut()
            .filter_map(|turn| turn.task.take())
            .collect();
        for task in external_tasks {
            task.await.context("stop external executor")?;
        }
        let _ = self.inner.voice_ipc_shutdown.send(true);
        if let Some(task) = self.inner.voice_ipc_task.lock().await.take() {
            task.await
                .context("wait for voice tool IPC listener shutdown")?;
        }
        let _reconnect = self.inner.reconnect.lock().await;
        let already_stopped = self.inner.connection_state.swap(3, Ordering::AcqRel) == 3;
        if !already_stopped {
            if let Some(mut writer) = self.inner.writer.lock().await.take() {
                let _ = writer.close().await;
            }
        }
        let mut child_slot = self.inner.child.lock().await;
        if let Some(child) = child_slot.as_mut() {
            #[cfg(windows)]
            {
                child.kill().await.context("stop Codex app-server child")?;
                child.wait().await.context("reap Codex app-server child")?;
            }
            #[cfg(unix)]
            {
                stop_owned_child(child)
                    .await
                    .context("stop owned Codex app-server process group")?;
            }
            child_slot.take();
        }
        drop(child_slot);
        cleanup_owned_endpoints(
            &self.inner.endpoint,
            &self.inner.endpoint_dir,
            &self.inner.voice_tool_endpoint,
            &self.inner.voice_tool_endpoint_dir,
        )?;
        Ok(())
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        send_json(
            &self.inner,
            json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
        .await
    }
    async fn ensure_connected(&self) -> Result<()> {
        if self.inner.connection_state.load(Ordering::Acquire) == 3 {
            bail!("Codex app-server has been shut down");
        }
        if self.inner.connection_state.load(Ordering::Acquire) == 2 {
            return Ok(());
        }
        let _reconnect = self.inner.reconnect.lock().await;
        if self.inner.connection_state.load(Ordering::Acquire) == 3 {
            bail!("Codex app-server has been shut down");
        }
        if self.inner.connection_state.load(Ordering::Acquire) == 2 {
            return Ok(());
        }
        self.inner.connection_state.store(1, Ordering::Release);
        {
            let mut child = self.inner.child.lock().await;
            let child = child
                .as_mut()
                .ok_or_else(|| anyhow!("Codex app-server child has already shut down"))?;
            if let Some(status) = child
                .try_wait()
                .context("check Codex app-server connection state")?
            {
                bail!(
                    "Codex app-server process exited; restart the bridge to resume its saved thread (status {status})"
                );
            }
        }
        let stream = UnixStream::connect(&self.inner.endpoint)
            .await
            .with_context(|| {
                format!(
                    "reconnect to Codex socket {}",
                    self.inner.endpoint.display()
                )
            })?;
        let socket = websocket(stream)
            .await
            .context("reconnect Codex WebSocket")?;
        let (writer, reader) = socket.split();
        let generation = {
            let mut writer_slot = self.inner.writer.lock().await;
            let generation = self
                .inner
                .connection_generation
                .fetch_add(1, Ordering::AcqRel)
                + 1;
            *writer_slot = Some(writer);
            generation
        };
        spawn_reader(reader, Arc::downgrade(&self.inner), generation);

        let ready = async {
            self.request_raw("initialize", initialize_params()).await?;
            self.notify("initialized", json!({})).await?;
            let saved = StateStore::new(&self.inner.config.state_dir).load()?;
            let active_thread_id = self.thread_id();
            if !active_thread_id.is_empty()
                && saved.thread_id.as_deref() == Some(active_thread_id.as_str())
            {
                let mut params = json!({"threadId":active_thread_id});
                if let Some(selection) = saved.model_selection.as_ref() {
                    apply_model_selection(&mut params, selection);
                }
                let child_model = self
                    .inner
                    .applied_subagent_model
                    .lock()
                    .map_err(|_| anyhow!("subagent model lock poisoned"))?
                    .clone();
                merge_config_overrides(&mut params, &child_model);
                let response = self
                    .request_raw("thread/resume", params)
                    .await
                    .context("rejoin Codex thread after socket reconnect")?;
                if let Some(selection) = saved.model_selection.as_ref() {
                    validate_model_selection_response(&response, selection)?;
                }
                self.remember_subagent_model(child_model)?;
                let update = thread_state_summary(&response);
                let mut state = self
                    .inner
                    .thread_state
                    .lock()
                    .map_err(|_| anyhow!("thread state lock poisoned"))?;
                merge_thread_state(&mut state, &update);
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        match ready {
            Ok(()) => {
                self.inner.connection_state.store(2, Ordering::Release);
                Ok(())
            }
            Err(err) => {
                mark_disconnected(&self.inner);
                self.inner.writer.lock().await.take();
                Err(err)
            }
        }
    }
}

async fn connect_when_ready(endpoint: &PathBuf, child: &mut Child) -> Result<UnixStream> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut last_error = None;
    loop {
        if let Some(status) = child.try_wait().context("check Codex app-server startup")? {
            bail!("Codex app-server exited before opening its socket (status {status})");
        }
        match UnixStream::connect(endpoint).await {
            Ok(stream) => return Ok(stream),
            Err(err) => last_error = Some(err),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(last_error
                .map(anyhow::Error::from)
                .unwrap_or_else(|| anyhow!("socket was not created")))
            .with_context(|| format!("connect to Codex socket {}", endpoint.display()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn send_json(inner: &Inner, value: Value) -> Result<()> {
    send_json_with_commit(&inner.writer, value, &inner.next_observation_order, |_| {
        Ok(())
    })
    .await
}

pub(crate) async fn send_json_with_commit<S, T, F>(
    writer_lock: &AsyncMutex<Option<S>>,
    value: Value,
    received_fence: &AtomicU64,
    commit: F,
) -> Result<T>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
    F: FnOnce(&AtomicU64) -> Result<T>,
{
    let text = serde_json::to_string(&value)?;
    let mut sink = writer_lock.lock().await;
    let writer = sink
        .as_mut()
        .ok_or_else(|| anyhow!("Codex app-server WebSocket is disconnected"))?;
    futures_util::future::poll_fn(|cx| std::pin::Pin::new(&mut *writer).poll_ready(cx))
        .await
        .context("prepare Codex app-server WebSocket frame")?;
    // No await between this synchronous boundary and start_send. A later
    // flush/response failure does not undo the attempted native call boundary.
    let committed = commit(received_fence)?;
    std::pin::Pin::new(&mut *writer)
        .start_send(Message::Text(text.into()))
        .context("write Codex app-server WebSocket frame")?;
    writer
        .flush()
        .await
        .context("write Codex app-server WebSocket frame")?;
    Ok(committed)
}

async fn stop_owned_child(child: &mut Child) -> Result<()> {
    #[cfg(unix)]
    {
        use nix::errno::Errno;
        use nix::sys::signal::{Signal, kill, killpg};
        use nix::unistd::Pid;

        let process_group = child.id().map(|pid| Pid::from_raw(pid as i32));
        if let Some(group) = process_group {
            if let Err(error) = killpg(group, Signal::SIGTERM)
                && error != Errno::ESRCH
            {
                return Err(error).context("signal owned Codex app-server process group");
            }
        }
        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(error).context("wait for Codex app-server shutdown"),
            Err(_) => {
                if let Some(group) = process_group {
                    if let Err(error) = killpg(group, Signal::SIGKILL)
                        && error != Errno::ESRCH
                    {
                        return Err(error).context("kill surviving Codex app-server process group");
                    }
                } else {
                    child.kill().await.context("stop Codex app-server")?;
                }
                child.wait().await.context("reap Codex app-server")?;
            }
        };
        if let Some(group) = process_group {
            let group_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                match kill(Pid::from_raw(-group.as_raw()), None) {
                    Ok(()) => {
                        if tokio::time::Instant::now() >= group_deadline {
                            if let Err(error) = killpg(group, Signal::SIGKILL)
                                && error != Errno::ESRCH
                            {
                                return Err(error)
                                    .context("kill surviving Codex app-server process group");
                            }
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Err(Errno::ESRCH) => return Ok(()),
                    Err(error) => {
                        return Err(error).context("check owned Codex app-server process group");
                    }
                }
            }
            let kill_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                match kill(Pid::from_raw(-group.as_raw()), None) {
                    Ok(()) if tokio::time::Instant::now() < kill_deadline => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Ok(()) => {
                        return Err(anyhow!(
                            "owned Codex app-server process group survived SIGKILL"
                        ));
                    }
                    Err(Errno::ESRCH) => return Ok(()),
                    Err(error) => {
                        return Err(error).context("check killed Codex app-server process group");
                    }
                }
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        child.kill().await.context("stop Codex app-server")?;
        child.wait().await.context("reap Codex app-server")?;
        Ok(())
    }
}

fn with_stop_error(error: anyhow::Error, stop_error: Option<anyhow::Error>) -> anyhow::Error {
    match stop_error {
        Some(stop_error) => error.context(format!(
            "also failed to stop owned Codex process group: {stop_error:#}"
        )),
        None => error,
    }
}

async fn send_json_for_generation(
    inner: &Inner,
    expected_generation: u64,
    value: Value,
) -> Result<()> {
    let text = serde_json::to_string(&value)?;
    let mut sink = inner.writer.lock().await;
    let current_generation = inner.connection_generation.load(Ordering::Acquire);
    if current_generation != expected_generation {
        bail!(
            "native connection generation changed from {expected_generation} to {current_generation} before response"
        );
    }
    let request_key = server_request_key(
        expected_generation,
        value
            .get("id")
            .ok_or_else(|| anyhow!("generation-aware native response has no request ID"))?,
    );
    let mut requests = inner.server_requests.lock().await;
    claim_server_request(&mut requests, expected_generation, value.get("id").unwrap())?;
    let writer = sink
        .as_mut()
        .ok_or_else(|| anyhow!("Codex app-server WebSocket is disconnected"))?;
    writer
        .send(Message::Text(text.into()))
        .await
        .context("write Codex app-server WebSocket frame")
}

fn activate_broadcast_events(
    events: &broadcast::Sender<Value>,
    startup_events: &Mutex<Vec<Value>>,
    live_events: &AtomicBool,
) -> broadcast::Receiver<Value> {
    let receiver = events.subscribe();
    if !live_events.load(Ordering::Acquire) {
        if let Ok(mut buffered) = startup_events.lock() {
            for event in buffered.drain(..) {
                let _ = events.send(event);
            }
            live_events.store(true, Ordering::Release);
        }
    }
    receiver
}

fn subscribe_bridge_events(
    bridge_events: &Mutex<BridgeEventBuffer>,
    events: &broadcast::Sender<Value>,
    startup_events: &Mutex<Vec<Value>>,
    live_events: &AtomicBool,
) -> Result<mpsc::UnboundedReceiver<Value>> {
    // Activate and flush the legacy broadcast path too. Otherwise its startup
    // vector keeps growing even though the bridge has switched to its own queue.
    drop(activate_broadcast_events(
        events,
        startup_events,
        live_events,
    ));
    bridge_events
        .lock()
        .map_err(|_| anyhow!("native bridge event buffer lock poisoned"))?
        .subscribe()
}

fn publish_event(inner: &Inner, value: Value) {
    if let Ok(mut bridge_events) = inner.bridge_events.lock() {
        bridge_events.publish(value.clone());
    }
    if inner.live_events.load(Ordering::Acquire) {
        let _ = inner.events.send(value);
    } else if let Ok(mut events) = inner.startup_events.lock() {
        if inner.live_events.load(Ordering::Acquire) {
            let _ = inner.events.send(value);
        } else {
            events.push(value);
        }
    }
}

fn request_id_key(id: &Value) -> String {
    match id {
        Value::String(value) => format!("string:{value}"),
        Value::Number(value) => format!("number:{value}"),
        Value::Bool(value) => format!("bool:{value}"),
        Value::Null => "null".to_owned(),
        _ => format!("json:{id}"),
    }
}

fn server_request_key(generation: u64, id: &Value) -> ServerRequestKey {
    ServerRequestKey {
        generation,
        id: request_id_key(id),
    }
}

fn register_server_request(
    registry: &mut HashMap<ServerRequestKey, ServerRequestState>,
    generation: u64,
    id: &Value,
) {
    registry
        .entry(server_request_key(generation, id))
        .or_insert(ServerRequestState {
            generation,
            responded: false,
        });
}

fn resolve_server_request(
    registry: &mut HashMap<ServerRequestKey, ServerRequestState>,
    generation: u64,
    id: &Value,
) -> bool {
    registry
        .remove(&server_request_key(generation, id))
        .is_some()
}

fn claim_server_request(
    registry: &mut HashMap<ServerRequestKey, ServerRequestState>,
    generation: u64,
    id: &Value,
) -> Result<()> {
    let key = server_request_key(generation, id);
    let request = registry
        .get_mut(&key)
        .ok_or_else(|| anyhow!("native server request {key:?} is no longer active"))?;
    if request.generation != generation {
        bail!("native server request {key:?} belongs to another generation");
    }
    if request.responded {
        bail!("native server request {key:?} already has a response");
    }
    request.responded = true;
    Ok(())
}

fn cleanup_endpoint(endpoint: &Path, endpoint_dir: &Path) -> Result<()> {
    match std::fs::remove_file(endpoint) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("remove owned Codex socket {}", endpoint.display()));
        }
    }
    match std::fs::remove_dir(endpoint_dir) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "remove owned Codex socket directory {}",
                    endpoint_dir.display()
                )
            });
        }
    }
    Ok(())
}

fn cleanup_owned_endpoints(
    endpoint: &Path,
    endpoint_dir: &Path,
    voice_endpoint: &Path,
    voice_endpoint_dir: &Path,
) -> Result<()> {
    let endpoint_result = cleanup_endpoint(endpoint, endpoint_dir);
    let voice_result = cleanup_endpoint(voice_endpoint, voice_endpoint_dir);
    match (endpoint_result, voice_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(endpoint_error), Err(voice_error)) => Err(endpoint_error.context(format!(
            "voice tool endpoint cleanup also failed: {voice_error:#}"
        ))),
    }
}

/// Select a short, unique private directory below the user's temp directory.
/// macOS limits Unix-domain socket paths to a small fixed byte budget; keeping
/// the generated directory name short leaves room for both production socket
/// names while preserving a per-process private directory.
fn private_socket_endpoint_dir(temp_dir: &Path) -> PathBuf {
    temp_dir.join(format!("cw-{}", Uuid::new_v4().simple()))
}

async fn serve_voice_tool_ipc(
    mut listener: UnixListener,
    inner: Weak<Inner>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok(stream) => {
                        let Some(inner) = inner.upgrade() else { return };
                        tokio::spawn(async move {
                            if let Err(error) = handle_voice_tool_connection(stream, &inner).await {
                                tracing::warn!(%error, "voice tool MCP IPC request failed");
                            }
                        });
                    }
                    Err(error) => {
                        tracing::error!(%error, "voice tool MCP IPC listener stopped");
                        return;
                    }
                }
            }
        }
    }
}

async fn handle_voice_tool_connection(mut stream: UnixStream, inner: &Arc<Inner>) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut request_bytes = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        let read = stream
            .read(&mut byte)
            .await
            .context("read voice tool IPC request")?;
        if read == 0 {
            bail!("voice tool IPC closed before sending a request");
        }
        if byte[0] == b'\n' {
            break;
        }
        request_bytes.push(byte[0]);
    }
    let request: Value =
        serde_json::from_slice(&request_bytes).context("parse voice tool IPC JSON-RPC request")?;
    let id = request
        .get("id")
        .cloned()
        .context("voice tool IPC request has no JSON-RPC id")?;
    let response = if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        voice_rpc_error(id, -32600, "JSON-RPC version must be 2.0")
    } else {
        match request.get("method").and_then(Value::as_str) {
            Some(method @ ("thread/tool/list" | "thread/tool/call")) => {
                let mut params = request.get("params").cloned().unwrap_or_else(|| json!({}));
                let result = async {
                    let binding = external::take_binding(&mut params)?
                        .context("native tool request omitted external response binding")?;
                    external::validate_binding(inner, &binding)?;
                    let receipt = bind_native_tool_run(&mut params, &binding);
                    let (result, ()) = Native {
                        inner: inner.clone(),
                    }
                    .request_raw_with_commit(method, params, |_| {
                        external::validate_binding(inner, &binding)
                    })
                    .await?;
                    if let Some(receipt) = receipt {
                        external::record_native_call(inner, &binding, receipt);
                    }
                    Ok::<_, anyhow::Error>(result)
                }
                .await;
                native_tool_rpc_response(id, result)
            }
            Some("harness/call") => {
                let args = request.get("params").cloned().unwrap_or_else(|| json!({}));
                match handle_harness_call(inner, args).await {
                    Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                    Err(error) => voice_rpc_error(id, -32000, &format!("{error:#}")),
                }
            }
            Some("voice_leave") => {
                let args = request.get("params").cloned().unwrap_or_else(|| json!({}));
                match crate::voice_tools::parse_leave_summary(&args, inner.config.language) {
                    Err(error) => voice_rpc_error(id, -32602, &error.to_string()),
                    Ok(summary) => {
                        let handler = inner
                            .voice_leave_handler
                            .read()
                            .map_err(|_| anyhow!("voice tool handler lock poisoned"))?
                            .clone();
                        match handler {
                            Some(handler) => match handler(summary).await {
                                Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                                Err(error) => voice_rpc_error(id, -32000, &format!("{error:#}")),
                            },
                            None => {
                                voice_rpc_error(id, -32000, "voice_leave handler is not attached")
                            }
                        }
                    }
                }
            }
            Some(method) if is_discord_tool_method(method) => {
                let mut args = request.get("params").cloned().unwrap_or_else(|| json!({}));
                if !args.is_object() {
                    voice_rpc_error(id, -32602, "Discord tool parameters must be an object")
                } else {
                    let result = async {
                        if let Some(binding) = external::take_binding(&mut args)? {
                            external::validate_binding(inner, &binding)?;
                        }
                        let handler = inner
                            .discord_tool_handler
                            .read()
                            .map_err(|_| anyhow!("Discord tool handler lock poisoned"))?
                            .clone()
                            .context("Discord tool handler is not attached")?;
                        handler(method.to_owned(), args).await
                    }
                    .await;
                    native_tool_rpc_response(id, result)
                }
            }
            Some(method) => voice_rpc_error(id, -32601, &format!("method not found: {method}")),
            None => voice_rpc_error(id, -32600, "request has no JSON-RPC method"),
        }
    };
    let mut bytes = serde_json::to_vec(&response).context("serialize voice tool IPC response")?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .await
        .context("write voice tool IPC response")?;
    stream
        .flush()
        .await
        .context("flush voice tool IPC response")?;
    Ok(())
}

fn snapshot_belongs_to_primary(response: &Value, primary: &str) -> bool {
    !primary.is_empty() && response.pointer("/thread/id").and_then(Value::as_str) == Some(primary)
}

async fn handle_harness_call(inner: &Arc<Inner>, mut params: Value) -> Result<Value> {
    let binding = external::take_binding(&mut params)?;
    if let Some(binding) = &binding {
        external::validate_binding(inner, binding)?;
        bind_native_tool_run(&mut params, binding);
    }
    let caller = params
        .get("threadId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .context("Task tool call is missing its caller thread")?;
    let tool = params
        .get("tool")
        .and_then(Value::as_str)
        .context("Task tool name is missing")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        bail!("Task tool arguments must be an object");
    }
    let generation = inner.connection_generation.load(Ordering::Acquire);
    let weak = Arc::downgrade(inner);
    let rpc: crate::harness::Rpc = Arc::new(move |method, arguments| {
        let weak = weak.clone();
        let binding = binding.clone();
        Box::pin(async move {
            let inner = weak.upgrade().context("Codex connection ended")?;
            if inner.connection_generation.load(Ordering::Acquire) != generation {
                bail!("Codex reconnected; tool was not sent");
            }
            let native = Native { inner };
            let (value, ()) = native
                .request_raw_with_commit(&method, arguments, |_| {
                    if native.inner.connection_generation.load(Ordering::Acquire) != generation {
                        bail!("Codex reconnected; tool was not sent");
                    }
                    if let Some(binding) = &binding {
                        external::validate_binding(&native.inner, binding)?;
                    }
                    Ok(())
                })
                .await?;
            Ok(value)
        })
    });
    let context = crate::harness::ToolContext {
        rpc,
        events: inner.events.clone(),
        thread_id: caller.to_owned(),
        turn_id: params
            .get("turnId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .map(Ok)
            .unwrap_or_else(|| external::active_turn_id(inner, caller))?,
        thread_start: json!({
            "cwd":inner.config.project_dir,
            "ephemeral":false,
            "approvalPolicy":NativeFeatureStore::new(&inner.config.state_dir).read()?.approval_policy,
            "baseInstructions":crate::instructions::agent_base_instructions(&inner.config.name, inner.config.language)
        }),
    };
    if crate::task_tools::handles(tool) {
        crate::task_tools::execute(context, tool, arguments).await
    } else if crate::worktree_tools::handles(tool) {
        let service = inner
            .harness_worktrees
            .get_or_try_init(|| async {
                let home = inner
                    .config
                    .codex_home
                    .clone()
                    .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
                    .or_else(|| {
                        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                            .map(|path| PathBuf::from(path).join(".codex"))
                    })
                    .context("Cannot resolve Codex home for managed worktrees")?;
                crate::worktree_tools::WorktreeTools::new(home)
            })
            .await?;
        service.execute(context, tool, arguments).await
    } else {
        bail!("Unknown task/worktree tool: {tool}")
    }
}

fn is_discord_tool_method(method: &str) -> bool {
    matches!(
        method,
        "voice_join"
            | "discord_list_channels"
            | "discord_read_voice_inbox"
            | "discord_send_message"
            | "discord_get_messages"
            | "discord_download_attachment"
            | "discord_search_messages"
            | "discord_list_users"
    )
}

fn voice_rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc":"2.0",
        "id":id,
        "error":{"code":code,"message":message}
    })
}

fn bind_native_tool_run(
    params: &mut Value,
    binding: &crate::agy_mcp::NativeRunBinding,
) -> Option<Value> {
    params["threadId"] = json!(binding.thread_id);
    params["turnId"] = json!(binding.turn_id);
    params["runId"] = json!(binding.run_id);
    params
        .as_object_mut()
        .and_then(|params| params.remove("discodexMcpCall"))
}

fn native_tool_rpc_response(id: Value, result: Result<Value>) -> Value {
    match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(error) => {
            if let Some(native) = error.downcast_ref::<NativeRequestError>() {
                voice_rpc_error(id, native.code.unwrap_or(-32000), &format!("{error:#}"))
            } else {
                voice_rpc_error(id, -32000, &format!("{error:#}"))
            }
        }
    }
}

fn bridge_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None)
}

async fn websocket(stream: UnixStream) -> Result<Socket> {
    let request = "ws://localhost".into_client_request()?;
    let (socket, _) = client_async_with_config(request, stream, Some(bridge_websocket_config()))
        .await
        .context("complete Codex app-server WebSocket handshake")?;
    Ok(socket)
}

fn initialize_params() -> Value {
    json!({
        "clientInfo": {
            "name": "discodex",
            "title": "Discodex",
            "version": env!("CARGO_PKG_VERSION")
        },
        "capabilities": {
            "experimentalApi": true,
            "requestAttestation": false
        }
    })
}

fn merge_config_overrides(params: &mut Value, overrides: &Value) {
    if let Some(entries) = overrides.as_object() {
        if !entries.is_empty() && !params.get("config").is_some_and(Value::is_object) {
            params["config"] = json!({});
        }
        for (key, value) in entries {
            params["config"][key] = value.clone();
        }
    }
}
fn prepare_model_params_with_preferences(
    params: &mut Value,
    selection: Option<&ModelSelection>,
    preferences: &NativeFeaturePreferences,
) -> Result<Value> {
    if let Some(selection) = selection {
        if selection.executor == ModelExecutor::Antigravity && !preferences.antigravity_enabled {
            bail!(
                "Antigravity is disabled in this profile; enable it in settings to use this selection"
            );
        }
        apply_model_selection(params, selection);
    }
    let overrides = native_features::subagent_model_overrides(preferences);
    merge_config_overrides(params, &overrides);
    Ok(overrides)
}

fn apply_model_selection(params: &mut Value, selection: &ModelSelection) {
    params["model"] = Value::String(selection.model.clone());
    merge_config_overrides(
        params,
        &json!({"model_executor": selection.executor.as_str()}),
    );
    if selection.executor == ModelExecutor::Codex {
        params["modelProvider"] = Value::String(selection.provider.clone());
    }
}

fn preserve_thread_fork_settings(
    params: &mut Value,
    previous: &Value,
    configured_profile: Option<&str>,
) {
    for field in ["approvalPolicy", "approvalsReviewer", "serviceTier"] {
        if let Some(value) = previous.get(field).filter(|value| !value.is_null()) {
            params[field] = value.clone();
        }
    }
    if let Some(cwd) = previous.get("cwd").and_then(Value::as_str) {
        params["cwd"] = Value::String(cwd.to_owned());
    }
    if let Some(roots) = previous
        .get("runtimeWorkspaceRoots")
        .filter(|value| !value.is_null())
    {
        params["runtimeWorkspaceRoots"] = roots.clone();
    }
    if let Some(instructions) = previous
        .get("developerInstructions")
        .or_else(|| previous.pointer("/collaborationMode/settings/developerInstructions"))
        .filter(|value| !value.is_null())
    {
        params["developerInstructions"] = instructions.clone();
    }
    if let Some(profile) = previous
        .pointer("/activePermissionProfile/id")
        .and_then(Value::as_str)
    {
        params["permissions"] = Value::String(profile.to_owned());
    } else if previous
        .get("sandbox")
        .or_else(|| previous.get("sandboxPolicy"))
        .is_none_or(Value::is_null)
        && let Some(profile) = configured_profile
    {
        params["permissions"] = Value::String(profile.to_owned());
    }
    // A thread snapshot's `sandbox` is SandboxPolicy, while thread/fork accepts
    // only SandboxMode. When policy is observed, leave both unset so core fork
    // restores the persisted permission settings from the source history.
}

fn thread_state_after_fork(response: &Value, previous: &Value, fork_params: &Value) -> Value {
    let mut next = thread_state_summary(response);
    if next.get("collaborationMode").is_none()
        && let Some(value) = previous.get("collaborationMode")
    {
        next["collaborationMode"] = value.clone();
    }
    if next.get("developerInstructions").is_none() {
        if let Some(value) = fork_params
            .get("developerInstructions")
            .or_else(|| previous.get("developerInstructions"))
        {
            next["developerInstructions"] = value.clone();
        }
    }
    next
}

fn validate_model_selection_response(response: &Value, selection: &ModelSelection) -> Result<()> {
    let thread = thread_state_summary(response);
    let model = thread.get("model").and_then(Value::as_str);
    let provider = thread.get("modelProvider").and_then(Value::as_str);
    let executor = thread
        .get("modelExecutor")
        .and_then(Value::as_str)
        .unwrap_or(ModelExecutor::Codex.as_str());
    if model != Some(selection.model.as_str())
        || executor != selection.executor.as_str()
        || (selection.executor == ModelExecutor::Codex
            && provider != Some(selection.provider.as_str()))
    {
        bail!(
            "Codex applied executor/model/provider {executor}/{model:?}/{provider:?}, expected {}/{}/{}",
            selection.executor.as_str(),
            selection.model,
            selection.provider
        );
    }
    Ok(())
}
async fn resume_model_thread<F, Fut>(
    request: &mut F,
    events: &mut broadcast::Receiver<Value>,
    thread_id: &str,
    params: &Value,
    selection: &ModelSelection,
) -> Result<Value>
where
    F: FnMut(&'static str, Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    loop {
        match request("thread/resume", params.clone()).await {
            Ok(response) => {
                if response_thread_id(&response)? != thread_id {
                    bail!("Codex resumed a different thread");
                }
                validate_model_selection_response(&response, selection)?;
                return Ok(response);
            }
            Err(error)
                if error
                    .downcast_ref::<NativeRequestError>()
                    .is_some_and(NativeRequestError::is_thread_closing) =>
            {
                tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        let event = events
                            .recv()
                            .await
                            .context("native event stream closed while resuming thread")?;
                        match event.get("method").and_then(Value::as_str) {
                            Some("thread/closed")
                                if event.pointer("/params/threadId").and_then(Value::as_str)
                                    == Some(thread_id) =>
                            {
                                return Ok::<(), anyhow::Error>(());
                            }
                            Some("discodex/connection-closed") => {
                                bail!("Codex connection closed while waiting for thread unload")
                            }
                            _ => {}
                        }
                    }
                })
                .await
                .context("timed out waiting for Codex thread to close")??;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn switch_model_thread<F, Fut>(
    mut request: F,
    events: &mut broadcast::Receiver<Value>,
    thread_id: &str,
    params: &Value,
    selection: &ModelSelection,
    rollback: &Value,
    previous: &ModelSelection,
) -> Result<Value>
where
    F: FnMut(&'static str, Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let unsubscribe = json!({"threadId":thread_id});
    request("thread/unsubscribe", unsubscribe.clone()).await?;
    match resume_model_thread(&mut request, events, thread_id, params, selection).await {
        Ok(response) => Ok(response),
        Err(error) => {
            // A successful but mismatched resume may have reattached the listener already.
            let restored = async {
                request("thread/unsubscribe", unsubscribe).await?;
                resume_model_thread(&mut request, events, thread_id, rollback, previous).await
            }
            .await;
            match restored {
                Ok(_) => Err(error).context("could not switch model/provider; restored the previous conversation"),
                Err(restore_error) => Err(error).context(format!("could not switch model/provider; restoring the previous conversation also failed: {restore_error:#}")),
            }
        }
    }
}

fn response_thread_id(response: &Value) -> Result<String> {
    response
        .pointer("/thread/id")
        .or_else(|| response.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("Codex app-server returned no thread ID")
}

async fn update_approval_policy<R, F, S>(
    mut request: R,
    events: &mut broadcast::Receiver<Value>,
    thread_id: &str,
    policy: &Value,
    save_confirmed: S,
) -> Result<Value>
where
    R: FnMut(&'static str, Value) -> F,
    F: std::future::Future<Output = Result<Value>>,
    S: FnOnce(&Value) -> Result<()>,
{
    request(
        "thread/settings/update",
        json!({
            "threadId": thread_id, "approvalPolicy": policy
        }),
    )
    .await?;
    let notified = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = events.recv().await.context("native event stream closed")?;
            if event.get("method").and_then(Value::as_str) != Some("thread/settings/updated")
                || event.pointer("/params/threadId").and_then(Value::as_str) != Some(thread_id)
            {
                continue;
            }
            return event
                .pointer("/params/threadSettings")
                .cloned()
                .context("thread/settings/updated omitted threadSettings");
        }
    })
    .await;
    let response = match notified {
        Ok(result) => result?,
        // A no-op update need not emit a change notification; read the actual
        // native setting rather than treating the empty update response as proof.
        Err(_) => request("thread/resume", json!({"threadId": thread_id}))
            .await
            .context("read actual approval policy after thread/settings/update")?,
    };
    let summary = thread_state_summary(&response);
    if summary.get("approvalPolicy") != Some(policy) {
        bail!(
            "Codex reported approval policy {}, expected {}",
            summary.get("approvalPolicy").unwrap_or(&Value::Null),
            policy
        );
    }
    save_confirmed(policy)?;
    Ok(summary)
}

fn thread_state_summary(response: &Value) -> Value {
    let thread = response.get("thread").unwrap_or(response);
    let fields = [
        "id",
        "cwd",
        "ephemeral",
        "model",
        "modelProvider",
        "modelExecutor",
        "serviceTier",
        "runtimeWorkspaceRoots",
        "developerInstructions",
        "permissionProfile",
        "activePermissionProfile",
        "sandboxPolicy",
        "collaborationMode",
        "approvalPolicy",
        "sandbox",
        "approvalsReviewer",
        "status",
        "turns",
    ];
    let mut summary = serde_json::Map::new();
    for field in fields {
        if let Some(value) = thread.get(field).or_else(|| response.get(field)) {
            summary.insert(field.to_owned(), value.clone());
        }
    }
    // Thread snapshots use reasoningEffort; settings notifications use effort.
    // Keep one internal field so refreshes and UI consumers see the latest value.
    // Preserve explicit null; only an absent field preserves cached state.
    if let Some(effort) = thread
        .get("reasoningEffort")
        .or_else(|| response.get("reasoningEffort"))
        .or_else(|| thread.get("effort"))
        .or_else(|| response.get("effort"))
    {
        summary.insert("effort".to_owned(), effort.clone());
    }
    Value::Object(summary)
}

fn merge_thread_state(target: &mut Value, update: &Value) {
    if !target.is_object() {
        *target = Value::Object(serde_json::Map::new());
    }
    if let (Some(target), Some(update)) = (target.as_object_mut(), update.as_object()) {
        for (key, value) in update {
            target.insert(key.clone(), value.clone());
        }
    }
}

pub(crate) fn stamp_state_observation(value: &mut Value, generation: u64, order: u64) {
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "_discodex_connection_generation".to_owned(),
            json!(generation),
        );
        object.insert("_discodex_observation_order".to_owned(), json!(order));
    }
}

pub(crate) fn stamp_state_snapshot(value: &mut Value, generation: u64, order: u64, fence: u64) {
    stamp_state_observation(value, generation, order);
    if let Some(object) = value.as_object_mut() {
        object.insert("_discodex_observation_fence".to_owned(), json!(fence));
    }
}

pub(crate) fn state_observation_fence(value: &Value) -> Option<u64> {
    value.get("_discodex_observation_fence")?.as_u64()
}

pub(crate) fn state_observation_order(value: &Value) -> Option<(u64, u64)> {
    let generation = value.get("_discodex_connection_generation")?.as_u64()?;
    let order = value.get("_discodex_observation_order")?.as_u64()?;
    (generation > 0 && order > 0).then_some((generation, order))
}

fn spawn_reader(
    mut reader: futures_util::stream::SplitStream<Socket>,
    inner: Weak<Inner>,
    generation: u64,
) {
    tokio::spawn(async move {
        while let Some(frame) = reader.next().await {
            let frame = match frame {
                Ok(frame) => frame,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        connection_generation = generation,
                        "Codex app-server WebSocket reader stopped after an error"
                    );
                    break;
                }
            };
            let text = match frame {
                Message::Text(text) => text,
                Message::Close(_) => break,
                Message::Ping(data) => {
                    if let Some(inner) = inner.upgrade() {
                        let mut writer = inner.writer.lock().await;
                        if let Some(writer) = writer.as_mut() {
                            let _ = writer.send(Message::Pong(data)).await;
                        }
                    } else {
                        break;
                    }
                    continue;
                }
                _ => continue,
            };
            let Ok(mut value) = serde_json::from_str::<Value>(&text) else {
                tracing::warn!("Codex app-server sent an invalid JSON frame");
                continue;
            };
            let Some(inner) = inner.upgrade() else { break };
            // Assign once in receive order; direct replies and synthetic events share it.
            let observation_order = inner.next_observation_order.fetch_add(1, Ordering::Relaxed);
            if value.get("method").and_then(Value::as_str).is_some() {
                let method = value
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if matches!(
                    method.as_str(),
                    "turn/started"
                        | "turn/completed"
                        | "turn/plan/updated"
                        | "item/started"
                        | "item/completed"
                        | "item/agentMessage/delta"
                ) || method.starts_with("thread/realtime/")
                {
                    if let Some(params) = value.get_mut("params") {
                        stamp_state_observation(params, generation, observation_order);
                    }
                }
                if value.get("id").is_some() {
                    let mut requests = inner.server_requests.lock().await;
                    register_server_request(&mut requests, generation, value.get("id").unwrap());
                    if let Some(params) = value.get_mut("params").and_then(Value::as_object_mut) {
                        params.insert(
                            "_discodex_connection_generation".to_owned(),
                            json!(generation),
                        );
                    }
                }
                external::observe_completion(&inner, &value);
                if method == "turn/external/run" && value.get("id").is_some() {
                    if let Err(error) = external::spawn(inner.clone(), value.clone(), generation) {
                        let native = Native {
                            inner: inner.clone(),
                        };
                        let id = value["id"].clone();
                        tokio::spawn(async move {
                            let detail = crate::discord::sanitize_discord_error(
                                &native.inner.config,
                                &format!("{error:#}"),
                            );
                            if let Err(response_error) = native
                                .respond_for_generation(
                                    generation,
                                    id,
                                    json!({"success":false,"error":detail}),
                                )
                                .await
                            {
                                let response_detail = crate::discord::sanitize_discord_error(
                                    &native.inner.config,
                                    &format!("{response_error:#}"),
                                );
                                publish_native_failure(
                                    &native.inner,
                                    &format!(
                                        "Could not return the external execution error to Codex app-server. Native error: {detail}; response error: {response_detail}"
                                    ),
                                );
                            }
                        });
                    }
                    continue;
                }
                if method == "serverRequest/resolved" {
                    if let Some(params) = value.get_mut("params").and_then(Value::as_object_mut) {
                        params.insert(
                            "_discodex_connection_generation".to_owned(),
                            json!(generation),
                        );
                    }
                    if let Some(request_id) = value
                        .get("params")
                        .and_then(|params| params.get("requestId"))
                    {
                        let mut requests = inner.server_requests.lock().await;
                        resolve_server_request(&mut requests, generation, request_id);
                    }
                }
                if method == "thread/settings/updated" {
                    if let Some(params) = value.get_mut("params").and_then(Value::as_object_mut) {
                        params.insert(
                            "_discodex_connection_generation".to_owned(),
                            json!(generation),
                        );
                    }
                }
                if let Some(thread_id) = materialized_user_thread(&inner, &value) {
                    if let Err(error) =
                        StateStore::new(&inner.config.state_dir).save_thread_id(&thread_id)
                    {
                        tracing::error!(
                            %error,
                            "could not persist native thread after Codex materialized user input"
                        );
                    }
                }
                if value.get("method").and_then(Value::as_str) == Some("thread/settings/updated") {
                    if let Some(params) = value.get("params") {
                        let current_thread = inner.thread_id.lock().ok().map(|id| id.clone());
                        if current_thread.as_deref()
                            == params.get("threadId").and_then(Value::as_str)
                        {
                            if let Some(settings) = params.get("threadSettings") {
                                let update = thread_state_summary(&json!({ "thread": settings }));
                                if let Ok(mut state) = inner.thread_state.lock() {
                                    merge_thread_state(&mut state, &update);
                                }
                            }
                        }
                    }
                }
                publish_event(&inner, value);
                continue;
            }
            if let Some(id) = value.get("id") {
                let key = match id {
                    Value::String(value) => value.clone(),
                    _ => id.to_string(),
                };
                let (pending, map_poisoned) = take_pending_response(&inner.pending, &key);
                if map_poisoned {
                    publish_native_failure(
                        &inner,
                        "The native pending-response map lock was poisoned while routing a response.",
                    );
                }
                if let Some(pending) = pending {
                    let mut response = if let Some(error) = value.get("error") {
                        Err(NativeRequestError::from_response(
                            pending.method.clone(),
                            error,
                        ))
                    } else {
                        Ok(value.get("result").cloned().unwrap_or(Value::Null))
                    };
                    if matches!(
                        pending.method.as_str(),
                        "turn/start" | "thread/start" | "thread/read" | "thread/resume"
                    ) {
                        if let Ok(result) = response.as_mut() {
                            if pending.method == "turn/start" {
                                stamp_state_observation(result, generation, observation_order);
                            } else {
                                stamp_state_snapshot(
                                    result,
                                    generation,
                                    observation_order,
                                    pending.observation_fence,
                                );
                            }
                        }
                    }
                    if response.is_ok()
                        && matches!(
                            pending.method.as_str(),
                            "thread/start" | "thread/read" | "thread/resume"
                        )
                    {
                        if let Ok(result) = response.as_ref() {
                            let update = thread_state_summary(result);
                            // Background task reads/creates share the connection, but must not
                            // replace this Discord profile's primary session settings.
                            let primary = inner.thread_id.lock().ok().map(|id| id.clone());
                            if snapshot_belongs_to_primary(result, primary.as_deref().unwrap_or(""))
                            {
                                if let Ok(mut state) = inner.thread_state.lock() {
                                    merge_thread_state(&mut state, &update);
                                }
                            }
                            if let Ok(thread_id) = response_thread_id(result) {
                                publish_event(
                                    &inner,
                                    json!({
                                        "method": "discodex/thread-state",
                                        "params": {
                                            "threadId": thread_id,
                                            "thread": update,
                                            "_discodex_snapshot_method": pending.method,
                                            "_discodex_connection_generation": generation,
                                            "_discodex_observation_order": observation_order,
                                            "_discodex_observation_fence": pending.observation_fence
                                        }
                                    }),
                                );
                            }
                        }
                    }
                    let _ = pending.sender.send(response);
                    continue;
                }
            }
        }
        if let Some(inner) = inner.upgrade() {
            external::cancel_all(&inner, Some(generation));
            let connection_closed = json!({
                "jsonrpc": "2.0",
                "method": "discodex/connection-closed",
                "params": {
                    "_discodex_connection_generation": generation
                }
            });
            inner
                .server_requests
                .lock()
                .await
                .retain(|key, _| key.generation != generation);
            publish_event(&inner, connection_closed);
            if inner.connection_generation.load(Ordering::Acquire) == generation {
                mark_disconnected(&inner);
                inner.writer.lock().await.take();
                let (pending, map_poisoned) = drain_pending_responses(&inner.pending);
                if map_poisoned {
                    publish_native_failure(
                        &inner,
                        "The native pending-response map lock was poisoned while the connection closed.",
                    );
                }
                for pending in pending {
                    let _ = pending.sender.send(Err(NativeRequestError {
                        method: pending.method,
                        code: None,
                        message: if map_poisoned {
                            "connection closed while the pending-response map lock was poisoned"
                                .to_owned()
                        } else {
                            "connection closed".to_owned()
                        },
                    }));
                }
            }
        }
    });
}

fn materialized_user_thread(inner: &Inner, event: &Value) -> Option<String> {
    let params = event.get("params")?;
    let thread_id = params.get("threadId")?.as_str()?;
    if inner.thread_id.lock().ok()?.as_str() != thread_id {
        return None;
    }

    let method = event.get("method")?.as_str()?;
    let is_user_input = match method {
        // app-server emits this after adding an input item to the thread.
        "item/started" => {
            params.pointer("/item/type").and_then(Value::as_str) == Some("userMessage")
        }
        // Realtime transcript-done is a final, role-tagged native input event.
        "thread/realtime/transcript/done" => {
            params.get("role").and_then(Value::as_str) == Some("user")
        }
        // The completed realtime item is emitted after its canonical timeline commit.
        "thread/realtime/item/completed" => {
            params.pointer("/item/type").and_then(Value::as_str) == Some("transcriptSegment")
                && params.pointer("/item/role").and_then(Value::as_str) == Some("user")
        }
        _ => false,
    };
    is_user_input.then(|| thread_id.to_owned())
}

fn mark_disconnected(inner: &Inner) {
    let _ = inner
        .connection_state
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
            (state != 3).then_some(0)
        });
}

pub(crate) fn resolve_codex_path(path: &Path) -> Result<PathBuf> {
    if path.is_file() {
        return Ok(path.to_owned());
    }

    #[cfg(windows)]
    if let Some(expected_family) = windows_store_codex_package_family(path) {
        const POWERSHELL: &str = r#"$expected=$env:DISCODEX_EXPECTED_PACKAGE_FAMILY; $pkg=Get-AppxPackage -Name OpenAI.Codex | Where-Object { $_.Name -ceq 'OpenAI.Codex' -and $_.PackageFamilyName -ieq $expected } | Select-Object -First 1; if ($pkg) { $root=$pkg.InstallLocation; $candidate=Join-Path $root 'app/resources/codex.exe'; if (Test-Path -LiteralPath $candidate -PathType Leaf) { (Resolve-Path -LiteralPath $candidate).Path; exit 0 }; $manifestPath=Join-Path $root 'AppxManifest.xml'; if (Test-Path -LiteralPath $manifestPath) { [xml]$manifest=Get-Content -LiteralPath $manifestPath; foreach ($app in $manifest.Package.Applications.Application) { if ($app.Executable) { $entry=Join-Path $root $app.Executable; if ((Test-Path -LiteralPath $entry -PathType Leaf) -and ([IO.Path]::GetFileName($entry) -ieq 'codex.exe')) { (Resolve-Path -LiteralPath $entry).Path } } } } }"#;
        let output = std::process::Command::new("powershell.exe")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["-NoProfile", "-NonInteractive", "-Command", POWERSHELL])
            .env("DISCODEX_EXPECTED_PACKAGE_FAMILY", &expected_family)
            .output()
            .context("rediscover the selected OpenAI.Codex Store app core")?;
        if output.status.success() {
            if let Some(candidate) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .map(PathBuf::from)
                .find(|candidate| candidate.is_file())
            {
                if windows_store_codex_package_family(&candidate)
                    .is_some_and(|family| family == expected_family)
                {
                    return Ok(candidate);
                }
            }
        }
    }

    bail!("selected Codex executable is missing: {}", path.display())
}

#[cfg(windows)]
fn windows_store_codex_package_family(path: &Path) -> Option<String> {
    if !path
        .file_name()?
        .to_string_lossy()
        .eq_ignore_ascii_case("codex.exe")
    {
        return None;
    }
    let components: Vec<_> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    components.windows(2).find_map(|pair| {
        if !pair[0].eq_ignore_ascii_case("WindowsApps") {
            return None;
        }
        let identity: Vec<_> = pair[1].split('_').collect();
        if identity.len() == 5
            && identity[0].eq_ignore_ascii_case("OpenAI.Codex")
            && !identity[1].is_empty()
            && !identity[4].is_empty()
        {
            Some(format!(
                "{}_{}",
                identity[0].to_ascii_lowercase(),
                identity[4].to_ascii_lowercase()
            ))
        } else {
            None
        }
    })
}

#[cfg(all(test, windows))]
mod codex_path_tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn stale_store_core_is_recovered_only_for_the_recorded_codex_package() {
        let store_path = Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.930.4958.0_x64__2p2nqsd0c76g0\app\resources\codex.exe",
        );
        let updated_path = Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.930.7945.0_arm64__2p2nqsd0c76g0\app\resources\codex.exe",
        );
        let cli_path = Path::new(r"C:\Users\test\AppData\Local\Programs\Codex\codex.exe");
        let other_app = Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Other_1.0.0.0_x64__2p2nqsd0c76g0\app\resources\codex.exe",
        );
        let different_publisher = Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.930.7945.0_x64__differentpublisher\app\resources\codex.exe",
        );

        assert_eq!(
            windows_store_codex_package_family(store_path).as_deref(),
            Some("openai.codex_2p2nqsd0c76g0")
        );
        assert_eq!(
            windows_store_codex_package_family(store_path),
            windows_store_codex_package_family(updated_path)
        );
        assert_ne!(
            windows_store_codex_package_family(store_path),
            windows_store_codex_package_family(different_publisher)
        );
        assert_eq!(windows_store_codex_package_family(cli_path), None);
        assert_eq!(windows_store_codex_package_family(other_app), None);
    }
}

#[cfg(test)]
mod thread_state_tests {
    use super::*;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    #[test]
    fn model_fork_params_apply_requested_executor_and_keep_thread_settings() {
        let selection = ModelSelection {
            executor: ModelExecutor::Codex,
            provider: "example-provider".to_owned(),
            model: "example-model".to_owned(),
        };
        let mut params = json!({"threadId":"source-thread"});
        apply_model_selection(&mut params, &selection);
        preserve_thread_fork_settings(
            &mut params,
            &json!({
                "approvalPolicy":"on-request",
                "approvalsReviewer":"user",
                "serviceTier":"priority",
                "cwd":"/workspace/project",
                "runtimeWorkspaceRoots":["/workspace/project"],
                "developerInstructions":"keep current workspace guidance",
                "activePermissionProfile":{"id":"workspace-write"}
            }),
            Some("configured-profile"),
        );

        assert_eq!(params["threadId"], "source-thread");
        assert_eq!(params["model"], "example-model");
        assert_eq!(params["modelProvider"], "example-provider");
        assert_eq!(params["config"]["model_executor"], "codex");
        assert_eq!(params["approvalPolicy"], "on-request");
        assert_eq!(params["approvalsReviewer"], "user");
        assert_eq!(params["serviceTier"], "priority");
        assert_eq!(params["cwd"], "/workspace/project");
        assert_eq!(params["runtimeWorkspaceRoots"][0], "/workspace/project");
        assert_eq!(
            params["developerInstructions"],
            "keep current workspace guidance"
        );
        assert_eq!(params["permissions"], "workspace-write");
        assert!(params.get("sandbox").is_none());
    }

    #[test]
    fn fork_preserves_observed_sandbox_before_configured_profile_fallback() {
        let mut params = json!({"threadId":"source-thread"});
        preserve_thread_fork_settings(
            &mut params,
            &json!({"sandbox":{"type":"workspaceWrite", "networkAccess":false}}),
            Some("configured-profile"),
        );
        assert!(params.get("sandbox").is_none());
        assert!(params.get("permissions").is_none());
    }

    #[test]
    fn fork_response_keeps_effective_instructions_for_a_later_provider_switch() {
        let original_state = json!({
            "collaborationMode":{
                "mode":"custom",
                "settings":{"developerInstructions":"keep the current workflow"}
            }
        });
        let mut first_params = json!({"threadId":"source-thread"});
        preserve_thread_fork_settings(&mut first_params, &original_state, None);
        let first_response = json!({"thread":{
            "id":"first-fork",
            "model":"example-model",
            "modelProvider":"example-provider",
            "modelExecutor":"codex"
        }});
        let first_state = thread_state_after_fork(&first_response, &original_state, &first_params);

        let mut second_params = json!({"threadId":"first-fork"});
        preserve_thread_fork_settings(&mut second_params, &first_state, None);

        assert_eq!(
            second_params["developerInstructions"],
            "keep the current workflow"
        );
        assert_eq!(
            second_params["developerInstructions"],
            first_params["developerInstructions"]
        );
        assert_eq!(
            first_state["collaborationMode"],
            original_state["collaborationMode"]
        );
    }

    #[test]
    fn fork_response_validation_requires_the_requested_executor_and_model() {
        let selection = ModelSelection {
            executor: ModelExecutor::Codex,
            provider: "example-provider".to_owned(),
            model: "example-model".to_owned(),
        };
        let response = json!({"thread":{
            "id":"forked-thread",
            "model":"example-model",
            "modelProvider":"example-provider",
            "modelExecutor":"codex",
            "ephemeral":false
        }});
        validate_model_selection_response(&response, &selection).unwrap();
        let mismatched = json!({"thread":{
            "id":"forked-thread",
            "model":"other-model",
            "modelProvider":"example-provider",
            "modelExecutor":"codex"
        }});
        assert!(validate_model_selection_response(&mismatched, &selection).is_err());
    }

    #[tokio::test]
    async fn approval_policy_update_waits_for_matching_native_confirmation() {
        let (sender, mut events) = broadcast::channel(8);
        let mut calls = Vec::new();
        let result = update_approval_policy(
            |method, params| {
                calls.push((method.to_owned(), params));
                sender
                    .send(json!({"method":"thread/settings/updated","params":{
                        "threadId":"other-thread","threadSettings":{"approvalPolicy":"never"}
                    }}))
                    .unwrap();
                sender
                    .send(json!({"method":"thread/settings/updated","params":{
                        "threadId":"thread-example","threadSettings":{
                            "approvalPolicy":"on-request", "approvalsReviewer":"user"
                        }
                    }}))
                    .unwrap();
                std::future::ready(Ok(json!({})))
            },
            &mut events,
            "thread-example",
            &json!("on-request"),
            |_| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(result["approvalPolicy"], "on-request");
        assert_eq!(result["approvalsReviewer"], "user");
        assert_eq!(
            calls,
            vec![(
                "thread/settings/update".to_owned(),
                json!({
                    "threadId":"thread-example", "approvalPolicy":"on-request"
                })
            )]
        );
    }

    #[tokio::test]
    async fn approval_policy_readback_confirms_an_unchanged_explicit_never() {
        let (_sender, mut events) = broadcast::channel(8);
        let mut calls = Vec::new();
        let result = update_approval_policy(
            |method, params| {
                calls.push((method.to_owned(), params));
                std::future::ready(Ok(match method {
                    "thread/settings/update" => json!({}),
                    "thread/resume" => {
                        json!({"thread":{"id":"thread-example"},"approvalPolicy":"never"})
                    }
                    _ => panic!("unexpected method"),
                }))
            },
            &mut events,
            "thread-example",
            &json!("never"),
            |_| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(result["approvalPolicy"], "never");
        assert_eq!(
            calls[1],
            (
                "thread/resume".to_owned(),
                json!({"threadId":"thread-example"})
            )
        );
    }

    #[tokio::test]
    async fn approval_policy_rejection_and_mismatch_are_not_reported_as_success() {
        let (sender, mut events) = broadcast::channel(8);
        let error = update_approval_policy(
            |_, _| {
                sender
                    .send(json!({"method":"thread/settings/updated","params":{
                        "threadId":"thread-example","threadSettings":{"approvalPolicy":"never"}
                    }}))
                    .unwrap();
                std::future::ready(Ok(json!({})))
            },
            &mut events,
            "thread-example",
            &json!("on-request"),
            |_| Ok(()),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("never"));
        let mut calls = 0;
        let error = update_approval_policy(
            |_, _| {
                calls += 1;
                std::future::ready(Err(anyhow!("native requirements reject this policy")))
            },
            &mut events,
            "thread-example",
            &json!("never"),
            |_| Ok(()),
        )
        .await
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(error.to_string().contains("native requirements"));
    }

    #[tokio::test]
    async fn approval_policy_saves_only_matching_confirmation_to_the_selected_profile() {
        for (requested, effective, request_error, should_save) in [
            (json!("on-request"), json!("on-request"), None, true),
            (json!("never"), json!("never"), None, true),
            (json!("on-request"), json!("never"), None, false),
            (
                json!("never"),
                Value::Null,
                Some("native requirements rejected policy"),
                false,
            ),
        ] {
            let original = json!({"p":{"approvalPolicy":"untrusted","subagentModel":"p-child","codeMode":false},"q":{"approvalPolicy":"never","subagentModel":"q-child"},"shared":{"approval_policy":"never","model":"shared-model"}});
            let mut profiles = original.clone();
            let (sender, mut events) = broadcast::channel(8);
            let sequence = std::cell::RefCell::new(Vec::new());
            let result = update_approval_policy(
                |method, params| {
                    sequence.borrow_mut().push("request");
                    assert_eq!(method,"thread/settings/update");
                    assert_eq!(params,json!({"threadId":"thread-p","approvalPolicy":requested}));
                    if let Some(error) = request_error {
                        return std::future::ready(Err(anyhow!(error)));
                    }
                    sender.send(json!({"method":"thread/settings/updated","params":{"threadId":"thread-q","threadSettings":{"approvalPolicy":"never"}}})).unwrap();
                    sender.send(json!({"method":"thread/settings/updated","params":{"threadId":"thread-p","threadSettings":{"approvalPolicy":effective,"activePermissionProfile":{"id":"workspace-write"}}}})).unwrap();
                    std::future::ready(Ok(json!({})))
                },
                &mut events,"thread-p",&requested,
                |confirmed| {
                    sequence.borrow_mut().push("save");
                    profiles["p"]["approvalPolicy"] = confirmed.clone();
                    Ok(())
                },
            ).await;
            if should_save {
                let summary = result.unwrap();
                assert_eq!(summary["approvalPolicy"], requested);
                assert_eq!(summary["activePermissionProfile"]["id"], "workspace-write");
                assert_eq!(*sequence.borrow(), vec!["request", "save"]);
                let restored: NativeFeaturePreferences =
                    serde_json::from_value(profiles["p"].clone()).unwrap();
                assert_eq!(restored.approval_policy, requested);
                assert_eq!(restored.subagent_model.as_deref(), Some("p-child"));
                assert_eq!(restored.code_mode, Some(false));
            } else {
                let error = result.unwrap_err().to_string();
                assert!(error.contains(request_error.unwrap_or("Codex reported approval policy")));
                assert_eq!(*sequence.borrow(), vec!["request"]);
                assert_eq!(profiles, original);
            }
            assert_eq!(profiles["q"], original["q"]);
            assert_eq!(profiles["shared"], original["shared"]);
        }
    }

    #[tokio::test]
    async fn approval_policy_save_failure_is_not_success_and_keeps_the_native_cause() {
        let (sender, mut events) = broadcast::channel(4);
        let error=update_approval_policy(
            |_,_| {
                sender.send(json!({"method":"thread/settings/updated","params":{"threadId":"thread-p","threadSettings":{"approvalPolicy":"never"}}})).unwrap();
                std::future::ready(Ok(json!({})))
            }, &mut events,"thread-p",&json!("never"),
            |_| Err(anyhow!("profile persistence rejected the update")),
        ).await.unwrap_err();
        assert_eq!(error.to_string(), "profile persistence rejected the update");
    }

    #[test]
    fn history_fallback_requires_a_structured_unsupported_history_error() {
        for (code, message, expected) in [
            (-32601, "unknown method", true),
            (-32602, "unknown field historyMode", true),
            (-32600, "unsupported variant paginated", true),
            (-32600, "permission denied", false),
            (-32000, "thread/turns/list failed", false),
        ] {
            let error = anyhow::Error::new(NativeRequestError::from_response(
                "thread/turns/list".into(),
                &json!({"code":code,"message":message}),
            ));
            assert_eq!(is_history_pagination_unsupported(&error), expected);
        }
        assert!(!is_history_pagination_unsupported(&anyhow!(
            "unknown method"
        )));
    }

    #[test]
    fn configured_thread_settings_update_maps_off_to_null_and_omits_unset_values() {
        let config: Config = serde_json::from_value(json!({
            "profile": "worker",
            "name": "Worker",
            "tokenEnv": "TOKEN",
            "guildId": 1,
            "homeChannelId": 2,
            "transcriptChannelId": 3,
            "codexPath": "codex",
            "projectDir": ".",
            "stateDir": "state",
            "effort": "ultra",
            "serviceTier": "off"
        }))
        .expect("deserialize optional native defaults");

        assert_eq!(
            configured_thread_settings_update(&config, "thread-1"),
            Some(json!({
                "threadId": "thread-1",
                "effort": "ultra",
                "serviceTier": null
            }))
        );

        let unset: Config = serde_json::from_value(json!({
            "profile": "worker",
            "name": "Worker",
            "tokenEnv": "TOKEN",
            "guildId": 1,
            "homeChannelId": 2,
            "transcriptChannelId": 3,
            "codexPath": "codex",
            "projectDir": ".",
            "stateDir": "state"
        }))
        .expect("deserialize legacy config without native defaults");
        assert_eq!(configured_thread_settings_update(&unset, "thread-1"), None);
    }

    async fn receive_test_frame(
        config: WebSocketConfig,
        payload: Vec<u8>,
    ) -> Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>> {
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let sender = tokio::spawn(async move {
            let mut server = tokio_tungstenite::accept_async(server_stream).await?;
            server.send(Message::Binary(payload.into())).await?;
            Ok::<(), tokio_tungstenite::tungstenite::Error>(())
        });
        let request = "ws://localhost".into_client_request().unwrap();
        let (mut client, _) =
            tokio_tungstenite::client_async_with_config(request, client_stream, Some(config))
                .await
                .unwrap();
        let frame = client.next().await;
        drop(client);
        let _ = sender.await.expect("test server task should not panic");
        frame
    }

    #[tokio::test]
    async fn bridge_websocket_receives_a_single_frame_over_default_limits() {
        let payload = vec![0x5a; 17 * 1024 * 1024];
        let default_frame = receive_test_frame(WebSocketConfig::default(), payload.clone()).await;
        assert!(matches!(default_frame, Some(Err(_))));

        let config = bridge_websocket_config();
        assert_eq!(config.max_frame_size, None);
        assert_eq!(config.max_message_size, None);
        let defaults = WebSocketConfig::default();
        assert_eq!(config.read_buffer_size, defaults.read_buffer_size);
        assert_eq!(config.write_buffer_size, defaults.write_buffer_size);
        assert_eq!(config.max_write_buffer_size, defaults.max_write_buffer_size);
        assert_eq!(
            config.accept_unmasked_frames,
            defaults.accept_unmasked_frames
        );

        let received = receive_test_frame(config, payload.clone())
            .await
            .expect("server should send a frame")
            .expect("configured client should accept the large frame");
        match received {
            Message::Binary(bytes) => assert_eq!(bytes.as_ref(), payload.as_slice()),
            other => panic!("expected the binary frame, received {other:?}"),
        }
    }

    #[test]
    fn reader_observation_stamp_preserves_payload_and_matches_synthetic_duplicate() {
        let mut reply = json!({"thread": {"id": "thread", "status": {"type": "idle"}}});
        let mut event = json!({"threadId": "thread", "thread": reply["thread"].clone()});
        stamp_state_snapshot(&mut reply, 2, 41, 30);
        stamp_state_snapshot(&mut event, 2, 41, 30);
        assert_eq!(state_observation_order(&reply), Some((2, 41)));
        assert_eq!(
            state_observation_order(&event),
            state_observation_order(&reply)
        );
        assert_eq!(state_observation_fence(&reply), Some(30));
        assert_eq!(
            state_observation_fence(&event),
            state_observation_fence(&reply)
        );
        assert_eq!(reply["thread"]["id"], "thread");
        assert_eq!(event["thread"], reply["thread"]);
        assert!(
            thread_state_summary(&reply)
                .get("_discodex_observation_order")
                .is_none()
        );
        assert_eq!(state_observation_order(&json!({})), None);
    }

    #[test]
    fn native_effort_aliases_share_one_current_setting_across_refreshes() {
        let mut state = thread_state_summary(&json!({
            "thread": {"id":"thread-1"}, "reasoningEffort":"high"
        }));
        assert_eq!(state["effort"], "high");
        assert!(state.get("reasoningEffort").is_none());
        merge_thread_state(
            &mut state,
            &thread_state_summary(&json!({"thread":{"id":"thread-1"}})),
        );
        assert_eq!(state["effort"], "high");
        merge_thread_state(
            &mut state,
            &thread_state_summary(&json!({"thread":{"effort":"low"}})),
        );
        assert_eq!(state["effort"], "low");
        merge_thread_state(
            &mut state,
            &thread_state_summary(&json!({"thread":{"id":"thread-1", "reasoningEffort":"medium"}})),
        );
        assert_eq!(state["effort"], "medium");
        for response in [
            json!({"reasoningEffort":null, "effort":"high"}),
            json!({"thread":{"reasoningEffort":null, "effort":"high"}}),
        ] {
            merge_thread_state(&mut state, &thread_state_summary(&response));
            assert_eq!(state["effort"], Value::Null);
        }
        assert_eq!(
            thread_state_summary(&json!({"effort":"low"}))["effort"],
            "low"
        );
        assert_eq!(
            thread_state_summary(&json!({"reasoningEffort":"high", "effort":"low"}))["effort"],
            "high"
        );
    }

    #[test]
    fn thread_state_summary_preserves_observed_turn_snapshot() {
        let response = json!({
            "thread": {
                "id": "thread-1",
                "status": {"type": "idle"},
                "turns": [{"id": "turn-1", "status": "completed"}],
                "unmodeledField": "ignored"
            }
        });
        let summary = thread_state_summary(&response);
        assert_eq!(summary["id"], "thread-1");
        assert_eq!(summary["status"]["type"], "idle");
        assert_eq!(summary["turns"][0]["id"], "turn-1");
        assert!(summary.get("unmodeledField").is_none());
    }

    #[test]
    fn native_tool_rpc_response_preserves_typed_and_untyped_causes() {
        let typed = anyhow::Error::new(NativeRequestError::from_response(
            "thread/tool/call".into(),
            &json!({"code":-32602,"message":"tool argument `target` is missing"}),
        ))
        .context("execute native delegation tool");
        assert_eq!(
            native_tool_rpc_response(json!("request-a"), Err(typed)),
            json!({"jsonrpc":"2.0","id":"request-a","error":{
                "code":-32602,
                "message":"execute native delegation tool: Native request failed (thread/tool/call; code -32602): tool argument `target` is missing"
            }})
        );
        let untyped = anyhow!("connection closed").context("read native tool response");
        assert_eq!(
            native_tool_rpc_response(json!(2), Err(untyped)),
            json!({"jsonrpc":"2.0","id":2,"error":{
                "code":-32000,"message":"read native tool response: connection closed"
            }})
        );
        let result = json!({"output":{"type":"function_call_output","call_id":"call-a","output":"done"},"isError":false});
        assert_eq!(
            native_tool_rpc_response(json!("request-c"), Ok(result.clone())),
            json!({"jsonrpc":"2.0","id":"request-c","result":result})
        );
    }

    #[test]
    fn native_tool_run_binding_keeps_original_response_and_each_parent() {
        let arguments = json!({"threadId":"target-child","turnId":"tool-argument-turn"});
        for (parent, turn) in [("parent-a", "turn-a"), ("parent-b", "turn-b")] {
            let binding = crate::agy_mcp::NativeRunBinding {
                thread_id: parent.into(),
                turn_id: turn.into(),
                run_id: format!("{parent}-run"),
                generation: 3,
            };
            let receipt = json!({"name":"agent__send_message","arguments":arguments});
            let mut params = json!({
                "threadId":parent,"turnId":"model-supplied-turn","callId":"call-id",
                "namespace":"agent","name":"send_message","arguments":arguments,
                "discodexMcpCall":receipt
            });
            assert_eq!(bind_native_tool_run(&mut params, &binding), Some(receipt));
            assert_eq!(
                params,
                json!({
                    "threadId":parent,"turnId":turn,"runId":format!("{parent}-run"),"callId":"call-id",
                    "namespace":"agent","name":"send_message","arguments":arguments
                })
            );
        }
    }

    #[test]
    fn native_request_error_classifies_only_typed_turn_mismatch() {
        let mismatch = NativeRequestError::from_response(
            "turn/steer".to_owned(),
            &json!({
                "code": -32600,
                "message": "expected active turn id `old` but found `new`"
            }),
        );
        let mismatch: anyhow::Error = mismatch.into();
        let mismatch = mismatch
            .downcast_ref::<NativeRequestError>()
            .expect("anyhow conversion preserves native request error");
        assert!(mismatch.is_expected_turn_mismatch());

        let no_active = NativeRequestError::from_response(
            "turn/steer".to_owned(),
            &json!({"code": -32600, "message": "no active turn to steer"}),
        );
        assert!(no_active.is_expected_turn_mismatch());

        let transport = NativeRequestError::from_response(
            "turn/steer".to_owned(),
            &json!({"code": -32000, "message": "connection closed"}),
        );
        assert!(!transport.is_expected_turn_mismatch());

        let other_invalid_request = NativeRequestError::from_response(
            "turn/start".to_owned(),
            &json!({"code": -32600, "message": "invalid request"}),
        );
        assert!(!other_invalid_request.is_expected_turn_mismatch());
        assert_eq!(
            other_invalid_request.to_string(),
            "Native request failed (turn/start; code -32600): invalid request"
        );
        let missing_message =
            NativeRequestError::from_response("thread/resume".to_owned(), &json!({"code": -32602}));
        assert_eq!(
            missing_message.to_string(),
            "Native request failed (thread/resume)"
        );
        let empty_message = NativeRequestError::from_response(
            "thread/resume".to_owned(),
            &json!({"code": -32602, "message": "   "}),
        );
        assert_eq!(
            empty_message.to_string(),
            "Native request failed (thread/resume)"
        );
        record_native_request_error(&other_invalid_request);
    }

    #[test]
    fn native_request_error_logging_preserves_private_structured_fields() {
        struct BufferWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for BufferWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().expect("buffer lock").extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let captured = Arc::new(Mutex::new(Vec::new()));
        let error = NativeRequestError::from_response(
            "turn/steer".to_owned(),
            &json!({
                "code": -32600,
                "message": "expected active turn id `old` but found `new`"
            }),
        );
        let subscriber = tracing_subscriber::fmt()
            .with_writer({
                let captured = Arc::clone(&captured);
                move || BufferWriter(Arc::clone(&captured))
            })
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::WARN)
            .finish();
        tracing::subscriber::with_default(subscriber, || record_native_request_error(&error));

        let output = String::from_utf8(captured.lock().expect("buffer lock").clone())
            .expect("structured log is UTF-8");
        assert!(output.contains("native_method=turn/steer"));
        assert!(output.contains("native_error_code=Some(-32600)"));
        assert!(
            output.contains("native_error_message=expected active turn id `old` but found `new`")
        );
        assert_eq!(
            error.to_string(),
            "Native request failed (turn/steer; code -32600): expected active turn id `old` but found `new`"
        );
        let wrapped: anyhow::Error = error.into();
        assert!(wrapped.downcast_ref::<NativeRequestError>().is_some());
    }
}

#[cfg(test)]
mod server_request_registry_tests {
    use super::*;

    #[test]
    fn registry_claim_resolve_lifecycle_preserves_generation_and_id_type() {
        let mut registry = HashMap::new();
        let numeric = json!(1);
        let string = json!("1");
        register_server_request(&mut registry, 1, &numeric);
        register_server_request(&mut registry, 1, &string);
        register_server_request(&mut registry, 2, &numeric);
        assert!(!resolve_server_request(&mut registry, 3, &numeric));
        assert!(claim_server_request(&mut registry, 1, &numeric).is_ok());
        register_server_request(&mut registry, 1, &numeric);
        assert!(claim_server_request(&mut registry, 1, &numeric).is_err());
        assert!(claim_server_request(&mut registry, 1, &string).is_ok());
        assert!(resolve_server_request(&mut registry, 1, &numeric));
        assert!(claim_server_request(&mut registry, 1, &numeric).is_err());
        assert!(resolve_server_request(&mut registry, 2, &numeric));
        assert!(resolve_server_request(&mut registry, 1, &string));
    }

    #[test]
    fn registry_keys_preserve_id_type_and_generation() {
        let numeric = server_request_key(1, &json!(1));
        let string = server_request_key(1, &json!("1"));
        let next_generation = server_request_key(2, &json!(1));
        assert_ne!(numeric, string);
        assert_ne!(numeric, next_generation);
    }
}

#[cfg(test)]
mod harness_native_tests {
    use super::*;
    #[test]
    fn background_task_snapshots_do_not_replace_primary_settings() {
        assert!(snapshot_belongs_to_primary(
            &json!({"thread":{"id":"main"}}),
            "main"
        ));
        assert!(!snapshot_belongs_to_primary(
            &json!({"thread":{"id":"other"}}),
            "main"
        ));
        assert!(!snapshot_belongs_to_primary(
            &json!({"thread":{"id":"main"}}),
            ""
        ));
        assert!(!snapshot_belongs_to_primary(&json!({}), "main"));
    }
}

#[cfg(test)]
mod bridge_event_buffer_tests {
    use super::{BridgeEventBuffer, is_bridge_control_event};
    use serde_json::json;

    #[tokio::test]
    async fn startup_and_live_control_events_are_lossless_and_ordered() {
        let mut buffer = BridgeEventBuffer::new(true);
        for index in 0..300 {
            buffer.publish(json!({
                "method": "item/completed",
                "params": {"index": index}
            }));
        }

        let mut receiver = buffer.subscribe().expect("claim bridge receiver");
        buffer.publish(json!({
            "method": "turn/completed",
            "params": {"index": 300}
        }));

        for index in 0..301 {
            let event = receiver.recv().await.expect("receive queued event");
            let method = event["method"].as_str().expect("event method");
            let expected_method = if index == 300 {
                "turn/completed"
            } else {
                "item/completed"
            };
            assert_eq!(method, expected_method);
            assert_eq!(event["params"]["index"], index);
        }
    }

    #[test]
    fn bridge_queue_keeps_questions_and_discards_realtime_and_delta_events() {
        assert!(is_bridge_control_event(&json!({
            "method":"item/tool/requestUserInput",
            "id":17,
            "params":{"questions":[]}
        })));
        assert!(is_bridge_control_event(&json!({"method":"turn/completed"})));
        assert!(!is_bridge_control_event(
            &json!({"method":"item/agentMessage/delta"})
        ));
        assert!(!is_bridge_control_event(
            &json!({"method":"thread/realtime/audio"})
        ));
        assert!(!is_bridge_control_event(&json!({"id":17,"result":{}})));
    }

    #[test]
    fn bridge_receiver_can_be_claimed_only_once() {
        let mut buffer = BridgeEventBuffer::new(true);
        assert!(buffer.subscribe().is_ok());
        assert!(buffer.subscribe().is_err());
    }

    #[test]
    fn ordinary_native_instances_do_not_retain_bridge_control_events() {
        let mut buffer = BridgeEventBuffer::new(false);
        buffer.publish(serde_json::json!({"method":"turn/completed"}));

        assert!(buffer.startup_events.is_empty());
        assert!(buffer.subscribe().is_err());
    }

    #[tokio::test]
    async fn claiming_bridge_queue_also_flushes_legacy_startup_events() {
        use super::subscribe_bridge_events;
        use std::sync::Mutex;
        use std::sync::atomic::AtomicBool;
        use tokio::sync::broadcast;

        let (events, _) = broadcast::channel(8);
        let mut legacy_broadcast = events.subscribe();
        let startup_events = Mutex::new(vec![
            serde_json::json!({"method":"thread/started","params":{"index":0}}),
            serde_json::json!({"method":"turn/started","params":{"index":1}}),
        ]);
        let live_events = AtomicBool::new(false);
        let bridge_events = Mutex::new(BridgeEventBuffer::new(true));
        bridge_events
            .lock()
            .expect("bridge queue lock")
            .publish(serde_json::json!({"method":"turn/started","params":{"index":1}}));

        let mut bridge_receiver =
            subscribe_bridge_events(&bridge_events, &events, &startup_events, &live_events)
                .expect("claim bridge stream");

        assert!(live_events.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            startup_events
                .lock()
                .expect("startup queue lock")
                .is_empty()
        );
        assert_eq!(
            legacy_broadcast.recv().await.expect("first legacy event")["params"]["index"],
            0
        );
        assert_eq!(
            legacy_broadcast.recv().await.expect("second legacy event")["params"]["index"],
            1
        );

        assert_eq!(
            bridge_receiver.recv().await.expect("buffered bridge event")["params"]["index"],
            1
        );

        events
            .send(serde_json::json!({"method":"turn/completed"}))
            .expect("publish live event");
        assert_eq!(
            legacy_broadcast.recv().await.expect("live legacy event")["method"],
            "turn/completed"
        );
    }
}

#[cfg(test)]
#[path = "native_model_tests.rs"]
mod model_switch_tests;
