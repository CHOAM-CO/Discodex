//! Observations for the installation screen. No caller uses these to allow work.
use crate::config::Config;
use crate::i18n::{Language, MessageKey};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Observation {
    #[default]
    Unobserved,
    Passed,
    Incomplete {
        reason: String,
    },
    Failed {
        error: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub binding: String,
    pub run_id: String,
    pub thread_id: String,
    pub text: Observation,
    pub settings: Observation,
    pub question: Observation,
    pub approval: Observation,
    pub voice: Observation,
    pub handoff: Observation,
    pub transcript: Observation,
}

impl Report {
    pub fn ready(&self) -> bool {
        !self.thread_id.is_empty()
            && [
                &self.text,
                &self.settings,
                &self.approval,
                &self.voice,
                &self.handoff,
                &self.transcript,
            ]
            .iter()
            .all(|value| **value == Observation::Passed)
    }
    pub fn features(&self, language: Language) -> [(&'static str, &Observation); 7] {
        [
            (language.message(MessageKey::InstallText), &self.text),
            (
                language.message(MessageKey::InstallSettings),
                &self.settings,
            ),
            (
                language.message(MessageKey::InstallQuestions),
                &self.question,
            ),
            (
                language.message(MessageKey::InstallApprovals),
                &self.approval,
            ),
            (
                language.message(MessageKey::InstallTwoWayVoice),
                &self.voice,
            ),
            (
                language.message(MessageKey::InstallSameThreadHandoff),
                &self.handoff,
            ),
            (
                language.message(MessageKey::InstallFinalTranscript),
                &self.transcript,
            ),
        ]
    }
    pub fn summary(&self, language: Language) -> String {
        self.features(language)
            .iter()
            .map(|(name, observation)| {
                let detail = match observation {
                    Observation::Unobserved => {
                        language.message(MessageKey::InstallNotRunOrNotObserved)
                    }
                    Observation::Passed => language.message(MessageKey::InstallBehaviorConfirmed),
                    Observation::Incomplete { .. } => {
                        language.message(MessageKey::InstallSomeResultsAreNotYetConfirmed)
                    }
                    Observation::Failed { .. } => language
                        .message(MessageKey::InstallFailedSeeTheInstallationScreenAndPrivateL),
                };
                format!("{name}: {detail}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn binding(config: &Config, worker: &Path) -> Result<String> {
    let core = Sha256::digest(
        fs::read(&config.codex_path).context(
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnosticF4e7f5931179),
        )?,
    );
    let worker = Sha256::digest(
        fs::read(worker).context(
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnostic95ac1727669c),
        )?,
    );
    // No token or credential contents. These selected values identify this setup.
    let selected = json!({"profile":config.profile,"guild":config.guild_id,
        "home":config.home_channel_id,"transcript":config.transcript_channel_id,
        "operators":config.operator_ids,"voiceChannels":config.voice_channel_ids,
        "core":config.codex_path,"codexHome":config.codex_home,"project":config.project_dir,
        "state":config.state_dir,"model":config.model,"permissions":config.permissions,
        "voice":config.voice,"inputMode":config.input_mode,
        "coreSha":format!("{core:x}"),"workerSha":format!("{worker:x}")});
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&selected)?)
    ))
}

pub fn read(config: &Config) -> Result<Option<Report>> {
    let path = config.state_dir.join("installation-check.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context(
                crate::i18n::Language::default()
                    .message(crate::i18n::MessageKey::InstallDiagnostic3f9f034478d3),
            );
        }
    };
    let report: Report = serde_json::from_slice(&bytes).context(
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnosticAb449b135292),
    )?;
    let worker = config.worker_path.as_deref().context(
        crate::i18n::Language::default()
            .message(crate::i18n::MessageKey::InstallDiagnosticDe9c3b822f06),
    )?;
    Ok((report.binding == binding(config, worker)?).then_some(report))
}

type TurnKey = (u64, String, String);
#[derive(Default)]
struct TextTurn {
    input: u64,
    replied: bool,
    failed: bool,
    completed: bool,
}
struct Request {
    thread: String,
    turn: String,
    item: String,
    question: bool,
    answered: bool,
    resolved: bool,
    execution: Option<bool>,
    delivered: bool,
    delivery_failed: bool,
    failed: bool,
    terminal: Option<bool>,
}
struct VoiceCall {
    id: Uuid,
    thread: String,
    ssrc: Option<u32>,
    input: bool,
    output: bool,
    rr_after_output: Option<u32>,
    rr_progress: bool,
    roles: HashSet<(String, String)>,
    promotions: Vec<(u64, String, String)>,
}
struct State {
    report: Report,
    text: HashMap<TurnKey, TextTurn>,
    requests: HashMap<(u64, String), Request>,
    completed_turns: HashSet<(u64, String, String)>,
    completed_items: HashSet<(u64, String, String, String)>,
    settings: HashMap<u64, (String, bool, bool, Option<String>)>,
    call: Option<VoiceCall>,
}
pub struct Checks {
    path: PathBuf,
    language: Language,
    inner: Mutex<State>,
    updates: Option<tokio::sync::watch::Sender<Report>>,
}

impl Checks {
    pub fn start(config: &Config, thread_id: &str) -> Option<Arc<Self>> {
        let result = (|| -> Result<Arc<Self>> {
            let report = Report {
                binding: binding(config, &std::env::current_exe()?)?,
                run_id: Uuid::new_v4().to_string(),
                thread_id: thread_id.to_owned(),
                text: Observation::Unobserved,
                settings: Observation::Unobserved,
                question: Observation::Unobserved,
                approval: Observation::Unobserved,
                voice: Observation::Unobserved,
                handoff: Observation::Unobserved,
                transcript: Observation::Unobserved,
            };
            let (updates, mut receiver) = tokio::sync::watch::channel(report.clone());
            let checks = Arc::new(Self {
                path: config.state_dir.join("installation-check.json"),
                language: config.language,
                updates: Some(updates),
                inner: Mutex::new(State {
                    report,
                    text: HashMap::new(),
                    requests: HashMap::new(),
                    completed_turns: HashSet::new(),
                    completed_items: HashSet::new(),
                    settings: HashMap::new(),
                    call: None,
                }),
            });
            checks.save(
                &checks
                    .inner
                    .lock()
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "{}",
                            crate::i18n::Language::default()
                                .message(crate::i18n::MessageKey::InstallDiagnostic68513f2fa54e)
                        )
                    })?
                    .report,
            )?;
            let weak = Arc::downgrade(&checks);
            tokio::spawn(async move {
                while receiver.changed().await.is_ok() {
                    let report = receiver.borrow_and_update().clone();
                    let Some(checks) = weak.upgrade() else {
                        break;
                    };
                    // Filesystem IO is off the native event and audio callback paths.
                    match tokio::task::spawn_blocking(move || checks.save(&report)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "cannot store installation observations; bridge continues")
                        }
                        Err(error) => {
                            tracing::warn!(%error, "installation observation writer ended; bridge continues")
                        }
                    }
                }
            });
            Ok(checks)
        })();
        match result {
            Ok(checks) => Some(checks),
            Err(error) => {
                tracing::warn!(%error, "installation observations unavailable; bridge continues");
                None
            }
        }
    }
    fn save(&self, report: &Report) -> Result<()> {
        let parent = self.path.parent().context(
            crate::i18n::Language::default()
                .message(crate::i18n::MessageKey::InstallDiagnosticEf044f93e4ee),
        )?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(&serde_json::to_vec_pretty(report)?)?;
        temp.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
    fn update(&self, f: impl FnOnce(&mut State)) {
        // Observation contention must not stop an audio frame or native operation.
        let Ok(mut state) = self.inner.try_lock() else {
            return;
        };
        let previous = state.report.clone();
        f(&mut state);
        Self::complete(&mut state);
        if state.report != previous {
            if let Some(updates) = &self.updates {
                updates.send_replace(state.report.clone());
            }
        }
    }
    pub fn reset_thread(&self, thread: &str) {
        self.update(|state| {
            if state.report.thread_id == thread {
                return;
            }
            state.report.thread_id = thread.to_owned();
            state.report.text = Observation::Unobserved;
            state.report.settings = Observation::Unobserved;
            state.report.question = Observation::Unobserved;
            state.report.approval = Observation::Unobserved;
            state.report.voice = Observation::Unobserved;
            state.report.handoff = Observation::Unobserved;
            state.report.transcript = Observation::Unobserved;
            state.text.clear();
            state.requests.clear();
            state.completed_turns.clear();
            state.completed_items.clear();
            state.settings.clear();
            state.call = None;
        });
    }
    pub fn summary(&self) -> String {
        self.inner
            .lock()
            .map(|state| state.report.summary(self.language))
            .unwrap_or_else(|_| {
                self.language
                    .text(
                        "Could not read installation observations.",
                        "설치 기능 관측을 읽지 못했습니다.",
                        "无法读取安装功能观察结果。",
                        "インストール機能の観測結果を読み込めませんでした。",
                    )
                    .to_owned()
            })
    }
    pub fn input_consumed(&self, generation: u64, thread: &str, turn: &str, input: u64) {
        self.update(|state| {
            if generation > 0 && input > 0 && thread == state.report.thread_id && !turn.is_empty() {
                let text = state
                    .text
                    .entry((generation, thread.to_owned(), turn.to_owned()))
                    .or_default();
                if text.input != input {
                    *text = TextTurn {
                        input,
                        ..Default::default()
                    };
                }
            }
        });
    }
    pub fn reply_posted(
        &self,
        generation: u64,
        thread: &str,
        turn: &str,
        input: u64,
        body_bytes: usize,
        error: Option<&str>,
    ) {
        self.update(|state| {
            if let Some(text) =
                state
                    .text
                    .get_mut(&(generation, thread.to_owned(), turn.to_owned()))
            {
                if text.input != input || body_bytes == 0 {
                    return;
                }
                match error {
                    None => text.replied = true,
                    Some(error) => {
                        text.failed = true;
                        state.report.text = Observation::Failed {
                            error: error.to_owned(),
                        };
                    }
                }
            }
        });
    }
    pub fn text_error(&self, thread: &str, error: &str) {
        self.update(|state| {
            if thread == state.report.thread_id {
                state.report.text = Observation::Failed {
                    error: error.to_owned(),
                };
            }
        });
    }
    pub fn settings_confirmed(
        &self,
        interaction: u64,
        thread: &str,
        defaults_match: bool,
        current_applied: bool,
        error: Option<&str>,
    ) {
        self.update(|state| {
            state.settings.insert(
                interaction,
                (
                    thread.to_owned(),
                    defaults_match,
                    current_applied,
                    error.map(str::to_owned),
                ),
            );
        });
    }
    pub fn settings_response(&self, interaction: u64, response_error: Option<&str>) {
        self.update(|state| {
            if let Some((thread, defaults_match, current_applied, error)) =
                state.settings.remove(&interaction)
            {
                if thread != state.report.thread_id {
                    return;
                }
                let error = error.or_else(|| response_error.map(str::to_owned));
                state.report.settings = if defaults_match && current_applied && error.is_none() {
                    Observation::Passed
                } else if let Some(error) = error {
                    Observation::Failed { error }
                } else {
                    Observation::Incomplete {
                        reason:
                            "요청한 이후 설정이나 진행 중 설정의 적용값을 아직 확인하지 못했습니다."
                                .to_owned(),
                    }
                };
            }
        });
    }
    pub fn request_published(&self, generation: u64, id: &Value, method: &str, params: &Value) {
        self.update(|state| {
            let Some(thread) = params
                .get("threadId")
                .and_then(Value::as_str)
                .filter(|thread| *thread == state.report.thread_id)
            else {
                return;
            };
            let (Some(turn), Some(item)) = (
                params.get("turnId").and_then(Value::as_str),
                params.get("itemId").and_then(Value::as_str),
            ) else {
                return;
            };
            if generation == 0 || turn.is_empty() || item.is_empty() {
                return;
            }
            let question = matches!(
                method,
                "item/tool/requestUserInput" | "mcpServer/elicitation/request"
            );
            if !question
                && !matches!(
                    method,
                    "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
                )
            {
                return;
            }
            state.requests.insert(
                (generation, id.to_string()),
                Request {
                    thread: thread.to_owned(),
                    turn: turn.to_owned(),
                    item: item.to_owned(),
                    question,
                    answered: false,
                    resolved: false,
                    execution: None,
                    delivered: false,
                    delivery_failed: false,
                    failed: false,
                    terminal: None,
                },
            );
        });
    }
    pub fn request_answered(&self, generation: u64, id: &Value) {
        self.update(|state| {
            if let Some(request) = state.requests.get_mut(&(generation, id.to_string())) {
                request.answered = true;
            }
        });
    }
    pub fn request_error(&self, method: &str, params: &Value, error: &str) {
        self.update(|state| {
            if params.get("threadId").and_then(Value::as_str)
                != Some(state.report.thread_id.as_str())
            {
                return;
            }
            let question = matches!(
                method,
                "item/tool/requestUserInput" | "mcpServer/elicitation/request"
            );
            if question {
                state.report.question = Observation::Failed {
                    error: error.to_owned(),
                };
            } else if matches!(
                method,
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
            ) {
                state.report.approval = Observation::Failed {
                    error: error.to_owned(),
                };
            }
            for request in state.requests.values_mut() {
                if request.question == question
                    && params.get("turnId").and_then(Value::as_str) == Some(request.turn.as_str())
                    && params.get("itemId").and_then(Value::as_str) == Some(request.item.as_str())
                {
                    request.failed = true;
                }
            }
        });
    }
    pub fn native_event(&self, method: &str, params: &Value) {
        self.update(|state| {
            let Some(thread) = params
                .get("threadId")
                .and_then(Value::as_str)
                .filter(|thread| *thread == state.report.thread_id)
            else {
                return;
            };
            let generation = params
                .get("_discodex_connection_generation")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if method == "serverRequest/resolved" {
                if let Some(id) = params.get("requestId") {
                    if let Some(request) = state.requests.get_mut(&(generation, id.to_string())) {
                        request.resolved = true;
                    }
                }
            }
            if method == "turn/completed" {
                if let (Some(turn), Some(status)) = (
                    params.pointer("/turn/id").and_then(Value::as_str),
                    params.pointer("/turn/status").and_then(Value::as_str),
                ) {
                    if matches!(status, "completed" | "failed" | "interrupted") {
                        let completed = status == "completed";
                        if completed
                            && state.call.is_some()
                            && state.report.handoff != Observation::Passed
                        {
                            state.completed_turns.insert((
                                generation,
                                thread.to_owned(),
                                turn.to_owned(),
                            ));
                        }
                        if let Some(text) =
                            state
                                .text
                                .get_mut(&(generation, thread.to_owned(), turn.to_owned()))
                        {
                            text.completed = true;
                            text.failed |= !completed;
                            if !completed {
                                state.report.text = Observation::Failed {
                                    error: format!("native turn ended with {status}"),
                                };
                            }
                        }
                        for ((request_generation, _), request) in state.requests.iter_mut() {
                            if *request_generation == generation
                                && request.thread == thread
                                && request.turn == turn
                            {
                                request.terminal = Some(completed);
                            }
                        }
                    }
                }
            }
            if method == "item/completed" {
                let item = &params["item"];
                let (Some(turn), Some(item_id)) = (
                    params.get("turnId").and_then(Value::as_str),
                    item.get("id").and_then(Value::as_str),
                ) else {
                    return;
                };
                let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
                if !matches!(kind, "commandExecution" | "fileChange") {
                    return;
                }
                let successful = item.get("status").and_then(Value::as_str) == Some("completed")
                    && (kind != "commandExecution"
                        || item.get("exitCode").and_then(Value::as_i64) == Some(0));
                if successful && state.call.is_some() && state.report.handoff != Observation::Passed
                {
                    state.completed_items.insert((
                        generation,
                        thread.to_owned(),
                        turn.to_owned(),
                        item_id.to_owned(),
                    ));
                }
                for ((request_generation, _), request) in state.requests.iter_mut() {
                    if *request_generation == generation
                        && request.thread == thread
                        && request.turn == turn
                        && request.item == item_id
                        && !request.question
                    {
                        request.execution = Some(successful);
                    }
                }
            }
        });
    }
    pub fn approval_result_posted(&self, params: &Value, error: Option<&str>) {
        self.update(|state| {
            let generation = params
                .get("_discodex_connection_generation")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            for ((request_generation, _), request) in state.requests.iter_mut() {
                if *request_generation != generation || request.question {
                    continue;
                }
                if params.get("threadId").and_then(Value::as_str) == Some(request.thread.as_str())
                    && params.get("turnId").and_then(Value::as_str) == Some(request.turn.as_str())
                    && params.pointer("/item/id").and_then(Value::as_str)
                        == Some(request.item.as_str())
                {
                    request.delivered = error.is_none();
                    request.delivery_failed = error.is_some();
                    if let Some(error) = error {
                        state.report.approval = Observation::Failed {
                            error: error.to_owned(),
                        };
                    }
                }
            }
        });
    }
    pub fn note_voice_begin(&self, thread: &str, call: &Uuid) {
        self.update(|state| {
            if thread == state.report.thread_id {
                state.report.voice = Observation::Unobserved;
                state.report.handoff = Observation::Unobserved;
                state.report.transcript = Observation::Unobserved;
                state.completed_turns.clear();
                state.completed_items.clear();
                state.call = Some(VoiceCall {
                    id: *call,
                    thread: thread.to_owned(),
                    ssrc: None,
                    input: false,
                    output: false,
                    rr_after_output: None,
                    rr_progress: false,
                    roles: HashSet::new(),
                    promotions: Vec::new(),
                });
            }
        });
    }
    pub fn note_voice_end(&self, thread: &str, call: &Uuid) {
        self.update(|state| {
            if Self::call(state, thread, call).is_some() {
                state.call = None;
                state.completed_turns.clear();
                state.completed_items.clear();
            }
        });
    }
    fn call<'a>(state: &'a mut State, thread: &str, call: &Uuid) -> Option<&'a mut VoiceCall> {
        if state.report.thread_id != thread {
            return None;
        }
        state
            .call
            .as_mut()
            .filter(|active| active.id == *call && active.thread == thread)
    }
    pub fn note_voice_connected(&self, thread: &str, call: &Uuid, ssrc: u32) {
        self.update(|state| {
            if let Some(active) = Self::call(state, thread, call) {
                let changed = active.ssrc != Some(ssrc);
                if changed {
                    active.output = false;
                    active.rr_after_output = None;
                    active.rr_progress = false;
                }
                active.ssrc = Some(ssrc);
                if changed {
                    state.report.voice = Observation::Unobserved;
                }
            }
        });
    }
    pub fn note_voice_in(&self, thread: &str, call: &Uuid, samples: u64) {
        self.update(|state| {
            if let Some(active) = Self::call(state, thread, call) {
                active.input |= samples > 0;
            }
        });
    }
    pub fn note_voice_out(&self, thread: &str, call: &Uuid, samples: u64) {
        self.update(|state| {
            if let Some(active) = Self::call(state, thread, call) {
                active.output |= samples > 0;
            }
        });
    }
    pub fn note_voice_rr(&self, thread: &str, call: &Uuid, ssrc: u32, sequence: u32) {
        self.update(|state| {
            if let Some(active) = Self::call(state, thread, call) {
                if active.ssrc != Some(ssrc) || !active.output {
                    return;
                }
                if let Some(previous) = active.rr_after_output {
                    let advance = sequence.wrapping_sub(previous);
                    active.rr_progress |= advance > 0 && advance < (1 << 31);
                }
                active.rr_after_output = Some(sequence);
            }
        });
    }
    pub fn note_voice_error(&self, thread: &str, call: &Uuid, error: &str) {
        self.update(|state| {
            if let Some(active) = Self::call(state, thread, call) {
                active.input = false;
                active.output = false;
                active.rr_progress = false;
                active.rr_after_output = None;
                state.report.voice = Observation::Failed {
                    error: error.to_owned(),
                };
            }
        });
    }
    pub fn note_transcript(
        &self,
        thread: &str,
        call: &Uuid,
        session: &str,
        role: &str,
        item: &str,
        nonempty: bool,
        canonical: bool,
        all_pages: bool,
        error: Option<&str>,
    ) {
        self.update(|state| {
            if let Some(active) = Self::call(state, thread, call) {
                if session.is_empty()
                    || item.is_empty()
                    || !canonical
                    || !nonempty
                    || !matches!(role, "user" | "assistant")
                {
                    return;
                }
                if error.is_none() && all_pages {
                    active.roles.insert((session.to_owned(), role.to_owned()));
                } else if let Some(error) = error {
                    active.roles.clear();
                    state.report.transcript = Observation::Failed {
                        error: error.to_owned(),
                    };
                }
            }
        });
    }
    pub fn note_promotion(
        &self,
        thread: &str,
        call: &Uuid,
        session: &str,
        turn: &str,
        item: &str,
        generation: u64,
    ) {
        self.update(|state| {
            if state.report.handoff == Observation::Passed {
                return;
            }
            if let Some(active) = Self::call(state, thread, call) {
                if generation > 0 && !session.is_empty() && !turn.is_empty() && !item.is_empty() {
                    active
                        .promotions
                        .push((generation, turn.to_owned(), item.to_owned()));
                }
            }
        });
    }
    fn complete(state: &mut State) {
        if state
            .text
            .values()
            .any(|text| text.input > 0 && text.replied && text.completed && !text.failed)
        {
            state.report.text = Observation::Passed;
        }
        state
            .text
            .retain(|_, text| !(text.completed && (text.replied || text.failed)));
        state.requests.retain(|_, request| {
            let terminal = request.terminal == Some(true);
            if request.terminal == Some(false) || (request.failed && (request.resolved || request.terminal.is_some()))
                || (!request.question && request.execution == Some(false) && request.delivered)
                || (request.delivery_failed && request.terminal.is_some()) {
                let outcome = Observation::Failed { error: "native requested operation ended unsuccessfully; the actual item/turn result remains in native history and the Discord result".to_owned() };
                let slot = if request.question { &mut state.report.question } else { &mut state.report.approval };
                if !matches!(slot, Observation::Failed { .. }) { *slot = outcome; }
                return false;
            }
            if !request.failed
                && request.question
                && request.answered
                && request.resolved
                && terminal
            {
                state.report.question = Observation::Passed;
                return false;
            }
            if !request.failed
                && !request.question
                && request.answered
                && request.execution == Some(true)
                && request.delivered
                && terminal
            {
                state.report.approval = Observation::Passed;
                return false;
            }
            true
        });
        if let Some(call) = &state.call {
            if call.input && call.output && call.rr_progress {
                state.report.voice = Observation::Passed;
            }
            if call.roles.iter().any(|(session, role)| {
                role == "user"
                    && call
                        .roles
                        .contains(&(session.clone(), "assistant".to_owned()))
            }) {
                state.report.transcript = Observation::Passed;
            }
            if call.promotions.iter().any(|(generation, turn, item)| {
                state
                    .completed_turns
                    .contains(&(*generation, call.thread.clone(), turn.clone()))
                    && state.completed_items.contains(&(
                        *generation,
                        call.thread.clone(),
                        turn.clone(),
                        item.clone(),
                    ))
            }) {
                state.report.handoff = Observation::Passed;
            }
        }
        if state.call.is_none() || state.report.handoff == Observation::Passed {
            state.completed_turns.clear();
            state.completed_items.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_summary_uses_the_selected_language_for_labels_and_status() {
        let report = Report {
            binding: "fixture".into(),
            run_id: "run".into(),
            thread_id: "thread".into(),
            text: Observation::Passed,
            settings: Observation::Unobserved,
            question: Observation::Incomplete {
                reason: "pending".into(),
            },
            approval: Observation::Failed {
                error: "private detail".into(),
            },
            voice: Observation::Unobserved,
            handoff: Observation::Unobserved,
            transcript: Observation::Unobserved,
        };
        let english = report.summary(Language::English);
        let korean = report.summary(Language::Korean);
        assert!(english.contains("Text: Behavior confirmed"));
        assert!(english.contains("Settings: Not run or not observed"));
        assert!(korean.contains("텍스트: 실제 동작 확인"));
        assert!(korean.contains("설정: 미실행 또는 미관측"));
        assert!(!korean.contains("private detail"));
    }

    fn observer() -> (tempfile::TempDir, Checks) {
        let dir = tempfile::tempdir().unwrap();
        let checks = Checks {
            path: dir.path().join("installation-check.json"),
            language: Language::English,
            updates: None,
            inner: Mutex::new(State {
                report: Report {
                    binding: "fixture".into(),
                    run_id: Uuid::new_v4().to_string(),
                    thread_id: "T".into(),
                    text: Observation::Unobserved,
                    settings: Observation::Unobserved,
                    question: Observation::Unobserved,
                    approval: Observation::Unobserved,
                    voice: Observation::Unobserved,
                    handoff: Observation::Unobserved,
                    transcript: Observation::Unobserved,
                },
                text: HashMap::new(),
                requests: HashMap::new(),
                completed_turns: HashSet::new(),
                completed_items: HashSet::new(),
                settings: HashMap::new(),
                call: None,
            }),
        };
        (dir, checks)
    }
    fn event(turn: &str) -> Value {
        json!({"threadId":"T","turn":{"id":turn,"status":"completed"},"_discodex_connection_generation":1})
    }
    #[test]
    fn consumed_input_requires_its_reply_and_terminal_turn() {
        let (_dir, checks) = observer();
        checks.input_consumed(1, "T", "turn", 12);
        checks.reply_posted(1, "T", "turn", 13, 10, None);
        checks.native_event("turn/completed", &event("turn"));
        assert_eq!(
            checks.inner.lock().unwrap().report.text,
            Observation::Unobserved
        );
        checks.reply_posted(1, "T", "turn", 12, 10, None);
        assert_eq!(
            checks.inner.lock().unwrap().report.text,
            Observation::Passed
        );
        checks.reset_thread("other");
        assert!(!checks.inner.lock().unwrap().report.ready());
    }
    #[test]
    fn resolved_alone_does_not_confirm_a_question_response() {
        let (_dir, checks) = observer();
        let params = json!({"threadId":"T","turnId":"turn","itemId":"question"});
        checks.request_published(1, &json!(5), "item/tool/requestUserInput", &params);
        checks.native_event(
            "serverRequest/resolved",
            &json!({"threadId":"T","requestId":5,"_discodex_connection_generation":1}),
        );
        checks.native_event("turn/completed", &event("turn"));
        assert_eq!(
            checks.inner.lock().unwrap().report.question,
            Observation::Unobserved
        );
        checks.request_answered(2, &json!(5));
        assert_eq!(
            checks.inner.lock().unwrap().report.question,
            Observation::Unobserved
        );
        checks.request_answered(1, &json!(5));
        assert_eq!(
            checks.inner.lock().unwrap().report.question,
            Observation::Passed
        );
    }
    #[test]
    fn new_generation_cannot_reuse_an_old_completed_turn() {
        let (_dir, checks) = observer();
        checks.native_event("turn/completed", &event("turn"));
        let params = json!({"threadId":"T","turnId":"turn","itemId":"question"});
        checks.request_published(2, &json!(5), "item/tool/requestUserInput", &params);
        checks.request_answered(2, &json!(5));
        checks.native_event(
            "serverRequest/resolved",
            &json!({"threadId":"T","requestId":5,"_discodex_connection_generation":2}),
        );
        assert_eq!(
            checks.inner.lock().unwrap().report.question,
            Observation::Unobserved
        );
        let mut new_terminal = event("turn");
        new_terminal["_discodex_connection_generation"] = json!(2);
        checks.native_event("turn/completed", &new_terminal);
        assert_eq!(
            checks.inner.lock().unwrap().report.question,
            Observation::Passed
        );
    }
    #[test]
    fn steering_input_does_not_inherit_the_previous_inputs_reply() {
        let (_dir, checks) = observer();
        checks.input_consumed(1, "T", "turn", 12);
        checks.reply_posted(1, "T", "turn", 12, 10, None);
        checks.input_consumed(1, "T", "turn", 13);
        checks.native_event("turn/completed", &event("turn"));
        assert_eq!(
            checks.inner.lock().unwrap().report.text,
            Observation::Unobserved
        );
        checks.reply_posted(
            1,
            "T",
            "turn",
            13,
            10,
            Some("actual Discord publication failed"),
        );
        assert!(matches!(
            checks.inner.lock().unwrap().report.text,
            Observation::Failed { .. }
        ));
    }
    #[test]
    fn approval_needs_matching_execution_result_and_discord_delivery() {
        let (_dir, checks) = observer();
        let params = json!({"threadId":"T","turnId":"turn","itemId":"command"});
        checks.request_published(
            1,
            &json!(5),
            "item/commandExecution/requestApproval",
            &params,
        );
        checks.request_answered(1, &json!(5));
        checks.native_event("turn/completed", &event("turn"));
        let failed = json!({"threadId":"T","turnId":"turn","item":{"id":"command","type":"commandExecution","status":"completed","exitCode":9},"_discodex_connection_generation":1});
        checks.native_event("item/completed", &failed);
        checks.approval_result_posted(&failed, None);
        assert_ne!(
            checks.inner.lock().unwrap().report.approval,
            Observation::Passed
        );
        assert!(checks.inner.lock().unwrap().requests.is_empty());
        let params = json!({"threadId":"T","turnId":"recovery","itemId":"recovery-command"});
        checks.request_published(
            1,
            &json!(6),
            "item/commandExecution/requestApproval",
            &params,
        );
        checks.request_answered(1, &json!(6));
        let mut success = failed;
        success["turnId"] = json!("recovery");
        success["item"]["id"] = json!("recovery-command");
        success["item"]["exitCode"] = json!(0);
        checks.native_event("item/completed", &success);
        checks.approval_result_posted(&success, Some("actual Discord result reply failed"));
        assert!(matches!(
            checks.inner.lock().unwrap().report.approval,
            Observation::Failed { .. }
        ));
        checks.approval_result_posted(&success, None);
        checks.native_event("turn/completed", &event("recovery"));
        assert_eq!(
            checks.inner.lock().unwrap().report.approval,
            Observation::Passed
        );
    }
    #[test]
    fn approval_result_can_arrive_before_the_interaction_acknowledgment() {
        let (_dir, checks) = observer();
        let params = json!({"threadId":"T","turnId":"turn","itemId":"command"});
        checks.request_published(
            1,
            &json!(5),
            "item/commandExecution/requestApproval",
            &params,
        );
        let complete = json!({"threadId":"T","turnId":"turn","item":{"id":"command","type":"commandExecution","status":"completed","exitCode":0},"_discodex_connection_generation":1});
        checks.native_event("item/completed", &complete);
        checks.approval_result_posted(&complete, None);
        checks.native_event("turn/completed", &event("turn"));
        assert_eq!(
            checks.inner.lock().unwrap().report.approval,
            Observation::Unobserved
        );
        checks.request_answered(1, &json!(5));
        assert_eq!(
            checks.inner.lock().unwrap().report.approval,
            Observation::Passed
        );
    }
    #[test]
    fn canonical_promotion_and_transcript_must_pair_with_the_same_call_and_work() {
        let (_dir, checks) = observer();
        let call = Uuid::new_v4();
        checks.note_voice_begin("T", &call);
        checks.note_promotion("T", &call, "S", "work", "item", 1);
        checks.native_event("turn/completed", &event("work"));
        assert_eq!(
            checks.inner.lock().unwrap().report.handoff,
            Observation::Unobserved
        );
        checks.native_event("item/completed",&json!({"threadId":"other","turnId":"work","item":{"id":"item","type":"commandExecution","status":"completed","exitCode":0},"_discodex_connection_generation":1}));
        assert_eq!(
            checks.inner.lock().unwrap().report.handoff,
            Observation::Unobserved
        );
        checks.native_event("item/completed",&json!({"threadId":"T","turnId":"work","item":{"id":"item","type":"commandExecution","status":"completed","exitCode":0},"_discodex_connection_generation":1}));
        assert_eq!(
            checks.inner.lock().unwrap().report.handoff,
            Observation::Passed
        );
        checks.note_transcript("T", &call, "S", "user", "u", true, true, true, None);
        checks.note_transcript(
            "T",
            &call,
            "otherS",
            "assistant",
            "a",
            true,
            true,
            true,
            None,
        );
        assert_eq!(
            checks.inner.lock().unwrap().report.transcript,
            Observation::Unobserved
        );
        checks.note_transcript("T", &call, "S", "assistant", "a", true, true, false, None);
        assert_eq!(
            checks.inner.lock().unwrap().report.transcript,
            Observation::Unobserved
        );
        checks.note_transcript("T", &call, "S", "assistant", "a", true, true, true, None);
        assert_eq!(
            checks.inner.lock().unwrap().report.transcript,
            Observation::Passed
        );
        checks.note_voice_begin("T", &Uuid::new_v4());
        assert_eq!(
            checks.inner.lock().unwrap().report.transcript,
            Observation::Unobserved
        );
    }
    #[test]
    fn completed_correlations_live_only_until_the_current_call_or_request_finishes() {
        let (_dir, checks) = observer();
        let item = json!({"threadId":"T","turnId":"work","item":{"id":"item","type":"commandExecution","status":"completed","exitCode":0},"_discodex_connection_generation":1});
        checks.native_event("item/completed", &item);
        checks.native_event("turn/completed", &event("work"));
        assert!(checks.inner.lock().unwrap().completed_items.is_empty());
        assert!(checks.inner.lock().unwrap().completed_turns.is_empty());
        let old = Uuid::new_v4();
        let current = Uuid::new_v4();
        checks.note_voice_begin("T", &old);
        checks.native_event("item/completed", &item);
        checks.native_event("turn/completed", &event("work"));
        assert_eq!(checks.inner.lock().unwrap().completed_items.len(), 1);
        checks.note_voice_begin("T", &current);
        assert!(checks.inner.lock().unwrap().completed_items.is_empty());
        checks.native_event("item/completed", &item);
        checks.native_event("turn/completed", &event("work"));
        checks.note_voice_end("T", &old);
        assert_eq!(checks.inner.lock().unwrap().completed_items.len(), 1);
        checks.note_promotion("T", &current, "S", "work", "item", 1);
        assert_eq!(
            checks.inner.lock().unwrap().report.handoff,
            Observation::Passed
        );
        assert!(checks.inner.lock().unwrap().completed_items.is_empty());
        checks.native_event("item/completed", &item);
        checks.native_event("turn/completed", &event("work"));
        assert!(checks.inner.lock().unwrap().completed_items.is_empty());
        checks.note_voice_end("T", &current);
        assert!(checks.inner.lock().unwrap().call.is_none());
        let request = json!({"threadId":"T","turnId":"failed","itemId":"question"});
        checks.request_published(1, &json!(5), "item/tool/requestUserInput", &request);
        checks.request_error(
            "item/tool/requestUserInput",
            &request,
            "actual response write failed",
        );
        let mut terminal = event("failed");
        terminal["turn"]["status"] = json!("failed");
        checks.native_event("turn/completed", &terminal);
        assert!(checks.inner.lock().unwrap().requests.is_empty());
        assert_eq!(
            checks.inner.lock().unwrap().report.question,
            Observation::Failed {
                error: "actual response write failed".to_owned()
            }
        );
        let approval = json!({"threadId":"T","turnId":"approval","itemId":"command"});
        checks.request_published(
            1,
            &json!(6),
            "item/commandExecution/requestApproval",
            &approval,
        );
        checks.request_answered(1, &json!(6));
        let result = json!({"threadId":"T","turnId":"approval","item":{"id":"command","type":"commandExecution","status":"completed","exitCode":0},"_discodex_connection_generation":1});
        checks.native_event("item/completed", &result);
        checks.approval_result_posted(&result, Some("actual result publication failed"));
        assert_eq!(checks.inner.lock().unwrap().requests.len(), 1);
        // Another unresolved request must survive this request's terminal failure.
        let unrelated = json!({"threadId":"T","turnId":"other","itemId":"other-question"});
        checks.request_published(1, &json!(7), "item/tool/requestUserInput", &unrelated);
        checks.native_event("turn/completed", &event("approval"));
        let state = checks.inner.lock().unwrap();
        assert!(!state.requests.contains_key(&(1, json!(6).to_string())));
        assert!(state.requests.contains_key(&(1, json!(7).to_string())));
        assert_eq!(
            state.report.approval,
            Observation::Failed {
                error: "actual result publication failed".to_owned()
            }
        );
    }
    #[test]
    fn same_call_nonzero_media_needs_fresh_matching_receiver_progress() {
        let (_dir, checks) = observer();
        let call = Uuid::new_v4();
        let other = Uuid::new_v4();
        checks.note_voice_begin("T", &call);
        checks.note_voice_connected("T", &call, 8);
        checks.note_voice_in("T", &call, 3);
        checks.note_voice_rr("T", &call, 8, 4);
        checks.note_voice_out("T", &call, 5);
        checks.note_voice_rr("T", &other, 8, 6);
        checks.note_voice_rr("T", &call, 9, 6);
        checks.note_voice_rr("T", &call, 8, 4);
        assert_eq!(
            checks.inner.lock().unwrap().report.voice,
            Observation::Unobserved
        );
        checks.note_voice_rr("T", &call, 8, 5);
        assert_eq!(
            checks.inner.lock().unwrap().report.voice,
            Observation::Passed
        );
        checks.note_voice_connected("T", &call, 9);
        assert_eq!(
            checks.inner.lock().unwrap().report.voice,
            Observation::Unobserved
        );
    }
}
