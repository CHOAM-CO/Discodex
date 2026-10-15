use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail};
use futures_util::future::join_all;
use serenity::{
    all::{
        ChannelId, Context, CreateMessage, GatewayIntents, GuildId, Message, MessageReference,
        Ready, UserId,
    },
    async_trait,
    client::{Client, EventHandler},
    prelude::TypeMapKey,
};
use songbird::{Config as SongbirdConfig, SerenityInit, driver::DecodeMode};
use tokio::sync::{Mutex, MutexGuard, RwLock};
use tracing::{error, info, warn};

use crate::{
    commands,
    config::{Config, InputMode},
    native::{Native, NativeRequestError},
    voice::VoiceBridge,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReplyTarget {
    pub channel_id: ChannelId,
    pub message_id: serenity::all::MessageId,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingApproval {
    pub native_id: serde_json::Value,
    pub method: String,
    pub params: serde_json::Value,
    pub channel_id: ChannelId,
    pub message_id: serenity::all::MessageId,
    /// Set while one Discord interaction owns the right to answer this request.
    /// It prevents concurrent button/modal deliveries from submitting twice.
    pub claimed: bool,
    pub connection_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ApprovalItemKey {
    connection_generation: u64,
    thread_id: String,
    turn_id: String,
    item_id: String,
}

impl ApprovalItemKey {
    fn new(connection_generation: u64, params: &serde_json::Value, item_id: &str) -> Option<Self> {
        let thread_id = params.get("threadId")?.as_str()?;
        let turn_id = params.get("turnId")?.as_str()?;
        if connection_generation == 0
            || thread_id.is_empty()
            || turn_id.is_empty()
            || item_id.is_empty()
        {
            return None;
        }
        Some(Self {
            connection_generation,
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            item_id: item_id.to_owned(),
        })
    }
}

struct ApprovalExecution {
    item: ApprovalItemKey,
    item_type: &'static str,
    target: Option<ReplyTarget>,
}

/// Execution correlation outlives the interactive card and serverRequest/resolved.
#[derive(Default)]
pub(crate) struct ApprovalResults {
    consumed_targets: HashMap<(u64, String, String), ReplyTarget>,
    item_targets: HashMap<ApprovalItemKey, ReplyTarget>,
    // JSON serialization preserves the native request ID's number/string distinction.
    requests: HashMap<(u64, String), ApprovalExecution>,
}

impl ApprovalResults {
    fn record_consumed_target(&mut self, params: &serde_json::Value, target: ReplyTarget) {
        let Some(generation) = params
            .get("_discodex_connection_generation")
            .and_then(serde_json::Value::as_u64)
        else {
            return;
        };
        let Some(thread_id) = params.get("threadId").and_then(serde_json::Value::as_str) else {
            return;
        };
        let Some(turn_id) = params.get("turnId").and_then(serde_json::Value::as_str) else {
            return;
        };
        if generation > 0 && !thread_id.is_empty() && !turn_id.is_empty() {
            self.consumed_targets.insert(
                (generation, thread_id.to_owned(), turn_id.to_owned()),
                target,
            );
        }
    }

    fn freeze_item_target(&mut self, params: &serde_json::Value) {
        let Some(item) = params.get("item") else {
            return;
        };
        if !matches!(
            item.get("type").and_then(serde_json::Value::as_str),
            Some("fileChange" | "commandExecution")
        ) {
            return;
        }
        let Some(generation) = params
            .get("_discodex_connection_generation")
            .and_then(serde_json::Value::as_u64)
        else {
            return;
        };
        let Some(item_id) = item.get("id").and_then(serde_json::Value::as_str) else {
            return;
        };
        if let Some(key) = ApprovalItemKey::new(generation, params, item_id) {
            if let Some(target) = self
                .consumed_targets
                .get(&(generation, key.thread_id.clone(), key.turn_id.clone()))
                .cloned()
            {
                self.item_targets.entry(key).or_insert(target);
            }
        }
    }

    pub(crate) fn consumed_target(&self, params: &serde_json::Value) -> Option<ReplyTarget> {
        consumed_reply_target(&self.consumed_targets, params)
    }

    pub(crate) fn request_target(
        &self,
        generation: u64,
        params: &serde_json::Value,
    ) -> Option<ReplyTarget> {
        let item_id = params.get("itemId")?.as_str()?;
        let key = ApprovalItemKey::new(generation, params, item_id)?;
        self.item_targets.get(&key).cloned().or_else(|| {
            self.consumed_targets
                .get(&(generation, key.thread_id, key.turn_id))
                .cloned()
        })
    }

    pub(crate) fn track_request(
        &mut self,
        generation: u64,
        request_id: &serde_json::Value,
        method: &str,
        params: &serde_json::Value,
        target: Option<ReplyTarget>,
    ) {
        let item_type = match method {
            "item/fileChange/requestApproval" => "fileChange",
            "item/commandExecution/requestApproval" => "commandExecution",
            _ => return,
        };
        let Some(item_id) = params.get("itemId").and_then(serde_json::Value::as_str) else {
            return;
        };
        let Some(item) = ApprovalItemKey::new(generation, params, item_id) else {
            return;
        };
        self.requests
            .entry((generation, request_id.to_string()))
            .or_insert(ApprovalExecution {
                item,
                item_type,
                target,
            });
    }

    pub(crate) fn bind_request_card(
        &mut self,
        generation: u64,
        request_id: &serde_json::Value,
        target: ReplyTarget,
    ) {
        if let Some(request) = self.requests.get_mut(&(generation, request_id.to_string())) {
            request.target.get_or_insert(target);
        }
    }

    #[cfg(test)]
    fn take_replies(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Vec<(ReplyTarget, CreateMessage)> {
        self.take_replies_localized(method, params, &crate::i18n::Language::English)
    }

    fn take_replies_localized(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        language: &crate::i18n::Language,
    ) -> Vec<(ReplyTarget, CreateMessage)> {
        let Some(generation) = params
            .get("_discodex_connection_generation")
            .and_then(serde_json::Value::as_u64)
        else {
            return Vec::new();
        };
        if method == "turn/completed" {
            if let (Some(thread_id), Some(turn_id)) = (
                params.get("threadId").and_then(serde_json::Value::as_str),
                params
                    .pointer("/turn/id")
                    .and_then(serde_json::Value::as_str),
            ) {
                if matches!(
                    params
                        .pointer("/turn/status")
                        .and_then(serde_json::Value::as_str),
                    Some("completed" | "failed" | "interrupted")
                ) {
                    self.consumed_targets.remove(&(
                        generation,
                        thread_id.to_owned(),
                        turn_id.to_owned(),
                    ));
                }
            }
        }
        let (item_key, turn_key, content) = match method {
            "item/completed" => {
                let Some(item) = params.get("item") else {
                    return Vec::new();
                };
                let Some(item_id) = item.get("id").and_then(serde_json::Value::as_str) else {
                    return Vec::new();
                };
                let Some(key) = ApprovalItemKey::new(generation, params, item_id) else {
                    return Vec::new();
                };
                let item_type = item
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                // Official CommandExecutionStatus and PatchApplyStatus terminal values.
                let content = match (
                    item_type,
                    item.get("status").and_then(serde_json::Value::as_str),
                ) {
                    ("fileChange", Some("completed")) => language.text(
                        "File changes completed.",
                        "파일 변경이 완료되었습니다.",
                        "文件更改已完成。",
                        "ファイル変更が完了しました。",
                    ),
                    ("fileChange", Some("declined")) => language.text(
                        "File changes were declined.",
                        "파일 변경이 거부되었습니다.",
                        "文件更改已被拒绝。",
                        "ファイル変更が拒否されました。",
                    ),
                    ("fileChange", Some("failed")) => language.text(
                        "File changes failed.",
                        "파일 변경에 실패했습니다.",
                        "文件更改失败。",
                        "ファイル変更に失敗しました。",
                    ),
                    ("commandExecution", Some("completed"))
                        if item
                            .get("exitCode")
                            .and_then(serde_json::Value::as_i64)
                            .is_some_and(|code| code != 0) =>
                    {
                        language.text(
                            "Command execution failed.",
                            "명령 실행에 실패했습니다.",
                            "命令执行失败。",
                            "コマンドの実行に失敗しました。",
                        )
                    }
                    ("commandExecution", Some("completed")) => language.text(
                        "Command execution completed.",
                        "명령 실행이 완료되었습니다.",
                        "命令执行已完成。",
                        "コマンドの実行が完了しました。",
                    ),
                    ("commandExecution", Some("declined")) => language.text(
                        "Command execution was declined.",
                        "명령 실행이 거부되었습니다.",
                        "命令执行已被拒绝。",
                        "コマンドの実行が拒否されました。",
                    ),
                    ("commandExecution", Some("failed")) => language.text(
                        "Command execution failed.",
                        "명령 실행에 실패했습니다.",
                        "命令执行失败。",
                        "コマンドの実行に失敗しました。",
                    ),
                    _ => return Vec::new(),
                };
                self.item_targets.remove(&key);
                (Some((key, item_type)), None, content)
            }
            "turn/completed" => {
                let Some(turn) = params.get("turn") else {
                    return Vec::new();
                };
                // A turn interruption is observed; no approval decision is inferred.
                if turn.get("status").and_then(serde_json::Value::as_str) != Some("interrupted") {
                    return Vec::new();
                }
                let Some(thread_id) = params.get("threadId").and_then(serde_json::Value::as_str)
                else {
                    return Vec::new();
                };
                let Some(turn_id) = turn.get("id").and_then(serde_json::Value::as_str) else {
                    return Vec::new();
                };
                self.item_targets.retain(|key, _| {
                    !(key.connection_generation == generation
                        && key.thread_id == thread_id
                        && key.turn_id == turn_id)
                });
                (
                    None,
                    Some((thread_id, turn_id)),
                    language.text(
                        "The task with an approval request was interrupted.",
                        "승인 요청이 있던 작업의 실행이 중단되었습니다.",
                        "包含审批请求的任务已中断。",
                        "承認リクエストを含むタスクが中断されました。",
                    ),
                )
            }
            _ => return Vec::new(),
        };
        let mut targets = Vec::new();
        self.requests.retain(|_, request| {
            let matches = if let Some((key, item_type)) = &item_key {
                &request.item == key && request.item_type == *item_type
            } else if let Some((thread_id, turn_id)) = turn_key {
                request.item.connection_generation == generation
                    && request.item.thread_id == thread_id
                    && request.item.turn_id == turn_id
            } else {
                false
            };
            if matches {
                if let Some(target) = &request.target {
                    if !targets.contains(target) {
                        targets.push(target.clone());
                    }
                }
            }
            !matches
        });
        targets
            .into_iter()
            .map(|target| {
                let message = assistant_reply_message(&target, content);
                (target, message)
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct QueuedInput {
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<crate::attachments::DownloadedAttachment>,
    pub channel_id: u64,
    pub message_id: u64,
    #[serde(default)]
    pub native_thread_id: String,
    #[serde(default)]
    pub delivery: QueueDelivery,
    #[serde(default)]
    pub submission_generation: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum QueueDelivery {
    #[default]
    Queued,
    Submitting,
    Unknown,
    Rejected,
    Accepted,
}

pub(crate) struct BridgeState {
    pub config: Arc<Config>,
    pub installation_checks: Option<Arc<crate::installation_check::Checks>>,
    pub native: Native,
    pub voice: Arc<VoiceBridge>,
    pub input_mode: RwLock<InputMode>,
    pub input_mode_path: PathBuf,
    pub current_turn: Mutex<Option<String>>,
    pub active_turns: Mutex<HashMap<String, String>>,
    // Access only while current_turn is held, with the active-turn state update.
    turn_observation_orders: Mutex<HashMap<String, (u64, u64)>>,
    pub queued_inputs: Mutex<VecDeque<QueuedInput>>,
    pub queue_path: PathBuf,
    pub bot_user_id: Mutex<Option<UserId>>,
    /// clientUserMessageId -> source Discord message. Native user item events confirm
    /// consumption; assistant items freeze the then-current consumed source.
    pub input_targets: Mutex<HashMap<String, ReplyTarget>>,
    pub latest_consumed_targets: Mutex<HashMap<String, ReplyTarget>>,
    pub turn_failure_targets: Mutex<HashMap<(String, String), ReplyTarget>>,
    /// Per-thread observation watermark captured after a lagged native event stream.
    typing_observation_floor: Mutex<HashMap<String, (u64, u64)>>,
    pub item_targets: Mutex<HashMap<String, ReplyTarget>>,
    voice_item_destinations: Mutex<HashMap<(String, String), ChannelId>>,
    pub pending_approvals: Mutex<HashMap<String, PendingApproval>>,
    pub approval_results: Mutex<ApprovalResults>,
}

pub(crate) struct StateKey;

impl TypeMapKey for StateKey {
    type Value = Arc<BridgeState>;
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        let state = state_from_context(&ctx).await;
        let Some(state) = state else {
            error!("Discord bridge state is missing");
            return;
        };
        *state.bot_user_id.lock().await = Some(ready.user.id);
        if let Err(error) = commands::register_guild_commands(&ctx.http, &state.config).await {
            error!(%error, "failed to register guild commands");
        }
        info!(user = %ready.user.name, guild = state.config.guild_id, "Discord bridge connected");
    }

    async fn message(&self, ctx: Context, msg: Message) {
        let Some(state) = state_from_context(&ctx).await else {
            return;
        };
        if msg.guild_id.map(|id| id.get()) != Some(state.config.guild_id) {
            return;
        }
        let bot_id = *state.bot_user_id.lock().await;
        if bot_id == Some(msg.author.id) {
            return;
        }
        if !state.config.operator_ids.contains(&msg.author.id.get()) {
            return;
        }
        let home_channel = msg.channel_id.get() == state.config.home_channel_id;
        let mentioned = bot_id.is_some_and(|id| msg.mentions.iter().any(|user| user.id == id));
        let route = state.voice.input_gate.state.lock().await.route(
            msg.channel_id.get(),
            state.config.home_channel_id,
            mentioned,
        );
        if route == crate::voice_input::InputRoute::Ignore {
            return;
        }

        let (content, message_attachments) = crate::attachments::message_parts(&msg);
        if content.trim().is_empty() && message_attachments.is_empty() {
            return;
        }
        let input = message_input(&msg, home_channel, &ctx.http).await;
        let target = ReplyTarget {
            channel_id: msg.channel_id,
            message_id: msg.id,
        };
        let submission = async {
            if let crate::voice_input::InputRoute::Voice { call_id, thread_id } = &route {
                let total_bytes: u64 = message_attachments.iter().map(|file| u64::from(file.size)).sum();
                if total_bytes > 250 * 1024 * 1024 {
                    bail!("Discord message attachments exceed 250 MiB");
                }
                let mut attachments = Vec::with_capacity(message_attachments.len());
                for attachment in &message_attachments {
                    attachments.push(crate::attachments::download_attachment(&state.config, attachment).await?);
                }
                return submit_voice_input(&state, &input, &attachments, &target, *call_id, thread_id).await;
            }
            // Only directly received, authorized messages reach this download path.
            // Retrieval tools return metadata and never invoke this ingress handler.
            let total_bytes: u64 = message_attachments.iter().map(|file| u64::from(file.size)).sum();
            if total_bytes > 250 * 1024 * 1024 {
                bail!("Discord message attachments exceed 250 MiB");
            }
            let mut attachments = Vec::with_capacity(message_attachments.len());
            for attachment in &message_attachments {
                attachments.push(crate::attachments::download_attachment(&state.config, attachment).await?);
            }
            submit_input(&state, &input, &attachments, target.clone()).await
        }.await;
        match submission {
            Ok(()) => {}
            Err(error) => {
                warn!(%error, message_id = %msg.id, "failed to submit Discord input to Codex");
                if let Some(checks) = &state.installation_checks {
                    checks.text_error(&state.native.thread_id(), &format!("{error:#}"));
                }
                let (channel, builder) = if matches!(
                    route,
                    crate::voice_input::InputRoute::Voice { .. }
                ) {
                    (
                        target.channel_id,
                        assistant_reply_message(
                            &target,
                            if message_attachments.is_empty() {
                                state.config.language.text(
                                    "Could not send text to this call. Check whether the call is still active.",
                                    "이 통화에 텍스트를 전달하지 못했습니다. 통화가 계속 중인지 확인해 주세요.",
                                    "无法向此通话发送文本。请确认通话是否仍在进行。",
                                    "この通話にテキストを送信できませんでした。通話が継続中か確認してください。"
                                )
                            } else {
                                state.config.language.text(
                                    "Could not send this message and its attachments to the call. Check whether the call is still active.",
                                    "이 메시지와 첨부 파일을 통화에 전달하지 못했습니다. 통화가 계속 중인지 확인해 주세요.",
                                    "无法向此通话发送消息及附件。请确认通话是否仍在进行。",
                                    "このメッセージと添付ファイルを通話に送信できませんでした。通話が継続中か確認してください。"
                                )
                            },
                        ),
                    )
                } else {
                    conversation_reply_message_localized(
                        &target,
                        ChannelId::new(state.config.home_channel_id),
                        state.config.guild_id,
                        state.config.language.text(
                            "Could not send input to Codex.",
                            "Codex에 입력을 전달하지 못했습니다.",
                            "无法向 Codex 发送输入。",
                            "Codex に入力を送信できませんでした。",
                        ),
                        &state.config.language,
                    )
                };
                let _ = channel.send_message(&ctx.http, builder).await;
            }
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: serenity::all::Interaction) {
        let language = state_from_context(&ctx)
            .await
            .map(|state| state.config.language.clone())
            .unwrap_or(crate::i18n::Language::English);
        match interaction {
            serenity::all::Interaction::Command(command) => {
                if let Err(error) = commands::handle(&ctx, &command).await {
                    error!(%error, "Discord command failed");
                    let builder = serenity::builder::EditInteractionResponse::new()
                        .content(language.text(
                            "Could not process the Codex command.",
                            "Codex 명령을 처리하지 못했습니다.",
                            "无法处理 Codex 命令。",
                            "Codex コマンドを処理できませんでした。",
                        ))
                        .allowed_mentions(empty_mentions());
                    if command.edit_response(&ctx, builder).await.is_err() {
                        let _ = command
                            .create_response(
                                &ctx,
                                serenity::all::CreateInteractionResponse::Message(
                                    serenity::all::CreateInteractionResponseMessage::new()
                                        .content(language.text(
                                            "Could not process the Codex command.",
                                            "Codex 명령을 처리하지 못했습니다.",
                                            "无法处理 Codex 命令。",
                                            "Codex コマンドを処理できませんでした。",
                                        ))
                                        .allowed_mentions(empty_mentions())
                                        .ephemeral(true),
                                ),
                            )
                            .await;
                    }
                }
            }
            serenity::all::Interaction::Component(component) => {
                if let Err(error) = commands::handle_component(&ctx, &component).await {
                    error!(%error, "Discord component failed");
                    let builder = serenity::builder::EditInteractionResponse::new()
                        .content(language.text(
                            "Could not respond to the Codex request.",
                            "Codex 요청에 응답하지 못했습니다.",
                            "无法响应 Codex 请求。",
                            "Codex リクエストに応答できませんでした。",
                        ))
                        .allowed_mentions(empty_mentions());
                    if component.edit_response(&ctx, builder).await.is_err() {
                        let _ = component
                            .create_response(
                                &ctx,
                                serenity::all::CreateInteractionResponse::Message(
                                    serenity::all::CreateInteractionResponseMessage::new()
                                        .content(language.text(
                                            "Could not respond to the Codex request.",
                                            "Codex 요청에 응답하지 못했습니다.",
                                            "无法响应 Codex 请求。",
                                            "Codex リクエストに応答できませんでした。",
                                        ))
                                        .allowed_mentions(empty_mentions())
                                        .ephemeral(true),
                                ),
                            )
                            .await;
                    }
                }
            }
            serenity::all::Interaction::Modal(modal) => {
                if let Err(error) = commands::handle_modal(&ctx, &modal).await {
                    error!(%error, "Discord modal failed");
                    let builder = serenity::builder::EditInteractionResponse::new()
                        .content(language.text(
                            "Could not process the Codex input.",
                            "Codex 입력을 처리하지 못했습니다.",
                            "无法处理 Codex 输入。",
                            "Codex の入力を処理できませんでした。",
                        ))
                        .allowed_mentions(empty_mentions());
                    if modal.edit_response(&ctx, builder).await.is_err() {
                        let _ = modal
                            .create_response(
                                &ctx,
                                serenity::all::CreateInteractionResponse::Message(
                                    serenity::all::CreateInteractionResponseMessage::new()
                                        .content(language.text(
                                            "Could not process the Codex input.",
                                            "Codex 입력을 처리하지 못했습니다.",
                                            "无法处理 Codex 输入。",
                                            "Codex の入力を処理できませんでした。",
                                        ))
                                        .allowed_mentions(empty_mentions())
                                        .ephemeral(true),
                                ),
                            )
                            .await;
                    }
                }
            }
            _ => {}
        }
    }
}

pub async fn run(
    config: Config,
    native: Native,
    shutdown: tokio::sync::oneshot::Receiver<std::io::Result<()>>,
) -> anyhow::Result<()> {
    let config = Arc::new(config);
    let token = config.token().context("Discord bot token is unavailable")?;
    let songbird_config = SongbirdConfig::default().decode_mode(DecodeMode::Decrypt);
    let songbird = songbird::Songbird::serenity_from_config(songbird_config);
    let installation_checks =
        crate::installation_check::Checks::start(&config, &native.thread_id());
    let voice = VoiceBridge::new(
        config.clone(),
        native.clone(),
        Arc::new(serenity::http::Http::new(&token)),
        songbird.clone(),
    );
    let voice = Arc::new(match &installation_checks {
        Some(checks) => voice.with_installation_checks(checks.clone()),
        None => voice,
    });
    let input_mode_path = config.state_dir.join("discord-settings.json");
    let input_mode = load_input_mode(&input_mode_path)?.unwrap_or(config.input_mode);
    let mut queued_inputs = load_queue(&config.state_dir.join("discord-inputs.json"))?;
    let changed_queue = recover_submitting_inputs(&mut queued_inputs);
    let queue_path = config.state_dir.join("discord-inputs.json");
    if changed_queue {
        save_queue(&queue_path, &queued_inputs)?;
    }
    let state = Arc::new(BridgeState {
        input_mode: RwLock::new(input_mode),
        input_mode_path,
        config: config.clone(),
        installation_checks,
        native: native.clone(),
        voice,
        current_turn: Mutex::new(None),
        active_turns: Mutex::new(HashMap::new()),
        turn_observation_orders: Mutex::new(HashMap::new()),
        queued_inputs: Mutex::new(queued_inputs),
        queue_path,
        bot_user_id: Mutex::new(None),
        input_targets: Mutex::new(HashMap::new()),
        latest_consumed_targets: Mutex::new(HashMap::new()),
        turn_failure_targets: Mutex::new(HashMap::new()),
        typing_observation_floor: Mutex::new(HashMap::new()),
        item_targets: Mutex::new(HashMap::new()),
        voice_item_destinations: Mutex::new(HashMap::new()),
        pending_approvals: Mutex::new(HashMap::new()),
        approval_results: Mutex::new(ApprovalResults::default()),
    });

    let voice_tool = Arc::downgrade(&state.voice);
    let voice_guild_id = GuildId::new(config.guild_id);
    native.set_voice_leave_handler(Arc::new(move |summary| {
        let voice = voice_tool.upgrade();
        Box::pin(async move {
            let voice = voice
                .ok_or_else(|| anyhow::anyhow!("Discord voice bridge is no longer available"))?;
            serde_json::to_value(voice.leave_with_result(voice_guild_id, Some(summary)).await)
                .context("serialize voice leave result")
        })
    }));

    let discord_tools = Arc::new(crate::discord_tools::DiscordTools::new(
        config.clone(),
        Arc::new(serenity::http::Http::new(&token)),
    ));
    let tool_state = Arc::downgrade(&state);
    native.set_discord_tool_handler(Arc::new(move |name, arguments| {
        let state = tool_state.upgrade();
        let tools = discord_tools.clone();
        Box::pin(async move {
            let _state =
                state.ok_or_else(|| anyhow::anyhow!("Discord bridge is no longer available"))?;
            tools.invoke(&name, arguments).await
        })
    }));

    let mut client = Client::builder(
        token.clone(),
        GatewayIntents::GUILDS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILD_VOICE_STATES,
    )
    .event_handler(Handler)
    .register_songbird_with(songbird.clone())
    .await?;
    client.data.write().await.insert::<StateKey>(state.clone());

    let event_http = Arc::new(serenity::http::Http::new(&token));
    let cleanup_http = event_http.clone();
    let (event_stop_tx, event_stop_rx) = tokio::sync::oneshot::channel();
    let mut events = native.subscribe();
    let event_state = state.clone();
    let event_task = tokio::spawn(async move {
        let mut event_stop_rx = event_stop_rx;
        // The startup resume happened before this subscriber existed. Obtain an
        // authoritative idle/active observation before recovering a saved queue.
        if let Err(error) = event_state.native.refresh_thread_state().await {
            warn!(%error, "could not observe startup turn state for queued input");
        }
        loop {
            tokio::select! {
                _ = &mut event_stop_rx => break,
                _ = event_state.voice.input_gate.ready.notified() => {
                    if let Err(error) = submit_next_queued_input(&event_state, &event_state.native.thread_id()).await {
                        warn!(%error, "failed to dispatch queued input after voice ended");
                    }
                }
                result = events.recv() => match result {
                    Ok(envelope) => handle_native_event(&event_http, &event_state, envelope).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        warn!(count, "native event receiver lagged");
                        record_typing_observation_floor(&event_state).await;
                        if let Err(error) = commands::reconcile_pending_requests(&event_http, &event_state).await {
                            warn!(%error, "failed to reconcile pending Discord requests after event lag")
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    });

    let (typing_stop_tx, typing_stop_rx) = tokio::sync::oneshot::channel();
    let typing_state = state.clone();
    let typing_http = cleanup_http.clone();
    let typing_task = tokio::spawn(async move {
        run_typing_indicator(typing_state, typing_http, typing_stop_rx).await;
    });

    let result = tokio::select! {
        result = client.start() => result.map_err(anyhow::Error::from),
        signal = shutdown => {
            match signal {
                Ok(Ok(())) => {
                    info!("shutdown signal received; closing Discord voice and native child");
                    Ok(())
                }
                Ok(Err(error)) => Err(anyhow::Error::from(error).context("wait for bridge shutdown signal")),
                Err(error) => Err(anyhow::Error::from(error).context("shutdown signal task ended unexpectedly")),
            }
        }
    };
    let _ = event_stop_tx.send(());
    let _ = typing_stop_tx.send(());
    let _ = event_task.await;
    let _ = typing_task.await;
    commands::close_pending_requests(cleanup_http.as_ref(), &state).await;
    let voice_cleanup = state.voice.leave(GuildId::new(state.config.guild_id)).await;
    result?;
    voice_cleanup?;
    Ok(())
}

const TYPING_REFRESH_INTERVAL: Duration = Duration::from_secs(7);
const TYPING_RETRY_INTERVAL: Duration = Duration::from_secs(2);
const TYPING_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

fn eligible_typing_channels(
    connected: bool,
    generation: u64,
    active_turns: &HashMap<String, String>,
    observation_orders: &HashMap<String, (u64, u64)>,
    turn_targets: &HashMap<(String, String), ReplyTarget>,
    observation_floor: &HashMap<String, (u64, u64)>,
    home_channel: ChannelId,
) -> HashSet<ChannelId> {
    if !connected {
        return HashSet::new();
    }
    active_turns
        .iter()
        .filter_map(|(thread_id, turn_id)| {
            let (observed_generation, order) = observation_orders.get(thread_id)?;
            if *observed_generation != generation
                || observation_floor.get(thread_id).is_some_and(
                    |(floor_generation, floor_order)| {
                        *floor_generation == generation && *order <= *floor_order
                    },
                )
            {
                return None;
            }
            turn_targets
                .get(&(thread_id.clone(), turn_id.clone()))
                .map(|_| home_channel)
        })
        .collect()
}

fn due_typing_channels(
    active: &HashSet<ChannelId>,
    next_refresh: &mut HashMap<ChannelId, Instant>,
    now: Instant,
) -> Vec<ChannelId> {
    next_refresh.retain(|channel, _| active.contains(channel));
    active
        .iter()
        .filter(|channel| next_refresh.get(channel).is_none_or(|next| *next <= now))
        .copied()
        .collect()
}

async fn record_typing_observation_floor(state: &BridgeState) {
    let _current = state.current_turn.lock().await;
    let orders = state.turn_observation_orders.lock().await.clone();
    let mut floor = state.typing_observation_floor.lock().await;
    for (thread_id, order) in orders {
        floor.insert(thread_id, order);
    }
}

async fn current_typing_channels(state: &BridgeState) -> HashSet<ChannelId> {
    let connected = state.native.is_connected();
    let generation = state.native.current_generation();
    // Keep the same lock order used by turn-state updates.
    let _current = state.current_turn.lock().await;
    let active = state.active_turns.lock().await.clone();
    let orders = state.turn_observation_orders.lock().await.clone();
    drop(_current);
    let targets = state.turn_failure_targets.lock().await;
    let floor = state.typing_observation_floor.lock().await;
    eligible_typing_channels(
        connected,
        generation,
        &active,
        &orders,
        &targets,
        &floor,
        ChannelId::new(state.config.home_channel_id),
    )
}

async fn run_typing_indicator(
    state: Arc<BridgeState>,
    http: Arc<serenity::http::Http>,
    stop: tokio::sync::oneshot::Receiver<()>,
) {
    let mut stop = stop;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut generation = state.native.current_generation();
    let mut next_refresh = HashMap::new();

    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = tick.tick() => {
                let current_generation = state.native.current_generation();
                if current_generation != generation {
                    generation = current_generation;
                    next_refresh.clear();
                }
                if !state.native.is_connected() {
                    next_refresh.clear();
                    continue;
                }
                let active = current_typing_channels(&state).await;
                let due = due_typing_channels(&active, &mut next_refresh, Instant::now());
                if due.is_empty() {
                    continue;
                }

                let requests = join_all(due.iter().copied().map(|channel_id| {
                    let http = http.clone();
                    async move {
                        let result = tokio::time::timeout(
                            TYPING_REQUEST_TIMEOUT,
                            channel_id.broadcast_typing(http.as_ref()),
                        )
                        .await;
                        (channel_id, result)
                    }
                }));
                let results = tokio::select! {
                    _ = &mut stop => break,
                    results = requests => results,
                };
                let completed_at = Instant::now();
                for (channel_id, result) in results {
                    match result {
                        Ok(Ok(())) => {
                            next_refresh.insert(channel_id, completed_at + TYPING_REFRESH_INTERVAL);
                        }
                        Ok(Err(error)) => {
                            warn!(%error, %channel_id, "failed to refresh Discord typing indicator");
                            next_refresh.insert(channel_id, completed_at + TYPING_RETRY_INTERVAL);
                        }
                        Err(_) => {
                            warn!(%channel_id, "timed out refreshing Discord typing indicator");
                            next_refresh.insert(channel_id, completed_at + TYPING_RETRY_INTERVAL);
                        }
                    }
                }
            }
        }
    }
}

async fn state_from_context(ctx: &Context) -> Option<Arc<BridgeState>> {
    ctx.data.read().await.get::<StateKey>().cloned()
}

async fn message_input(msg: &Message, home_channel: bool, http: &serenity::http::Http) -> String {
    let author_name = msg
        .member
        .as_ref()
        .and_then(|member| member.nick.as_deref())
        .or(msg.author.global_name.as_deref())
        .unwrap_or(&msg.author.name);
    let source = if home_channel {
        serde_json::json!({"author": author_name})
    } else {
        let channel_name = msg
            .channel_id
            .to_channel(http)
            .await
            .ok()
            .and_then(|channel| channel.guild().map(|channel| channel.name))
            .unwrap_or_else(|| "unavailable".to_owned());
        let server_name = match msg.guild_id {
            Some(guild_id) => guild_id
                .to_partial_guild(http)
                .await
                .ok()
                .map(|guild| guild.name)
                .unwrap_or_else(|| "unavailable".to_owned()),
            None => "unavailable".to_owned(),
        };
        serde_json::json!({
            "server": server_name,
            "channel": channel_name,
            "author": author_name,
        })
    };
    let forwarded = crate::attachments::forwarded_message_data(msg);
    crate::attachments::format_ingress_message(&msg.content, &forwarded, source)
}

async fn submit_voice_input(
    state: &BridgeState,
    input: &str,
    attachments: &[crate::attachments::DownloadedAttachment],
    target: &ReplyTarget,
    call_id: uuid::Uuid,
    thread_id: &str,
) -> anyhow::Result<()> {
    // Keep the call fixed across formatting/download delays and the native write.
    // A message captured for call A must never fall through into call B or text.
    let phase = state.voice.input_gate.state.lock().await;
    if !voice_input_matches(&phase, call_id, thread_id, target.channel_id.get()) {
        bail!(
            "{}",
            state.config.language.text(
                "The voice call that received this message has ended or changed.",
                "이 메시지를 받은 음성 통화가 이미 종료되었거나 바뀌었습니다.",
                "接收此消息的语音通话已结束或发生变化。",
                "このメッセージを受け取った音声通話は終了したか、別の通話に切り替わりました。"
            )
        );
    }
    record_input(
        state,
        input,
        attachments,
        target,
        thread_id,
        QueueDelivery::Submitting,
    )
    .await?;
    let result = state
        .native
        .request(
            "thread/realtime/appendText",
            serde_json::json!({
                "threadId": thread_id,
                "text": crate::attachments::native_input_text(input, attachments),
                "role": "user"
            }),
        )
        .await;
    match result {
        Ok(_) => mark_input_accepted(state, target.message_id.get(), thread_id).await?,
        Err(error) => {
            mark_input_unknown(state, target.message_id.get()).await?;
            return Err(error);
        }
    }
    Ok(())
}

fn voice_input_matches(
    phase: &crate::voice_input::VoiceInputPhase,
    call_id: uuid::Uuid,
    thread_id: &str,
    channel_id: u64,
) -> bool {
    matches!(phase, crate::voice_input::VoiceInputPhase::Active {
        call_id: active_call, thread_id: active_thread, transcript_channel_id,
    } if *active_call == call_id && active_thread == thread_id && *transcript_channel_id == channel_id)
}

async fn submit_input(
    state: &BridgeState,
    input: &str,
    attachments: &[crate::attachments::DownloadedAttachment],
    target: ReplyTarget,
) -> anyhow::Result<()> {
    // Hold admission through the native write. Voice start/stop uses this same
    // mutex, so no ordinary turn can slip across a realtime transition.
    let phase = state.voice.input_gate.state.lock().await;
    let thread_id = state.native.thread_id();
    if !phase.allow_ordinary() {
        return record_input(
            state,
            input,
            attachments,
            &target,
            &thread_id,
            QueueDelivery::Queued,
        )
        .await;
    }
    // Do not let a new post overtake messages that arrived during the call.
    let queued_before =
        state.queued_inputs.lock().await.iter().any(|item| {
            item.native_thread_id == thread_id && item.delivery == QueueDelivery::Queued
        });
    if queued_before {
        record_input(
            state,
            input,
            attachments,
            &target,
            &thread_id,
            QueueDelivery::Queued,
        )
        .await?;
        drop(phase);
        return submit_next_queued_input(state, &thread_id).await;
    }
    // Serialize observation and admission with terminal events and thread replacement.
    // The reader resolves recovery RPCs independently of Discord event handling.
    let mut active = state.current_turn.lock().await;
    if state.native.prepare_turn_input().await?.is_some() {
        // Another caller may have reconnected before this guard was acquired.
        // Read actual state instead of replaying its cached active-turn snapshot.
        let snapshot = read_recovery_turn_snapshot(&state.native).await?;
        {
            let active_turn = snapshot_active_turn(snapshot.get("thread").unwrap_or(&snapshot))
                .ok_or_else(|| {
                    anyhow::anyhow!("native turn state was not observable after reconnect")
                })?;
            let thread_id = state.native.thread_id();
            let mut active_turns = state.active_turns.lock().await;
            let mut orders = state.turn_observation_orders.lock().await;
            if !apply_turn_state_observation(
                &mut active,
                &mut active_turns,
                &mut orders,
                &thread_id,
                &thread_id,
                state.native.current_generation(),
                &active_turn,
                &snapshot,
            ) {
                bail!("native recovery turn observation was obsolete");
            }
        }
    }
    let thread_id = state.native.thread_id();
    if let Some(turn_id) = active.clone() {
        if *state.input_mode.read().await == InputMode::Steering {
            record_input(
                state,
                input,
                attachments,
                &target,
                &thread_id,
                QueueDelivery::Submitting,
            )
            .await?;
            drop(active);
            let client_id = target.message_id.get().to_string();
            state
                .input_targets
                .lock()
                .await
                .insert(client_id.clone(), target.clone());
            if let Err(error) = state
                .native
                .request_prepared(
                    "turn/steer",
                    serde_json::json!({
                        "threadId": thread_id.clone(),
                        "expectedTurnId": turn_id,
                        "clientUserMessageId": client_id,
                        "input": crate::attachments::native_input(input, attachments)
                    }),
                )
                .await
            {
                if error
                    .downcast_ref::<NativeRequestError>()
                    .is_some_and(NativeRequestError::is_expected_turn_mismatch)
                {
                    mark_input_rejected(state, target.message_id.get()).await?;
                    let mut recovered_current = state.current_turn.lock().await;
                    if state.native.thread_id() != thread_id {
                        return Err(error);
                    }
                    let actual = read_recovery_turn_snapshot(&state.native).await?;
                    let actual_turn = snapshot_active_turn(actual.get("thread").unwrap_or(&actual));
                    let Some(actual_turn) = actual_turn else {
                        return Err(anyhow::anyhow!(
                            "native turn state was not observable after turn mismatch"
                        ));
                    };
                    let actual_thread_id = state.native.thread_id();
                    {
                        let mut active_turns = state.active_turns.lock().await;
                        let mut orders = state.turn_observation_orders.lock().await;
                        if !apply_turn_state_observation(
                            &mut recovered_current,
                            &mut active_turns,
                            &mut orders,
                            &actual_thread_id,
                            &thread_id,
                            state.native.current_generation(),
                            &actual_turn,
                            &actual,
                        ) {
                            bail!("native recovery turn observation was obsolete");
                        }
                    }
                    mark_input_submitting(state, target.message_id.get()).await?;
                    let retry = match actual_turn {
                        Some(actual_turn_id) => {
                            drop(recovered_current);
                            state
                                .native
                                .request_prepared(
                                    "turn/steer",
                                    serde_json::json!({
                                        "threadId": actual_thread_id.clone(),
                                        "expectedTurnId": actual_turn_id,
                                        "clientUserMessageId": client_id,
                                        "input": crate::attachments::native_input(input, attachments)
                                    }),
                                )
                                .await
                        }
                        None => {
                            let result = start_turn_locked(
                                state,
                                &thread_id,
                                input,
                                attachments,
                                &target,
                                &mut recovered_current,
                            )
                            .await
                            .map(|_| serde_json::Value::Null);
                            drop(recovered_current);
                            result
                        }
                    };
                    match retry {
                        Ok(_) => {
                            mark_input_accepted(state, target.message_id.get(), &thread_id).await?;
                            return Ok(());
                        }
                        Err(retry_error) => {
                            if retry_error
                                .downcast_ref::<NativeRequestError>()
                                .is_some_and(NativeRequestError::is_expected_turn_mismatch)
                            {
                                mark_input_rejected(state, target.message_id.get()).await?;
                            } else {
                                mark_input_unknown(state, target.message_id.get()).await?;
                            }
                            return Err(retry_error);
                        }
                    }
                }
                mark_input_unknown(state, target.message_id.get()).await?;
                return Err(error);
            }
            mark_input_accepted(state, target.message_id.get(), &thread_id).await?;
            // The active turn's target is immutable. A steering input does not retarget
            // an assistant message that was already being generated.
            return Ok(());
        }
        record_input(
            state,
            input,
            attachments,
            &target,
            &thread_id,
            QueueDelivery::Queued,
        )
        .await?;
        drop(active);
        return Ok(());
    } else {
        record_input(
            state,
            input,
            attachments,
            &target,
            &thread_id,
            QueueDelivery::Submitting,
        )
        .await?;
        let result =
            start_turn_locked(state, &thread_id, input, attachments, &target, &mut active).await;
        drop(active);
        if let Err(error) = result {
            mark_input_unknown(state, target.message_id.get()).await?;
            return Err(error);
        }
        mark_input_accepted(state, target.message_id.get(), &thread_id).await?;
    }
    Ok(())
}

async fn record_input(
    state: &BridgeState,
    input: &str,
    attachments: &[crate::attachments::DownloadedAttachment],
    target: &ReplyTarget,
    thread_id: &str,
    delivery: QueueDelivery,
) -> anyhow::Result<()> {
    let mut queue = state.queued_inputs.lock().await;
    admit_input_and_save(
        &mut queue,
        input,
        target.channel_id.get(),
        target.message_id.get(),
        thread_id,
        delivery,
        state.native.current_generation(),
        &state.queue_path,
        attachments,
    )
}

fn admit_input_and_save(
    queue: &mut VecDeque<QueuedInput>,
    text: &str,
    channel_id: u64,
    message_id: u64,
    native_thread_id: &str,
    delivery: QueueDelivery,
    generation: u64,
    path: &std::path::Path,
    attachments: &[crate::attachments::DownloadedAttachment],
) -> anyhow::Result<()> {
    let mut staged = queue.clone();
    admit_input(
        &mut staged,
        text,
        channel_id,
        message_id,
        native_thread_id,
        delivery,
        generation,
    )?;
    staged
        .back_mut()
        .expect("admitted input is present")
        .attachments = attachments.to_vec();
    save_queue(path, &staged)?;
    *queue = staged;
    Ok(())
}

fn accept_input_and_save(
    queue: &mut VecDeque<QueuedInput>,
    message_id: u64,
    native_thread_id: &str,
    path: &std::path::Path,
) -> anyhow::Result<bool> {
    let Some(input) = queue
        .iter_mut()
        .find(|input| input.message_id == message_id && input.native_thread_id == native_thread_id)
    else {
        return Ok(false);
    };
    // Native acceptance is definite even if its journal write fails.
    input.delivery = QueueDelivery::Accepted;
    save_queue(path, queue)?;
    Ok(true)
}

async fn mark_input_accepted(
    state: &BridgeState,
    message_id: u64,
    native_thread_id: &str,
) -> anyhow::Result<()> {
    let mut queue = state.queued_inputs.lock().await;
    accept_input_and_save(&mut queue, message_id, native_thread_id, &state.queue_path)?;
    Ok(())
}

pub(crate) fn pending_input_count(queue: &VecDeque<QueuedInput>) -> usize {
    queue
        .iter()
        .filter(|input| input.delivery != QueueDelivery::Accepted)
        .count()
}

fn admit_input(
    queue: &mut VecDeque<QueuedInput>,
    text: &str,
    channel_id: u64,
    message_id: u64,
    native_thread_id: &str,
    delivery: QueueDelivery,
    submission_generation: u64,
) -> anyhow::Result<()> {
    if queue.iter().any(|queued| queued.message_id == message_id) {
        anyhow::bail!("Discord message {message_id} already has a recorded native submission");
    }
    queue.push_back(QueuedInput {
        attachments: Vec::new(),
        text: text.to_owned(),
        channel_id,
        message_id,
        native_thread_id: native_thread_id.to_owned(),
        delivery,
        submission_generation,
    });
    Ok(())
}

async fn mark_input_unknown(state: &BridgeState, message_id: u64) -> anyhow::Result<()> {
    let mut queue = state.queued_inputs.lock().await;
    transition_input_delivery(&mut queue, message_id, QueueDelivery::Unknown, None);
    save_queue(&state.queue_path, &queue)
}

async fn mark_input_submitting(state: &BridgeState, message_id: u64) -> anyhow::Result<()> {
    let mut queue = state.queued_inputs.lock().await;
    transition_input_delivery(
        &mut queue,
        message_id,
        QueueDelivery::Submitting,
        Some(state.native.current_generation()),
    );
    save_queue(&state.queue_path, &queue)
}

async fn mark_input_rejected(state: &BridgeState, message_id: u64) -> anyhow::Result<()> {
    let mut queue = state.queued_inputs.lock().await;
    transition_input_delivery(&mut queue, message_id, QueueDelivery::Rejected, None);
    save_queue(&state.queue_path, &queue)
}

fn transition_input_delivery(
    queue: &mut VecDeque<QueuedInput>,
    message_id: u64,
    delivery: QueueDelivery,
    submission_generation: Option<u64>,
) -> bool {
    let Some(input) = queue
        .iter_mut()
        .find(|input| input.message_id == message_id)
    else {
        return false;
    };
    if input.delivery == QueueDelivery::Accepted {
        return false;
    }
    input.delivery = delivery;
    if let Some(generation) = submission_generation {
        input.submission_generation = generation;
    }
    true
}

async fn submit_next_queued_input(state: &BridgeState, thread_id: &str) -> anyhow::Result<()> {
    let phase = state.voice.input_gate.state.lock().await;
    let mut current = state.current_turn.lock().await;
    if (thread_id == state.native.thread_id() && current.is_some())
        || state.active_turns.lock().await.contains_key(thread_id)
    {
        return Ok(());
    }
    let next = {
        let mut queue = state.queued_inputs.lock().await;
        let generation = state.native.current_generation();
        claim_queued_input_when_idle(&phase, &mut queue, thread_id, generation, &state.queue_path)?
    };
    let Some(item) = next else {
        return Ok(());
    };
    let target = ReplyTarget {
        channel_id: ChannelId::new(item.channel_id),
        message_id: serenity::all::MessageId::new(item.message_id),
    };
    let result = start_turn_locked(
        state,
        &item.native_thread_id,
        &item.text,
        &item.attachments,
        &target,
        &mut current,
    )
    .await;
    drop(current);
    match result {
        Ok(()) => {
            mark_input_accepted(state, item.message_id, &item.native_thread_id).await?;
        }
        Err(error) => {
            let mut queue = state.queued_inputs.lock().await;
            transition_input_delivery(&mut queue, item.message_id, QueueDelivery::Unknown, None);
            if let Err(save_error) = save_queue(&state.queue_path, &queue) {
                warn!(%save_error, "failed to persist unknown next-turn delivery");
            }
            warn!(%error, discord_message_id = item.message_id, "queued Discord input delivery is unknown and will not be retried automatically");
        }
    }
    Ok(())
}

fn claim_queued_input_when_idle(
    phase: &crate::voice_input::VoiceInputPhase,
    queue: &mut VecDeque<QueuedInput>,
    thread_id: &str,
    generation: u64,
    path: &std::path::Path,
) -> anyhow::Result<Option<QueuedInput>> {
    if !phase.allow_ordinary() {
        return Ok(None);
    }
    claim_next_queued_input_and_save(queue, thread_id, generation, path)
}

fn claim_next_queued_input_and_save(
    queue: &mut VecDeque<QueuedInput>,
    thread_id: &str,
    generation: u64,
    path: &std::path::Path,
) -> anyhow::Result<Option<QueuedInput>> {
    let mut staged = queue.clone();
    let Some(input) = claim_next_queued_input(&mut staged, thread_id, generation) else {
        return Ok(None);
    };
    save_queue(path, &staged)?;
    *queue = staged;
    Ok(Some(input))
}

fn claim_next_queued_input(
    queue: &mut VecDeque<QueuedInput>,
    thread_id: &str,
    generation: u64,
) -> Option<QueuedInput> {
    if queue.iter().any(|item| {
        item.native_thread_id == thread_id
            && item.delivery == QueueDelivery::Submitting
            && item.submission_generation == generation
    }) {
        return None;
    }
    queue
        .iter_mut()
        .find(|item| item.native_thread_id == thread_id && item.delivery == QueueDelivery::Queued)
        .map(|item| {
            item.delivery = QueueDelivery::Submitting;
            item.submission_generation = generation;
            item.clone()
        })
}

async fn start_turn_locked(
    state: &BridgeState,
    thread_id: &str,
    input: &str,
    attachments: &[crate::attachments::DownloadedAttachment],
    target: &ReplyTarget,
    current: &mut MutexGuard<'_, Option<String>>,
) -> anyhow::Result<()> {
    let client_id = target.message_id.get().to_string();
    state
        .input_targets
        .lock()
        .await
        .insert(client_id.clone(), target.clone());
    let response = state
        .native
        .request_prepared(
            "turn/start",
            serde_json::json!({
                "threadId": thread_id,
                "input": crate::attachments::native_input(input, attachments),
                "clientUserMessageId": client_id
            }),
        )
        .await?;
    let mut active_turns = state.active_turns.lock().await;
    let mut orders = state.turn_observation_orders.lock().await;
    apply_started_turn_response(
        &mut **current,
        &mut active_turns,
        &mut orders,
        thread_id,
        &state.native.thread_id(),
        state.native.current_generation(),
        &response,
    )
}

fn accept_turn_observation(
    orders: &mut HashMap<String, (u64, u64)>,
    thread_id: &str,
    generation: u64,
    observation: &serde_json::Value,
) -> bool {
    let Some(order) = crate::native::state_observation_order(observation) else {
        return false;
    };
    if order.0 != generation
        || orders
            .get(thread_id)
            .is_some_and(|previous| order <= *previous)
    {
        return false;
    }
    if let Some(fence) = crate::native::state_observation_fence(observation) {
        if orders
            .get(thread_id)
            .is_some_and(|previous| previous.0 == order.0 && previous.1 > fence)
        {
            return false;
        }
    }
    orders.insert(thread_id.to_owned(), order);
    true
}

fn apply_turn_state_observation(
    current: &mut Option<String>,
    active_turns: &mut HashMap<String, String>,
    orders: &mut HashMap<String, (u64, u64)>,
    thread_id: &str,
    current_thread_id: &str,
    generation: u64,
    active_turn: &Option<String>,
    observation: &serde_json::Value,
) -> bool {
    if !accept_turn_observation(orders, thread_id, generation, observation) {
        return false;
    }
    if let Some(turn_id) = active_turn {
        active_turns.insert(thread_id.to_owned(), turn_id.to_owned());
    } else {
        active_turns.remove(thread_id);
    }
    if thread_id == current_thread_id {
        *current = active_turn.clone();
    }
    true
}

fn apply_completed_turn_observation(
    current: &mut Option<String>,
    active_turns: &mut HashMap<String, String>,
    orders: &mut HashMap<String, (u64, u64)>,
    thread_id: &str,
    current_thread_id: &str,
    generation: u64,
    turn_id: &str,
    observation: &serde_json::Value,
) -> bool {
    let was_active = active_turns
        .get(thread_id)
        .is_some_and(|active| active == turn_id);
    let was_current = current.as_deref() == Some(turn_id) && thread_id == current_thread_id;
    if !accept_turn_observation(orders, thread_id, generation, observation) {
        // A terminal notification can precede its accepted start reply on the wire.
        // It still ends that same turn, without moving the watermark backwards.
        let same_generation = crate::native::state_observation_order(observation)
            .is_some_and(|order| order.0 == generation);
        if !same_generation || (!was_active && !was_current) {
            return false;
        }
    }
    if was_active {
        active_turns.remove(thread_id);
    }
    if was_current {
        *current = None;
    }
    was_active
}

fn apply_started_turn_response(
    current: &mut Option<String>,
    active_turns: &mut HashMap<String, String>,
    orders: &mut HashMap<String, (u64, u64)>,
    thread_id: &str,
    current_thread_id: &str,
    generation: u64,
    response: &serde_json::Value,
) -> anyhow::Result<()> {
    let turn_id = response
        .pointer("/turn/id")
        .and_then(serde_json::Value::as_str)
        .context("native turn/start response omitted turn ID")?;
    crate::native::state_observation_order(response)
        .context("native turn/start response omitted internal observation order")?;
    apply_turn_state_observation(
        current,
        active_turns,
        orders,
        thread_id,
        current_thread_id,
        generation,
        &Some(turn_id.to_owned()),
        response,
    );
    Ok(())
}

fn load_queue(path: &std::path::Path) -> anyhow::Result<VecDeque<QueuedInput>> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(VecDeque::new()),
        Err(error) => {
            Err(error).with_context(|| format!("read queued Discord inputs {}", path.display()))
        }
    }
}

fn recover_submitting_inputs(queue: &mut VecDeque<QueuedInput>) -> bool {
    let mut changed = false;
    for input in queue {
        if input.delivery == QueueDelivery::Submitting {
            input.delivery = QueueDelivery::Unknown;
            changed = true;
        }
    }
    changed
}

fn load_input_mode(path: &std::path::Path) -> anyhow::Result<Option<InputMode>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("parse Discord settings {}", path.display())
        })?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("read Discord settings {}", path.display()))
        }
    }
}

pub(crate) fn save_input_mode(path: &std::path::Path, mode: InputMode) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .context("Discord settings path has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec(&mode)?)?;
    fs::rename(&temp, path).with_context(|| format!("replace Discord settings {}", path.display()))
}

fn save_queue(path: &std::path::Path, queue: &VecDeque<QueuedInput>) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .context("Discord queue has no parent directory")?;
    fs::create_dir_all(parent)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(queue)?)?;
    fs::rename(&temp, path)
        .with_context(|| format!("replace queued Discord inputs {}", path.display()))
}

fn native_agent_item_key(params: &serde_json::Value) -> Option<(String, String)> {
    Some((
        params.get("threadId")?.as_str()?.to_owned(),
        params.pointer("/item/id")?.as_str()?.to_owned(),
    ))
}

fn remember_voice_item_destination(
    destinations: &mut HashMap<(String, String), ChannelId>,
    params: &serde_json::Value,
    destination: ChannelId,
) -> bool {
    let Some(key) = native_agent_item_key(params) else {
        return false;
    };
    destinations.insert(key, destination);
    true
}

fn take_voice_item_destination(
    destinations: &mut HashMap<(String, String), ChannelId>,
    params: &serde_json::Value,
) -> Option<ChannelId> {
    let key = native_agent_item_key(params)?;
    destinations.remove(&key)
}

async fn handle_native_event(
    http: &serenity::http::Http,
    state: &BridgeState,
    envelope: serde_json::Value,
) {
    let method = envelope
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let params = envelope
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if envelope.get("method").is_some() && envelope.get("id").is_some() {
        if let Err(error) = commands::publish_server_request(http, state, envelope).await {
            error!(%error, "failed to publish native request in Discord");
            if let Some(checks) = &state.installation_checks {
                checks.request_error(&method, &params, &format!("{error:#}"));
            }
        }
        return;
    }
    if let Some(checks) = &state.installation_checks {
        checks.reset_thread(&state.native.thread_id());
        checks.native_event(&method, &params);
    }
    let approval_replies = state.approval_results.lock().await.take_replies_localized(
        &method,
        &params,
        &state.config.language,
    );
    let mut approval_post_error = None;
    let approval_post_attempted = !approval_replies.is_empty();
    for (target, message) in approval_replies {
        // The captured target is the published request card: text-owned cards live in
        // HOME, while voice-owned cards remain in their transcript thread.
        if let Err(error) = target.channel_id.send_message(http, message).await {
            warn!(%error, "failed to publish native approval execution result");
            approval_post_error = Some(format!("{error:#}"));
        }
    }
    if approval_post_attempted {
        if let Some(checks) = &state.installation_checks {
            checks.approval_result_posted(&params, approval_post_error.as_deref());
        }
    }
    match method.as_str() {
        "discodex/thread-state" => {
            apply_thread_snapshot(state, &params).await;
        }
        "discodex/connection-closed" => {
            if let Err(error) = commands::mark_connection_closed(http, state, &params).await {
                warn!(%error, "failed to close pending Discord requests after native connection close");
            }
        }
        "serverRequest/resolved" => {
            if let Err(error) = commands::mark_request_resolved(http, state, &params).await {
                warn!(%error, "failed to close resolved native request in Discord");
            }
        }
        "error" => {
            warn!(native_error = ?params, "native reported an error notification");
        }
        "turn/started" => {
            let turn_id = params
                .get("turn")
                .and_then(|v| v.get("id"))
                .or_else(|| params.get("turnId"))
                .and_then(serde_json::Value::as_str);
            if let Some(turn_id) = turn_id {
                let thread_id = params
                    .get("threadId")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let mut current = state.current_turn.lock().await;
                let mut active_turns = state.active_turns.lock().await;
                let mut orders = state.turn_observation_orders.lock().await;
                apply_turn_state_observation(
                    &mut current,
                    &mut active_turns,
                    &mut orders,
                    thread_id,
                    &state.native.thread_id(),
                    state.native.current_generation(),
                    &Some(turn_id.to_owned()),
                    &params,
                );
            }
        }
        "turn/completed" => {
            let turn_id = params
                .get("turn")
                .and_then(|v| v.get("id"))
                .or_else(|| params.get("turnId"))
                .and_then(serde_json::Value::as_str);
            let thread_id = params
                .get("threadId")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if let Some(turn_id) = turn_id {
                let turn = params.get("turn").unwrap_or(&serde_json::Value::Null);
                if turn_failed(turn) {
                    warn!(native_thread_id = thread_id, native_turn_id = turn_id, native_error = ?turn.get("error"), "native turn failed");
                }
                let failure_reply = take_failed_turn_reply(
                    &mut *state.turn_failure_targets.lock().await,
                    thread_id,
                    turn_id,
                    turn,
                );
                if let Some((target, _message)) = failure_reply {
                    let content = state.config.language.text(
                        "Codex could not complete the task. Please try again.",
                        "Codex가 작업을 완료하지 못했습니다. 다시 시도해 주세요.",
                        "Codex 无法完成此任务。请重试。",
                        "Codex はタスクを完了できませんでした。もう一度お試しください。",
                    );
                    let (channel, message) = conversation_reply_message_localized(
                        &target,
                        ChannelId::new(state.config.home_channel_id),
                        state.config.guild_id,
                        content,
                        &state.config.language,
                    );
                    if let Err(error) = channel.send_message(http, message).await {
                        warn!(%error, discord_message_id = %target.message_id, "failed to publish native turn failure");
                    }
                }
                let mut current = state.current_turn.lock().await;
                let was_active = {
                    let mut active_turns = state.active_turns.lock().await;
                    let mut orders = state.turn_observation_orders.lock().await;
                    apply_completed_turn_observation(
                        &mut current,
                        &mut active_turns,
                        &mut orders,
                        thread_id,
                        &state.native.thread_id(),
                        state.native.current_generation(),
                        turn_id,
                        &params,
                    )
                };
                drop(current);
                if was_active {
                    if let Err(error) = submit_next_queued_input(state, thread_id).await {
                        warn!(%error, "failed to dispatch next-turn input after completion");
                    }
                }
            }
        }
        "item/started" => {
            let item = params.get("item").cloned().unwrap_or_default();
            match item.get("type").and_then(serde_json::Value::as_str) {
                Some("userMessage") => {
                    if let Some(client_id) =
                        item.get("clientId").and_then(serde_json::Value::as_str)
                    {
                        if let Some(target) = state.input_targets.lock().await.remove(client_id) {
                            let thread_id = params
                                .get("threadId")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("");
                            if let Some(turn_id) =
                                params.get("turnId").and_then(serde_json::Value::as_str)
                            {
                                if let Some(checks) = &state.installation_checks {
                                    let generation = params
                                        .get("_discodex_connection_generation")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0);
                                    checks.input_consumed(
                                        generation,
                                        thread_id,
                                        turn_id,
                                        target.message_id.get(),
                                    );
                                }
                                bind_turn_failure_target(
                                    &mut *state.turn_failure_targets.lock().await,
                                    thread_id,
                                    turn_id,
                                    target.clone(),
                                );
                            }
                            state
                                .approval_results
                                .lock()
                                .await
                                .record_consumed_target(&params, target.clone());
                            state
                                .latest_consumed_targets
                                .lock()
                                .await
                                .insert(thread_id.to_owned(), target);
                        }
                        if let Ok(message_id) = client_id.parse::<u64>() {
                            let mut queue = state.queued_inputs.lock().await;
                            let thread_id = params
                                .get("threadId")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("");
                            if let Err(error) = accept_input_and_save(
                                &mut queue,
                                message_id,
                                thread_id,
                                &state.queue_path,
                            ) {
                                warn!(%error, "failed to persist native-consumed input acceptance");
                            }
                        }
                    }
                }
                Some("fileChange" | "commandExecution") => {
                    state
                        .approval_results
                        .lock()
                        .await
                        .freeze_item_target(&params);
                }
                Some("agentMessage") => {
                    match state
                        .voice
                        .native_transcript_destination_for_event("item/started", &envelope)
                    {
                        Ok(Some(destination)) => {
                            remember_voice_item_destination(
                                &mut *state.voice_item_destinations.lock().await,
                                &params,
                                destination,
                            );
                            // The voice reader publishes the native completion
                            // to the owning call's transcript thread.
                            return;
                        }
                        Ok(None) => {}
                        Err(error) => warn!(%error, "could not resolve native voice item owner"),
                    }
                    if let Some(item_id) = item.get("id").and_then(serde_json::Value::as_str) {
                        let target = consumed_reply_target(
                            &state.approval_results.lock().await.consumed_targets,
                            &params,
                        );
                        if let Some(target) = target {
                            let mut item_targets = state.item_targets.lock().await;
                            bind_item_reply_target(&mut *item_targets, item_id, target);
                        }
                    }
                }
                _ => {}
            }
        }
        "item/completed" => {
            let item = params.get("item").cloned().unwrap_or_default();
            if item.get("type").and_then(serde_json::Value::as_str) == Some("agentMessage") {
                let remembered_voice_destination = take_voice_item_destination(
                    &mut *state.voice_item_destinations.lock().await,
                    &params,
                );
                let current_voice_destination = match state
                    .voice
                    .native_transcript_destination_for_event("item/completed", &envelope)
                {
                    Ok(destination) => destination,
                    Err(error) => {
                        warn!(%error, "could not resolve completed native voice item owner");
                        None
                    }
                };
                if remembered_voice_destination.is_some() || current_voice_destination.is_some() {
                    return;
                }
                let text = item
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let item_id = item.get("id").and_then(serde_json::Value::as_str);
                let target = if let Some(id) = item_id {
                    state.item_targets.lock().await.remove(id)
                } else {
                    None
                };
                if let Some(target) = target {
                    if !text.is_empty() {
                        let chunks = split_content(text, 1650);
                        let mut publish_error = None;
                        for (index, chunk) in chunks.iter().enumerate() {
                            let content = (*chunk).to_owned();
                            let home = ChannelId::new(state.config.home_channel_id);
                            let (channel, builder) = if index > 0 && target.channel_id != home {
                                (
                                    home,
                                    CreateMessage::new()
                                        .content(content)
                                        .allowed_mentions(empty_mentions()),
                                )
                            } else {
                                conversation_reply_message_localized(
                                    &target,
                                    home,
                                    state.config.guild_id,
                                    &content,
                                    &state.config.language,
                                )
                            };
                            if let Err(error) = channel.send_message(http, builder).await {
                                warn!(%error, native_item_id = ?item_id, discord_message_id = %target.message_id, "failed to publish native assistant output");
                                publish_error = Some(format!("{error:#}"));
                                break;
                            }
                        }
                        if let Some(checks) = &state.installation_checks {
                            let generation = params
                                .get("_discodex_connection_generation")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0);
                            let thread = params
                                .get("threadId")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("");
                            let turn = params
                                .get("turnId")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("");
                            checks.reply_posted(
                                generation,
                                thread,
                                turn,
                                target.message_id.get(),
                                text.len(),
                                publish_error.as_deref(),
                            );
                        }
                    }
                }
            }
        }
        "thread/settings/updated" => {
            let notice = state.config.language.text(
                "Codex settings changed. Check the current values with /discodex status.",
                "Codex 설정이 변경되었습니다. 현재값은 /discodex status에서 확인할 수 있습니다.",
                "Codex 设置已更改。请使用 /discodex status 查看当前值。",
                "Codex の設定が変更されました。/discodex status で現在の値を確認してください。",
            );
            let _ = ChannelId::new(state.config.home_channel_id)
                .send_message(
                    http,
                    CreateMessage::new()
                        .content(notice)
                        .allowed_mentions(empty_mentions()),
                )
                .await;
        }
        _ => {}
    }
}

async fn apply_thread_snapshot(state: &BridgeState, params: &serde_json::Value) {
    let Some(thread_id) = params.get("threadId").and_then(serde_json::Value::as_str) else {
        return;
    };
    let observed = snapshot_active_turn(params.get("thread").unwrap_or(&serde_json::Value::Null));
    let Some(active_turn) = observed.as_ref() else {
        return;
    };
    let mut current = state.current_turn.lock().await;
    {
        let mut active_turns = state.active_turns.lock().await;
        let mut orders = state.turn_observation_orders.lock().await;
        if !apply_turn_state_observation(
            &mut current,
            &mut active_turns,
            &mut orders,
            thread_id,
            &state.native.thread_id(),
            state.native.current_generation(),
            active_turn,
            params,
        ) {
            return;
        }
    }
    let authoritative_idle = active_turn.is_none();
    drop(current);
    if authoritative_idle {
        if let Err(error) = submit_next_queued_input(state, thread_id).await {
            warn!(%error, "failed to dispatch next-turn input after idle observation");
        }
    }
}

pub(crate) fn snapshot_active_turn(thread: &serde_json::Value) -> Option<Option<String>> {
    let status_type = thread
        .pointer("/status/type")
        .and_then(serde_json::Value::as_str)
        .or_else(|| thread.get("status").and_then(serde_json::Value::as_str));
    if status_type == Some("idle") {
        return Some(None);
    }
    let turns = thread.get("turns")?.as_array()?;
    let active = turns.iter().rev().find(|turn| {
        turn.get("status").and_then(serde_json::Value::as_str) == Some("inProgress")
    })?;
    active
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(|id| Some(id.to_owned()))
}

pub(crate) async fn read_recovery_turn_snapshot(
    native: &Native,
) -> anyhow::Result<serde_json::Value> {
    let thread_id = native.thread_id();
    let metadata = native
        .request(
            "thread/read",
            serde_json::json!({"threadId": thread_id, "includeTurns": false}),
        )
        .await?;
    if metadata
        .pointer("/thread/id")
        .and_then(serde_json::Value::as_str)
        != Some(thread_id.as_str())
    {
        bail!("native recovery metadata returned an unexpected durable thread identity");
    }
    let metadata_thread = metadata.get("thread").unwrap_or(&metadata);
    if snapshot_active_turn(metadata_thread).is_some() {
        return Ok(metadata);
    }
    let history = native
        .request(
            "thread/read",
            serde_json::json!({"threadId": thread_id, "includeTurns": true}),
        )
        .await?;
    if history
        .pointer("/thread/id")
        .and_then(serde_json::Value::as_str)
        != Some(thread_id.as_str())
    {
        bail!("native recovery history returned an unexpected durable thread identity");
    }
    Ok(history)
}

fn bind_turn_failure_target(
    turn_targets: &mut HashMap<(String, String), ReplyTarget>,
    thread_id: &str,
    turn_id: &str,
    target: ReplyTarget,
) {
    if !thread_id.is_empty() && !turn_id.is_empty() {
        turn_targets
            .entry((thread_id.to_owned(), turn_id.to_owned()))
            .or_insert(target);
    }
}

fn turn_failed(turn: &serde_json::Value) -> bool {
    turn.pointer("/status/type")
        .and_then(serde_json::Value::as_str)
        .or_else(|| turn.get("status").and_then(serde_json::Value::as_str))
        == Some("failed")
}

fn take_failed_turn_reply(
    turn_targets: &mut HashMap<(String, String), ReplyTarget>,
    thread_id: &str,
    turn_id: &str,
    turn: &serde_json::Value,
) -> Option<(ReplyTarget, CreateMessage)> {
    let target = turn_targets.remove(&(thread_id.to_owned(), turn_id.to_owned()))?;
    if !turn_failed(turn) {
        return None;
    }
    let message = assistant_reply_message(
        &target,
        "Codex could not complete the task. Please try again.",
    );
    Some((target, message))
}

pub(crate) fn source_message_url(guild_id: u64, target: &ReplyTarget) -> String {
    format!(
        "https://discord.com/channels/{guild_id}/{}/{}",
        target.channel_id.get(),
        target.message_id.get()
    )
}

fn with_source_link_localized(
    target: &ReplyTarget,
    home_channel: ChannelId,
    guild_id: u64,
    content: &str,
    language: &crate::i18n::Language,
) -> String {
    if target.channel_id == home_channel {
        content.to_owned()
    } else {
        format!(
            "{content}\n\n{}: {}",
            language.text(
                "Source message",
                "원본 메시지",
                "原始消息",
                "元のメッセージ"
            ),
            source_message_url(guild_id, target)
        )
    }
}

#[cfg(test)]
pub(crate) fn conversation_reply_message(
    target: &ReplyTarget,
    home_channel: ChannelId,
    guild_id: u64,
    content: &str,
) -> (ChannelId, CreateMessage) {
    conversation_reply_message_localized(
        target,
        home_channel,
        guild_id,
        content,
        &crate::i18n::Language::English,
    )
}

pub(crate) fn conversation_reply_message_localized(
    target: &ReplyTarget,
    home_channel: ChannelId,
    guild_id: u64,
    content: &str,
    language: &crate::i18n::Language,
) -> (ChannelId, CreateMessage) {
    let content = with_source_link_localized(target, home_channel, guild_id, content, language);
    let builder = if target.channel_id == home_channel {
        assistant_reply_message(target, &content)
    } else {
        CreateMessage::new()
            .content(content)
            .allowed_mentions(empty_mentions())
    };
    (home_channel, builder)
}

fn assistant_reply_message(target: &ReplyTarget, content: &str) -> CreateMessage {
    CreateMessage::new()
        .content(content)
        .reference_message(
            MessageReference::new(
                serenity::all::MessageReferenceKind::default(),
                target.channel_id,
            )
            .message_id(target.message_id),
        )
        .allowed_mentions(empty_mentions())
}

fn consumed_reply_target(
    consumed_targets: &HashMap<(u64, String, String), ReplyTarget>,
    params: &serde_json::Value,
) -> Option<ReplyTarget> {
    let generation = params.get("_discodex_connection_generation")?.as_u64()?;
    let thread_id = params.get("threadId")?.as_str()?;
    let turn_id = params.get("turnId")?.as_str()?;
    consumed_targets
        .get(&(generation, thread_id.to_owned(), turn_id.to_owned()))
        .cloned()
}

fn bind_item_reply_target(
    item_targets: &mut HashMap<String, ReplyTarget>,
    item_id: &str,
    target: ReplyTarget,
) {
    item_targets.insert(item_id.to_owned(), target);
}

fn split_content(text: &str, max_bytes: usize) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while remaining.len() > max_bytes {
        let mut end = max_bytes;
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        if let Some(boundary) = remaining[..end]
            .rfind('\n')
            .filter(|boundary| *boundary > 0)
        {
            end = boundary;
        }
        chunks.push(&remaining[..end]);
        remaining = &remaining[end..];
    }
    if !remaining.is_empty() {
        chunks.push(remaining);
    }
    chunks
}

fn empty_mentions() -> serenity::builder::CreateAllowedMentions {
    serenity::builder::CreateAllowedMentions::new()
        .all_users(false)
        .all_roles(false)
        .everyone(false)
        .replied_user(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_targets_require_connected_active_current_turn_source_and_post_lag_observation() {
        let active = HashMap::from([
            ("thread-a".to_owned(), "turn-a".to_owned()),
            ("thread-b".to_owned(), "turn-b".to_owned()),
            ("thread-c".to_owned(), "turn-c".to_owned()),
            ("thread-d".to_owned(), "turn-d".to_owned()),
        ]);
        let mut orders = HashMap::from([
            ("thread-a".to_owned(), (4, 12)),
            ("thread-b".to_owned(), (3, 25)),
            ("thread-c".to_owned(), (4, 9)),
            ("thread-d".to_owned(), (4, 13)),
        ]);
        let targets = HashMap::from([
            (
                ("thread-a".to_owned(), "turn-a".to_owned()),
                ReplyTarget {
                    channel_id: ChannelId::new(7),
                    message_id: serenity::all::MessageId::new(70),
                },
            ),
            (
                ("thread-b".to_owned(), "turn-b".to_owned()),
                ReplyTarget {
                    channel_id: ChannelId::new(8),
                    message_id: serenity::all::MessageId::new(80),
                },
            ),
            (
                ("thread-c".to_owned(), "turn-c".to_owned()),
                ReplyTarget {
                    channel_id: ChannelId::new(9),
                    message_id: serenity::all::MessageId::new(90),
                },
            ),
            (
                ("thread-d".to_owned(), "turn-d".to_owned()),
                ReplyTarget {
                    channel_id: ChannelId::new(7),
                    message_id: serenity::all::MessageId::new(71),
                },
            ),
        ]);
        let floor = HashMap::from([("thread-c".to_owned(), (4, 9))]);
        let selected = eligible_typing_channels(
            true,
            4,
            &active,
            &orders,
            &targets,
            &floor,
            ChannelId::new(99),
        );
        assert_eq!(selected, HashSet::from([ChannelId::new(99)]));

        // A newer authoritative state observation after lag makes that turn eligible again.
        orders.insert("thread-c".to_owned(), (4, 10));
        let selected = eligible_typing_channels(
            true,
            4,
            &active,
            &orders,
            &targets,
            &floor,
            ChannelId::new(99),
        );
        assert_eq!(selected, HashSet::from([ChannelId::new(99)]));

        assert!(
            eligible_typing_channels(
                false,
                4,
                &active,
                &orders,
                &targets,
                &floor,
                ChannelId::new(99)
            )
            .is_empty()
        );
        assert!(
            eligible_typing_channels(
                true,
                4,
                &HashMap::new(),
                &orders,
                &targets,
                &floor,
                ChannelId::new(99)
            )
            .is_empty()
        );
        assert!(
            eligible_typing_channels(
                true,
                5,
                &active,
                &orders,
                &targets,
                &floor,
                ChannelId::new(99)
            )
            .is_empty()
        );
    }

    #[test]
    fn conversation_reply_routes_home_and_only_references_same_channel_source() {
        let home = ChannelId::new(5);
        let same = ReplyTarget {
            channel_id: home,
            message_id: serenity::all::MessageId::new(20),
        };
        let (destination, builder) = conversation_reply_message(&same, home, 9, "answer");
        let payload = serde_json::to_value(builder).unwrap();
        assert_eq!(destination, home);
        assert_eq!(payload["message_reference"]["channel_id"], "5");
        assert_eq!(payload["message_reference"]["message_id"], "20");
        assert_eq!(payload["content"], "answer");

        let elsewhere = ReplyTarget {
            channel_id: ChannelId::new(6),
            message_id: serenity::all::MessageId::new(21),
        };
        let (destination, builder) = conversation_reply_message(&elsewhere, home, 9, "answer");
        let payload = serde_json::to_value(builder).unwrap();
        assert_eq!(destination, home);
        assert!(payload.get("message_reference").is_none());
        assert_eq!(
            payload["content"],
            "answer\n\nSource message: https://discord.com/channels/9/6/21"
        );
    }

    #[test]
    fn source_message_label_follows_the_configured_language() {
        let home = ChannelId::new(5);
        let elsewhere = ReplyTarget {
            channel_id: ChannelId::new(6),
            message_id: serenity::all::MessageId::new(21),
        };
        for (language, label) in [
            (crate::i18n::Language::English, "Source message"),
            (crate::i18n::Language::Korean, "원본 메시지"),
            (crate::i18n::Language::Chinese, "原始消息"),
            (crate::i18n::Language::Japanese, "元のメッセージ"),
        ] {
            let (_, builder) =
                conversation_reply_message_localized(&elsewhere, home, 9, "answer", &language);
            let payload = serde_json::to_value(builder).unwrap();
            assert_eq!(
                payload["content"],
                format!("answer\n\n{label}: https://discord.com/channels/9/6/21")
            );
        }
    }

    #[test]
    fn typing_refresh_schedule_deduplicates_channels_and_prunes_idle_targets() {
        let now = Instant::now();
        let channel = ChannelId::new(7);
        let other = ChannelId::new(8);
        let active = HashSet::from([channel, other]);
        let mut schedule = HashMap::new();
        assert_eq!(due_typing_channels(&active, &mut schedule, now).len(), 2);
        schedule.insert(channel, now + TYPING_REFRESH_INTERVAL);
        schedule.insert(other, now + TYPING_REFRESH_INTERVAL);
        assert!(due_typing_channels(&active, &mut schedule, now).is_empty());
        assert_eq!(
            due_typing_channels(&HashSet::new(), &mut schedule, now),
            Vec::<ChannelId>::new()
        );
        assert!(schedule.is_empty());
    }

    #[test]
    fn completed_voice_items_keep_their_session_destination_after_three_rejoins() {
        let mut destinations = HashMap::new();
        for (item_id, destination) in [("voice-A", 201), ("voice-B", 202), ("voice-C", 203)] {
            let started = serde_json::json!({
                "threadId": "persistent-codex-thread",
                "item": {"id": item_id}
            });
            assert!(remember_voice_item_destination(
                &mut destinations,
                &started,
                ChannelId::new(destination),
            ));
        }

        let late_first = serde_json::json!({
            "threadId": "persistent-codex-thread",
            "item": {"id": "voice-A"}
        });
        let late_second = serde_json::json!({
            "threadId": "persistent-codex-thread",
            "item": {"id": "voice-B"}
        });
        let after_third_call = serde_json::json!({
            "threadId": "persistent-codex-thread",
            "item": {"id": "voice-C"}
        });

        assert_eq!(
            take_voice_item_destination(&mut destinations, &late_first),
            Some(ChannelId::new(201)),
        );
        assert_eq!(
            take_voice_item_destination(&mut destinations, &late_second),
            Some(ChannelId::new(202)),
        );
        assert_eq!(
            take_voice_item_destination(&mut destinations, &after_third_call),
            Some(ChannelId::new(203)),
        );
        assert!(destinations.is_empty());

        let ordinary_text_item = serde_json::json!({
            "threadId": "persistent-codex-thread",
            "item": {"id": "text-item"}
        });
        assert!(!destinations.contains_key(&native_agent_item_key(&ordinary_text_item).unwrap()));
    }

    fn observed(generation: u64, order: u64, mut value: serde_json::Value) -> serde_json::Value {
        crate::native::stamp_state_observation(&mut value, generation, order);
        value
    }

    #[test]
    fn accepted_start_response_publishes_actual_turn_identity_before_event_dispatch() {
        let mut current = None;
        let mut active_turns = HashMap::new();
        let mut orders = HashMap::new();
        apply_started_turn_response(
            &mut current,
            &mut active_turns,
            &mut orders,
            "thread-a",
            "thread-a",
            1,
            &observed(
                1,
                1,
                serde_json::json!({"turn": {"id": "accepted-turn-b", "status": "inProgress"}}),
            ),
        )
        .expect("accepted turn response");
        assert_eq!(current.as_deref(), Some("accepted-turn-b"));
        assert_eq!(
            active_turns.get("thread-a").map(String::as_str),
            Some("accepted-turn-b")
        );
    }

    #[test]
    fn accepted_start_for_other_thread_preserves_current_turn() {
        let mut current = Some("current-turn".to_owned());
        let mut active_turns =
            HashMap::from([("thread-current".to_owned(), "current-turn".to_owned())]);
        let mut orders = HashMap::new();
        apply_started_turn_response(
            &mut current,
            &mut active_turns,
            &mut orders,
            "thread-old",
            "thread-current",
            1,
            &observed(
                1,
                1,
                serde_json::json!({"turn": {"id": "accepted-old-thread-turn"}}),
            ),
        )
        .expect("accepted old-thread turn response");
        assert_eq!(current.as_deref(), Some("current-turn"));
        assert_eq!(
            active_turns.get("thread-current").map(String::as_str),
            Some("current-turn")
        );
        assert_eq!(
            active_turns.get("thread-old").map(String::as_str),
            Some("accepted-old-thread-turn")
        );
    }

    #[test]
    fn start_response_without_actual_turn_id_preserves_observed_state() {
        let mut current = Some("observed-turn".to_owned());
        let mut active_turns = HashMap::from([("thread-a".to_owned(), "observed-turn".to_owned())]);
        let original_turns = active_turns.clone();
        let mut orders = HashMap::new();
        for response in [
            serde_json::json!({"turn": {"status": "inProgress"}}),
            serde_json::json!({"turn": {"id": 7}}),
            serde_json::Value::Null,
        ] {
            assert!(
                apply_started_turn_response(
                    &mut current,
                    &mut active_turns,
                    &mut orders,
                    "thread-a",
                    "thread-a",
                    1,
                    &response,
                )
                .is_err()
            );
            assert_eq!(current.as_deref(), Some("observed-turn"));
            assert_eq!(active_turns, original_turns);
        }
    }

    #[test]
    fn accepted_turn_rejects_older_idle_and_started_state_observations() {
        let mut current = None;
        let mut active = HashMap::new();
        let mut orders = HashMap::new();
        apply_started_turn_response(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            &observed(1, 20, serde_json::json!({"turn": {"id": "accepted-b"}})),
        )
        .expect("actual accepted turn ID");
        for (order, turn) in [(10, None), (15, Some("old-a".to_owned()))] {
            assert!(!apply_turn_state_observation(
                &mut current,
                &mut active,
                &mut orders,
                "thread",
                "thread",
                1,
                &turn,
                &observed(1, order, serde_json::json!({})),
            ));
            assert_eq!(current.as_deref(), Some("accepted-b"));
            assert_eq!(active.get("thread").map(String::as_str), Some("accepted-b"));
            assert_eq!(orders.get("thread"), Some(&(1, 20)));
        }
    }

    #[test]
    fn newer_terminal_or_idle_state_allows_one_next_turn_claim() {
        for completed in [true, false] {
            let mut current = None;
            let mut active = HashMap::new();
            let mut orders = HashMap::new();
            apply_started_turn_response(
                &mut current,
                &mut active,
                &mut orders,
                "thread",
                "thread",
                1,
                &observed(1, 20, serde_json::json!({"turn": {"id": "accepted-b"}})),
            )
            .expect("actual accepted turn ID");
            let observation = observed(1, 21, serde_json::json!({}));
            if completed {
                assert!(apply_completed_turn_observation(
                    &mut current,
                    &mut active,
                    &mut orders,
                    "thread",
                    "thread",
                    1,
                    "accepted-b",
                    &observation,
                ));
            } else {
                assert!(apply_turn_state_observation(
                    &mut current,
                    &mut active,
                    &mut orders,
                    "thread",
                    "thread",
                    1,
                    &None,
                    &observation,
                ));
            }
            assert!(current.is_none());
            assert!(!active.contains_key("thread"));
            let mut queue = VecDeque::from([QueuedInput {
                attachments: Vec::new(),
                text: "next".to_owned(),
                channel_id: 42,
                message_id: 77,
                native_thread_id: "thread".to_owned(),
                delivery: QueueDelivery::Queued,
                submission_generation: 1,
            }]);
            let claimed = claim_next_queued_input(&mut queue, "thread", 1)
                .expect("idle permits the production claim");
            assert_eq!(claimed.message_id, 77);
            assert_eq!(claimed.delivery, QueueDelivery::Submitting);
            assert!(claim_next_queued_input(&mut queue, "thread", 1).is_none());
            assert!(!apply_completed_turn_observation(
                &mut current,
                &mut active,
                &mut orders,
                "thread",
                "thread",
                1,
                "accepted-b",
                &observation,
            ));
        }
    }

    #[test]
    fn direct_recovery_and_queued_snapshot_duplicate_share_one_observation() {
        let mut current = Some("previous".to_owned());
        let mut active = HashMap::from([("thread".to_owned(), "previous".to_owned())]);
        let mut orders = HashMap::new();
        let mut direct = observed(
            1,
            30,
            serde_json::json!({
                "thread": {"id": "thread", "status": {"type": "idle"}}
            }),
        );
        crate::native::stamp_state_snapshot(&mut direct, 1, 30, 20);
        let idle = snapshot_active_turn(direct.get("thread").expect("thread"))
            .expect("actual observed idle");
        assert!(apply_turn_state_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            &idle,
            &direct,
        ));
        let mut duplicate = observed(
            1,
            30,
            serde_json::json!({
                "threadId": "thread", "thread": direct["thread"].clone()
            }),
        );
        crate::native::stamp_state_snapshot(&mut duplicate, 1, 30, 20);
        assert!(!apply_turn_state_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            &idle,
            &duplicate,
        ));
        apply_started_turn_response(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            &observed(1, 40, serde_json::json!({"turn": {"id": "accepted-b"}})),
        )
        .expect("accepted after recovery");
        assert!(!apply_turn_state_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            &idle,
            &duplicate,
        ));
        assert_eq!(current.as_deref(), Some("accepted-b"));
        assert_eq!(active.get("thread").map(String::as_str), Some("accepted-b"));
    }

    #[test]
    fn observation_watermarks_isolate_threads_and_reject_obsolete_generations() {
        let mut current = None;
        let mut active = HashMap::new();
        let mut orders = HashMap::new();
        assert!(apply_turn_state_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread-a",
            "thread-a",
            2,
            &Some("a".to_owned()),
            &observed(2, 100, serde_json::json!({})),
        ));
        assert!(apply_turn_state_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread-b",
            "thread-a",
            2,
            &Some("b".to_owned()),
            &observed(2, 50, serde_json::json!({})),
        ));
        assert_eq!(current.as_deref(), Some("a"));
        assert_eq!(active.get("thread-b").map(String::as_str), Some("b"));
        assert!(!apply_turn_state_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread-a",
            "thread-a",
            2,
            &None,
            &observed(1, 999, serde_json::json!({})),
        ));
        assert_eq!(orders.get("thread-a"), Some(&(2, 100)));
        assert!(apply_turn_state_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread-a",
            "thread-a",
            3,
            &None,
            &observed(3, 101, serde_json::json!({})),
        ));
        assert!(current.is_none());
        assert_eq!(active.get("thread-b").map(String::as_str), Some("b"));
        apply_started_turn_response(
            &mut current,
            &mut active,
            &mut orders,
            "thread-a",
            "thread-a",
            3,
            &observed(2, 102, serde_json::json!({"turn": {"id": "old-accepted"}})),
        )
        .expect("stale state does not discard a definite native acceptance");
        assert!(current.is_none());
        assert_eq!(orders.get("thread-a"), Some(&(3, 101)));
        assert!(!active.contains_key("thread-a"));
    }

    #[test]
    fn snapshot_issued_before_new_turn_cannot_clear_it_with_a_later_reply() {
        let mut current = None;
        let mut active = HashMap::new();
        let mut orders = HashMap::new();
        apply_started_turn_response(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            &observed(1, 20, serde_json::json!({"turn": {"id": "accepted-b"}})),
        )
        .expect("accepted turn");
        let mut delayed_read = serde_json::json!({"thread": {"status": {"type": "idle"}}});
        crate::native::stamp_state_snapshot(&mut delayed_read, 1, 30, 10);
        assert!(!apply_turn_state_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            &None,
            &delayed_read,
        ));
        assert_eq!(current.as_deref(), Some("accepted-b"));
        assert_eq!(orders.get("thread"), Some(&(1, 20)));
        let mut fresh_read = serde_json::json!({"thread": {"status": {"type": "idle"}}});
        crate::native::stamp_state_snapshot(&mut fresh_read, 1, 40, 20);
        assert!(apply_turn_state_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            &None,
            &fresh_read,
        ));
        assert!(current.is_none());
        assert!(!active.contains_key("thread"));
        assert_eq!(orders.get("thread"), Some(&(1, 40)));
    }

    #[test]
    fn terminal_received_before_start_reply_clears_only_that_accepted_turn() {
        let mut current = None;
        let mut active = HashMap::new();
        let mut orders = HashMap::new();
        apply_started_turn_response(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            &observed(1, 20, serde_json::json!({"turn": {"id": "accepted-b"}})),
        )
        .expect("accepted response releases current guard before queued completion");
        let earlier_completion = observed(1, 15, serde_json::json!({}));
        assert!(!apply_completed_turn_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            "old-a",
            &earlier_completion,
        ));
        assert_eq!(current.as_deref(), Some("accepted-b"));
        assert!(apply_completed_turn_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            "accepted-b",
            &earlier_completion,
        ));
        assert!(current.is_none());
        assert!(!active.contains_key("thread"));
        assert_eq!(orders.get("thread"), Some(&(1, 20)));
        assert!(!apply_completed_turn_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            "accepted-b",
            &earlier_completion,
        ));
        assert!(!apply_turn_state_observation(
            &mut current,
            &mut active,
            &mut orders,
            "thread",
            "thread",
            1,
            &Some("accepted-b".to_owned()),
            &observed(1, 10, serde_json::json!({})),
        ));
    }

    #[test]
    fn discord_chunks_preserve_unicode_without_exceeding_byte_limit() {
        let source = "한국어🙂line\n".repeat(900);
        let chunks = split_content(&source, 257);
        assert!(chunks.iter().all(|chunk| chunk.len() <= 257));
        assert_eq!(chunks.concat(), source);
    }

    #[test]
    fn assistant_reply_payload_references_source_and_contains_only_native_text() {
        let target = ReplyTarget {
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
        };
        let payload = serde_json::to_value(assistant_reply_message(&target, "native answer"))
            .expect("serialize Discord reply payload");

        assert_eq!(payload["content"], "native answer");
        assert_eq!(payload["message_reference"]["message_id"], "76");
        assert_eq!(payload["message_reference"]["channel_id"], "42");
        assert_eq!(payload["allowed_mentions"]["replied_user"], false);
    }

    fn approval_item_event(generation: u64, item_type: &str, status: &str) -> serde_json::Value {
        let mut params = serde_json::json!({
            "threadId": "private-thread", "turnId": "private-turn", "startedAtMs": 900, "completedAtMs": 1000,
            "item": {
                "id": "private-item", "type": item_type, "status": status,
                "changes": [{"path": "/PRIVATE_PATH", "kind": {"type": "add"}, "diff": "PRIVATE_DIFF"}],
                "command": "PRIVATE_COMMAND", "aggregatedOutput": "PRIVATE_OUTPUT",
                "commandActions": [], "cwd": "/PRIVATE_PATH", "exitCode": 0,
                "content": [], "text": "PRIVATE_ANSWER", "clientId": "76"
            }
        });
        crate::native::stamp_state_observation(&mut params, generation, 1);
        params
    }

    fn approval_request_params() -> serde_json::Value {
        serde_json::json!({
            "threadId": "private-thread", "turnId": "private-turn",
            "itemId": "private-item", "startedAtMs": 900,
            "approvalId": "private-callback", "reason": "PRIVATE_REASON"
        })
    }

    #[test]
    fn approval_result_notification_follows_the_configured_language() {
        let source = ReplyTarget {
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
        };
        for (language, expected) in [
            (crate::i18n::Language::English, "File changes completed."),
            (crate::i18n::Language::Korean, "파일 변경이 완료되었습니다."),
            (crate::i18n::Language::Chinese, "文件更改已完成。"),
            (
                crate::i18n::Language::Japanese,
                "ファイル変更が完了しました。",
            ),
        ] {
            let mut results = ApprovalResults::default();
            results.track_request(
                1,
                &serde_json::json!(17),
                "item/fileChange/requestApproval",
                &approval_request_params(),
                Some(source.clone()),
            );
            let mut replies = results.take_replies_localized(
                "item/completed",
                &approval_item_event(1, "fileChange", "completed"),
                &language,
            );
            assert_eq!(replies.len(), 1);
            let (_, message) = replies.remove(0);
            let payload = serde_json::to_value(message).unwrap();
            assert_eq!(payload["content"], expected);
        }
    }

    #[test]
    fn approval_results_survive_resolved_and_report_only_observed_execution_outcomes() {
        let source = ReplyTarget {
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
        };
        for (item_type, status, expected) in [
            ("fileChange", "completed", "File changes completed."),
            ("fileChange", "declined", "File changes were declined."),
            ("fileChange", "failed", "File changes failed."),
            (
                "commandExecution",
                "completed",
                "Command execution completed.",
            ),
            (
                "commandExecution",
                "declined",
                "Command execution was declined.",
            ),
            ("commandExecution", "failed", "Command execution failed."),
        ] {
            let mut results = ApprovalResults::default();
            let started = approval_item_event(1, item_type, "inProgress");
            results.record_consumed_target(
                &approval_item_event(1, "userMessage", "inProgress"),
                source.clone(),
            );
            results.freeze_item_target(&started);
            let request = approval_request_params();
            let method = format!("item/{item_type}/requestApproval");
            let target = results.request_target(1, &request);
            // Registration does not require a published card. Distinct callbacks for
            // one parent item retain their request IDs and yield one item result.
            results.track_request(1, &serde_json::json!(17), &method, &request, target.clone());
            results.track_request(1, &serde_json::json!(18), &method, &request, target);
            results.bind_request_card(
                1,
                &serde_json::json!(17),
                ReplyTarget {
                    channel_id: ChannelId::new(99),
                    message_id: serenity::all::MessageId::new(88),
                },
            );
            let resolved = serde_json::json!({
                "threadId": "private-thread", "requestId": 17,
                "_discodex_connection_generation": 1
            });
            assert!(
                results
                    .take_replies("serverRequest/resolved", &resolved)
                    .is_empty()
            );
            assert_eq!(results.requests.len(), 2);
            assert!(results.take_replies("item/completed", &started).is_empty());
            let completed = approval_item_event(1, item_type, status);
            let mut replies = results.take_replies("item/completed", &completed);
            assert_eq!(replies.len(), 1);
            let (target, message) = replies.remove(0);
            assert_eq!(target, source);
            let payload = serde_json::to_value(message).expect("serialize observed result reply");
            assert_eq!(payload["content"], expected);
            assert_eq!(payload["message_reference"]["channel_id"], "42");
            assert_eq!(payload["message_reference"]["message_id"], "76");
            assert_eq!(payload["allowed_mentions"]["parse"], serde_json::json!([]));
            assert_eq!(payload["allowed_mentions"]["replied_user"], false);
            assert!(!payload.to_string().contains("private-"));
            assert!(!payload.to_string().contains("PRIVATE_"));
            assert!(results.requests.is_empty());
            assert!(
                results
                    .take_replies("item/completed", &completed)
                    .is_empty()
            );
            assert!(results.take_replies("turn/completed", &serde_json::json!({
                "threadId": "private-thread", "turn": {"id": "private-turn", "status": "completed"},
                "_discodex_connection_generation": 1
            })).is_empty());
        }
        let request = approval_request_params();
        let mut results = ApprovalResults::default();
        results.track_request(
            1,
            &serde_json::json!(17),
            "item/commandExecution/requestApproval",
            &request,
            Some(source),
        );
        let mut nonzero_exit = approval_item_event(1, "commandExecution", "completed");
        nonzero_exit["item"]["exitCode"] = serde_json::json!(7);
        let (_, message) = results
            .take_replies("item/completed", &nonzero_exit)
            .remove(0);
        assert_eq!(
            serde_json::to_value(message).unwrap()["content"],
            "Command execution failed."
        );
        assert!(
            results
                .take_replies(
                    "item/completed",
                    &approval_item_event(1, "fileChange", "completed")
                )
                .is_empty()
        );
    }

    #[test]
    fn approval_result_identity_and_frozen_source_survive_steering_and_reconnect() {
        let first = ReplyTarget {
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
        };
        let later = ReplyTarget {
            channel_id: ChannelId::new(43),
            message_id: serenity::all::MessageId::new(77),
        };
        let mut results = ApprovalResults::default();
        let started = approval_item_event(1, "fileChange", "inProgress");
        results.record_consumed_target(
            &approval_item_event(1, "userMessage", "inProgress"),
            first.clone(),
        );
        results.freeze_item_target(&started);
        results.record_consumed_target(
            &approval_item_event(1, "userMessage", "inProgress"),
            later.clone(),
        );
        results.freeze_item_target(&started);
        let request = approval_request_params();
        let frozen = results.request_target(1, &request);
        assert_eq!(frozen, Some(first.clone()));
        assert!(results.request_target(2, &request).is_none());
        let mut unrelated_request = request.clone();
        unrelated_request["turnId"] = serde_json::json!("unrelated-turn");
        assert!(results.request_target(1, &unrelated_request).is_none());
        results.track_request(
            1,
            &serde_json::json!(17),
            "item/fileChange/requestApproval",
            &request,
            frozen,
        );
        results.record_consumed_target(
            &approval_item_event(2, "userMessage", "inProgress"),
            later.clone(),
        );
        results.freeze_item_target(&approval_item_event(2, "fileChange", "inProgress"));
        let generation_two_target = results.request_target(2, &request);
        results.track_request(
            2,
            &serde_json::json!(17),
            "item/fileChange/requestApproval",
            &request,
            generation_two_target,
        );
        let mut other_request = request.clone();
        other_request["itemId"] = serde_json::json!("private-other-item");
        let other_item_target = results.request_target(1, &other_request);
        results.track_request(
            1,
            &serde_json::json!("17"),
            "item/fileChange/requestApproval",
            &other_request,
            other_item_target,
        );
        // The same native request cannot be reassociated with a later source/item.
        results.track_request(
            1,
            &serde_json::json!(17),
            "item/fileChange/requestApproval",
            &other_request,
            Some(later.clone()),
        );
        assert_eq!(results.requests.len(), 3);
        assert!(
            results
                .take_replies(
                    "item/completed",
                    &approval_item_event(3, "fileChange", "declined")
                )
                .is_empty()
        );
        for field in ["threadId", "turnId"] {
            let mut unrelated = approval_item_event(1, "fileChange", "declined");
            unrelated[field] = serde_json::json!("unrelated");
            assert!(
                results
                    .take_replies("item/completed", &unrelated)
                    .is_empty()
            );
        }
        assert!(
            results
                .take_replies(
                    "item/completed",
                    &approval_item_event(1, "commandExecution", "completed")
                )
                .is_empty()
        );
        assert!(
            results
                .take_replies(
                    "item/completed",
                    &approval_item_event(1, "agentMessage", "completed")
                )
                .is_empty()
        );
        let mut other_item = approval_item_event(1, "fileChange", "declined");
        other_item["item"]["id"] = serde_json::json!("private-other-item");
        assert_eq!(
            results
                .take_replies("item/completed", &other_item)
                .remove(0)
                .0,
            later
        );
        assert_eq!(results.requests.len(), 2);
        assert_eq!(
            results
                .take_replies(
                    "item/completed",
                    &approval_item_event(2, "fileChange", "completed")
                )
                .remove(0)
                .0,
            later
        );
        assert_eq!(results.requests.len(), 1);
        assert_eq!(
            results
                .take_replies(
                    "item/completed",
                    &approval_item_event(1, "fileChange", "declined")
                )
                .remove(0)
                .0,
            first
        );
        assert!(results.requests.is_empty());
    }

    #[test]
    fn approval_request_context_keeps_consumed_text_source_during_active_voice() {
        let original = ReplyTarget {
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
        };
        let steering = ReplyTarget {
            channel_id: ChannelId::new(43),
            message_id: serenity::all::MessageId::new(77),
        };
        let voice_channel = ChannelId::new(44);
        let home_channel = ChannelId::new(45);
        let home_card = ReplyTarget {
            channel_id: home_channel,
            message_id: serenity::all::MessageId::new(78),
        };
        let voice_card = ReplyTarget {
            channel_id: voice_channel,
            message_id: serenity::all::MessageId::new(79),
        };
        let assert_reply = |target: ReplyTarget, message: CreateMessage, expected: &ReplyTarget| {
            assert_eq!(&target, expected);
            let payload =
                serde_json::to_value(message).expect("serialize correlated approval result");
            assert_eq!(
                payload["message_reference"]["channel_id"],
                expected.channel_id.to_string()
            );
            assert_eq!(
                payload["message_reference"]["message_id"],
                expected.message_id.to_string()
            );
            assert_eq!(payload["allowed_mentions"]["parse"], serde_json::json!([]));
            assert_eq!(payload["allowed_mentions"]["replied_user"], false);
            assert!(!payload.to_string().contains("PRIVATE_"));
            assert!(!payload.to_string().contains("private-"));
        };
        for item_type in ["fileChange", "commandExecution"] {
            for status in ["completed", "declined", "failed", "interrupted"] {
                let mut results = ApprovalResults::default();
                results.record_consumed_target(
                    &approval_item_event(1, "userMessage", "inProgress"),
                    original.clone(),
                );
                results.freeze_item_target(&approval_item_event(1, item_type, "inProgress"));
                results.record_consumed_target(
                    &approval_item_event(1, "userMessage", "inProgress"),
                    steering.clone(),
                );
                let request = approval_request_params();
                let (channel, target) = commands::server_request_context(
                    Some(voice_channel),
                    results.request_target(1, &request),
                    home_channel,
                );
                assert_eq!(channel, home_channel);
                assert_eq!(target, Some(original.clone()));
                results.track_request(
                    1,
                    &serde_json::json!(17),
                    &format!("item/{item_type}/requestApproval"),
                    &request,
                    None,
                );
                results.bind_request_card(1, &serde_json::json!(17), home_card.clone());
                let resolved = serde_json::json!({"threadId": "private-thread", "requestId": 17, "_discodex_connection_generation": 1});
                assert!(
                    results
                        .take_replies("serverRequest/resolved", &resolved)
                        .is_empty()
                );
                let (method, terminal) = if status == "interrupted" {
                    (
                        "turn/completed",
                        serde_json::json!({"threadId": "private-thread", "turn": {"id": "private-turn", "status": "interrupted"}, "_discodex_connection_generation": 1}),
                    )
                } else {
                    ("item/completed", approval_item_event(1, item_type, status))
                };
                let mut replies = results.take_replies(method, &terminal);
                assert_eq!(replies.len(), 1);
                let (target, message) = replies.remove(0);
                assert_reply(target, message, &home_card);
            }
        }
        let mut results = ApprovalResults::default();
        let request = approval_request_params();
        // A pure voice turn has no consumed Discord text target to substitute.
        results.freeze_item_target(&approval_item_event(1, "fileChange", "inProgress"));
        let (channel, target) = commands::server_request_context(
            Some(voice_channel),
            results.request_target(1, &request),
            steering.channel_id,
        );
        assert_eq!(channel, voice_channel);
        assert!(target.is_none());
        results.track_request(
            1,
            &serde_json::json!(17),
            "item/fileChange/requestApproval",
            &request,
            target,
        );
        results.bind_request_card(1, &serde_json::json!(17), voice_card.clone());
        results.bind_request_card(1, &serde_json::json!(17), steering.clone());
        assert!(results.take_replies("serverRequest/resolved", &serde_json::json!({"threadId": "private-thread", "requestId": 17, "_discodex_connection_generation": 1})).is_empty());
        let (target, message) = results
            .take_replies(
                "item/completed",
                &approval_item_event(1, "fileChange", "completed"),
            )
            .remove(0);
        assert_reply(target, message, &voice_card);
        results.record_consumed_target(
            &approval_item_event(1, "userMessage", "inProgress"),
            original,
        );
        results.freeze_item_target(&approval_item_event(1, "fileChange", "inProgress"));
        for (generation, field) in [(2, None), (1, Some("threadId")), (1, Some("turnId"))] {
            let mut unrelated = request.clone();
            if let Some(field) = field {
                unrelated[field] = serde_json::json!("unrelated");
            }
            let (_, target) = commands::server_request_context(
                Some(voice_channel),
                results.request_target(generation, &unrelated),
                steering.channel_id,
            );
            assert!(target.is_none());
        }
    }

    #[test]
    fn approval_result_turn_interruption_replies_only_to_the_captured_related_request_card() {
        let card = ReplyTarget {
            channel_id: ChannelId::new(44),
            message_id: serenity::all::MessageId::new(78),
        };
        let later_card = ReplyTarget {
            channel_id: ChannelId::new(45),
            message_id: serenity::all::MessageId::new(79),
        };
        let mut results = ApprovalResults::default();
        results.track_request(
            2,
            &serde_json::json!("card-request"),
            "item/fileChange/requestApproval",
            &approval_request_params(),
            None,
        );
        // Without a correlated Discord consumption, the published request card
        // supplies the same frozen fallback for voice and unknown-source text.
        results.bind_request_card(2, &serde_json::json!("card-request"), card.clone());
        results.bind_request_card(2, &serde_json::json!("card-request"), later_card);
        let mut turn = serde_json::json!({
            "threadId": "private-thread", "turn": {"id": "private-turn", "status": "interrupted"},
            "_discodex_connection_generation": 2
        });
        for status in ["completed", "failed", "inProgress"] {
            turn["turn"]["status"] = serde_json::json!(status);
            assert!(results.take_replies("turn/completed", &turn).is_empty());
        }
        turn["turn"]["status"] = serde_json::json!("interrupted");
        for (pointer, value) in [
            ("/_discodex_connection_generation", serde_json::json!(1)),
            ("/threadId", serde_json::json!("other")),
            ("/turn/id", serde_json::json!("other")),
        ] {
            let mut unrelated = turn.clone();
            *unrelated.pointer_mut(pointer).unwrap() = value;
            assert!(
                results
                    .take_replies("turn/completed", &unrelated)
                    .is_empty()
            );
        }
        let (target, message) = results.take_replies("turn/completed", &turn).remove(0);
        assert_eq!(target, card);
        let payload = serde_json::to_value(message).unwrap();
        assert_eq!(
            payload["content"],
            "The task with an approval request was interrupted."
        );
        assert_eq!(payload["message_reference"]["channel_id"], "44");
        assert_eq!(payload["message_reference"]["message_id"], "78");
        assert_eq!(payload["allowed_mentions"]["parse"], serde_json::json!([]));
        assert_eq!(payload["allowed_mentions"]["replied_user"], false);
        assert!(results.take_replies("turn/completed", &turn).is_empty());
    }

    #[test]
    fn failed_turn_reply_keeps_consumed_source_and_is_published_only_once() {
        let first = ReplyTarget {
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
        };
        let later = ReplyTarget {
            channel_id: ChannelId::new(43),
            message_id: serenity::all::MessageId::new(77),
        };
        let mut turn_targets = HashMap::new();
        bind_turn_failure_target(&mut turn_targets, "thread", "turn-a", first.clone());
        bind_turn_failure_target(&mut turn_targets, "thread", "turn-b", later.clone());
        bind_turn_failure_target(&mut turn_targets, "thread", "turn-a", later);
        let failed = serde_json::json!({
            "status": "failed",
            "error": {"message": "PRIVATE_INTERNAL_PAYLOAD", "requestId": "private-id"}
        });
        let (target, message) =
            take_failed_turn_reply(&mut turn_targets, "thread", "turn-a", &failed)
                .expect("failed turn replies to its consumed input");
        assert_eq!(target, first);
        let payload = serde_json::to_value(message).expect("serialize public failure reply");
        assert_eq!(
            payload["content"],
            "Codex could not complete the task. Please try again."
        );
        assert_eq!(payload["message_reference"]["message_id"], "76");
        assert_eq!(payload["message_reference"]["channel_id"], "42");
        assert_eq!(payload["allowed_mentions"]["parse"], serde_json::json!([]));
        assert_eq!(payload["allowed_mentions"]["replied_user"], false);
        assert!(!payload.to_string().contains("PRIVATE_INTERNAL_PAYLOAD"));
        assert!(!payload.to_string().contains("private-id"));
        assert!(take_failed_turn_reply(&mut turn_targets, "thread", "turn-a", &failed).is_none());
        assert!(
            take_failed_turn_reply(
                &mut turn_targets,
                "thread",
                "turn-b",
                &serde_json::json!({"status": {"type": "completed"}}),
            )
            .is_none()
        );
        assert!(turn_targets.is_empty());
        assert!(turn_failed(
            &serde_json::json!({"status": {"type": "failed"}})
        ));
        assert!(!turn_failed(&serde_json::json!({
            "error": {"message": "PRIVATE_INTERNAL_PAYLOAD"}, "willRetry": false
        })));
    }

    #[test]
    fn agent_message_reply_uses_current_consumed_text_and_preserves_its_frozen_body() {
        let original = ReplyTarget {
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
        };
        let steering = ReplyTarget {
            channel_id: ChannelId::new(43),
            message_id: serenity::all::MessageId::new(77),
        };
        let mut results = ApprovalResults::default();
        let mut item_targets = HashMap::new();
        let latest_thread_target = HashMap::from([("private-thread".to_owned(), original.clone())]);
        let body = "  원문 설명\n```rust\nfn main() {\n    println!(\"공백 보존\");\n}\n```\n\n  "
            .repeat(80);
        // Ordinary text and text consumed during a call use this same production
        // routing path; neither start nor completion consults the voice session.
        for turn_id in ["text-turn", "call-text-turn"] {
            let mut consumed = approval_item_event(1, "userMessage", "inProgress");
            consumed["turnId"] = serde_json::json!(turn_id);
            results.record_consumed_target(&consumed, original.clone());
            let mut started = consumed.clone();
            started["item"] =
                serde_json::json!({"id": turn_id, "type": "agentMessage", "text": ""});
            let source = consumed_reply_target(&results.consumed_targets, &started)
                .expect("the actual current turn consumed the Discord text");
            assert_eq!(source, original);
            bind_item_reply_target(&mut item_targets, turn_id, source);
            results.record_consumed_target(&consumed, steering.clone());
            assert_eq!(
                consumed_reply_target(&results.consumed_targets, &started),
                Some(steering.clone())
            );
            let target = item_targets
                .remove(turn_id)
                .expect("agent item keeps the source frozen at start");
            assert_eq!(target, original);
            let mut delivered_body = String::new();
            for chunk in split_content(&body, 1650) {
                let payload = serde_json::to_value(assistant_reply_message(&target, chunk))
                    .expect("serialize native text reply");
                delivered_body.push_str(payload["content"].as_str().unwrap());
                assert_eq!(payload["message_reference"]["channel_id"], "42");
                assert_eq!(payload["message_reference"]["message_id"], "76");
                assert_eq!(payload["allowed_mentions"]["parse"], serde_json::json!([]));
                assert_eq!(payload["allowed_mentions"]["replied_user"], false);
            }
            assert_eq!(delivered_body, body);
        }
        assert!(item_targets.is_empty());
        // The historical thread endpoint remains available elsewhere, but it is
        // never a fallback for a pure voice/current turn without consumed text.
        assert!(latest_thread_target.contains_key("private-thread"));
        let mut pure_voice = approval_item_event(1, "agentMessage", "inProgress");
        pure_voice["turnId"] = serde_json::json!("voice-only-turn");
        assert!(consumed_reply_target(&results.consumed_targets, &pure_voice).is_none());
        let mut matched = pure_voice.clone();
        matched["turnId"] = serde_json::json!("call-text-turn");
        for (pointer, value) in [
            ("/_discodex_connection_generation", serde_json::json!(2)),
            ("/threadId", serde_json::json!("other-thread")),
            ("/turnId", serde_json::json!("other-turn")),
        ] {
            let mut unrelated = matched.clone();
            *unrelated.pointer_mut(pointer).unwrap() = value;
            assert!(consumed_reply_target(&results.consumed_targets, &unrelated).is_none());
        }
    }

    #[test]
    fn steering_consumption_does_not_retarget_an_started_assistant_item() {
        let first = ReplyTarget {
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
        };
        let later = ReplyTarget {
            channel_id: ChannelId::new(43),
            message_id: serenity::all::MessageId::new(77),
        };
        let mut consumed_targets = HashMap::from([("thread".to_owned(), first.clone())]);
        let mut item_targets = HashMap::new();

        bind_item_reply_target(
            &mut item_targets,
            "assistant-item",
            consumed_targets
                .get("thread")
                .expect("consumed input")
                .clone(),
        );
        consumed_targets.insert("thread".to_owned(), later);

        assert_eq!(item_targets.remove("assistant-item"), Some(first));
    }

    #[test]
    fn thread_snapshot_reconciles_only_observed_turn_status() {
        assert_eq!(
            snapshot_active_turn(&serde_json::json!({
                "status": {"type": "idle"},
                "turns": []
            })),
            Some(None)
        );
        assert_eq!(
            snapshot_active_turn(&serde_json::json!({
                "status": {"type": "active", "activeFlags": []},
                "turns": [
                    {"id": "finished", "status": "completed"},
                    {"id": "active", "status": "inProgress"}
                ]
            })),
            Some(Some("active".to_owned()))
        );
        assert_eq!(
            snapshot_active_turn(&serde_json::json!({
                "status": {"type": "active", "activeFlags": []},
                "turns": []
            })),
            None
        );
        assert_eq!(
            snapshot_active_turn(&serde_json::json!({
                "status": {"type": "active", "activeFlags": []},
                "turns": [{"status": "inProgress"}]
            })),
            None
        );
    }

    #[test]
    fn accepted_delivery_keeps_original_identity_across_reopen_and_redelivery() {
        let dir = tempfile::tempdir().expect("temporary journal");
        let path = dir.path().join("discord-inputs.json");
        let mut queue = VecDeque::new();
        admit_input_and_save(
            &mut queue,
            "same body",
            42,
            77,
            "thread-old",
            QueueDelivery::Submitting,
            7,
            &path,
            &[],
        )
        .expect("durable original admission");
        let original = queue[0].clone();
        assert!(
            !accept_input_and_save(&mut queue, 77, "thread-other", &path)
                .expect("other thread cannot confirm this client ID")
        );
        assert_eq!(queue[0], original);
        assert!(
            accept_input_and_save(&mut queue, 77, "thread-old", &path)
                .expect("actual original thread acceptance")
        );
        let mut accepted = original.clone();
        accepted.delivery = QueueDelivery::Accepted;
        assert_eq!(queue[0], accepted);
        assert_eq!(pending_input_count(&queue), 0);
        queue = load_queue(&path).expect("reopen accepted delivery");
        assert!(!recover_submitting_inputs(&mut queue));
        assert_eq!(queue[0], accepted);
        assert!(
            admit_input_and_save(
                &mut queue,
                "same body",
                43,
                77,
                "thread-new",
                QueueDelivery::Submitting,
                8,
                &path,
                &[],
            )
            .is_err()
        );
        assert_eq!(queue, VecDeque::from([accepted.clone()]));
        assert_eq!(load_queue(&path).expect("unchanged accepted record"), queue);
        admit_input_and_save(
            &mut queue,
            "same body",
            42,
            78,
            "thread-old",
            QueueDelivery::Queued,
            0,
            &path,
            &[],
        )
        .expect("distinct Discord ID with identical body");
        assert_eq!(pending_input_count(&queue), 1);
        let next = claim_next_queued_input_and_save(&mut queue, "thread-old", 8, &path)
            .expect("durable claim")
            .expect("accepted history does not block new input");
        assert_eq!(next.message_id, 78);
        assert_eq!(next.delivery, QueueDelivery::Submitting);
        assert_eq!(next.submission_generation, 8);
        assert_eq!(queue[0], accepted);
        assert_eq!(load_queue(&path).expect("reopen distinct claim"), queue);
    }

    #[test]
    fn voice_queue_waits_through_all_call_phases_then_retains_fifo_and_attachments() {
        use crate::voice_input::VoiceInputPhase;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("discord-inputs.json");
        let attachment = crate::attachments::DownloadedAttachment {
            filename: "reference.png".into(),
            path: dir.path().join("reference.png"),
            is_image: true,
        };
        let mut queue = VecDeque::new();
        admit_input_and_save(
            &mut queue,
            "home",
            10,
            100,
            "thread",
            QueueDelivery::Queued,
            1,
            &path,
            &[attachment.clone()],
        )
        .unwrap();
        admit_input_and_save(
            &mut queue,
            "mention",
            20,
            101,
            "thread",
            QueueDelivery::Queued,
            1,
            &path,
            &[],
        )
        .unwrap();
        let original = queue.clone();
        for phase in [
            VoiceInputPhase::Starting,
            VoiceInputPhase::Active {
                call_id: uuid::Uuid::new_v4(),
                thread_id: "thread".into(),
                transcript_channel_id: 30,
            },
            VoiceInputPhase::Ending,
            VoiceInputPhase::Blocked,
        ] {
            assert!(
                claim_queued_input_when_idle(&phase, &mut queue, "thread", 1, &path)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(queue, original);
            assert_eq!(load_queue(&path).unwrap(), original);
        }
        // Restart preserves the queued messages and their actual source/attachments.
        queue = load_queue(&path).unwrap();
        assert!(!recover_submitting_inputs(&mut queue));
        let first =
            claim_queued_input_when_idle(&VoiceInputPhase::Idle, &mut queue, "thread", 2, &path)
                .unwrap()
                .unwrap();
        assert_eq!((first.channel_id, first.message_id), (10, 100));
        assert_eq!(first.attachments, vec![attachment]);
        assert!(
            claim_queued_input_when_idle(&VoiceInputPhase::Idle, &mut queue, "thread", 2, &path)
                .unwrap()
                .is_none()
        );
        accept_input_and_save(&mut queue, 100, "thread", &path).unwrap();
        let second =
            claim_queued_input_when_idle(&VoiceInputPhase::Idle, &mut queue, "thread", 2, &path)
                .unwrap()
                .unwrap();
        assert_eq!((second.channel_id, second.message_id), (20, 101));
    }

    #[test]
    fn captured_voice_input_never_moves_to_another_call_or_normal_text() {
        use crate::voice_input::VoiceInputPhase;
        let first = uuid::Uuid::new_v4();
        let second = uuid::Uuid::new_v4();
        let phase = VoiceInputPhase::Active {
            call_id: first,
            thread_id: "thread".into(),
            transcript_channel_id: 30,
        };
        assert!(voice_input_matches(&phase, first, "thread", 30));
        assert!(!voice_input_matches(&phase, first, "thread", 10));
        assert!(!voice_input_matches(&phase, first, "another-thread", 30));
        for phase in [
            VoiceInputPhase::Ending,
            VoiceInputPhase::Idle,
            VoiceInputPhase::Active {
                call_id: second,
                thread_id: "thread".into(),
                transcript_channel_id: 31,
            },
        ] {
            assert!(!voice_input_matches(&phase, first, "thread", 30));
        }
    }

    #[test]
    fn failed_admission_or_claim_preserves_memory_and_original_queued_journal() {
        let dir = tempfile::tempdir().expect("temporary journal");
        let path = dir.path().join("discord-inputs.json");
        let mut queue = VecDeque::from([
            QueuedInput {
                attachments: Vec::new(),
                text: "unknown original".to_owned(),
                channel_id: 42,
                message_id: 80,
                native_thread_id: "thread".to_owned(),
                delivery: QueueDelivery::Unknown,
                submission_generation: 7,
            },
            QueuedInput {
                attachments: Vec::new(),
                text: "queued original".to_owned(),
                channel_id: 43,
                message_id: 81,
                native_thread_id: "thread".to_owned(),
                delivery: QueueDelivery::Queued,
                submission_generation: 7,
            },
        ]);
        save_queue(&path, &queue).expect("original queued journal");
        let original = queue.clone();
        let blocked_temp = path.with_extension("json.tmp");
        fs::create_dir(&blocked_temp).expect("block actual journal write");
        assert!(
            admit_input_and_save(
                &mut queue,
                "not submitted",
                44,
                82,
                "thread",
                QueueDelivery::Submitting,
                8,
                &path,
                &[],
            )
            .is_err()
        );
        assert_eq!(queue, original);
        assert_eq!(load_queue(&path).expect("original disk journal"), original);
        assert!(claim_next_queued_input_and_save(&mut queue, "thread", 8, &path).is_err());
        assert_eq!(queue, original);
        assert_eq!(
            load_queue(&path).expect("queued disk identity survives failed claim"),
            original
        );
        fs::remove_dir(&blocked_temp).expect("allow actual journal write");
        let claimed = claim_next_queued_input_and_save(&mut queue, "thread", 8, &path)
            .expect("later durable admission")
            .expect("original queued input remains eligible");
        assert_eq!(claimed.message_id, 81);
        assert_eq!(claimed.channel_id, 43);
        assert_eq!(claimed.text, "queued original");
        assert_eq!(claimed.submission_generation, 8);
        assert_eq!(queue[0], original[0]);
        assert!(
            claim_next_queued_input_and_save(&mut queue, "thread", 8, &path)
                .expect("no duplicate claim")
                .is_none()
        );
    }

    #[test]
    fn acceptance_write_failure_does_not_reclassify_native_acceptance_as_unknown() {
        let dir = tempfile::tempdir().expect("temporary journal");
        let path = dir.path().join("discord-inputs.json");
        let mut queue = VecDeque::from([QueuedInput {
            attachments: Vec::new(),
            text: "accepted body".to_owned(),
            channel_id: 42,
            message_id: 77,
            native_thread_id: "thread-old".to_owned(),
            delivery: QueueDelivery::Submitting,
            submission_generation: 7,
        }]);
        save_queue(&path, &queue).expect("durable submitting admission");
        let blocked_temp = path.with_extension("json.tmp");
        fs::create_dir(&blocked_temp).expect("block actual acceptance journal write");
        assert!(accept_input_and_save(&mut queue, 77, "thread-old", &path).is_err());
        assert_eq!(queue[0].delivery, QueueDelivery::Accepted);
        assert_eq!(queue[0].submission_generation, 7);
        assert_eq!(queue[0].native_thread_id, "thread-old");
        assert!(!transition_input_delivery(
            &mut queue,
            77,
            QueueDelivery::Unknown,
            None
        ));
        assert!(!transition_input_delivery(
            &mut queue,
            77,
            QueueDelivery::Rejected,
            None
        ));
        assert_eq!(queue[0].delivery, QueueDelivery::Accepted);
        assert!(
            admit_input(
                &mut queue,
                "accepted body",
                42,
                77,
                "thread-new",
                QueueDelivery::Submitting,
                8
            )
            .is_err()
        );
        fs::remove_dir(&blocked_temp).expect("restore journal write");
        assert!(
            accept_input_and_save(&mut queue, 77, "thread-old", &path)
                .expect("confirmation retries persistence without another native submission")
        );
        assert_eq!(load_queue(&path).expect("reopen accepted identity"), queue);
    }

    #[test]
    fn queued_next_turn_inputs_survive_store_reopen() {
        let dir = tempfile::tempdir().expect("temporary state folder");
        let path = dir.path().join("discord-inputs.json");
        let mut queue = VecDeque::new();
        queue.push_back(QueuedInput {
            attachments: Vec::new(),
            text: "later input".to_owned(),
            channel_id: 42,
            message_id: 76,
            native_thread_id: "thread-a".to_owned(),
            delivery: QueueDelivery::Queued,
            submission_generation: 0,
        });
        save_queue(&path, &queue).expect("persist next-turn input");
        let reopened = load_queue(&path).expect("load next-turn input");
        assert_eq!(reopened, queue);
    }

    #[test]
    fn submitting_input_becomes_unknown_after_reopen_and_keeps_message_identity() {
        let dir = tempfile::tempdir().expect("temporary state folder");
        let path = dir.path().join("discord-inputs.json");
        let mut queue = VecDeque::from([QueuedInput {
            attachments: Vec::new(),
            text: "accepted response may be lost".to_owned(),
            channel_id: 42,
            message_id: 77,
            native_thread_id: "thread-a".to_owned(),
            delivery: QueueDelivery::Submitting,
            submission_generation: 7,
        }]);
        save_queue(&path, &queue).expect("persist submitting input");
        queue = load_queue(&path).expect("reopen submitting input");
        assert!(recover_submitting_inputs(&mut queue));
        save_queue(&path, &queue).expect("persist unknown input");
        queue = load_queue(&path).expect("reopen unknown input");
        assert_eq!(queue[0].message_id, 77);
        assert_eq!(queue[0].delivery, QueueDelivery::Unknown);

        assert!(
            admit_input(
                &mut queue,
                "accepted response may be lost",
                42,
                77,
                "thread-a",
                QueueDelivery::Submitting,
                7,
            )
            .is_err()
        );
        assert!(
            admit_input(
                &mut queue,
                "accepted response may be lost",
                42,
                78,
                "thread-a",
                QueueDelivery::Submitting,
                8,
            )
            .is_ok()
        );
        save_queue(&path, &queue).expect("persist reopened admissions");
        let reopened = load_queue(&path).expect("reopen admissions");
        assert_eq!(reopened.len(), 2);
        assert_eq!(reopened[0].message_id, 77);
        assert_eq!(reopened[0].delivery, QueueDelivery::Unknown);
        assert_eq!(reopened[1].message_id, 78);
        assert_eq!(reopened[1].delivery, QueueDelivery::Submitting);
        assert_eq!(reopened[0].submission_generation, 7);
        assert_eq!(reopened[1].submission_generation, 8);
    }

    #[test]
    fn reconnect_generation_does_not_block_new_queued_input() {
        let dir = tempfile::tempdir().expect("temporary state folder");
        let path = dir.path().join("discord-inputs.json");
        let mut queue = VecDeque::from([
            QueuedInput {
                attachments: Vec::new(),
                text: "uncertain first input".to_owned(),
                channel_id: 42,
                message_id: 80,
                native_thread_id: "thread-a".to_owned(),
                delivery: QueueDelivery::Submitting,
                submission_generation: 7,
            },
            QueuedInput {
                attachments: Vec::new(),
                text: "queued second input".to_owned(),
                channel_id: 42,
                message_id: 81,
                native_thread_id: "thread-a".to_owned(),
                delivery: QueueDelivery::Queued,
                submission_generation: 7,
            },
            QueuedInput {
                attachments: Vec::new(),
                text: "queued third input".to_owned(),
                channel_id: 42,
                message_id: 82,
                native_thread_id: "thread-a".to_owned(),
                delivery: QueueDelivery::Queued,
                submission_generation: 7,
            },
        ]);
        save_queue(&path, &queue).expect("persist reconnect queue");
        queue = load_queue(&path).expect("reopen reconnect queue");

        let next = claim_next_queued_input(&mut queue, "thread-a", 8)
            .expect("new generation should claim queued input");
        assert_eq!(next.message_id, 81);
        assert_eq!(next.delivery, QueueDelivery::Submitting);
        assert_eq!(next.submission_generation, 8);
        assert_eq!(queue[0].message_id, 80);
        assert_eq!(queue[0].delivery, QueueDelivery::Submitting);
        assert_eq!(queue[0].submission_generation, 7);
        assert!(claim_next_queued_input(&mut queue, "thread-a", 8).is_none());
        accept_input_and_save(&mut queue, 81, "thread-a", &path).expect("persist accepted input");
        let following = claim_next_queued_input(&mut queue, "thread-a", 8)
            .expect("next queued input should proceed after N is consumed");
        assert_eq!(following.message_id, 82);
        assert_eq!(following.delivery, QueueDelivery::Submitting);
        assert_eq!(following.submission_generation, 8);
        save_queue(&path, &queue).expect("persist generation transition");
        let reopened = load_queue(&path).expect("reopen generation transition");
        assert_eq!(reopened[0].message_id, 80);
        assert_eq!(reopened[1].message_id, 81);
        assert_eq!(reopened[1].delivery, QueueDelivery::Accepted);
        assert_eq!(reopened[1].submission_generation, 8);
        assert_eq!(reopened[2].message_id, 82);
        assert_eq!(reopened[2].delivery, QueueDelivery::Submitting);
        assert_eq!(reopened[2].submission_generation, 8);
    }

    #[test]
    fn rejected_input_reenters_current_generation_without_claiming_other_queue() {
        let dir = tempfile::tempdir().expect("temporary state folder");
        let path = dir.path().join("discord-inputs.json");
        let mut queue = VecDeque::from([
            QueuedInput {
                attachments: Vec::new(),
                text: "rejected input".to_owned(),
                channel_id: 42,
                message_id: 90,
                native_thread_id: "thread-a".to_owned(),
                delivery: QueueDelivery::Rejected,
                submission_generation: 7,
            },
            QueuedInput {
                attachments: Vec::new(),
                text: "unrelated queued input".to_owned(),
                channel_id: 42,
                message_id: 91,
                native_thread_id: "thread-a".to_owned(),
                delivery: QueueDelivery::Queued,
                submission_generation: 7,
            },
        ]);
        save_queue(&path, &queue).expect("persist rejected input");
        queue = load_queue(&path).expect("reopen rejected input");
        assert!(transition_input_delivery(
            &mut queue,
            90,
            QueueDelivery::Submitting,
            Some(8),
        ));
        save_queue(&path, &queue).expect("persist retry admission");
        queue = load_queue(&path).expect("reopen retry admission");
        assert_eq!(queue[0].delivery, QueueDelivery::Submitting);
        assert_eq!(queue[0].submission_generation, 8);
        assert!(claim_next_queued_input(&mut queue, "thread-a", 8).is_none());
        accept_input_and_save(&mut queue, 90, "thread-a", &path).expect("persist accepted retry");
        assert_eq!(
            claim_next_queued_input(&mut queue, "thread-a", 8)
                .unwrap()
                .message_id,
            91
        );
    }

    #[test]
    fn attachment_paths_survive_queue_restart_and_claim() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("queue.json");
        let files = vec![crate::attachments::DownloadedAttachment {
            filename: "photo.png".to_owned(),
            path: dir.path().join("download.png"),
            is_image: true,
        }];
        let mut queue = VecDeque::new();
        admit_input_and_save(
            &mut queue,
            "look at this",
            42,
            77,
            "thread",
            QueueDelivery::Queued,
            1,
            &journal,
            &files,
        )
        .unwrap();
        let mut recovered = load_queue(&journal).unwrap();
        assert_eq!(recovered, queue);
        let claimed = claim_next_queued_input_and_save(&mut recovered, "thread", 2, &journal)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.attachments, files);
        assert_eq!(claimed.channel_id, 42);
        assert_eq!(claimed.delivery, QueueDelivery::Submitting);
        assert!(
            admit_input_and_save(
                &mut recovered,
                "replacement",
                43,
                77,
                "thread",
                QueueDelivery::Queued,
                2,
                &journal,
                &[]
            )
            .is_err()
        );
        assert_eq!(load_queue(&journal).unwrap()[0].attachments, files);
    }

    #[test]
    fn older_queued_inputs_have_no_implicit_attachments() {
        let input: QueuedInput = serde_json::from_value(serde_json::json!({
            "text": "old input", "channel_id": 42, "message_id": 77
        }))
        .unwrap();
        assert!(input.attachments.is_empty());
        assert_eq!(input.text, "old input");
        assert_eq!(input.delivery, QueueDelivery::Queued);
    }
}
