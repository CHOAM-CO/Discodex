use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, anyhow, bail};
use serde_json::{Value, json};
use serenity::{
    all::{
        ButtonStyle, ChannelId, ChannelType, CommandDataOption, CommandDataOptionValue,
        CommandInteraction, CommandOptionType, ComponentInteraction, Context, CreateActionRow,
        CreateButton, CreateCommand, CreateCommandOption, CreateInteractionResponse,
        CreateInteractionResponseMessage, CreateModal, GuildChannel, GuildId, Member,
        ModalInteraction, PartialGuild, Permissions, UserId,
    },
    builder::{CreateAllowedMentions, CreateMessage},
};
use uuid::Uuid;

use crate::discord::{BridgeState, PendingApproval, ReplyTarget, StateKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsKind {
    Hub,
    Model,
    Effort,
    Fast,
    Permissions,
    Input,
    Voice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SettingsChoice {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SettingsMenuState {
    pub claimed: bool,
    pub requester_id: u64,
    pub connection_generation: u64,
    pub thread_id: String,
    pub kind: SettingsKind,
    pub page: usize,
    pub choices: Vec<SettingsChoice>,
    pub current: String,
    pub current_value: Option<String>,
    pub model_scope: Option<String>,
}

pub fn definitions(language: crate::i18n::Language) -> Vec<CreateCommand> {
    let mut command = CreateCommand::new("discodex").description(language.text(
        "Manage the current Discodex conversation and settings",
        "현재 Discodex 대화와 설정을 관리합니다",
        "管理当前 Discodex 对话和设置",
        "現在の Discodex 会話と設定を管理します",
    ));
    command = command
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "settings",
            language.text(
                "Choose conversation settings with buttons",
                "버튼으로 대화 설정을 선택합니다",
                "使用按钮选择对话设置",
                "ボタンで会話設定を選択します",
            ),
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "voice",
            language.text(
                "Choose a voice profile with buttons",
                "버튼으로 음성 프로필을 선택합니다",
                "使用按钮选择语音配置",
                "ボタンで音声プロファイルを選択します",
            ),
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "status",
            language.text(
                "Show conversation connection and settings",
                "현재 대화 연결과 설정을 조회합니다",
                "查看当前对话连接和设置",
                "会話の接続状態と設定を表示します",
            ),
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "interrupt",
            language.text(
                "Stop the current response",
                "현재 진행 중인 응답을 중단합니다",
                "中断当前回复",
                "現在の応答を中断します",
            ),
        ))
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "new",
                language.text(
                    "Start a new Codex conversation",
                    "새 Codex 대화를 시작합니다",
                    "开始新的 Codex 对话",
                    "新しい Codex 会話を開始します",
                ),
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "active-turn",
                    language.text(
                        "How to handle the active response",
                        "진행 중인 응답의 처리 방식",
                        "如何处理正在进行的回复",
                        "進行中の応答の扱い",
                    ),
                )
                .required(false)
                .add_string_choice(
                    language.text(
                        "Finish, then start a new conversation",
                        "마친 뒤 새 대화",
                        "完成后开始新对话",
                        "完了後に新しい会話を開始",
                    ),
                    "finish",
                )
                .add_string_choice(
                    language.text("Interrupt first", "먼저 중단", "先中断", "先に中断"),
                    "interrupt",
                ),
            ),
        )
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "voice-leave",
            language.text(
                "Leave the current voice call",
                "현재 음성 통화를 종료합니다",
                "离开当前语音通话",
                "現在の音声通話から退出します",
            ),
        ))
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "voice-join",
                language.text(
                    "Join an allowed voice channel",
                    "허용한 음성 채널에 참여합니다",
                    "加入允许的语音频道",
                    "許可された音声チャンネルに参加します",
                ),
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::Channel,
                    "channel",
                    language.text(
                        "Allowed voice channel to join",
                        "참여할 허용 음성 채널",
                        "要加入的允许语音频道",
                        "参加する許可済み音声チャンネル",
                    ),
                )
                .required(true),
            ),
        );
    vec![command]
}

pub async fn handle(ctx: &Context, interaction: &CommandInteraction) -> anyhow::Result<()> {
    let state = state_async(ctx).await?;
    if let Err(error) = authorize(
        &state,
        interaction.guild_id,
        interaction.user.id,
        state.config.language,
    ) {
        let chunks = split_discord_content(&format!("{error:#}"));
        interaction
            .create_response(
                ctx,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(chunks[0].clone())
                        .ephemeral(true),
                ),
            )
            .await?;
        send_command_followups(ctx, interaction, &chunks[1..]).await?;
        return Ok(());
    }
    interaction
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    if settings_ui_requested(interaction) {
        let response = match settings_menu_from_command(interaction, &state).await {
            Ok((content, rows)) => {
                send_command_settings_panel(ctx, interaction, &content, rows).await
            }
            Err(error) => {
                tracing::warn!(error = ?error, "failed to build Discord settings menu");
                interaction
                    .edit_response(
                        ctx,
                        serenity::builder::EditInteractionResponse::new().content(
                            state.config.language.text(
                                "Could not load the current settings choices.",
                                "현재 설정 선택지를 불러오지 못했습니다.",
                                "无法加载当前设置选项。",
                                "現在の設定候補を読み込めませんでした。",
                            ),
                        ),
                    )
                    .await
                    .map(|_| ())
            }
        };
        if let Some(checks) = &state.installation_checks {
            let error = response.as_ref().err().map(|error| format!("{error:#}"));
            checks.settings_response(interaction.id.get(), error.as_deref());
        }
        response?;
        return Ok(());
    }

    let content = match handle_authorized(ctx, interaction, state.clone()).await {
        Ok(content) => content,
        Err(error) => {
            tracing::warn!(error = ?error, "Discord Codex command failed");
            if let Some(checks) = &state.installation_checks {
                if interaction.data.options.first().is_some_and(|sub| {
                    matches!(
                        sub.name.as_str(),
                        "model" | "effort" | "fast" | "permissions"
                    )
                }) {
                    checks.settings_confirmed(
                        interaction.id.get(),
                        &state.native.thread_id(),
                        false,
                        false,
                        Some(&format!("{error:#}")),
                    );
                }
            }
            state
                .config
                .language
                .text(
                    "The Discodex request failed. Check the input and connection status.",
                    "Discodex 요청을 처리하지 못했습니다. 입력과 연결 상태를 확인해 주세요.",
                    "Discodex 请求未能处理。请检查输入和连接状态。",
                    "Discodex リクエストを処理できませんでした。入力と接続状態を確認してください。",
                )
                .to_owned()
        }
    };
    let chunks = split_discord_content(&content);
    let response = interaction
        .edit_response(
            ctx,
            serenity::builder::EditInteractionResponse::new()
                .content(chunks[0].clone())
                .allowed_mentions(no_mentions()),
        )
        .await;
    if let Some(checks) = &state.installation_checks {
        let error = response.as_ref().err().map(|error| format!("{error:#}"));
        checks.settings_response(interaction.id.get(), error.as_deref());
    }
    response?;
    send_command_followups(ctx, interaction, &chunks[1..]).await?;
    Ok(())
}

async fn send_command_followups(
    ctx: &Context,
    interaction: &CommandInteraction,
    chunks: &[String],
) -> anyhow::Result<()> {
    for chunk in chunks {
        interaction
            .create_followup(
                ctx,
                serenity::builder::CreateInteractionResponseFollowup::new()
                    .content(chunk)
                    .ephemeral(true)
                    .allowed_mentions(no_mentions()),
            )
            .await?;
    }
    Ok(())
}

async fn handle_authorized(
    ctx: &Context,
    interaction: &CommandInteraction,
    state: Arc<BridgeState>,
) -> anyhow::Result<String> {
    let language = state.config.language;
    let Some(subcommand) = interaction.data.options.first() else {
        return Ok(language
            .text(
                "Use `/discodex status`.",
                "`/discodex status`를 사용하세요.",
                "请使用 `/discodex status`。",
                "`/discodex status` を使用してください。",
            )
            .to_owned());
    };
    let sub = subcommand.name.as_str();
    let no_options = Vec::new();
    let sub_options = match &subcommand.value {
        CommandDataOptionValue::SubCommand(options) => options,
        _ => &no_options,
    };
    let arg = sub_options.first();
    let arg_string = || arg.and_then(option_string).map(str::to_owned);
    let mut output = String::new();
    match sub {
        "status" => {
            let thread = state.native.refresh_thread_state().await;
            match thread {
                Ok(value) => {
                    output = format!(
                        "{}\n{}: {}\n{}: {}",
                        language.text(
                            "Current Codex conversation status",
                            "현재 Codex 대화 상태",
                            "当前 Codex 对话状态",
                            "現在の Codex 会話の状態"
                        ),
                        language.text("Voice call", "음성 통화", "语音通话", "音声通話"),
                        if state
                            .voice
                            .is_active(GuildId::new(state.config.guild_id))
                            .await
                        {
                            language.text("Connected", "연결됨", "已连接", "接続中")
                        } else {
                            language.text("Not connected", "연결되지 않음", "未连接", "未接続")
                        },
                        language.text("Settings", "현재 설정", "当前设置", "現在の設定"),
                        format_settings_with_language(&settings_snapshot(&value), language)
                    )
                }
                Err(error) => {
                    tracing::warn!(error = ?error, "failed to refresh native thread state for Discord status");
                    output = language
                        .text(
                            "Could not read the current conversation status.",
                            "현재 대화 상태를 조회하지 못했습니다.",
                            "无法读取当前对话状态。",
                            "現在の会話状態を取得できませんでした。",
                        )
                        .to_owned()
                }
            }
            output.push_str(&format!(
                "\n{}: {}",
                language.text(
                    "Input delivery",
                    "입력 전달 방식",
                    "输入传递方式",
                    "入力の送信方法"
                ),
                input_mode(*state.input_mode.read().await, language)
            ));
            output.push_str(&format!(
                "\n{}: {}",
                language.text(
                    "Active response",
                    "진행 중인 응답",
                    "正在进行的回复",
                    "進行中の応答"
                ),
                if state.current_turn.lock().await.is_some() {
                    language.text("Yes", "있음", "有", "あり")
                } else {
                    language.text("No", "없음", "没有", "なし")
                }
            ));
            output.push_str(&format!(
                "\n{}: {}",
                language.text(
                    "Queued inputs",
                    "대기 중인 입력",
                    "排队的输入",
                    "待機中の入力"
                ),
                crate::discord::pending_input_count(&*state.queued_inputs.lock().await)
            ));
            if let Some(checks) = &state.installation_checks {
                output.push_str(&format!(
                    "\n{}\n",
                    language.text(
                        "Installation feature checks",
                        "설치 기능 확인",
                        "安装功能检查",
                        "インストール機能の確認"
                    )
                ));
                output.push_str(&checks.summary());
            }
        }
        "interrupt" => {
            let turn_id = state
                .current_turn
                .lock()
                .await
                .clone()
                .ok_or_else(|| anyhow!("현재 진행 중인 응답이 없습니다"))?;
            let result = state
                .native
                .request(
                    "turn/interrupt",
                    json!({"threadId":state.native.thread_id(),"turnId":turn_id}),
                )
                .await?;
            let _ = result;
            output = language
                .text(
                    "Asked the current response to stop.",
                    "현재 진행 중인 응답의 중단을 요청했습니다.",
                    "已请求中断当前回复。",
                    "現在の応答に中断を要求しました。",
                )
                .to_owned();
        }
        "new" => {
            let active_turn = state.current_turn.lock().await.clone();
            let expected_active_turn = active_turn.clone();
            let active_choice = arg_string();
            if active_turn.is_some() && active_choice.is_none() {
                bail!(
                    language.text("A response is in progress. Choose how to handle it before starting a new conversation.", "진행 중인 응답이 있습니다. 새 대화를 시작하기 전에 처리 방식을 선택해 주세요.", "有回复正在进行。开始新对话前，请选择如何处理。", "応答が進行中です。新しい会話を始める前に処理方法を選択してください。")
                );
            }
            if let Some(turn_id) = active_turn {
                match active_choice.as_deref() {
                    Some("interrupt") => {
                        let result = state
                            .native
                            .request(
                                "turn/interrupt",
                                json!({"threadId":state.native.thread_id(),"turnId":turn_id}),
                            )
                            .await?;
                        let _ = result;
                        output.push_str(language.text(
                            "Asked the existing response to stop.\n",
                            "기존 응답의 중단을 요청했습니다.\n",
                            "已请求中断现有回复。\n",
                            "既存の応答に中断を要求しました。\n",
                        ));
                    }
                    Some("finish") => output.push_str(language.text(
                        "The existing response will continue in the previous conversation.\n",
                        "기존 응답은 이전 대화에서 계속 생성됩니다.\n",
                        "现有回复将继续在之前的对话中生成。\n",
                        "既存の応答は前の会話で引き続き生成されます。\n",
                    )),
                    _ => bail!(language.text(
                        "Choose how to handle the active response.",
                        "진행 중인 응답의 처리 방식을 선택해 주세요",
                        "请选择如何处理正在进行的回复。",
                        "進行中の応答の扱いを選択してください。"
                    )),
                }
            }
            if !state.voice.input_gate.state.lock().await.allow_ordinary() {
                state
                    .voice
                    .leave(GuildId::new(state.config.guild_id))
                    .await?;
            }
            let input_phase = state.voice.input_gate.state.lock().await;
            if !input_phase.allow_ordinary() {
                bail!(language.text(
                    "Wait for the voice call to end before starting a new conversation.",
                    "음성 통화 종료가 확인된 뒤 새 대화를 시작해 주세요.",
                    "请先确认语音通话已结束，再开始新对话。",
                    "音声通話の終了を確認してから新しい会話を始めてください。"
                ));
            }
            let _settings_change = state.settings_change_lock.lock().await;
            let previous_thread_id = state.native.thread_id();
            if state.queued_inputs.lock().await.iter().any(|input| {
                input.native_thread_id == previous_thread_id
                    && matches!(
                        input.delivery,
                        crate::discord::QueueDelivery::Queued
                            | crate::discord::QueueDelivery::Submitting
                    )
            }) {
                bail!(language.text("Wait for queued messages in this conversation to finish before starting a new one.", "현재 대화의 대기 메시지가 처리된 뒤 새 대화를 시작해 주세요.", "请等待当前对话中的排队消息处理完毕后再开始新对话。", "この会話の待機中メッセージが処理されてから新しい会話を始めてください。"));
            }
            // Input observation must not pair the new thread with the previous turn.
            let mut current = state.current_turn.lock().await;
            if current
                .as_ref()
                .is_some_and(|turn| Some(turn) != expected_active_turn.as_ref())
            {
                bail!(
                    language.text("A queued message has started. Choose again how to handle the active work before starting a new conversation.", "대기 메시지의 작업이 시작되었습니다. 새 대화를 시작하기 전에 진행 중인 작업의 처리 방식을 다시 선택해 주세요.", "排队消息已开始处理。开始新对话前，请重新选择如何处理当前任务。", "待機中のメッセージの処理が始まりました。新しい会話を始める前に、進行中の作業の扱いを再度選択してください。")
                );
            }
            let started = state.native.start_new().await;
            if started.is_ok() || state.native.thread_id() != previous_thread_id {
                *current = None;
            }
            drop(current);
            let thread_id = started?;
            if let Some(checks) = &state.installation_checks {
                checks.reset_thread(&thread_id);
            }
            output.push_str(language.text(
                "Started a new Codex conversation.",
                "새 Codex 대화를 시작했습니다.",
                "已开始新的 Codex 对话。",
                "新しい Codex 会話を開始しました。",
            ));
        }
        "voice-join" => {
            let channel_id = arg
                .and_then(option_channel)
                .ok_or_else(|| anyhow!("음성 채널을 지정하세요"))?;
            if !state.config.voice_channel_ids.contains(&channel_id.get()) {
                bail!(language.text(
                    "This voice channel is not allowlisted.",
                    "허용 목록에 없는 음성 채널입니다",
                    "此语音频道不在允许列表中。",
                    "この音声チャンネルは許可リストにありません。"
                ));
            }
            state
                .voice
                .join(
                    GuildId::new(state.config.guild_id),
                    channel_id,
                    interaction.user.id,
                )
                .await?;
            output = format!(
                "{} <#{}>{}",
                language.text(
                    "Joined voice channel",
                    "음성 채널에 참여했습니다",
                    "已加入语音频道",
                    "音声チャンネルに参加しました"
                ),
                channel_id.get(),
                language.text(".", ".", "。", "。")
            );
        }
        "voice-leave" => {
            state
                .voice
                .leave(GuildId::new(state.config.guild_id))
                .await?;
            output = language
                .text(
                    "Left the voice call.",
                    "음성 통화를 종료했습니다.",
                    "已退出语音通话。",
                    "音声通話から退出しました。",
                )
                .to_owned();
        }
        _ => bail!(
            "{}: {sub}",
            language.text(
                "Unknown `/discodex` command",
                "알 수 없는 `/discodex` 명령입니다",
                "未知的 `/discodex` 命令",
                "不明な `/discodex` コマンドです"
            )
        ),
    }
    Ok(output)
}

const SETTINGS_PAGE_SIZE: usize = 20;

fn settings_ui_requested(interaction: &CommandInteraction) -> bool {
    interaction
        .data
        .options
        .first()
        .is_some_and(|subcommand| matches!(subcommand.name.as_str(), "settings" | "voice"))
}

async fn send_command_settings_panel(
    ctx: &Context,
    interaction: &CommandInteraction,
    content: &str,
    rows: Vec<CreateActionRow>,
) -> serenity::Result<()> {
    let chunks = split_discord_content(content);
    interaction
        .edit_response(
            ctx,
            serenity::builder::EditInteractionResponse::new()
                .content(chunks.first().cloned().unwrap_or_default())
                .components(rows)
                .allowed_mentions(no_mentions()),
        )
        .await?;
    for chunk in chunks.iter().skip(1) {
        interaction
            .create_followup(
                ctx,
                serenity::builder::CreateInteractionResponseFollowup::new()
                    .content(chunk)
                    .ephemeral(true)
                    .allowed_mentions(no_mentions()),
            )
            .await?;
    }
    Ok(())
}

async fn settings_menu_from_command(
    interaction: &CommandInteraction,
    state: &Arc<BridgeState>,
) -> anyhow::Result<(String, Vec<CreateActionRow>)> {
    let subcommand = interaction
        .data
        .options
        .first()
        .ok_or_else(|| anyhow!("setting command is missing"))?;
    let kind = match subcommand.name.as_str() {
        "settings" => SettingsKind::Hub,
        "voice" => SettingsKind::Voice,
        _ => bail!("unsupported settings menu"),
    };
    let id = Uuid::new_v4().simple().to_string();
    let menu = build_settings_menu(
        state,
        kind,
        0,
        interaction.user.id.get(),
        state.native.current_generation(),
    )
    .await?;
    let response = render_settings_menu(&id, &menu, state.config.language);
    let current_generation = state.native.current_generation();
    let current_thread_id = state.native.thread_id();
    let mut menus = state.settings_menus.lock().await;
    menus.retain(|_, previous| {
        previous.connection_generation == current_generation
            && previous.thread_id == current_thread_id
    });
    menus.insert(id, menu);
    drop(menus);
    Ok(response)
}

async fn build_settings_menu(
    state: &BridgeState,
    kind: SettingsKind,
    page: usize,
    requester_id: u64,
    connection_generation: u64,
) -> anyhow::Result<SettingsMenuState> {
    let language = state.config.language;
    if state.native.current_generation() != connection_generation {
        bail!("Codex connection changed while settings were opening");
    }
    let target_thread_id = state.native.thread_id();
    let thread = state.native.refresh_thread_state().await?;
    let model_scope = thread
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let (current_value, current, choices) = match kind {
        SettingsKind::Hub => {
            let mut current = format_settings_with_language(&settings_snapshot(&thread), language);
            let mode = *state.input_mode.read().await;
            current.push_str(&format!(
                "\n{}: {}",
                language.text(
                    "Input delivery",
                    "입력 전달 방식",
                    "输入传递方式",
                    "入力の送信方法"
                ),
                input_mode(mode, language)
            ));
            let selected_voice = state.voice.selected_voice()?;
            let voice = selected_voice.unwrap_or_else(|| {
                language
                    .text(
                        "Codex default",
                        "Codex 기본 설정",
                        "Codex 默认设置",
                        "Codex の既定設定",
                    )
                    .to_owned()
            });
            current.push_str(&format!(
                "\n{}: {voice}",
                language.text(
                    "Voice profile",
                    "음성 프로필",
                    "语音配置",
                    "音声プロファイル"
                )
            ));
            let choices = [
                ("model", "Model", "모델", "模型", "モデル"),
                (
                    "effort",
                    "Reasoning effort",
                    "추론 강도",
                    "推理强度",
                    "推論の強さ",
                ),
                ("fast", "Fast mode", "Fast 모드", "Fast 模式", "Fastモード"),
                (
                    "permissions",
                    "Permission profile",
                    "권한 프로필",
                    "权限配置",
                    "権限プロファイル",
                ),
                (
                    "input",
                    "Input delivery",
                    "입력 전달 방식",
                    "输入传递方式",
                    "入力の送信方法",
                ),
                (
                    "voice",
                    "Voice profile",
                    "음성 프로필",
                    "语音配置",
                    "音声プロファイル",
                ),
            ]
            .into_iter()
            .map(|(value, en, ko, zh, ja)| SettingsChoice {
                value: value.to_owned(),
                label: language.text(en, ko, zh, ja).to_owned(),
            })
            .collect();
            (None, current, choices)
        }
        SettingsKind::Model => {
            let catalog = full_catalog(state, "model/list", json!({"includeHidden":false})).await?;
            let choices = catalog
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|model| {
                    let value = model.get("id")?.as_str()?;
                    let label = model
                        .get("displayName")
                        .and_then(Value::as_str)
                        .or_else(|| model.get("name").and_then(Value::as_str))
                        .unwrap_or(value);
                    Some(SettingsChoice {
                        value: value.to_owned(),
                        label: label.to_owned(),
                    })
                })
                .collect::<Vec<_>>();
            let selected = model_scope.clone();
            let current = choice_current_label(&choices, selected.as_deref(), language);
            (selected, current, choices)
        }
        SettingsKind::Effort | SettingsKind::Fast => {
            let catalog = full_catalog(state, "model/list", json!({"includeHidden":false})).await?;
            let model = selected_model(&catalog, &model_scope)?;
            if kind == SettingsKind::Effort {
                let choices = model
                    .get("supportedReasoningEfforts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|item| {
                        item.get("reasoningEffort")
                            .and_then(Value::as_str)
                            .or_else(|| item.as_str())
                            .map(|value| SettingsChoice {
                                value: value.to_owned(),
                                label: value.to_owned(),
                            })
                    })
                    .collect::<Vec<_>>();
                let selected = thread
                    .get("effort")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let current = choice_current_label(&choices, selected.as_deref(), language);
                (selected, current, choices)
            } else {
                let mut choices = vec![SettingsChoice {
                    value: "off".to_owned(),
                    label: language.text("Off", "끔", "关闭", "オフ").to_owned(),
                }];
                choices.extend(
                    model
                        .get("serviceTiers")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|item| {
                            let value = item
                                .get("id")
                                .and_then(Value::as_str)
                                .or_else(|| item.as_str())?;
                            let label = item.get("name").and_then(Value::as_str).unwrap_or(value);
                            Some(SettingsChoice {
                                value: value.to_owned(),
                                label: label.to_owned(),
                            })
                        }),
                );
                let selected = thread
                    .get("serviceTier")
                    .filter(|value| !value.is_null())
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| "off".to_owned());
                let current = choice_current_label(&choices, Some(&selected), language);
                (Some(selected), current, choices)
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
                .text(
                    "Codex default",
                    "Codex 기본 설정",
                    "Codex 默认设置",
                    "Codex の既定設定",
                )
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
    let page = if choices.is_empty() {
        0
    } else {
        page.min((choices.len() - 1) / SETTINGS_PAGE_SIZE)
    };
    Ok(SettingsMenuState {
        claimed: false,
        requester_id,
        connection_generation,
        thread_id: target_thread_id,
        kind,
        page,
        choices,
        current,
        current_value,
        model_scope,
    })
}

fn choice_current_label(
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
                .text("Not reported", "확인할 수 없음", "未报告", "未取得")
                .to_owned()
        })
}

fn settings_kind_title(kind: SettingsKind, language: crate::i18n::Language) -> &'static str {
    match kind {
        SettingsKind::Hub => language.text("Settings", "설정", "设置", "設定"),
        SettingsKind::Model => language.text("Model", "모델", "模型", "モデル"),
        SettingsKind::Effort => {
            language.text("Reasoning effort", "추론 강도", "推理强度", "推論の強さ")
        }
        SettingsKind::Fast => language.text("Fast mode", "Fast 모드", "Fast 模式", "Fastモード"),
        SettingsKind::Permissions => language.text(
            "Permission profile",
            "권한 프로필",
            "权限配置",
            "権限プロファイル",
        ),
        SettingsKind::Input => language.text(
            "Input delivery",
            "입력 전달 방식",
            "输入传递方式",
            "入力の送信方法",
        ),
        SettingsKind::Voice => language.text(
            "Voice profile",
            "음성 프로필",
            "语音配置",
            "音声プロファイル",
        ),
    }
}

fn render_settings_menu(
    menu_id: &str,
    menu: &SettingsMenuState,
    language: crate::i18n::Language,
) -> (String, Vec<CreateActionRow>) {
    let mut content = format!(
        "{}\n{}: {}",
        settings_kind_title(menu.kind, language),
        language.text("Current", "현재", "当前", "現在"),
        menu.current
    );
    if menu.kind != SettingsKind::Hub {
        let pages = menu.choices.len().div_ceil(SETTINGS_PAGE_SIZE).max(1);
        content.push_str(&format!(
            "\n{} {}/{}",
            language.text(
                "Choose an option · page",
                "선택지 · 페이지",
                "选择一项 · 页码",
                "選択肢 · ページ"
            ),
            menu.page + 1,
            pages
        ));
    } else {
        content.push_str(&format!(
            "\n{}",
            language.text(
                "Choose a setting",
                "설정을 선택하세요",
                "请选择设置",
                "設定を選んでください"
            )
        ));
    }

    if menu.kind != SettingsKind::Hub {
        let start = menu.page * SETTINGS_PAGE_SIZE;
        let end = (start + SETTINGS_PAGE_SIZE).min(menu.choices.len());
        for choice in &menu.choices[start..end] {
            content.push_str(&format!("\n• {} — {}", choice.label, choice.value));
        }
    }

    let start = if menu.kind == SettingsKind::Hub {
        0
    } else {
        menu.page * SETTINGS_PAGE_SIZE
    };
    let end = (start
        + if menu.kind == SettingsKind::Hub {
            menu.choices.len()
        } else {
            SETTINGS_PAGE_SIZE
        })
    .min(menu.choices.len());
    let mut rows = Vec::new();
    for (offset, group) in menu.choices[start..end].chunks(5).enumerate() {
        let buttons = group
            .iter()
            .enumerate()
            .map(|(within, choice)| {
                let index = start + offset * 5 + within;
                let is_hub = menu.kind == SettingsKind::Hub;
                let marker =
                    if !is_hub && menu.current_value.as_deref() == Some(choice.value.as_str()) {
                        "✓ "
                    } else {
                        ""
                    };
                let custom_id = if is_hub {
                    format!("codex:settings:{menu_id}:o{index}")
                } else {
                    format!("codex:settings:{menu_id}:c{index}")
                };
                let label = discord_button_label(&format!("{marker}{}", choice.label));
                CreateButton::new(custom_id)
                    .label(label)
                    .style(if marker.is_empty() {
                        ButtonStyle::Secondary
                    } else {
                        ButtonStyle::Success
                    })
            })
            .collect();
        rows.push(CreateActionRow::Buttons(buttons));
    }
    if menu.kind != SettingsKind::Hub {
        let pages = menu.choices.len().div_ceil(SETTINGS_PAGE_SIZE).max(1);
        let nav = vec![
            CreateButton::new(format!(
                "codex:settings:{menu_id}:p{}",
                menu.page.saturating_sub(1)
            ))
            .label(language.text("Previous", "이전", "上一页", "前へ"))
            .style(ButtonStyle::Secondary)
            .disabled(menu.page == 0),
            CreateButton::new(format!("codex:settings:{menu_id}:h"))
                .label(language.text("Settings", "설정", "设置", "設定"))
                .style(ButtonStyle::Primary),
            CreateButton::new(format!(
                "codex:settings:{menu_id}:p{}",
                (menu.page + 1).min(pages - 1)
            ))
            .label(language.text("Next", "다음", "下一页", "次へ"))
            .style(ButtonStyle::Secondary)
            .disabled(menu.page + 1 >= pages),
        ];
        rows.push(CreateActionRow::Buttons(nav));
    }
    (content, rows)
}

fn split_discord_content(content: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut count = 0;
    for character in content.chars() {
        if count == 2000 {
            chunks.push(std::mem::take(&mut current));
            count = 0;
        }
        current.push(character);
        count += 1;
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn discord_button_label(value: &str) -> String {
    let mut chars = value.chars();
    let label = chars.by_ref().take(80).collect::<String>();
    if chars.next().is_some() {
        label.chars().take(77).collect::<String>() + "…"
    } else {
        label
    }
}

fn settings_kind_from_key(value: &str) -> Option<SettingsKind> {
    match value {
        "model" => Some(SettingsKind::Model),
        "effort" => Some(SettingsKind::Effort),
        "fast" => Some(SettingsKind::Fast),
        "permissions" => Some(SettingsKind::Permissions),
        "input" => Some(SettingsKind::Input),
        "voice" => Some(SettingsKind::Voice),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SettingsClaimError {
    Missing,
    OtherUser,
    Busy,
}

fn claim_settings_menu(
    menus: &mut HashMap<String, SettingsMenuState>,
    menu_id: &str,
    requester_id: u64,
) -> Result<SettingsMenuState, SettingsClaimError> {
    let menu = menus.get_mut(menu_id).ok_or(SettingsClaimError::Missing)?;
    if menu.requester_id != requester_id {
        return Err(SettingsClaimError::OtherUser);
    }
    if menu.claimed {
        return Err(SettingsClaimError::Busy);
    }
    menu.claimed = true;
    Ok(menu.clone())
}

fn release_settings_menu(menus: &mut HashMap<String, SettingsMenuState>, menu_id: &str) {
    if let Some(menu) = menus.get_mut(menu_id) {
        menu.claimed = false;
    }
}

fn finish_settings_update(
    menus: &mut HashMap<String, SettingsMenuState>,
    previous_id: &str,
    replacement_id: &str,
    delivered: bool,
) {
    if delivered {
        menus.remove(previous_id);
    } else {
        // A failed HTTP response can be ambiguous: either version may be visible.
        release_settings_menu(menus, previous_id);
    }
    release_settings_menu(menus, replacement_id);
}

async fn handle_settings_component(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
) -> anyhow::Result<()> {
    let parts = interaction.data.custom_id.split(':').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "codex" || parts[1] != "settings" {
        bail!("unknown settings component id");
    }
    let menu_id = parts[2];
    let action = parts[3];
    let claimed = claim_settings_menu(
        &mut *state.settings_menus.lock().await,
        menu_id,
        interaction.user.id.get(),
    );
    let menu = match claimed {
        Ok(menu) => menu,
        Err(reason) => {
            let language = state.config.language;
            let content = match reason {
                SettingsClaimError::Missing => language.text(
                    "This panel is no longer active. Open `/discodex settings` again.",
                    "이 설정 화면은 더 이상 유효하지 않습니다. `/discodex settings`를 다시 여세요.",
                    "此设置面板已失效。请重新打开 `/discodex settings`。",
                    "このパネルは無効です。`/discodex settings` を開き直してください。",
                ),
                SettingsClaimError::OtherUser => language.text(
                    "Only the person who opened this settings panel can use it.",
                    "이 설정 화면을 연 사용자만 사용할 수 있습니다.",
                    "只有打开此设置面板的用户可以使用它。",
                    "この設定パネルを開いたユーザーだけが操作できます。",
                ),
                SettingsClaimError::Busy => language.text(
                    "This setting is still being processed. Please wait a moment.",
                    "설정을 처리 중입니다. 잠시 기다려 주세요.",
                    "正在处理设置，请稍候。",
                    "設定を処理中です。少々お待ちください。",
                ),
            };
            interaction
                .create_response(
                    ctx,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content(content)
                            .ephemeral(true),
                    ),
                )
                .await?;
            return Ok(());
        }
    };
    let result = async {
        interaction
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await
            .context("acknowledge settings button")?;
        if menu.connection_generation != state.native.current_generation()
            || menu.thread_id != state.native.thread_id()
        {
            interaction
                .edit_response(
                    ctx,
                    serenity::builder::EditInteractionResponse::new()
                        .content(state.config.language.text(
                            "The Codex connection changed. Open settings again.",
                            "Codex 연결이 변경되었습니다. 설정을 다시 여세요.",
                            "Codex 连接已更改，请重新打开设置。",
                            "Codex 接続が変わりました。設定を開き直してください。",
                        ))
                        .components(Vec::new()),
                )
                .await?;
            state.settings_menus.lock().await.remove(menu_id);
            return Ok(());
        }
        handle_claimed_settings_action(ctx, interaction, state, menu_id, action, menu.clone()).await
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(error = ?error, "Discord settings action failed; existing buttons remain retryable");
        let notice = state.config.language.text(
            "Could not update settings. You can retry the buttons below.",
            "설정을 갱신하지 못했습니다. 아래 버튼으로 다시 시도할 수 있습니다.",
            "无法更新设置。可以重试下方按钮。",
            "設定を更新できませんでした。下のボタンで再試行できます。",
        );
        // An edit error can mean the new panel reached Discord but its response
        // was lost. Never overwrite either visible version with a recovery edit.
        release_settings_menu(&mut *state.settings_menus.lock().await, menu_id);
        let detail = settings_error_detail(&error);
        for chunk in split_discord_content(&format!("{notice}\n{detail}")) {
            interaction
                .create_followup(
                    ctx,
                    serenity::builder::CreateInteractionResponseFollowup::new()
                        .content(chunk)
                        .ephemeral(true)
                        .allowed_mentions(no_mentions()),
                )
                .await?;
        }
    }
    Ok(())
}

fn settings_error_detail(error: &anyhow::Error) -> String {
    // HTTP errors can contain interaction-token URLs. The API error itself is
    // enough to explain a failed edit without displaying those URLs.
    for cause in error.chain() {
        if let Some(serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(
            response,
        ))) = cause.downcast_ref::<serenity::Error>()
        {
            return format!(
                "Discord {}: {} ({})",
                response.status_code, response.error.message, response.error.code
            );
        }
        if cause.downcast_ref::<serenity::Error>().is_some() {
            return format!("Discord: {}", error);
        }
    }
    format!("{error:#}")
}

async fn handle_claimed_settings_action(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
    menu_id: &str,
    action: &str,
    mut menu: SettingsMenuState,
) -> anyhow::Result<()> {
    let prefix = action
        .chars()
        .next()
        .ok_or_else(|| anyhow!("empty settings action"))?;
    let suffix = action
        .strip_prefix(prefix)
        .ok_or_else(|| anyhow!("invalid settings action"))?;
    let index = if suffix.is_empty() {
        None
    } else {
        suffix.parse::<usize>().ok()
    };
    if prefix == 'o' && menu.kind == SettingsKind::Hub {
        let index = index.ok_or_else(|| anyhow!("invalid settings choice"))?;
        let selected = menu
            .choices
            .get(index)
            .ok_or_else(|| anyhow!("unknown settings choice"))?;
        menu.kind = settings_kind_from_key(&selected.value)
            .ok_or_else(|| anyhow!("unknown settings choice"))?;
        menu.page = 0;
    } else if prefix == 'h' && suffix.is_empty() {
        menu.kind = SettingsKind::Hub;
        menu.page = 0;
    } else if prefix == 'p' {
        menu.page = index.unwrap_or(0);
    } else if prefix == 'c' {
        let index = index.ok_or_else(|| anyhow!("invalid settings choice"))?;
        let selected = menu
            .choices
            .get(index)
            .cloned()
            .ok_or_else(|| anyhow!("unknown settings choice"))?;
        let fresh = build_settings_menu(
            state,
            menu.kind,
            menu.page,
            menu.requester_id,
            menu.connection_generation,
        )
        .await?;
        if matches!(menu.kind, SettingsKind::Effort | SettingsKind::Fast)
            && fresh.model_scope != menu.model_scope
        {
            return update_settings_panel(ctx, interaction, state, menu_id, fresh).await;
        }
        if !fresh
            .choices
            .iter()
            .any(|choice| choice.value == selected.value)
        {
            let stale = state.config.language.text(
                "That choice is no longer available. Current choices were refreshed.",
                "선택한 항목을 더 이상 사용할 수 없어 최신 선택지를 표시했습니다.",
                "此选项已不可用，已刷新当前选项。",
                "選んだ項目は利用できなくなったため、選択肢を更新しました。",
            );
            return update_settings_panel_with_notice(
                ctx,
                interaction,
                state,
                menu_id,
                fresh,
                stale,
            )
            .await;
        }
        let result = match apply_setting_choice(
            state,
            interaction.id.get(),
            menu.kind,
            &selected.value,
            Some(&menu.thread_id),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(error = ?error, "failed to apply Discord settings choice");
                state
                    .config
                    .language
                    .text(
                        "Could not apply that setting. The current choices are shown below.",
                        "설정을 적용하지 못했습니다. 현재 선택지를 아래에 표시합니다.",
                        "无法应用该设置。当前选项如下。",
                        "設定を適用できませんでした。現在の選択肢を表示します。",
                    )
                    .to_owned()
            }
        };
        let refreshed = build_settings_menu(
            state,
            menu.kind,
            menu.page,
            menu.requester_id,
            menu.connection_generation,
        )
        .await?;
        return update_settings_panel_with_notice(
            ctx,
            interaction,
            state,
            menu_id,
            refreshed,
            &result,
        )
        .await;
    } else {
        bail!("unknown settings action");
    }

    let refreshed = build_settings_menu(
        state,
        menu.kind,
        menu.page,
        menu.requester_id,
        menu.connection_generation,
    )
    .await?;
    update_settings_panel(ctx, interaction, state, menu_id, refreshed).await
}

async fn update_settings_panel(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
    previous_id: &str,
    menu: SettingsMenuState,
) -> anyhow::Result<()> {
    update_settings_panel_with_notice(ctx, interaction, state, previous_id, menu, "").await
}

async fn update_settings_panel_with_notice(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
    previous_id: &str,
    mut menu: SettingsMenuState,
    notice: &str,
) -> anyhow::Result<()> {
    let menu_id = Uuid::new_v4().simple().to_string();
    let (content, rows) = render_settings_menu(&menu_id, &menu, state.config.language);
    let content = if notice.is_empty() {
        content
    } else {
        format!("{notice}\n\n{content}")
    };
    let chunks = split_discord_content(&content);
    menu.claimed = true;
    state
        .settings_menus
        .lock()
        .await
        .insert(menu_id.clone(), menu);
    let response = interaction
        .edit_response(
            ctx,
            serenity::builder::EditInteractionResponse::new()
                .content(chunks.first().cloned().unwrap_or_default())
                .components(rows)
                .allowed_mentions(no_mentions()),
        )
        .await;
    finish_settings_update(
        &mut *state.settings_menus.lock().await,
        previous_id,
        &menu_id,
        response.is_ok(),
    );
    response.context("edit settings panel")?;
    for chunk in chunks.iter().skip(1) {
        if let Err(error) = interaction
            .create_followup(
                ctx,
                serenity::builder::CreateInteractionResponseFollowup::new()
                    .content(chunk)
                    .ephemeral(true)
                    .allowed_mentions(no_mentions()),
            )
            .await
        {
            tracing::warn!(%error, "settings panel updated but continuation delivery failed");
        }
    }
    Ok(())
}

async fn apply_setting_choice(
    state: &BridgeState,
    interaction_id: u64,
    kind: SettingsKind,
    requested: &str,
    expected_thread_id: Option<&str>,
) -> anyhow::Result<String> {
    let _settings_change = state.settings_change_lock.lock().await;
    if expected_thread_id.is_some_and(|thread| thread != state.native.thread_id()) {
        bail!("Codex conversation changed before the setting could be applied");
    }
    let thread_id = expected_thread_id
        .map(str::to_owned)
        .unwrap_or_else(|| state.native.thread_id());
    let language = state.config.language;
    match kind {
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
            let active_turn = state.current_turn.lock().await.is_some();
            if let Some(checks) = &state.installation_checks {
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
            Ok(format_permission_settings_result_with_language(
                &display_name,
                &settings,
                active_turn,
                language,
            ))
        }
        SettingsKind::Input => {
            let mode = match requested {
                "steering" => crate::config::InputMode::Steering,
                "next-turn" => crate::config::InputMode::NextTurn,
                _ => bail!("unsupported input mode"),
            };
            let mut current_mode = state.input_mode.write().await;
            crate::discord::save_input_mode(&state.input_mode_path, mode)?;
            *current_mode = mode;
            Ok(format!(
                "{} `{}`",
                language.text(
                    "Input delivery set to",
                    "입력 전달 방식:",
                    "输入传递方式已设为",
                    "入力の送信方法:"
                ),
                input_mode(mode, language)
            ))
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
                bail!("voice is no longer available");
            }
            let selected = if requested.is_empty() {
                None
            } else {
                Some(requested)
            };
            state.voice.set_voice(selected)?;
            Ok(language.text(
                "Voice profile saved for the next call; the active call is unchanged.",
                "음성 프로필을 저장했습니다. 현재 통화에는 적용되지 않고 다음 통화부터 적용됩니다.",
                "语音配置已保存，将用于下次通话；当前通话不变。",
                "音声プロファイルを保存しました。現在の通話は変わらず、次回の通話から適用されます。",
            ).to_owned())
        }
        SettingsKind::Model | SettingsKind::Effort | SettingsKind::Fast => {
            let sub = match kind {
                SettingsKind::Model => "model",
                SettingsKind::Effort => "effort",
                SettingsKind::Fast => "fast",
                _ => unreachable!(),
            };
            let thread = state.native.refresh_thread_state().await?;
            let current_model = thread
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let catalog = full_catalog(state, "model/list", json!({"includeHidden":false})).await?;
            validate_setting_choice(sub, requested, &catalog, &current_model).await?;
            let (field, value) = match kind {
                SettingsKind::Model => ("model", Value::String(requested.to_owned())),
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
            state
                .native
                .request("thread/settings/update", params)
                .await?;
            let live_result = if let Some(turn_id) = state.current_turn.lock().await.clone() {
                let mut live = json!({"threadId":thread_id,"turnId":turn_id});
                live[field] = if kind == SettingsKind::Fast && requested.eq_ignore_ascii_case("off")
                {
                    Value::Null
                } else {
                    Value::String(requested.to_owned())
                };
                Some(state.native.request("turn/settings/update", live).await)
            } else {
                None
            };
            let snapshot = state.native.refresh_thread_state().await;
            if let Some(Err(error)) = live_result.as_ref() {
                tracing::warn!(error = ?error, "failed to update settings for active native turn");
            }
            if let Err(error) = snapshot.as_ref() {
                tracing::warn!(error = ?error, "failed to refresh settings for Discord response");
            }
            if let Some(checks) = &state.installation_checks {
                let defaults_match =
                    snapshot.as_ref().ok().and_then(|value| value.get(field)) == Some(&value);
                let current_applied = live_result.as_ref().is_none_or(|result| {
                    result
                        .as_ref()
                        .ok()
                        .and_then(|result| result.get("status"))
                        .and_then(Value::as_str)
                        == Some("applied")
                });
                let error = snapshot
                    .as_ref()
                    .err()
                    .map(|error| format!("{error:#}"))
                    .or_else(|| {
                        live_result
                            .as_ref()
                            .and_then(|result| result.as_ref().err())
                            .map(|error| format!("{error:#}"))
                    });
                checks.settings_confirmed(
                    interaction_id,
                    &state.native.thread_id(),
                    defaults_match,
                    current_applied,
                    error.as_deref(),
                );
            }
            Ok(format_settings_update_result_with_language(
                requested,
                snapshot.as_ref().ok(),
                live_result.as_ref(),
                language,
            ))
        }
        SettingsKind::Hub => bail!("Choose a setting first."),
    }
}

fn option_string(option: &CommandDataOption) -> Option<&str> {
    match &option.value {
        CommandDataOptionValue::String(value) => Some(value),
        _ => None,
    }
}

fn option_channel(option: &CommandDataOption) -> Option<ChannelId> {
    match option.value {
        CommandDataOptionValue::Channel(id) => Some(id),
        _ => None,
    }
}

pub(crate) async fn handle_component(
    ctx: &Context,
    interaction: &ComponentInteraction,
) -> anyhow::Result<()> {
    let state = state_async(ctx).await?;
    authorize(
        &state,
        interaction.guild_id,
        interaction.user.id,
        state.config.language,
    )?;
    if interaction.data.custom_id.starts_with("codex:settings:") {
        return handle_settings_component(ctx, interaction, &state).await;
    }
    let parts: Vec<_> = interaction.data.custom_id.split(':').collect();
    if parts.len() != 4 || parts[0] != "codex" || parts[1] != "approval" {
        bail!("알 수 없는 component id");
    }
    let key = parts[2];
    let decision = parts[3];
    let current_generation = state.native.current_generation();
    let pending = {
        let pending_map = state.pending_approvals.lock().await;
        pending_map
            .get(key)
            .cloned()
            .ok_or_else(|| anyhow!("native 요청이 이미 처리되었거나 만료되었습니다"))?
    };
    if decision == "answer" {
        if pending.method == "mcpServer/elicitation/request"
            && mcp_tool_approval_persist_options(&pending.params).is_some()
        {
            bail!("MCP 도구 실행 승인은 버튼으로 응답해야 합니다");
        }
        if pending.claimed {
            bail!("native 요청에 대한 다른 Discord 응답이 이미 처리 중입니다");
        }
        if pending.connection_generation != current_generation {
            let stale_pending = {
                let mut pending_map = state.pending_approvals.lock().await;
                let current = pending_map
                    .get_mut(key)
                    .ok_or_else(|| anyhow!("native 요청이 이미 처리되었거나 만료되었습니다"))?;
                if current.claimed {
                    bail!("native 요청에 대한 다른 Discord 응답이 이미 처리 중입니다");
                }
                current.claimed = true;
                current.clone()
            };
            if close_pending_message(ctx, &stale_pending).await {
                remove_pending_if_same(&state, key, &stale_pending).await;
            }
            bail!("native 요청이 이전 연결 세대에 속해 있어 응답하지 않았습니다");
        }
        let modal = input_modal_with_language(
            &pending.method,
            key,
            &pending.params,
            state.config.language,
        )?;
        interaction
            .create_response(ctx, CreateInteractionResponse::Modal(modal))
            .await?;
        return Ok(());
    }
    let (pending, result) = {
        let mut pending_map = state.pending_approvals.lock().await;
        claim_approval(&mut pending_map, key, decision, current_generation)?
    };
    if let Err(error) = interaction
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
    {
        release_pending(&state, key).await;
        return Err(error.into());
    }
    let response = {
        let mut pending_map = state.pending_approvals.lock().await;
        let current = pending_map
            .get(key)
            .ok_or_else(|| anyhow!("native 요청이 이미 처리되었거나 만료되었습니다"))?;
        let response = state
            .native
            .respond_for_generation(
                current.connection_generation,
                current.native_id.clone(),
                result,
            )
            .await;
        response
    };
    if let Err(error) = response {
        tracing::warn!(error = ?error, "failed to send Discord approval decision");
        if let Some(checks) = &state.installation_checks {
            checks.request_error(&pending.method, &pending.params, &format!("{error:#}"));
        }
        interaction
            .edit_response(
                ctx,
                serenity::builder::EditInteractionResponse::new()
                    .content(state.config.language.text("Could not send the choice. Check whether the request is still active, then try again.", "선택을 전달하지 못했습니다. 요청이 아직 처리 중인지 확인한 뒤 다시 시도해 주세요.", "无法发送此选择。请确认请求仍在处理中，然后重试。", "選択を送信できませんでした。リクエストがまだ有効か確認してから再試行してください。")),
            )
            .await?;
        return Ok(());
    }
    let response_edit = interaction
        .edit_response(
            ctx,
            serenity::builder::EditInteractionResponse::new().content(state.config.language.text(
                "Choice sent to the request.",
                "요청에 선택을 전달했습니다.",
                "已向请求发送选择。",
                "リクエストに選択を送信しました。",
            )),
        )
        .await;
    let mut message = interaction.message.clone();
    let card_edit = message
        .edit(
            ctx,
            serenity::builder::EditMessage::new().components(Vec::new()),
        )
        .await;
    if let Err(error) = &card_edit {
        tracing::warn!(
            ?error,
            "failed to close Discord approval card after response"
        );
    } else {
        remove_pending_if_same(&state, key, &pending).await;
    }
    if let Err(error) = response_edit {
        if let Some(checks) = &state.installation_checks {
            checks.request_error(&pending.method, &pending.params, &format!("{error:#}"));
        }
        return Err(error.into());
    }
    if let Some(checks) = &state.installation_checks {
        checks.request_answered(pending.connection_generation, &pending.native_id);
    }
    Ok(())
}

pub(crate) async fn handle_modal(
    ctx: &Context,
    interaction: &ModalInteraction,
) -> anyhow::Result<()> {
    let state = state_async(ctx).await?;
    authorize(
        &state,
        interaction.guild_id,
        interaction.user.id,
        state.config.language,
    )?;
    let Some(key) = interaction.data.custom_id.strip_prefix("codex:input:") else {
        bail!("알 수 없는 modal id")
    };
    let pending = state
        .pending_approvals
        .lock()
        .await
        .get(key)
        .cloned()
        .ok_or_else(|| anyhow!("native 요청이 이미 처리되었거나 만료되었습니다"))?;
    interaction
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    let raw_components = serde_json::to_value(&interaction.data.components)?;
    let mut submitted = serde_json::Map::new();
    collect_modal_values(&raw_components, &mut submitted);
    let result = (|| -> anyhow::Result<Value> {
        match pending.method.as_str() {
            "item/tool/requestUserInput" => user_input_result(&pending.params, submitted),
            "mcpServer/elicitation/request" => elicitation_result(&pending.params, submitted),
            _ => bail!("이 요청은 답변 입력을 받지 않습니다"),
        }
    })();
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(error = ?error, "failed to read Discord modal answer");
            interaction
                .edit_response(
                    ctx,
                    serenity::builder::EditInteractionResponse::new()
                        .content(state.config.language.text(
                        "Could not read the answer. Use the displayed format and try again.",
                        "답변을 읽지 못했습니다. 표시된 형식에 맞게 입력한 뒤 다시 시도해 주세요.",
                        "无法读取答案。请按显示的格式输入后重试。",
                        "回答を読み取れませんでした。表示された形式で入力して再試行してください。",
                    )),
                )
                .await?;
            return Ok(());
        }
    };
    {
        let mut pending_map = state.pending_approvals.lock().await;
        claim_pending(&mut pending_map, key, state.native.current_generation())?;
    }
    let response = {
        let mut pending_map = state.pending_approvals.lock().await;
        let current = pending_map
            .get(key)
            .ok_or_else(|| anyhow!("native 요청이 이미 처리되었거나 만료되었습니다"))?;
        let response = state
            .native
            .respond_for_generation(
                current.connection_generation,
                current.native_id.clone(),
                result,
            )
            .await;
        response
    };
    if let Err(error) = response {
        tracing::warn!(error = ?error, "failed to send Discord modal answer");
        if let Some(checks) = &state.installation_checks {
            checks.request_error(&pending.method, &pending.params, &format!("{error:#}"));
        }
        interaction
            .edit_response(
                ctx,
                serenity::builder::EditInteractionResponse::new()
                    .content(state.config.language.text("Could not send the answer. Check whether the request is still active, then try again.", "답변을 전달하지 못했습니다. 요청이 아직 처리 중인지 확인한 뒤 다시 시도해 주세요.", "无法发送答案。请确认请求仍在处理中，然后重试。", "回答を送信できませんでした。リクエストがまだ有効か確認してから再試行してください。")),
            )
            .await?;
        return Ok(());
    }
    let response_edit = interaction
        .edit_response(
            ctx,
            serenity::builder::EditInteractionResponse::new().content(state.config.language.text(
                "Answer sent to the request.",
                "요청에 답변을 전달했습니다.",
                "已向请求发送答案。",
                "リクエストに回答を送信しました。",
            )),
        )
        .await;
    let card_edit = if let Some(mut message) = pending_message(ctx, &pending).await {
        Some(
            message
                .edit(
                    ctx,
                    serenity::builder::EditMessage::new().components(Vec::new()),
                )
                .await,
        )
    } else {
        None
    };
    if let Some(Err(error)) = &card_edit {
        tracing::warn!(?error, "failed to close Discord input card after response");
    } else if card_edit.is_some() {
        remove_pending_if_same(&state, key, &pending).await;
    }
    if let Err(error) = response_edit {
        if let Some(checks) = &state.installation_checks {
            checks.request_error(&pending.method, &pending.params, &format!("{error:#}"));
        }
        return Err(error.into());
    }
    if let Some(checks) = &state.installation_checks {
        checks.request_answered(pending.connection_generation, &pending.native_id);
    }
    Ok(())
}

pub(crate) fn server_request_context(
    active_voice_channel: Option<ChannelId>,
    source_target: Option<ReplyTarget>,
    fallback_channel: ChannelId,
) -> (ChannelId, Option<ReplyTarget>) {
    let channel_id = if source_target.is_some() {
        fallback_channel
    } else {
        active_voice_channel.unwrap_or(fallback_channel)
    };
    (channel_id, source_target)
}

pub(crate) async fn publish_server_request(
    http: &serenity::http::Http,
    state: &BridgeState,
    envelope: Value,
) -> anyhow::Result<()> {
    let method = envelope
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let Some(id) = envelope.get("id").cloned() else {
        bail!("native request has no id")
    };
    let mut params = envelope.get("params").cloned().unwrap_or(Value::Null);
    let correlation_params = params.clone();
    let connection_generation = params
        .as_object_mut()
        .and_then(|params| params.remove("_discodex_connection_generation"))
        .and_then(|value| value.as_u64())
        .context("native request has no connection generation metadata")?;
    let key = Uuid::new_v4().simple().to_string();
    let thread_id = params.get("threadId").and_then(Value::as_str).unwrap_or("");
    let active_voice = state
        .voice
        .session_info(GuildId::new(state.config.guild_id))
        .await
        .filter(|session| session.native_thread_id == thread_id);
    let source_target = if matches!(
        method.as_str(),
        "item/fileChange/requestApproval" | "item/commandExecution/requestApproval"
    ) {
        state
            .approval_results
            .lock()
            .await
            .request_target(connection_generation, &params)
    } else {
        state
            .approval_results
            .lock()
            .await
            .consumed_target(&correlation_params)
    };
    let (channel_id, result_target) = server_request_context(
        active_voice
            .as_ref()
            .map(|session| ChannelId::new(session.transcript_thread_id)),
        source_target,
        ChannelId::new(state.config.home_channel_id),
    );
    let source_link = result_target
        .as_ref()
        .filter(|target| target.channel_id.get() != state.config.home_channel_id)
        .map(|target| crate::discord::source_message_url(state.config.guild_id, target));
    let builders = server_request_messages_with_source_link_language(
        &method,
        &key,
        &params,
        source_link.as_deref(),
        state.config.language,
    );
    state.approval_results.lock().await.track_request(
        connection_generation,
        &id,
        &method,
        &params,
        None,
    );
    let message = match send_server_request_messages(http, channel_id, &builders).await {
        Ok(message) => message,
        Err(error) if channel_id.get() != state.config.home_channel_id => {
            tracing::warn!(%error, channel_id = channel_id.get(), "native request publication failed at the originating channel; retrying home channel");
            send_server_request_messages(
                http,
                ChannelId::new(state.config.home_channel_id),
                &builders,
            )
            .await
            .context(
                "native request publication failed in both the originating and home channels",
            )?
        }
        Err(error) => return Err(error.into()),
    };
    state.approval_results.lock().await.bind_request_card(
        connection_generation,
        &id,
        crate::discord::ReplyTarget {
            channel_id: message.channel_id,
            message_id: message.id,
        },
    );
    if let Some(checks) = &state.installation_checks {
        checks.request_published(connection_generation, &id, &method, &params);
    }
    state.pending_approvals.lock().await.insert(
        key,
        PendingApproval {
            native_id: id,
            method,
            params,
            channel_id: message.channel_id,
            message_id: message.id,
            claimed: false,
            connection_generation,
        },
    );
    Ok(())
}

async fn send_server_request_messages(
    http: &serenity::http::Http,
    channel_id: ChannelId,
    builders: &[CreateMessage],
) -> anyhow::Result<serenity::all::Message> {
    let mut final_message = None;
    for builder in builders {
        final_message = Some(channel_id.send_message(http, builder.clone()).await?);
    }
    final_message.context("native request display has no message")
}

fn claim_pending(
    pending: &mut std::collections::HashMap<String, PendingApproval>,
    key: &str,
    current_generation: u64,
) -> anyhow::Result<PendingApproval> {
    let item = pending
        .get_mut(key)
        .ok_or_else(|| anyhow!("native 요청이 이미 처리되었거나 만료되었습니다"))?;
    if item.claimed {
        bail!("native 요청에 대한 다른 Discord 응답이 이미 처리 중입니다");
    }
    if item.connection_generation != current_generation {
        item.claimed = true;
        bail!("native 요청이 이전 연결 세대에 속해 있어 응답하지 않았습니다");
    }
    item.claimed = true;
    Ok(item.clone())
}

fn claim_approval(
    pending: &mut std::collections::HashMap<String, PendingApproval>,
    key: &str,
    decision: &str,
    current_generation: u64,
) -> anyhow::Result<(PendingApproval, Value)> {
    let current = pending
        .get(key)
        .ok_or_else(|| anyhow!("native 요청이 이미 처리되었거나 만료되었습니다"))?;
    let result = approval_result(&current.method, decision, current)?;
    let claimed = claim_pending(pending, key, current_generation)?;
    Ok((claimed, result))
}

async fn release_pending(state: &BridgeState, key: &str) {
    if let Some(item) = state.pending_approvals.lock().await.get_mut(key) {
        item.claimed = false;
    }
}

fn command_approval_decisions(params: &Value) -> Vec<(String, Value)> {
    if let Some(available) = params.get("availableDecisions").and_then(Value::as_array) {
        return available
            .iter()
            .enumerate()
            .map(|(index, decision)| (format!("offered-{index}"), decision.clone()))
            .collect();
    }
    let mut decisions = ["accept", "acceptForSession", "decline", "cancel"]
        .into_iter()
        .map(|decision| (decision.to_owned(), json!(decision)))
        .collect::<Vec<_>>();
    if let Some(amendment) = params
        .get("proposedExecpolicyAmendment")
        .and_then(Value::as_array)
    {
        decisions.push((
            "execpolicy".to_owned(),
            json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": amendment}}),
        ));
    }
    if let Some(amendments) = params
        .get("proposedNetworkPolicyAmendments")
        .and_then(Value::as_array)
    {
        for (index, amendment) in amendments.iter().enumerate() {
            decisions.push((
                format!("network-{index}"),
                json!({"applyNetworkPolicyAmendment": {"network_policy_amendment": amendment}}),
            ));
        }
    }
    decisions
}

fn command_approval_label(decision: &Value) -> (String, ButtonStyle) {
    command_approval_label_with_language(decision, crate::i18n::Language::English)
}

fn command_approval_label_with_language(
    decision: &Value,
    language: crate::i18n::Language,
) -> (String, ButtonStyle) {
    if let Some(amendment) =
        decision.pointer("/applyNetworkPolicyAmendment/network_policy_amendment")
    {
        let (label, style) = match amendment.get("action").and_then(Value::as_str) {
            Some("allow") => (
                language.text("Allow host", "호스트 허용 규칙", "允许主机", "ホストを許可"),
                ButtonStyle::Primary,
            ),
            Some("deny") => (
                language.text("Deny host", "호스트 거부 규칙", "拒绝主机", "ホストを拒否"),
                ButtonStyle::Secondary,
            ),
            _ => (
                language.text("Host rule", "호스트 규칙", "主机规则", "ホストルール"),
                ButtonStyle::Secondary,
            ),
        };
        let host = amendment.get("host").and_then(Value::as_str).unwrap_or("");
        let label = if host.is_empty() {
            label.to_owned()
        } else {
            format!("{label}: {host}")
        };
        return (label.chars().take(80).collect(), style);
    }
    let (label, style) = match decision.as_str() {
        Some("accept") => (
            language.text("Allow", "허용", "允许", "許可"),
            ButtonStyle::Success,
        ),
        Some("acceptForSession") => (
            language.text(
                "Allow for session",
                "세션 허용",
                "本次会话允许",
                "セッション中は許可",
            ),
            ButtonStyle::Primary,
        ),
        Some("decline") => (
            language.text(
                "Decline and continue",
                "거절 후 계속",
                "拒绝并继续",
                "拒否して続行",
            ),
            ButtonStyle::Danger,
        ),
        Some("cancel") => (
            language.text(
                "Decline and stop",
                "거절·중단",
                "拒绝并停止",
                "拒否して中止",
            ),
            ButtonStyle::Secondary,
        ),
        _ if decision.get("acceptWithExecpolicyAmendment").is_some() => (
            language.text(
                "Allow with policy change",
                "정책 수정 후 허용",
                "修改策略后允许",
                "ポリシーを変更して許可",
            ),
            ButtonStyle::Primary,
        ),
        _ => (
            language.text("Choose", "선택", "选择", "選択"),
            ButtonStyle::Secondary,
        ),
    };
    (label.to_owned(), style)
}

fn mcp_tool_approval_persist_options(params: &Value) -> Option<Vec<&'static str>> {
    if !matches!(
        params.get("mode").and_then(Value::as_str),
        Some("form" | "openai/form" | "openaiForm")
    ) || params
        .pointer("/_meta/codex_approval_kind")
        .and_then(Value::as_str)
        != Some("mcp_tool_call")
    {
        return None;
    }
    let schema = params.get("requestedSchema")?;
    if schema != &json!({"type":"object", "properties":{}}) {
        return None;
    }

    let mut options = Vec::new();
    let values = match params.pointer("/_meta/persist") {
        Some(Value::String(value)) => vec![value.as_str()],
        Some(Value::Array(values)) => values.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    for value in values {
        match value {
            "session" if !options.contains(&"session") => options.push("session"),
            "always" if !options.contains(&"always") => options.push("always"),
            _ => {}
        }
    }
    Some(options)
}

fn mcp_tool_approval_result(params: &Value, decision: &str) -> anyhow::Result<Value> {
    let Some(persist_options) = mcp_tool_approval_persist_options(params) else {
        bail!("이 elicitation은 MCP 도구 실행 승인이 아닙니다");
    };
    match decision {
        "mcp-allow" => Ok(json!({"action":"accept"})),
        "mcp-session" if persist_options.contains(&"session") => {
            Ok(json!({"action":"accept","_meta":{"persist":"session"}}))
        }
        "mcp-always" if persist_options.contains(&"always") => {
            Ok(json!({"action":"accept","_meta":{"persist":"always"}}))
        }
        "decline" | "cancel" => Ok(json!({"action":decision})),
        _ => bail!("지원되지 않는 MCP 도구 승인 선택값: {decision}"),
    }
}

fn approval_actions(method: &str, key: &str, params: &Value) -> Vec<CreateActionRow> {
    approval_actions_with_language(method, key, params, crate::i18n::Language::English)
}

fn approval_actions_with_language(
    method: &str,
    key: &str,
    params: &Value,
    language: crate::i18n::Language,
) -> Vec<CreateActionRow> {
    if method == "mcpServer/elicitation/request"
        && let Some(persist_options) = mcp_tool_approval_persist_options(params)
    {
        let mut buttons = vec![
            CreateButton::new(format!("codex:approval:{key}:mcp-allow"))
                .label(language.text("Allow once", "이번만 허용", "仅允许一次", "今回のみ許可"))
                .style(ButtonStyle::Success),
        ];
        if persist_options.contains(&"session") {
            buttons.push(
                CreateButton::new(format!("codex:approval:{key}:mcp-session"))
                    .label(language.text(
                        "Allow for session",
                        "세션 허용",
                        "本次会话允许",
                        "セッション中は許可",
                    ))
                    .style(ButtonStyle::Primary),
            );
        }
        if persist_options.contains(&"always") {
            buttons.push(
                CreateButton::new(format!("codex:approval:{key}:mcp-always"))
                    .label(language.text("Always allow", "항상 허용", "始终允许", "常に許可"))
                    .style(ButtonStyle::Primary),
            );
        }
        buttons.extend([
            CreateButton::new(format!("codex:approval:{key}:decline"))
                .label(language.text("Decline", "거절", "拒绝", "拒否"))
                .style(ButtonStyle::Danger),
            CreateButton::new(format!("codex:approval:{key}:cancel"))
                .label(language.text("Cancel", "취소", "取消", "キャンセル"))
                .style(ButtonStyle::Secondary),
        ]);
        return vec![CreateActionRow::Buttons(buttons)];
    }
    if method == "mcpServer/elicitation/request"
        && !matches!(
            params.get("mode").and_then(Value::as_str),
            None | Some("form" | "openai/form" | "openaiForm")
        )
    {
        return vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("codex:approval:{key}:decline"))
                .label(language.text("Decline", "거절", "拒绝", "拒否"))
                .style(ButtonStyle::Danger),
            CreateButton::new(format!("codex:approval:{key}:cancel"))
                .label(language.text("Cancel", "취소", "取消", "キャンセル"))
                .style(ButtonStyle::Secondary),
        ])];
    }
    if method == "item/commandExecution/requestApproval" {
        let buttons = command_approval_decisions(params)
            .into_iter()
            .map(|(alias, decision)| {
                let (label, style) = command_approval_label_with_language(&decision, language);
                CreateButton::new(format!("codex:approval:{key}:{alias}"))
                    .label(label)
                    .style(style)
            })
            .collect::<Vec<_>>();
        return buttons
            .chunks(5)
            .map(|buttons| CreateActionRow::Buttons(buttons.to_vec()))
            .collect();
    }
    let decisions: &[(&str, &str, ButtonStyle)] = match method {
        "item/fileChange/requestApproval" => &[
            (
                "accept",
                language.text("Allow", "허용", "允许", "許可"),
                ButtonStyle::Success,
            ),
            (
                "acceptForSession",
                language.text(
                    "Allow for session",
                    "세션 허용",
                    "本次会话允许",
                    "セッション中は許可",
                ),
                ButtonStyle::Primary,
            ),
            (
                "decline",
                language.text("Decline", "거절", "拒绝", "拒否"),
                ButtonStyle::Danger,
            ),
            (
                "cancel",
                language.text(
                    "Decline and stop",
                    "거절·중단",
                    "拒绝并停止",
                    "拒否して中止",
                ),
                ButtonStyle::Secondary,
            ),
        ],
        "item/permissions/requestApproval" => &[
            (
                "turn",
                language.text(
                    "Allow this response",
                    "이번 응답 허용",
                    "允许本次回复",
                    "今回の応答で許可",
                ),
                ButtonStyle::Success,
            ),
            (
                "session",
                language.text(
                    "Allow for session",
                    "세션 허용",
                    "本次会话允许",
                    "セッション中は許可",
                ),
                ButtonStyle::Primary,
            ),
            (
                "none",
                language.text(
                    "Do not grant more permissions",
                    "추가 권한 미부여",
                    "不授予额外权限",
                    "追加権限を付与しない",
                ),
                ButtonStyle::Danger,
            ),
        ],
        "item/tool/requestUserInput" => &[(
            "answer",
            language.text("Write a response", "응답 작성", "填写回复", "回答を入力"),
            ButtonStyle::Primary,
        )],
        "mcpServer/elicitation/request" => &[
            (
                "answer",
                language.text("Write a response", "응답 작성", "填写回复", "回答を入力"),
                ButtonStyle::Primary,
            ),
            (
                "decline",
                language.text("Decline", "거절", "拒绝", "拒否"),
                ButtonStyle::Danger,
            ),
            (
                "cancel",
                language.text("Cancel", "취소", "取消", "キャンセル"),
                ButtonStyle::Secondary,
            ),
        ],
        "execCommandApproval" => &[
            (
                "approved",
                language.text("Allow", "허용", "允许", "許可"),
                ButtonStyle::Success,
            ),
            (
                "approved_for_session",
                language.text(
                    "Allow for session",
                    "세션 허용",
                    "本次会话允许",
                    "セッション中は許可",
                ),
                ButtonStyle::Primary,
            ),
            (
                "denied",
                language.text(
                    "Decline and continue",
                    "거절 후 계속",
                    "拒绝并继续",
                    "拒否して続行",
                ),
                ButtonStyle::Danger,
            ),
            (
                "abort",
                language.text(
                    "Cancel response",
                    "응답 취소",
                    "取消回复",
                    "応答をキャンセル",
                ),
                ButtonStyle::Secondary,
            ),
        ],
        "applyPatchApproval" => &[
            (
                "approved",
                language.text("Allow", "허용", "允许", "許可"),
                ButtonStyle::Success,
            ),
            (
                "approved_for_session",
                language.text(
                    "Allow for session",
                    "세션 허용",
                    "本次会话允许",
                    "セッション中は許可",
                ),
                ButtonStyle::Primary,
            ),
            (
                "denied",
                language.text(
                    "Decline and continue",
                    "거절 후 계속",
                    "拒绝并继续",
                    "拒否して続行",
                ),
                ButtonStyle::Danger,
            ),
            (
                "abort",
                language.text(
                    "Cancel response",
                    "응답 취소",
                    "取消回复",
                    "応答をキャンセル",
                ),
                ButtonStyle::Secondary,
            ),
        ],
        _ => &[],
    };
    let mut rows = Vec::new();
    if !decisions.is_empty() {
        rows.push(CreateActionRow::Buttons(
            decisions
                .iter()
                .map(|(decision, label, style)| {
                    CreateButton::new(format!("codex:approval:{key}:{decision}"))
                        .label(*label)
                        .style(*style)
                })
                .collect(),
        ));
    }
    rows
}

fn approval_result(
    method: &str,
    decision: &str,
    pending: &crate::discord::PendingApproval,
) -> anyhow::Result<Value> {
    match method {
        "item/commandExecution/requestApproval" => {
            let value = command_approval_decisions(&pending.params)
                .into_iter()
                .find(|(alias, _)| alias == decision)
                .map(|(_, value)| value)
                .ok_or_else(|| anyhow!("지원되지 않는 선택값: {decision}"))?;
            Ok(json!({"decision": value}))
        }
        "item/fileChange/requestApproval" => {
            if !["accept", "acceptForSession", "decline", "cancel"].contains(&decision) {
                bail!("지원되지 않는 선택값: {decision}");
            }
            Ok(json!({"decision":decision}))
        }
        "execCommandApproval" | "applyPatchApproval" => match decision {
            "approved" => Ok(json!({"reviewDecision":"approved"})),
            "approved_for_session" => Ok(json!({"reviewDecision":"approved_for_session"})),
            "denied" => Ok(
                json!({"reviewDecision":{"denied":{"rejection":"Discord operator declined the request"}}}),
            ),
            "abort" => Ok(json!({"reviewDecision":"abort"})),
            "execpolicy" => {
                let amendments = pending
                    .params
                    .get("proposedExecpolicyAmendment")
                    .cloned()
                    .or_else(|| pending.params.get("execpolicyAmendment").cloned())
                    .ok_or_else(|| {
                        anyhow!("native request has no proposed execpolicy amendment")
                    })?;
                Ok(
                    json!({"reviewDecision":{"approved_execpolicy_amendment":{"proposed_execpolicy_amendment":amendments}}}),
                )
            }
            "network-allow" | "network-deny" => {
                let mut amendment = pending
                    .params
                    .get("networkPolicyAmendment")
                    .cloned()
                    .ok_or_else(|| anyhow!("native request has no network policy amendment"))?;
                amendment["action"] = Value::String(
                    if decision == "network-allow" {
                        "allow"
                    } else {
                        "deny"
                    }
                    .to_owned(),
                );
                Ok(
                    json!({"reviewDecision":{"network_policy_amendment":{"network_policy_amendment":amendment}}}),
                )
            }
            _ => bail!("지원되지 않는 선택값: {decision}"),
        },
        "item/permissions/requestApproval" => match decision {
            "turn" | "session" => Ok(
                json!({"permissions":pending.params.get("permissions").cloned().unwrap_or_else(|| json!({})),"scope":if decision == "turn" { "turn" } else { "session" }}),
            ),
            "none" => Ok(json!({"permissions":{},"scope":"turn"})),
            _ => bail!("지원되지 않는 권한 선택값: {decision}"),
        },
        "mcpServer/elicitation/request" => {
            if mcp_tool_approval_persist_options(&pending.params).is_some() {
                mcp_tool_approval_result(&pending.params, decision)
            } else {
                match decision {
                    "decline" | "cancel" => Ok(json!({"action":decision})),
                    _ => bail!("지원되지 않는 elicitation 선택값: {decision}"),
                }
            }
        }
        _ => bail!("이 native 요청 종류는 Discord 승인 버튼에서 선택할 수 없습니다"),
    }
}

pub async fn register_commands(config: &crate::config::Config) -> anyhow::Result<()> {
    let token = config.token()?;
    let http = discord_command_http(&token).await?;
    register_guild_commands(&http, config).await?;
    Ok(())
}

async fn discord_command_http(token: &str) -> anyhow::Result<serenity::http::Http> {
    let http = serenity::http::Http::new(token);
    let application = http
        .get_current_application_info()
        .await
        .context("read the authenticated Discord application's ID")?;
    http.set_application_id(application.id);
    Ok(http)
}

pub(crate) async fn register_guild_commands(
    http: &serenity::http::Http,
    config: &crate::config::Config,
) -> anyhow::Result<()> {
    let guild = GuildId::new(config.guild_id);
    let marker = command_marker(&config.state_dir);
    let owned_id = read_command_id(&marker)?;
    let definition = definitions(config.language)
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("/discodex command definition is missing"))?;
    let command = if let Some(id) = owned_id {
        let existing = guild.get_commands(http).await?;
        if existing.iter().any(|command| command.id.get() == id) {
            guild
                .edit_command(http, serenity::all::CommandId::new(id), definition)
                .await?
        } else {
            let created = create_if_available(http, guild, definition).await?;
            write_command_id(&marker, created.id.get())?;
            return Ok(());
        }
    } else {
        let created = create_if_available(http, guild, definition).await?;
        write_command_id(&marker, created.id.get())?;
        return Ok(());
    };
    write_command_id(&marker, command.id.get())?;
    Ok(())
}

async fn create_if_available(
    http: &serenity::http::Http,
    guild: GuildId,
    definition: CreateCommand,
) -> anyhow::Result<serenity::all::Command> {
    let existing = guild.get_commands(http).await?;
    if existing.iter().any(|command| command.name == "discodex") {
        bail!(
            "길드에 이미 이 설치가 소유하지 않는 `/discodex` 명령이 있습니다. 기존 명령은 그대로 두었습니다."
        );
    }
    Ok(guild.create_command(http, definition).await?)
}

pub async fn unregister_commands(config: &crate::config::Config) -> anyhow::Result<()> {
    let token = config.token()?;
    let http = discord_command_http(&token).await?;
    let guild = GuildId::new(config.guild_id);
    let marker = command_marker(&config.state_dir);
    if let Some(id) = read_command_id(&marker)? {
        let exists = guild
            .get_commands(&http)
            .await?
            .iter()
            .any(|command| command.id.get() == id);
        if exists {
            guild
                .delete_command(&http, serenity::all::CommandId::new(id))
                .await?;
        }
        fs::remove_file(&marker).with_context(|| {
            format!(
                "remove Discord command ownership marker {}",
                marker.display()
            )
        })?;
    }
    Ok(())
}

async fn full_catalog(state: &BridgeState, method: &str, base: Value) -> anyhow::Result<Value> {
    let mut page_params = base;
    let mut all = Vec::new();
    loop {
        let page = state
            .native
            .request(method, page_params.clone())
            .await
            .with_context(|| format!("load {method} catalog"))?;
        if let Some(data) = page.get("data").and_then(Value::as_array) {
            all.extend(data.iter().cloned());
        }
        match page.get("nextCursor").and_then(Value::as_str) {
            Some(cursor) => page_params["cursor"] = Value::String(cursor.to_owned()),
            None => return Ok(json!({"data":all,"nextCursor":null})),
        }
    }
}

fn command_marker(state_dir: &Path) -> PathBuf {
    state_dir.join("discord-command-id.json")
}

fn read_command_id(path: &Path) -> anyhow::Result<Option<u64>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("parse Discord command ownership marker {}", path.display())
        })?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("read Discord command ownership marker {}", path.display())),
    }
}

fn write_command_id(path: &Path, id: u64) -> anyhow::Result<()> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow!("command ownership marker has no parent"))?,
    )?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec(&id)?)?;
    fs::rename(&tmp, path)
        .with_context(|| format!("save Discord command ownership marker {}", path.display()))
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct DiscordValidation {
    pub bot_id: u64,
    pub guild_id: u64,
    pub guild_name: String,
    pub channels: Vec<DiscordChannelValidation>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct DiscordChannelValidation {
    pub id: u64,
    pub name: String,
    pub kind: String,
    pub effective_permissions: u64,
    pub required_permissions: u64,
    pub warnings: Vec<String>,
}

fn validate_discord_channel_permissions(
    guild: &PartialGuild,
    member: &Member,
    channel: &GuildChannel,
    is_home: bool,
    is_transcript: bool,
    is_voice: bool,
) -> anyhow::Result<DiscordChannelValidation> {
    if channel.guild_id != guild.id {
        bail!(
            "configured channel {} belongs to a different guild",
            channel.id
        );
    }
    let mut contexts = Vec::new();
    let mut required = Permissions::empty();
    if is_home || is_transcript {
        if channel.kind != ChannelType::Text {
            bail!(
                "configured text channel {} is not a text channel",
                channel.id
            );
        }
        required |= Permissions::VIEW_CHANNEL
            | Permissions::SEND_MESSAGES
            | Permissions::READ_MESSAGE_HISTORY;
    }
    if is_home {
        contexts.push("home text");
    }
    if is_transcript {
        contexts.push("transcript text");
        required |= Permissions::CREATE_PUBLIC_THREADS | Permissions::SEND_MESSAGES_IN_THREADS;
    }
    if is_voice {
        if !matches!(channel.kind, ChannelType::Voice | ChannelType::Stage) {
            bail!(
                "configured voice channel {} is not a voice or stage channel",
                channel.id
            );
        }
        contexts.push("configured voice");
        required |= Permissions::VIEW_CHANNEL | Permissions::CONNECT | Permissions::SPEAK;
    }
    let effective = guild.user_permissions_in(channel, member);
    let missing = required & !effective;
    if !missing.is_empty() {
        bail!(
            "configured Discord channel {} ({}) lacks required bot permissions: {}",
            channel.id,
            contexts.join(", "),
            missing.get_permission_names().join(", "),
        );
    }
    let kind = match channel.kind {
        ChannelType::Text => "text",
        ChannelType::Voice => "voice",
        ChannelType::Stage => "stage",
        _ => "other",
    }
    .to_owned();
    let warnings = if is_voice && channel.kind == ChannelType::Stage {
        vec!["Stage 채널의 역할·채널 권한을 확인했습니다. 봇의 발언 억제 해제 상태와 실제 발언은 확인하지 않았습니다.".to_owned()]
    } else {
        Vec::new()
    };
    Ok(DiscordChannelValidation {
        id: channel.id.get(),
        name: channel.name.clone(),
        kind,
        effective_permissions: effective.bits(),
        required_permissions: required.bits(),
        warnings,
    })
}

pub async fn validate_discord(config: &crate::config::Config) -> anyhow::Result<DiscordValidation> {
    let token = config.token()?;
    let http = serenity::http::Http::new(&token);
    let bot = http
        .get_current_user()
        .await
        .context("Discord bot authentication failed")?;
    let guild = GuildId::new(config.guild_id)
        .to_partial_guild(&http)
        .await
        .context("bot cannot access configured guild")?;
    let member = guild
        .member(&http, bot.id)
        .await
        .context("cannot read configured guild membership for authenticated Discord bot")?;
    let mut ids = vec![config.home_channel_id, config.transcript_channel_id];
    ids.extend(config.voice_channel_ids.iter().copied());
    ids.sort_unstable();
    ids.dedup();
    let mut channels = Vec::new();
    for id in ids {
        let channel = http
            .get_channel(ChannelId::new(id))
            .await
            .with_context(|| format!("cannot read Discord channel {id}"))?;
        let guild_channel = channel
            .guild()
            .ok_or_else(|| anyhow!("configured channel {id} is not a guild channel"))?;
        channels.push(validate_discord_channel_permissions(
            &guild,
            &member,
            &guild_channel,
            id == config.home_channel_id,
            id == config.transcript_channel_id,
            config.voice_channel_ids.contains(&id),
        )?);
    }
    Ok(DiscordValidation {
        bot_id: bot.id.get(),
        guild_id: guild.id.get(),
        guild_name: guild.name,
        channels,
    })
}

async fn state_async(ctx: &Context) -> anyhow::Result<Arc<BridgeState>> {
    ctx.data
        .read()
        .await
        .get::<StateKey>()
        .cloned()
        .ok_or_else(|| anyhow!("Discord bridge state is missing"))
}

async fn validate_setting_choice(
    sub: &str,
    requested: &str,
    catalog: &Value,
    current_model: &Option<String>,
) -> anyhow::Result<()> {
    match sub {
        "model" => {
            if !catalog_has_model(catalog, requested) {
                bail!(
                    "모델 `{requested}`을(를) 현재 사용할 수 없습니다. `/discodex models`에서 선택 가능한 모델을 확인하세요."
                );
            }
        }
        "effort" => {
            let model = selected_model(catalog, current_model)?;
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
                });
            if !valid {
                bail!(
                    "추론 강도 `{requested}`는 현재 모델에서 제공하지 않습니다. `/discodex models`에서 사용할 수 있는 강도를 확인하세요."
                );
            }
        }
        "fast" => {
            if requested.eq_ignore_ascii_case("off") {
                return Ok(());
            }
            let model = selected_model(catalog, current_model)?;
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
                    "서비스 등급 `{requested}`는 현재 모델에서 제공하지 않습니다. `/discodex models`에서 사용할 수 있는 등급을 확인하세요."
                );
            }
        }
        _ => {}
    }
    Ok(())
}

fn catalog_has_model(catalog: &Value, requested: &str) -> bool {
    catalog
        .get("data")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("id").and_then(Value::as_str) == Some(requested))
        })
}

fn selected_model<'a>(
    catalog: &'a Value,
    current_model: &Option<String>,
) -> anyhow::Result<&'a Value> {
    let data = catalog
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("모델 목록을 읽을 수 없습니다"))?;
    let current_model = current_model
        .as_deref()
        .ok_or_else(|| anyhow!("현재 모델을 확인할 수 없습니다"))?;
    data.iter()
        .find(|model| model.get("id").and_then(Value::as_str) == Some(current_model))
        .ok_or_else(|| anyhow!("현재 모델을 모델 목록에서 찾을 수 없습니다"))
}

fn current_turn_settings_result(result: Option<&anyhow::Result<Value>>) -> &'static str {
    current_turn_settings_result_with_language(result, crate::i18n::Language::English)
}

fn current_turn_settings_result_with_language(
    result: Option<&anyhow::Result<Value>>,
    language: crate::i18n::Language,
) -> &'static str {
    match result {
        Some(Ok(value)) => match value.get("status").and_then(Value::as_str) {
            Some("applied") => {
                language.text("Updated settings for later work in the active response. Already-started work is unchanged.", "진행 중인 응답에서 이후 작업이 사용할 설정을 갱신했습니다. 이미 시작된 작업은 바꾸지 않습니다.", "已更新当前回复后续任务使用的设置。已开始的任务不会更改。", "進行中の応答で後続の作業に使う設定を更新しました。すでに始まった作業は変わりません。")
            }
            Some("targetUnavailable") => language.text("Could not apply settings to the active response.", "진행 중인 응답에 설정을 적용할 수 없었습니다.", "无法将设置应用于当前回复。", "進行中の応答に設定を適用できませんでした。"),
            _ => language.text("Could not confirm whether settings were applied to the active response.", "진행 중인 응답의 설정 적용 결과를 확인하지 못했습니다.", "无法确认设置是否已应用于当前回复。", "進行中の応答に設定が適用されたか確認できませんでした。"),
        },
        Some(Err(_)) => language.text("Could not send the settings update to the active response.", "진행 중인 응답에 설정 변경 요청을 전달하지 못했습니다.", "无法向当前回复发送设置更新。", "進行中の応答に設定変更を送信できませんでした。"),
        None => language.text("No response is in progress.", "진행 중인 응답이 없습니다.", "当前没有正在进行的回复。", "進行中の応答はありません。"),
    }
}

fn format_settings_update_result(
    requested: &str,
    defaults: Option<&Value>,
    live_result: Option<&anyhow::Result<Value>>,
) -> String {
    format_settings_update_result_with_language(
        requested,
        defaults,
        live_result,
        crate::i18n::Language::English,
    )
}

fn format_settings_update_result_with_language(
    requested: &str,
    defaults: Option<&Value>,
    live_result: Option<&anyhow::Result<Value>>,
    language: crate::i18n::Language,
) -> String {
    let defaults = defaults
        .map(|value| format_settings_with_language(&settings_snapshot(value), language))
        .unwrap_or_else(|| {
            language
                .text(
                    "Could not read settings.",
                    "조회하지 못했습니다.",
                    "无法读取设置。",
                    "設定を取得できませんでした。",
                )
                .to_owned()
        });
    format!(
        "{}: `{requested}`\n{}\n{}:\n{defaults}\n{}: {}",
        language.text("Requested value", "요청한 값", "请求的值", "要求された値"),
        language.text(
            "Sent the conversation-default update request.",
            "대화 기본값 변경 요청을 전달했습니다.",
            "已发送对话默认值更新请求。",
            "会話の既定値の更新を要求しました。"
        ),
        language.text(
            "Conversation defaults after update",
            "이후 대화 기본값",
            "更新后的对话默认值",
            "更新後の会話の既定値"
        ),
        language.text("Current response", "현재 응답", "当前回复", "現在の応答"),
        current_turn_settings_result_with_language(live_result, language),
    )
}

fn format_permission_settings_result(
    display_name: &str,
    settings: &Value,
    active_turn: bool,
) -> String {
    format_permission_settings_result_with_language(
        display_name,
        settings,
        active_turn,
        crate::i18n::Language::English,
    )
}

fn format_permission_settings_result_with_language(
    display_name: &str,
    settings: &Value,
    active_turn: bool,
    language: crate::i18n::Language,
) -> String {
    let current = if active_turn {
        language.text(
            "Permission use in the active response was not confirmed.",
            "진행 중인 응답의 권한 적용은 확인하지 않았습니다.",
            "未确认当前回复是否已应用权限。",
            "進行中の応答への権限適用は確認していません。",
        )
    } else {
        language.text(
            "No response is in progress.",
            "진행 중인 응답이 없습니다.",
            "当前没有正在进行的回复。",
            "進行中の応答はありません。",
        )
    };
    let prefix = language.text(
        "Set permission profile to",
        "권한 프로필을",
        "已将权限配置设为",
        "権限プロファイルを",
    );
    let suffix = language.text(
        ".",
        "로 설정하고 이후 대화 기본값을 확인했습니다.",
        "。",
        "に設定し、会話の既定値を確認しました。",
    );
    format!(
        "{prefix} `{display_name}`{suffix}\n{}:\n{}\n{}: {current}",
        language.text(
            "Conversation defaults after update",
            "이후 대화 기본값",
            "更新后的对话默认值",
            "更新後の会話の既定値"
        ),
        format_settings_with_language(settings, language),
        language.text("Current response", "현재 응답", "当前回复", "現在の応答"),
    )
}

fn settings_snapshot(thread: &Value) -> Value {
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

fn permission_profile_display_name(profile: &Value, index: usize) -> String {
    permission_profile_display_name_with_language(profile, index, crate::i18n::Language::English)
}

fn permission_profile_display_name_with_language(
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
                    .text("Read only", "읽기 전용 (Read Only)", "只读", "読み取り専用")
                    .to_owned(),
            ),
            Some(":danger-full-access") => Some(
                language
                    .text(
                        "Full access",
                        "전체 접근 (Full Access)",
                        "完全访问",
                        "フルアクセス",
                    )
                    .to_owned(),
            ),
            Some(":workspace") => Some(
                language
                    .text(
                        "Workspace",
                        "작업 공간 (Workspace)",
                        "工作区",
                        "ワークスペース",
                    )
                    .to_owned(),
            ),
            _ => None,
        })
        .unwrap_or_else(|| {
            format!(
                "{} {}",
                language.text(
                    "Permission profile",
                    "권한 프로필",
                    "权限配置",
                    "権限プロファイル"
                ),
                index + 1
            )
        })
}

fn permission_profile_aliases(display_name: &str, id: Option<&str>) -> Vec<String> {
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

fn humanize_permission_profile_name(value: &str) -> String {
    humanize_permission_profile_name_with_language(value, crate::i18n::Language::English)
}

fn humanize_permission_profile_name_with_language(
    value: &str,
    language: crate::i18n::Language,
) -> String {
    match value {
        ":read-only" => language
            .text("Read only", "읽기 전용 (Read Only)", "只读", "読み取り専用")
            .to_owned(),
        ":danger-full-access" => language
            .text(
                "Full access",
                "전체 접근 (Full Access)",
                "完全访问",
                "フルアクセス",
            )
            .to_owned(),
        ":workspace" => language
            .text(
                "Workspace",
                "작업 공간 (Workspace)",
                "工作区",
                "ワークスペース",
            )
            .to_owned(),
        _ => value.to_owned(),
    }
}

fn resolve_permission_profile(
    catalog: &Value,
    requested: &str,
) -> anyhow::Result<(String, String)> {
    resolve_permission_profile_with_language(catalog, requested, crate::i18n::Language::English)
}

fn resolve_permission_profile_with_language(
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

fn format_settings(settings: &Value) -> String {
    format_settings_with_language(settings, crate::i18n::Language::English)
}

fn format_settings_with_language(settings: &Value, language: crate::i18n::Language) -> String {
    let labels = [
        ("model", language.text("Model", "모델", "模型", "モデル")),
        (
            "effort",
            language.text("Reasoning effort", "추론 강도", "推理强度", "推論の強さ"),
        ),
        (
            "serviceTier",
            language.text("Service tier", "서비스 등급", "服务等级", "サービスティア"),
        ),
        (
            "activePermissionProfile",
            language.text(
                "Permission profile",
                "권한 프로필",
                "权限配置",
                "権限プロファイル",
            ),
        ),
        (
            "approvalPolicy",
            language.text("Approval policy", "승인 방식", "审批策略", "承認ポリシー"),
        ),
        (
            "sandboxPolicy",
            language.text(
                "File access",
                "파일 접근 범위",
                "文件访问",
                "ファイルアクセス",
            ),
        ),
        (
            "approvalsReviewer",
            language.text(
                "Approval reviewer",
                "승인 검토",
                "审批审核者",
                "承認レビュー担当",
            ),
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

fn format_sandbox_policy(value: &Value) -> String {
    format_sandbox_policy_with_language(value, crate::i18n::Language::English)
}

fn format_sandbox_policy_with_language(value: &Value, language: crate::i18n::Language) -> String {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| value.as_str());
    let access = match kind {
        Some("readOnly") => language.text("Read only", "읽기 전용", "只读", "読み取り専用"),
        Some("workspaceWrite") => language.text(
            "Read and write in the workspace",
            "작업 공간 안에서 읽기·쓰기",
            "在工作区内读写",
            "ワークスペース内で読み書き",
        ),
        Some("dangerFullAccess") => language.text(
            "Unrestricted file access",
            "제한 없는 파일 접근",
            "不受限制的文件访问",
            "制限のないファイルアクセス",
        ),
        _ => language.text(
            "Current native setting",
            "현재 native 설정",
            "当前 native 设置",
            "現在の native 設定",
        ),
    };
    let mut details = vec![access.to_owned()];
    for (key, label) in [
        (
            "readableRoots",
            language.text(
                "Readable locations",
                "읽기 가능 위치",
                "可读位置",
                "読み取り可能な場所",
            ),
        ),
        (
            "writableRoots",
            language.text(
                "Writable locations",
                "쓰기 가능 위치",
                "可写位置",
                "書き込み可能な場所",
            ),
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
            language.text(
                "Network access",
                "네트워크 접근",
                "网络访问",
                "ネットワークアクセス"
            ),
            if network {
                language.text("Allowed", "허용", "允许", "許可")
            } else {
                language.text("Blocked", "차단", "阻止", "ブロック")
            }
        ));
    }
    details.join(" · ")
}

fn format_approval_policy(value: &Value) -> String {
    format_approval_policy_with_language(value, crate::i18n::Language::English)
}

fn format_approval_policy_with_language(value: &Value, language: crate::i18n::Language) -> String {
    match value.as_str() {
        Some("on-request") => language
            .text(
                "Ask when needed",
                "필요할 때 확인",
                "需要时询问",
                "必要なときに確認",
            )
            .to_owned(),
        _ => language
            .text(
                "Current native setting",
                "현재 native 설정",
                "当前 native 设置",
                "現在の native 設定",
            )
            .to_owned(),
    }
}

fn format_approvals_reviewer(value: &Value) -> String {
    format_approvals_reviewer_with_language(value, crate::i18n::Language::English)
}

fn format_approvals_reviewer_with_language(
    value: &Value,
    language: crate::i18n::Language,
) -> String {
    match value.as_str() {
        Some("user") => language
            .text("User", "사용자", "用户", "ユーザー")
            .to_owned(),
        _ => language
            .text(
                "Current native setting",
                "현재 native 설정",
                "当前 native 设置",
                "現在の native 設定",
            )
            .to_owned(),
    }
}

fn format_active_permission_profile(value: &Value) -> Option<String> {
    format_active_permission_profile_with_language(value, crate::i18n::Language::English)
}

fn format_active_permission_profile_with_language(
    value: &Value,
    language: crate::i18n::Language,
) -> Option<String> {
    if value.as_str() == Some("native snapshot did not report this field") {
        return None;
    }
    if value.is_null() {
        return Some(
            language
                .text(
                    "No profile selected",
                    "선택된 프로필 없음",
                    "未选择配置",
                    "プロファイル未選択",
                )
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
                .text("Read only", "읽기 전용 (Read Only)", "只读", "読み取り専用")
                .to_owned(),
        ),
        Some(":danger-full-access") => Some(
            language
                .text(
                    "Full access",
                    "전체 접근 (Full Access)",
                    "完全访问",
                    "フルアクセス",
                )
                .to_owned(),
        ),
        Some(":workspace") => Some(
            language
                .text(
                    "Workspace",
                    "작업 공간 (Workspace)",
                    "工作区",
                    "ワークスペース",
                )
                .to_owned(),
        ),
        Some(_) => Some(
            language
                .text(
                    "Current permission profile",
                    "현재 권한 프로필",
                    "当前权限配置",
                    "現在の権限プロファイル",
                )
                .to_owned(),
        ),
        None => Some(
            setting_value_with_language(value, language).unwrap_or_else(|| {
                language
                    .text(
                        "Current permission profile",
                        "현재 권한 프로필",
                        "当前权限配置",
                        "現在の権限プロファイル",
                    )
                    .to_owned()
            }),
        ),
    }
}

fn setting_value(value: &Value) -> Option<String> {
    setting_value_with_language(value, crate::i18n::Language::English)
}

fn setting_value_with_language(value: &Value, language: crate::i18n::Language) -> Option<String> {
    match value {
        Value::Null => Some(language.text("Off", "해제", "关闭", "オフ").to_owned()),
        Value::String(text) if text == "native snapshot did not report this field" => None,
        Value::String(text) => Some(match text.as_str() {
            ":danger-full-access" => language
                .text(
                    "Full access",
                    "전체 접근 (Full Access)",
                    "完全访问",
                    "フルアクセス",
                )
                .to_owned(),
            ":workspace" => language
                .text(
                    "Workspace",
                    "작업 공간 (Workspace)",
                    "工作区",
                    "ワークスペース",
                )
                .to_owned(),
            _ => text.clone(),
        }),
        Value::Bool(value) => Some(
            if *value {
                language.text("Enabled", "사용", "已启用", "有効")
            } else {
                language.text("Disabled", "사용 안 함", "未启用", "無効")
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

fn field_label(value: &str) -> &str {
    field_label_with_language(value, crate::i18n::Language::English)
}

fn field_label_with_language(value: &str, language: crate::i18n::Language) -> &str {
    match value {
        "read" => language.text("Read", "읽기", "读取", "読み取り"),
        "write" => language.text("Write", "쓰기", "写入", "書き込み"),
        "network" => language.text("Network", "네트워크", "网络", "ネットワーク"),
        "roots" => language.text("Allowed paths", "허용 경로", "允许的路径", "許可パス"),
        "mode" | "scope" => language.text("Scope", "범위", "范围", "範囲"),
        _ => value,
    }
}

fn format_model_catalog(catalog: &Value) -> String {
    format_model_catalog_with_language(catalog, crate::i18n::Language::English)
}

fn format_model_catalog_with_language(catalog: &Value, language: crate::i18n::Language) -> String {
    let Some(models) = catalog.get("data").and_then(Value::as_array) else {
        return language
            .text(
                "Could not read the available models.",
                "사용 가능한 모델 목록을 읽지 못했습니다.",
                "无法读取可用模型列表。",
                "利用可能なモデル一覧を取得できませんでした。",
            )
            .to_owned();
    };
    if models.is_empty() {
        return language
            .text(
                "No models are available.",
                "사용 가능한 모델이 없습니다.",
                "没有可用模型。",
                "利用可能なモデルはありません。",
            )
            .to_owned();
    }
    let entries = models
        .iter()
        .map(|model| {
            let name = model
                .get("displayName")
                .and_then(Value::as_str)
                .or_else(|| model.get("name").and_then(Value::as_str))
                .or_else(|| model.get("id").and_then(Value::as_str))
                .unwrap_or(language.text(
                    "Unnamed model",
                    "이름 없는 모델",
                    "未命名模型",
                    "名前のないモデル",
                ));
            let value = model.get("id").and_then(Value::as_str).unwrap_or(name);
            let mut details = Vec::new();
            if let Some(efforts) = model
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
            {
                let values = efforts
                    .iter()
                    .filter_map(|item| {
                        item.get("reasoningEffort")
                            .and_then(Value::as_str)
                            .or_else(|| item.as_str())
                    })
                    .collect::<Vec<_>>();
                if !values.is_empty() {
                    details.push(format!(
                        "{}: {}",
                        language.text("Reasoning effort", "추론 강도", "推理强度", "推論の強さ"),
                        values.join(", ")
                    ));
                }
            }
            if let Some(tiers) = model.get("serviceTiers").and_then(Value::as_array) {
                let values = tiers
                    .iter()
                    .filter_map(|item| {
                        item.get("name")
                            .and_then(Value::as_str)
                            .or_else(|| item.get("id").and_then(Value::as_str))
                            .or_else(|| item.as_str())
                    })
                    .collect::<Vec<_>>();
                if !values.is_empty() {
                    details.push(format!(
                        "{}: {}",
                        language.text("Service tier", "서비스 등급", "服务等级", "サービスティア"),
                        values.join(", ")
                    ));
                }
            }
            format!(
                "• {name} (`{value}`){}",
                if details.is_empty() {
                    String::new()
                } else {
                    format!("\n  {}", details.join(" · "))
                }
            )
        })
        .collect::<Vec<_>>();
    format!(
        "{}\n{}",
        language.text(
            "Available models and settings",
            "사용 가능한 모델과 설정 선택지",
            "可用模型和设置选项",
            "利用可能なモデルと設定項目"
        ),
        entries.join("\n")
    )
}

fn format_permission_catalog(catalog: &Value) -> String {
    format_permission_catalog_with_language(catalog, crate::i18n::Language::English)
}

fn format_permission_catalog_with_language(
    catalog: &Value,
    language: crate::i18n::Language,
) -> String {
    let Some(profiles) = catalog.get("data").and_then(Value::as_array) else {
        return language
            .text(
                "Could not read the available permission profiles.",
                "사용 가능한 권한 프로필 목록을 읽지 못했습니다.",
                "无法读取可用权限配置列表。",
                "利用可能な権限プロファイル一覧を取得できませんでした。",
            )
            .to_owned();
    };
    if profiles.is_empty() {
        return language
            .text(
                "No permission profiles are available.",
                "사용 가능한 권한 프로필이 없습니다.",
                "没有可用权限配置。",
                "利用可能な権限プロファイルはありません。",
            )
            .to_owned();
    }
    let entries = profiles
        .iter()
        .enumerate()
        .map(|(index, profile)| {
            let name = permission_profile_display_name_with_language(profile, index, language);
            let allowed = profile.get("allowed").and_then(Value::as_bool);
            let status = match allowed {
                Some(true) => language.text("Available", "사용 가능", "可用", "利用可能"),
                Some(false) => language.text(
                    "Currently unavailable",
                    "현재 사용할 수 없음",
                    "当前不可用",
                    "現在利用できません",
                ),
                None => language.text(
                    "Status not reported",
                    "상태 미보고",
                    "未报告状态",
                    "状態未報告",
                ),
            };
            let description = profile
                .get("description")
                .and_then(Value::as_str)
                .map(|value| format!(" — {value}"))
                .unwrap_or_default();
            format!("• {name}: {status}{description}")
        })
        .collect::<Vec<_>>();
    format!(
        "{}\n{}",
        language.text(
            "Permission profiles (enter the display name to set one)",
            "권한 프로필 (설정 시 표시 이름을 입력하세요)",
            "权限配置（设置时请输入显示名称）",
            "権限プロファイル（設定時は表示名を入力してください）"
        ),
        entries.join("\n")
    )
}

fn server_request_messages(method: &str, key: &str, params: &Value) -> Vec<CreateMessage> {
    server_request_messages_with_language(method, key, params, crate::i18n::Language::English)
}

fn server_request_messages_with_language(
    method: &str,
    key: &str,
    params: &Value,
    language: crate::i18n::Language,
) -> Vec<CreateMessage> {
    server_request_messages_with_source_link_language(method, key, params, None, language)
}

fn server_request_messages_with_source_link(
    method: &str,
    key: &str,
    params: &Value,
    source_link: Option<&str>,
) -> Vec<CreateMessage> {
    server_request_messages_with_source_link_language(
        method,
        key,
        params,
        source_link,
        crate::i18n::Language::English,
    )
}

fn server_request_messages_with_source_link_language(
    method: &str,
    key: &str,
    params: &Value,
    source_link: Option<&str>,
    language: crate::i18n::Language,
) -> Vec<CreateMessage> {
    let content = describe_server_request_with_language(method, params, language);
    let chunks = if content.len() > 1800 {
        let mut chunks = Vec::new();
        let mut remaining = content.as_str();
        while !remaining.is_empty() {
            let mut end = remaining.len().min(1800);
            while !remaining.is_char_boundary(end) {
                end -= 1;
            }
            chunks.push(remaining[..end].to_owned());
            remaining = &remaining[end..];
        }
        chunks
    } else {
        vec![content]
    };
    let last = chunks.len() - 1;
    let actions = approval_actions_with_language(method, key, params, language);
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, mut content)| {
            if index == last {
                if let Some(source_link) = source_link {
                    content.push_str(&format!(
                        "\n\n{}: ",
                        language.text("Source message", "원본 메시지", "源消息", "元のメッセージ")
                    ));
                    content.push_str(source_link);
                }
            }
            let builder = CreateMessage::new()
                .content(content)
                .allowed_mentions(no_mentions());
            if index == last {
                builder.components(actions.clone())
            } else {
                builder
            }
        })
        .collect()
}

fn describe_server_request(method: &str, params: &Value) -> String {
    describe_server_request_with_language(method, params, crate::i18n::Language::English)
}

fn describe_server_request_with_language(
    method: &str,
    params: &Value,
    language: crate::i18n::Language,
) -> String {
    match method {
        "item/commandExecution/requestApproval" | "execCommandApproval" => {
            let reason = params
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or(language.text(
                    "Codex requested permission to run a terminal command.",
                    "Codex가 터미널 명령 실행을 요청했습니다.",
                    "Codex 请求运行终端命令的权限。",
                    "Codex がターミナルコマンドの実行許可を求めています。",
                ));
            let location = params
                .get("cwd")
                .and_then(Value::as_str)
                .map(|value| {
                    format!(
                        "\n{}: {value}",
                        language.text(
                            "Working directory",
                            "작업 위치",
                            "工作目录",
                            "作業ディレクトリ"
                        )
                    )
                })
                .unwrap_or_default();
            let network_scope = params
                .get("networkPolicyAmendment")
                .and_then(|amendment| {
                    ["host", "hostname", "domain"]
                        .iter()
                        .find_map(|key| amendment.get(*key).and_then(Value::as_str))
                })
                .map(|host| {
                    format!(
                        "\n{}: {host}",
                        language.text(
                            "Network target",
                            "네트워크 대상",
                            "网络目标",
                            "ネットワーク対象"
                        )
                    )
                })
                .unwrap_or_default();
            format!(
                "{reason}{location}{network_scope}\n{}",
                language.text(
                    "Choose the permission scope below.",
                    "아래에서 허용 범위를 선택해 주세요.",
                    "请在下方选择权限范围。",
                    "以下から許可する範囲を選択してください。"
                )
            )
        }
        "item/fileChange/requestApproval" | "applyPatchApproval" => {
            let files = params
                .get("changes")
                .and_then(Value::as_array)
                .map(|changes| {
                    changes
                        .iter()
                        .filter_map(|item| item.get("path").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let details = if files.is_empty() {
                "".to_owned()
            } else {
                format!(
                    "\n{}: {}",
                    language.text("Files", "대상 파일", "文件", "対象ファイル"),
                    files.join(", ")
                )
            };
            let reason = params
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or(language.text(
                    "Codex requested file changes.",
                    "Codex가 파일 변경을 요청했습니다.",
                    "Codex 请求更改文件。",
                    "Codex がファイル変更を求めています。",
                ));
            format!(
                "{reason}{details}\n{}",
                language.text(
                    "Choose the permission scope below.",
                    "아래에서 허용 범위를 선택해 주세요.",
                    "请在下方选择权限范围。",
                    "以下から許可する範囲を選択してください。"
                )
            )
        }
        "item/permissions/requestApproval" => {
            let permission = params
                .get("permissions")
                .map(|value| setting_value_with_language(value, language).unwrap_or_default())
                .unwrap_or_default();
            format!(
                "{}\n{}: {}\n{}",
                language.text(
                    "Codex requested additional permissions.",
                    "Codex가 추가 권한을 요청했습니다.",
                    "Codex 请求额外权限。",
                    "Codex が追加権限を求めています。"
                ),
                language.text(
                    "Requested permissions",
                    "요청한 권한",
                    "请求的权限",
                    "要求された権限"
                ),
                if permission.is_empty() {
                    language.text(
                        "No details were provided.",
                        "세부 항목이 제공되지 않았습니다.",
                        "未提供详细信息。",
                        "詳細はありません。",
                    )
                } else {
                    &permission
                },
                language.text(
                    "Choose the permission scope below.",
                    "아래에서 허용 범위를 선택해 주세요.",
                    "请在下方选择权限范围。",
                    "以下から許可する範囲を選択してください。"
                )
            )
        }
        "item/tool/requestUserInput" => {
            let questions = params
                .get("questions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let details = questions
                .iter()
                .enumerate()
                .map(|(index, question)| {
                    let header = question.get("header").and_then(Value::as_str).unwrap_or("");
                    let prompt =
                        question
                            .get("question")
                            .and_then(Value::as_str)
                            .unwrap_or(language.text(
                                "Enter your answer.",
                                "답변을 입력해 주세요.",
                                "请输入答案。",
                                "回答を入力してください。",
                            ));
                    let options = question
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| {
                                    let label = item.get("label").and_then(Value::as_str)?;
                                    Some(match item.get("description").and_then(Value::as_str) {
                                        Some(desc) => format!("{label} — {desc}"),
                                        None => label.to_owned(),
                                    })
                                })
                                .collect::<Vec<_>>()
                                .join(" / ")
                        })
                        .unwrap_or_default();
                    let title = if header.is_empty() {
                        format!("{}. {prompt}", index + 1)
                    } else {
                        format!("{}. {header}: {prompt}", index + 1)
                    };
                    if options.is_empty() {
                        title
                    } else {
                        format!(
                            "{title}\n{}: {options}",
                            language.text("Options", "선택지", "选项", "選択肢")
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            format!(
                "{}\n\n{details}\n\n{}",
                language.text(
                    "Codex asked you to answer the following questions.",
                    "Codex가 다음 질문에 답변을 요청했습니다.",
                    "Codex 请您回答以下问题。",
                    "Codex が次の質問への回答を求めています。"
                ),
                language.text(
                    "Press ‘Write a response’ to answer.",
                    "‘응답 작성’을 눌러 답변해 주세요.",
                    "点击“填写回复”作答。",
                    "「回答を入力」を押して回答してください。"
                )
            )
        }
        "mcpServer/elicitation/request" => {
            let message = params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or(language.text(
                    "The MCP tool requested additional information.",
                    "MCP 도구가 추가 정보를 요청했습니다.",
                    "MCP 工具请求补充信息。",
                    "MCP ツールが追加情報を求めています。",
                ));
            if mcp_tool_approval_persist_options(params).is_some() {
                format!(
                    "{}\n{message}",
                    language.text(
                        "MCP tool execution approval is required.",
                        "MCP 도구 실행 승인이 필요합니다.",
                        "需要批准执行 MCP 工具。",
                        "MCP ツールの実行承認が必要です。"
                    )
                )
            } else {
                let schema = describe_schema_with_language(
                    params.get("requestedSchema").unwrap_or(&Value::Null),
                    language,
                );
                format!(
                    "{}\n{message}\n{}: {schema}",
                    language.text(
                        "The MCP tool requested additional information.",
                        "MCP 도구가 추가 정보를 요청했습니다.",
                        "MCP 工具请求补充信息。",
                        "MCP ツールが追加情報を求めています。"
                    ),
                    language.text(
                        "Requested fields",
                        "요청한 항목",
                        "请求的字段",
                        "要求された項目"
                    )
                )
            }
        }
        _ => language
            .text(
                "This request has no choices that can be handled in Discord.",
                "이 요청은 Discord에서 처리할 수 있는 선택 항목을 제공하지 않습니다.",
                "此请求没有可在 Discord 中处理的选项。",
                "このリクエストには Discord で処理できる選択肢がありません。",
            )
            .to_owned(),
    }
}

fn describe_schema(schema: &Value) -> String {
    describe_schema_with_language(schema, crate::i18n::Language::English)
}

fn describe_schema_with_language(schema: &Value, language: crate::i18n::Language) -> String {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return schema
            .get("type")
            .and_then(Value::as_str)
            .map(|value| schema_type(value, language))
            .unwrap_or(
                language
                    .text(
                        "Requested information",
                        "요청한 정보",
                        "所需信息",
                        "求められた情報",
                    )
                    .to_owned(),
            );
    };
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    properties
        .iter()
        .map(|(name, spec)| {
            let title = spec
                .get("title")
                .and_then(Value::as_str)
                .map(|title| format!("{title} ({name})"))
                .unwrap_or_else(|| name.to_owned());
            let description = spec
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            let kind = spec
                .get("type")
                .and_then(Value::as_str)
                .map(|value| schema_type(value, language))
                .unwrap_or_default();
            let choices = describe_schema_choices_with_language(spec, language);
            format!(
                "{title}{}{}{}{}",
                if required.contains(&name.as_str()) {
                    language.text(" (required)", " (필수)", "（必填）", "（必須）")
                } else {
                    ""
                },
                if kind.is_empty() {
                    "".to_owned()
                } else {
                    format!(" · {kind}")
                },
                if description.is_empty() {
                    "".to_owned()
                } else {
                    format!(" — {description}")
                },
                if choices.is_empty() {
                    "".to_owned()
                } else {
                    format!(
                        " · 선택지{}: {choices}",
                        if spec.get("type").and_then(Value::as_str) == Some("array") {
                            language.text(" (multiple)", " (여러 개)", "（可多选）", "（複数可）")
                        } else {
                            ""
                        }
                    )
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_schema_choices(spec: &Value) -> String {
    describe_schema_choices_with_language(spec, crate::i18n::Language::English)
}

fn describe_schema_choices_with_language(spec: &Value, language: crate::i18n::Language) -> String {
    let choices = if spec.get("type").and_then(Value::as_str) == Some("array") {
        spec.get("items").unwrap_or(spec)
    } else {
        spec
    };
    if let Some(values) = choices.get("enum").and_then(Value::as_array) {
        let labels = choices.get("enumNames").and_then(Value::as_array);
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                labels
                    .and_then(|labels| labels.get(index))
                    .and_then(Value::as_str)
                    .map(|label| format!("{label} (값: {value})"))
                    .unwrap_or_else(|| value.to_string())
            })
            .collect::<Vec<_>>()
            .join(" / ")
    } else {
        choices
            .get("oneOf")
            .or_else(|| choices.get("anyOf"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let value = item.get("const")?;
                        Some(match item.get("title").and_then(Value::as_str) {
                            Some(label) => format!(
                                "{label} ({}: {value})",
                                language.text("value", "값", "值", "値")
                            ),
                            None => value.to_string(),
                        })
                    })
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default()
    }
}

fn schema_type_korean(value: &str) -> String {
    schema_type(value, crate::i18n::Language::English)
}

fn schema_type(value: &str, language: crate::i18n::Language) -> String {
    match value {
        "string" => language.text("text", "문자", "文本", "テキスト"),
        "number" | "integer" => language.text("number", "숫자", "数字", "数値"),
        "boolean" => language.text("yes/no", "예/아니요", "是/否", "はい/いいえ"),
        "array" => language.text("list", "목록", "列表", "リスト"),
        "object" => language.text("multiple fields", "여러 항목", "多个字段", "複数項目"),
        _ => value,
    }
    .to_owned()
}

fn elicitation_result(
    params: &Value,
    submitted: serde_json::Map<String, Value>,
) -> anyhow::Result<Value> {
    if mcp_tool_approval_persist_options(params).is_some() {
        bail!("MCP 도구 실행 승인은 입력 폼으로 답할 수 없습니다");
    }
    if !matches!(
        params.get("mode").and_then(Value::as_str),
        None | Some("form" | "openai/form" | "openaiForm")
    ) {
        bail!("지원하지 않는 elicitation 입력 형식입니다");
    }
    let value = submitted
        .get("elicitation_json")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("elicitation JSON input is missing"))?;
    Ok(
        json!({"action":"accept","content":serde_json::from_str::<Value>(value).context("elicitation response must be JSON")?}),
    )
}

fn user_input_result(
    params: &Value,
    submitted: serde_json::Map<String, Value>,
) -> anyhow::Result<Value> {
    let questions = params.get("questions").and_then(Value::as_array);
    if let Some(questions) = questions.filter(|questions| questions.len() > 5) {
        let raw = submitted
            .get("answers_text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(json!({"answers":parse_numbered_answers(raw, questions)?}))
    } else {
        Ok(json!({
            "answers": submitted
                .into_iter()
                .map(|(key, value)| (key, json!({"answers": [value]})))
                .collect::<serde_json::Map<_, _>>()
        }))
    }
}

fn parse_numbered_answers(
    raw: &str,
    questions: &[Value],
) -> anyhow::Result<serde_json::Map<String, Value>> {
    let mut values = std::collections::BTreeMap::new();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let (number, answer) = line.split_once([':', '：']).ok_or_else(|| {
            anyhow!("각 답변을 ‘번호:답변’ 형식으로 입력해 주세요 (콜론 뒤 공백도 답변에 포함됨)")
        })?;
        let number = number
            .trim()
            .parse::<usize>()
            .map_err(|_| anyhow!("답변 앞에 질문 번호를 입력해 주세요"))?;
        if number == 0 || number > questions.len() {
            bail!("질문 목록에 없는 번호가 있습니다");
        }
        values.insert(number, answer.to_owned());
    }
    let mut answers = serde_json::Map::new();
    for (index, question) in questions.iter().enumerate() {
        let id = question
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("native question is missing id"))?;
        let answer = values
            .get(&(index + 1))
            .ok_or_else(|| anyhow!("모든 질문에 답변해 주세요"))?;
        answers.insert(id.to_owned(), json!({"answers":[answer]}));
    }
    Ok(answers)
}

fn input_modal(method: &str, key: &str, params: &Value) -> anyhow::Result<CreateModal> {
    input_modal_with_language(method, key, params, crate::i18n::Language::English)
}

fn input_modal_with_language(
    method: &str,
    key: &str,
    params: &Value,
    language: crate::i18n::Language,
) -> anyhow::Result<CreateModal> {
    if method == "mcpServer/elicitation/request"
        && mcp_tool_approval_persist_options(params).is_some()
    {
        bail!("MCP 도구 실행 승인은 입력 폼으로 답할 수 없습니다");
    }
    if method == "mcpServer/elicitation/request"
        && params.get("mode").is_some()
        && !matches!(
            params.get("mode").and_then(Value::as_str),
            Some("form" | "openai/form" | "openaiForm")
        )
    {
        bail!("이 elicitation은 입력 폼으로 응답할 수 없습니다");
    }
    if params.get("questions").is_some() {
        let questions = params
            .get("questions")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("native user-input questions are not an array"))?;
        if questions.is_empty() {
            bail!("native user-input request has no questions");
        }
        if questions.len() > 5 {
            let input = serenity::builder::CreateInputText::new(
                serenity::all::InputTextStyle::Paragraph,
                language.text(
                    "Question numbers and answers",
                    "질문 번호와 답변",
                    "问题编号和答案",
                    "質問番号と回答",
                ),
                "answers_text",
            )
            .placeholder(language.text(
                "One number:answer per line. Example: 1:answer",
                "각 줄에 번호:답변. 예: 1:답변 (콜론 뒤 공백도 답변에 포함됨)",
                "每行填写 编号:答案。例如：1:答案",
                "各行に番号:回答を入力。例: 1:回答",
            ))
            .required(true);
            return Ok(CreateModal::new(
                format!("codex:input:{key}"),
                language.text(
                    "Answer requested by Codex",
                    "Codex가 요청한 답변",
                    "Codex 请求的回复",
                    "Codex が求める回答",
                ),
            )
            .components(vec![CreateActionRow::InputText(input)]));
        }
        let mut components = Vec::new();
        for question in questions {
            let id = question
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("native question is missing id"))?;
            let prompt = question
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or(id);
            let header = question
                .get("header")
                .and_then(Value::as_str)
                .unwrap_or(prompt);
            let mut label = header.chars().take(45).collect::<String>();
            if label.is_empty() {
                label = language.text("Answer", "답변", "回答", "回答").to_owned();
            }
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            let label = item.get("label").and_then(Value::as_str)?;
                            let description = item.get("description").and_then(Value::as_str);
                            Some(match description {
                                Some(description) => format!("{label}: {description}"),
                                None => label.to_owned(),
                            })
                        })
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .unwrap_or_default();
            let placeholder = if options.is_empty() {
                prompt.to_owned()
            } else {
                format!("{prompt} 선택지: {options}")
            };
            let input = serenity::builder::CreateInputText::new(
                serenity::all::InputTextStyle::Paragraph,
                label,
                id,
            )
            .placeholder(placeholder.chars().take(100).collect::<String>())
            .required(true);
            components.push(CreateActionRow::InputText(input));
        }
        return Ok(CreateModal::new(
            format!("codex:input:{key}"),
            language.text(
                "Answer requested by Codex",
                "Codex가 요청한 답변",
                "Codex 请求的回复",
                "Codex が求める回答",
            ),
        )
        .components(components));
    }
    let schema = params.get("requestedSchema").unwrap_or(params);
    let schema_summary = describe_schema_with_language(schema, language);
    let input = serenity::builder::CreateInputText::new(
        serenity::all::InputTextStyle::Paragraph,
        language.text(
            "Requested information",
            "요청한 정보",
            "所需信息",
            "求められた情報",
        ),
        "elicitation_json",
    )
    .placeholder(schema_summary.chars().take(100).collect::<String>())
    .required(true);
    Ok(CreateModal::new(
        format!("codex:input:{key}"),
        language.text(
            "Enter additional information",
            "추가 정보 입력",
            "输入其他信息",
            "追加情報を入力",
        ),
    )
    .components(vec![CreateActionRow::InputText(input)]))
}

fn collect_modal_values(value: &Value, output: &mut serde_json::Map<String, Value>) {
    match value {
        Value::Object(object) => {
            if let (Some(id), Some(text)) = (
                object.get("custom_id").and_then(Value::as_str),
                object.get("value").and_then(Value::as_str),
            ) {
                output.insert(id.to_owned(), Value::String(text.to_owned()));
            }
            for nested in object.values() {
                collect_modal_values(nested, output);
            }
        }
        Value::Array(array) => {
            for nested in array {
                collect_modal_values(nested, output);
            }
        }
        _ => {}
    }
}

async fn pending_message(
    ctx: &Context,
    pending: &crate::discord::PendingApproval,
) -> Option<serenity::all::Message> {
    pending
        .channel_id
        .message(ctx, pending.message_id)
        .await
        .ok()
}

async fn close_pending_message(ctx: &Context, pending: &crate::discord::PendingApproval) -> bool {
    if let Some(mut message) = pending_message(ctx, pending).await {
        message
            .edit(
                ctx,
                serenity::builder::EditMessage::new().components(Vec::new()),
            )
            .await
            .is_ok()
    } else {
        false
    }
}

async fn remove_pending_if_same(
    state: &BridgeState,
    key: &str,
    expected: &crate::discord::PendingApproval,
) {
    let mut pending = state.pending_approvals.lock().await;
    if pending.get(key).is_some_and(|current| {
        current.connection_generation == expected.connection_generation
            && current.native_id == expected.native_id
            && current.message_id == expected.message_id
    }) {
        pending.remove(key);
    }
}

pub(crate) async fn mark_request_resolved(
    http: &serenity::http::Http,
    state: &BridgeState,
    params: &Value,
) -> anyhow::Result<()> {
    let Some(request_id) = params.get("requestId") else {
        return Ok(());
    };
    let Some(connection_generation) = params
        .get("_discodex_connection_generation")
        .and_then(Value::as_u64)
    else {
        tracing::warn!("native resolved request has no connection generation metadata");
        return Ok(());
    };
    let requests = {
        let mut pending = state.pending_approvals.lock().await;
        claim_resolved_pending(&mut pending, request_id, connection_generation)
    };
    for (key, request) in requests {
        let channel = request.channel_id;
        let mut message = match channel.message(http, request.message_id).await {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(?error, "failed to read resolved Discord request message");
                continue;
            }
        };
        if let Err(error) = message
            .edit(
                http,
                serenity::builder::EditMessage::new()
                    .content(format!("{}\n\n요청이 처리되었습니다.", message.content))
                    .components(Vec::new()),
            )
            .await
        {
            tracing::warn!(?error, "failed to close resolved Discord request message");
        } else {
            remove_pending_if_same(state, &key, &request).await;
        }
    }
    Ok(())
}

pub(crate) async fn mark_connection_closed(
    http: &serenity::http::Http,
    state: &BridgeState,
    params: &Value,
) -> anyhow::Result<()> {
    let Some(connection_generation) = params
        .get("_discodex_connection_generation")
        .and_then(Value::as_u64)
    else {
        tracing::warn!("native connection-closed event has no connection generation metadata");
        return Ok(());
    };
    let requests = {
        let mut pending = state.pending_approvals.lock().await;
        claim_generation_pending(&mut pending, connection_generation)
    };
    for (key, request) in requests {
        let mut message = match request.channel_id.message(http, request.message_id).await {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    generation = connection_generation,
                    "failed to read stale Discord request message"
                );
                continue;
            }
        };
        if let Err(error) = message
            .edit(
                http,
                serenity::builder::EditMessage::new().components(Vec::new()),
            )
            .await
        {
            tracing::warn!(
                ?error,
                generation = connection_generation,
                "failed to close stale Discord request message"
            );
        } else {
            remove_pending_if_same(state, &key, &request).await;
        }
    }
    Ok(())
}

pub(crate) async fn reconcile_pending_requests(
    http: &serenity::http::Http,
    state: &BridgeState,
) -> anyhow::Result<()> {
    let candidates = state
        .pending_approvals
        .lock()
        .await
        .iter()
        .map(|(key, request)| (key.clone(), request.clone()))
        .collect::<Vec<_>>();
    for (key, request) in candidates {
        if state
            .native
            .is_server_request_active(request.connection_generation, &request.native_id)
            .await
        {
            continue;
        }
        let request = {
            let mut pending = state.pending_approvals.lock().await;
            let Some(current) = pending.get_mut(&key) else {
                continue;
            };
            if current.connection_generation != request.connection_generation
                || current.native_id != request.native_id
                || current.message_id != request.message_id
            {
                continue;
            }
            current.claimed = true;
            current.clone()
        };
        let mut message = match request.channel_id.message(http, request.message_id).await {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(?error, "failed to read lagged Discord request message");
                continue;
            }
        };
        if let Err(error) = message
            .edit(
                http,
                serenity::builder::EditMessage::new().components(Vec::new()),
            )
            .await
        {
            tracing::warn!(?error, "failed to close lagged Discord request message");
        } else {
            remove_pending_if_same(state, &key, &request).await;
        }
    }
    Ok(())
}

pub(crate) async fn close_pending_requests(http: &serenity::http::Http, state: &BridgeState) {
    let requests = {
        let mut pending = state.pending_approvals.lock().await;
        claim_all_pending(&mut pending)
    };
    for (key, request) in requests {
        let mut message = match request.channel_id.message(http, request.message_id).await {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "failed to read pending Discord request during shutdown"
                );
                continue;
            }
        };
        if let Err(error) = message
            .edit(
                http,
                serenity::builder::EditMessage::new().components(Vec::new()),
            )
            .await
        {
            tracing::warn!(
                ?error,
                "failed to close pending Discord request during shutdown"
            );
        } else {
            remove_pending_if_same(state, &key, &request).await;
        }
    }
}

fn claim_resolved_pending(
    pending: &mut std::collections::HashMap<String, PendingApproval>,
    request_id: &Value,
    connection_generation: u64,
) -> Vec<(String, PendingApproval)> {
    let keys = pending
        .iter()
        .filter(|(_, request)| {
            &request.native_id == request_id
                && request.connection_generation == connection_generation
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    keys.into_iter()
        .filter_map(|key| {
            let request = pending.get_mut(&key)?;
            request.claimed = true;
            Some((key, request.clone()))
        })
        .collect()
}

fn claim_generation_pending(
    pending: &mut std::collections::HashMap<String, PendingApproval>,
    connection_generation: u64,
) -> Vec<(String, PendingApproval)> {
    let keys = pending
        .iter()
        .filter(|(_, request)| request.connection_generation == connection_generation)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    keys.into_iter()
        .filter_map(|key| {
            let request = pending.get_mut(&key)?;
            request.claimed = true;
            Some((key, request.clone()))
        })
        .collect()
}

fn claim_all_pending(
    pending: &mut std::collections::HashMap<String, PendingApproval>,
) -> Vec<(String, PendingApproval)> {
    let keys = pending
        .iter()
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    keys.into_iter()
        .filter_map(|key| {
            let request = pending.get_mut(&key)?;
            request.claimed = true;
            Some((key, request.clone()))
        })
        .collect()
}

fn authorize(
    state: &BridgeState,
    guild_id: Option<GuildId>,
    user_id: UserId,
    language: crate::i18n::Language,
) -> anyhow::Result<()> {
    if guild_id.map(|id| id.get()) != Some(state.config.guild_id) {
        bail!(
            "{}",
            language.text(
                "This command is available only in the configured home server.",
                "이 명령은 설정된 홈 서버에서만 사용할 수 있습니다",
                "此命令只能在已配置的主服务器中使用。",
                "このコマンドは設定済みのホームサーバーでのみ使用できます。"
            )
        );
    }
    if !state.config.operator_ids.contains(&user_id.get()) {
        bail!(
            "{}",
            language.text(
                "This Discord user is not configured as an operator.",
                "이 Discord 사용자는 조작자로 설정되어 있지 않습니다",
                "此 Discord 用户未配置为操作员。",
                "この Discord ユーザーはオペレーターに設定されていません。"
            )
        );
    }
    Ok(())
}

fn no_mentions() -> CreateAllowedMentions {
    CreateAllowedMentions::new()
        .all_users(false)
        .all_roles(false)
        .everyone(false)
        .replied_user(false)
}

fn input_mode_korean(mode: crate::config::InputMode) -> &'static str {
    input_mode(mode, crate::i18n::Language::English)
}

fn input_mode(mode: crate::config::InputMode, language: crate::i18n::Language) -> &'static str {
    match mode {
        crate::config::InputMode::Steering => language.text(
            "Send to the active response",
            "실행 중인 응답에 전달",
            "发送到当前回复",
            "進行中の応答に送信",
        ),
        crate::config::InputMode::NextTurn => language.text(
            "Send before the next response",
            "다음 응답 전에 전달",
            "在下一条回复前发送",
            "次の応答の前に送信",
        ),
    }
}

#[cfg(test)]
mod discord_permission_tests {
    use super::*;
    use serenity::all::{Guild, Role, RoleId};

    fn fixture(
        everyone_permissions: Permissions,
        roles: &[(u64, Permissions)],
        member_roles: &[u64],
        overwrites: Value,
        kind: ChannelType,
    ) -> (PartialGuild, Member, GuildChannel) {
        let mut guild = Guild::default();
        guild.id = GuildId::new(100);
        guild.owner_id = UserId::new(999);
        for (id, permissions) in
            std::iter::once((100, everyone_permissions)).chain(roles.iter().copied())
        {
            let mut role = Role::default();
            role.id = RoleId::new(id);
            role.guild_id = guild.id;
            role.permissions = permissions;
            guild.roles.insert(role.id, role);
        }
        let mut member = Member::default();
        member.user.id = UserId::new(200);
        member.user.bot = true;
        member.guild_id = guild.id;
        member.roles = member_roles.iter().copied().map(RoleId::new).collect();
        let channel = serde_json::from_value(json!({
            "id": "101",
            "guild_id": "100",
            "name": "permission fixture",
            "type": u8::from(kind),
            "permission_overwrites": overwrites,
        }))
        .expect("typed Serenity channel fixture");
        (guild.into(), member, channel)
    }

    fn overwrite(id: u64, kind: u8, allow: Permissions, deny: Permissions) -> Value {
        json!({
            "id": id.to_string(),
            "type": kind,
            "allow": allow.bits().to_string(),
            "deny": deny.bits().to_string(),
        })
    }

    fn text_permissions() -> Permissions {
        Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES | Permissions::READ_MESSAGE_HISTORY
    }

    #[test]
    fn discord_permissions_use_authenticated_member_roles_for_home() {
        let (guild, mut member, channel) = fixture(
            Permissions::VIEW_CHANNEL,
            &[
                (20, Permissions::SEND_MESSAGES),
                (30, Permissions::READ_MESSAGE_HISTORY),
            ],
            &[20, 30],
            json!([]),
            ChannelType::Text,
        );
        let result =
            validate_discord_channel_permissions(&guild, &member, &channel, true, false, false)
                .unwrap();
        assert_eq!(result.effective_permissions, text_permissions().bits());
        assert_eq!(result.required_permissions, text_permissions().bits());
        member.roles.clear();
        let error =
            validate_discord_channel_permissions(&guild, &member, &channel, true, false, false)
                .unwrap_err()
                .to_string();
        assert!(error.contains("home text"));
        assert!(error.contains("Send Messages"));
        assert!(error.contains("Read Message History"));
    }

    #[test]
    fn discord_permissions_apply_everyone_roles_then_bot_member_overwrite() {
        let (guild, member, mut channel) = fixture(
            text_permissions(),
            &[(20, Permissions::empty()), (30, Permissions::empty())],
            &[20, 30],
            json!([
                overwrite(
                    100,
                    0,
                    Permissions::empty(),
                    Permissions::READ_MESSAGE_HISTORY
                ),
                overwrite(20, 0, Permissions::empty(), Permissions::SEND_MESSAGES),
                overwrite(
                    30,
                    0,
                    Permissions::SEND_MESSAGES | Permissions::READ_MESSAGE_HISTORY,
                    Permissions::empty()
                ),
                overwrite(40, 0, Permissions::empty(), text_permissions()),
                overwrite(201, 1, Permissions::empty(), text_permissions()),
                overwrite(200, 1, Permissions::empty(), Permissions::SEND_MESSAGES),
            ]),
            ChannelType::Text,
        );
        let error =
            validate_discord_channel_permissions(&guild, &member, &channel, true, false, false)
                .unwrap_err()
                .to_string();
        assert!(error.contains("Send Messages"));
        assert!(!error.contains("Read Message History"));
        channel.permission_overwrites.pop();
        let result =
            validate_discord_channel_permissions(&guild, &member, &channel, true, false, false)
                .unwrap();
        assert_eq!(result.effective_permissions, text_permissions().bits());
        channel.permission_overwrites.push(
            serde_json::from_value(overwrite(
                200,
                1,
                Permissions::SEND_MESSAGES,
                Permissions::READ_MESSAGE_HISTORY,
            ))
            .unwrap(),
        );
        let error =
            validate_discord_channel_permissions(&guild, &member, &channel, true, false, false)
                .unwrap_err()
                .to_string();
        assert!(error.contains("Read Message History"));
        assert!(!error.contains("Send Messages"));
    }

    #[test]
    fn discord_transcript_requires_public_creation_and_thread_send_without_private_creation() {
        let (mut guild, member, channel) =
            fixture(text_permissions(), &[], &[], json!([]), ChannelType::Text);
        let error =
            validate_discord_channel_permissions(&guild, &member, &channel, true, true, false)
                .unwrap_err()
                .to_string();
        assert!(error.contains("home text, transcript text"));
        assert!(error.contains("Create Public Threads"));
        assert!(error.contains("Send Messages in Threads"));
        let role = guild.roles.get_mut(&RoleId::new(100)).unwrap();
        role.permissions |= Permissions::CREATE_PUBLIC_THREADS;
        let error =
            validate_discord_channel_permissions(&guild, &member, &channel, false, true, false)
                .unwrap_err()
                .to_string();
        assert!(!error.contains("Create Public Threads"));
        assert!(error.contains("Send Messages in Threads"));
        guild.roles.get_mut(&RoleId::new(100)).unwrap().permissions |=
            Permissions::SEND_MESSAGES_IN_THREADS;
        let result =
            validate_discord_channel_permissions(&guild, &member, &channel, true, true, false)
                .unwrap();
        let required = Permissions::from_bits_retain(result.required_permissions);
        assert!(!required.contains(Permissions::CREATE_PRIVATE_THREADS));
        assert_eq!(result.effective_permissions, result.required_permissions);
    }

    #[test]
    fn discord_voice_requires_view_connect_and_speak_after_channel_overwrites() {
        let voice = Permissions::VIEW_CHANNEL | Permissions::CONNECT | Permissions::SPEAK;
        let (guild, member, mut channel) = fixture(
            Permissions::empty(),
            &[(20, voice)],
            &[20],
            json!([overwrite(20, 0, Permissions::empty(), Permissions::CONNECT)]),
            ChannelType::Voice,
        );
        let error =
            validate_discord_channel_permissions(&guild, &member, &channel, false, false, true)
                .unwrap_err()
                .to_string();
        assert!(error.contains("configured voice"));
        assert!(error.contains("Connect"));
        channel.permission_overwrites = serde_json::from_value(json!([overwrite(
            200,
            1,
            Permissions::empty(),
            Permissions::VIEW_CHANNEL
        ),]))
        .unwrap();
        let error =
            validate_discord_channel_permissions(&guild, &member, &channel, false, false, true)
                .unwrap_err()
                .to_string();
        assert!(error.contains("View Channel"));
        channel.permission_overwrites.clear();
        let result =
            validate_discord_channel_permissions(&guild, &member, &channel, false, false, true)
                .unwrap();
        assert_eq!(result.required_permissions, voice.bits());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn discord_owner_and_admin_bypass_channel_denials() {
        for administrator in [false, true] {
            let (mut guild, member, channel) = fixture(
                Permissions::empty(),
                &[(
                    20,
                    if administrator {
                        Permissions::ADMINISTRATOR
                    } else {
                        Permissions::empty()
                    },
                )],
                &[20],
                json!([
                    overwrite(100, 0, Permissions::empty(), Permissions::all()),
                    overwrite(20, 0, Permissions::empty(), Permissions::all()),
                    overwrite(200, 1, Permissions::empty(), Permissions::all()),
                ]),
                ChannelType::Text,
            );
            if !administrator {
                guild.owner_id = member.user.id;
            }
            let result =
                validate_discord_channel_permissions(&guild, &member, &channel, true, true, false)
                    .unwrap();
            assert_eq!(result.effective_permissions, Permissions::all().bits());
        }
    }

    #[test]
    fn discord_channel_types_and_stage_speaking_limit_are_reported() {
        let (guild, member, mut channel) = fixture(
            Permissions::VIEW_CHANNEL | Permissions::CONNECT | Permissions::SPEAK,
            &[],
            &[],
            json!([]),
            ChannelType::Stage,
        );
        let result =
            validate_discord_channel_permissions(&guild, &member, &channel, false, false, true)
                .unwrap();
        assert_eq!(result.kind, "stage");
        assert_eq!(result.warnings.len(), 1);
        assert!(
            !Permissions::from_bits_retain(result.required_permissions)
                .contains(Permissions::REQUEST_TO_SPEAK)
        );
        assert!(
            validate_discord_channel_permissions(&guild, &member, &channel, true, false, false,)
                .is_err()
        );
        channel.kind = ChannelType::Text;
        assert!(
            validate_discord_channel_permissions(&guild, &member, &channel, false, false, true,)
                .is_err()
        );
        channel.kind = ChannelType::Voice;
        channel.guild_id = GuildId::new(102);
        assert!(
            validate_discord_channel_permissions(&guild, &member, &channel, false, false, true,)
                .unwrap_err()
                .to_string()
                .contains("different guild")
        );
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;

    #[test]
    fn question_card_shows_question_and_choices_without_bridge_metadata() {
        let params = json!({
            "threadId":"thread-private-123",
            "questions":[{
                "id":"question-private-456",
                "header":"배포 방식",
                "question":"어느 환경에 배포할까요?",
                "options":[{"label":"테스트 서버","description":"검증용 환경"}]
            }]
        });
        let card = describe_server_request_with_language(
            "item/tool/requestUserInput",
            &params,
            crate::i18n::Language::Korean,
        );
        assert!(card.contains("어느 환경에 배포할까요?"));
        assert!(card.contains("테스트 서버"));
        assert!(!card.contains("thread-private-123"));
        assert!(!card.contains("question-private-456"));
        assert!(!card.contains("\"threadId\""));
        assert!(!card.contains("```"));
    }

    #[test]
    fn long_question_messages_preserve_full_text_and_bind_actions_only_to_last() {
        let method = "item/tool/requestUserInput";
        let key = "button-routing-key";
        let short_params = json!({
            "threadId": "private-thread-id",
            "questions": [{
                "id": "private-question-id",
                "header": "배포 방식",
                "question": "어느 환경에 배포할까요?",
                "options": [{"label": "A", "description": "테스트 서버"}]
            }]
        });
        let short_messages = server_request_messages_with_language(
            method,
            key,
            &short_params,
            crate::i18n::Language::Korean,
        );
        assert_eq!(short_messages.len(), 1);
        let short_message = serde_json::to_value(&short_messages[0]).unwrap();
        assert_eq!(
            short_message["content"],
            json!(
                "Codex가 다음 질문에 답변을 요청했습니다.\n\n1. 배포 방식: 어느 환경에 배포할까요?\n선택지: A — 테스트 서버\n\n‘응답 작성’을 눌러 답변해 주세요."
            )
        );
        assert_eq!(
            short_message["components"],
            serde_json::to_value(approval_actions_with_language(
                method,
                key,
                &short_params,
                crate::i18n::Language::Korean
            ))
            .unwrap()
        );
        assert!(short_message["attachments"].as_array().unwrap().is_empty());

        let prompt = format!("{} 끝까지 보존할 질문🙂", "질문🙂 ".repeat(401));
        let long_params = json!({
            "threadId": "private-thread-id",
            "_discodex_connection_generation": 999,
            "debug": "private-debug-metadata",
            "questions": [{
                "id": "private-question-id",
                "header": "전체 질문 — 확인",
                "question": prompt,
                "options": [
                    {"label": "A", "description": "첫 선택지🙂"},
                    {"label": "B", "description": "둘째 선택지 — 끝까지"}
                ]
            }]
        });
        let expected = format!(
            "Codex가 다음 질문에 답변을 요청했습니다.\n\n1. 전체 질문 — 확인: {prompt}\n선택지: A — 첫 선택지🙂 / B — 둘째 선택지 — 끝까지\n\n‘응답 작성’을 눌러 답변해 주세요."
        );
        let builders = server_request_messages_with_language(
            method,
            key,
            &long_params,
            crate::i18n::Language::Korean,
        );
        assert!(builders.len() > 1);
        let expected_actions = serde_json::to_value(approval_actions_with_language(
            method,
            key,
            &long_params,
            crate::i18n::Language::Korean,
        ))
        .unwrap();
        let expected_mentions = serde_json::to_value(no_mentions()).unwrap();
        let mut reconstructed = String::new();
        for (index, builder) in builders.iter().enumerate() {
            let serialized = serde_json::to_value(builder).unwrap();
            let content = serialized["content"].as_str().unwrap();
            assert!(content.len() <= 1800);
            reconstructed.push_str(content);
            assert_eq!(serialized["allowed_mentions"], expected_mentions);
            assert!(serialized["attachments"].as_array().unwrap().is_empty());
            if index + 1 == builders.len() {
                assert_eq!(serialized["components"], expected_actions);
                assert_eq!(
                    serialized["components"][0]["components"][0]["custom_id"],
                    json!(format!("codex:approval:{key}:answer"))
                );
            } else {
                assert!(serialized.get("components").is_none());
            }
            assert_eq!(serde_json::to_value(builder.clone()).unwrap(), serialized);
        }
        assert_eq!(reconstructed.as_bytes(), expected.as_bytes());
        assert!(reconstructed.find("선택지: A").unwrap() > 1800);
        for private in [
            "private-thread-id",
            "private-question-id",
            "private-debug-metadata",
            "threadId",
            "_discodex_connection_generation",
            "\"questions\"",
        ] {
            assert!(!reconstructed.contains(private));
        }

        let command_reason = "명령理由🙂".repeat(401);
        let command_params = json!({"reason": command_reason.clone()});
        let command_expected = format!("{}\n아래에서 허용 범위를 선택해 주세요.", command_reason);
        let command_messages = server_request_messages_with_language(
            "item/commandExecution/requestApproval",
            key,
            &command_params,
            crate::i18n::Language::Korean,
        );
        assert!(command_messages.len() > 1);
        let expected_command_actions = serde_json::to_value(approval_actions_with_language(
            "item/commandExecution/requestApproval",
            key,
            &command_params,
            crate::i18n::Language::Korean,
        ))
        .unwrap();
        let mut reconstructed_command = String::new();
        for (index, builder) in command_messages.iter().enumerate() {
            let serialized = serde_json::to_value(builder).unwrap();
            let content = serialized["content"].as_str().unwrap();
            assert!(content.len() <= 1800);
            reconstructed_command.push_str(content);
            if index + 1 == command_messages.len() {
                assert_eq!(serialized["components"], expected_command_actions);
            } else {
                assert!(serialized.get("components").is_none());
            }
        }
        assert_eq!(
            reconstructed_command.as_bytes(),
            command_expected.as_bytes()
        );
    }

    #[test]
    fn mcp_form_messages_show_schema_titles_choices_and_full_human_text() {
        let method = "mcpServer/elicitation/request";
        let key = "mcp-answer-routing-key";
        let message = format!("{} 원문 질문🙂", "m".repeat(1801));
        let params = json!({
            "mode": "form", "serverName": "private-server-name",
            "threadId": "private-thread-id", "turnId": "private-turn-id",
            "_meta": {"private": "private-debug-metadata"},
            "message": message,
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "a_plain": {"type": "string", "title": "배포 지역", "description": "지역을 선택하세요", "enum": ["서울", "부산"]},
                    "b_legacy": {"type": "string", "title": "사용 지역", "enum": ["us-west", "eu-north"], "enumNames": ["미국 서부", "북유럽"]},
                    "c_titled": {"type": "string", "title": "실행 환경", "oneOf": [{"const": "staging", "title": "검증"}, {"const": "prod", "title": "운영"}]},
                    "d_multi_plain": {"type": "array", "title": "여러 이름", "items": {"type": "string", "enum": ["alpha", "beta"]}},
                    "e_multi_titled": {"type": "array", "title": "여러 기능", "items": {"anyOf": [{"const": "log", "title": "로그"}, {"const": "cache", "title": "캐시"}]}},
                    "f_count": {"type": "number", "title": "개수"},
                    "g_flag": {"type": "boolean", "title": "확인"}
                },
                "required": ["a_plain", "c_titled", "d_multi_plain"]
            }
        });
        let expected_schema = "배포 지역 (a_plain) (필수) · 문자 — 지역을 선택하세요 · 선택지: \"서울\" / \"부산\", 사용 지역 (b_legacy) · 문자 · 선택지: 미국 서부 (값: \"us-west\") / 북유럽 (값: \"eu-north\"), 실행 환경 (c_titled) (필수) · 문자 · 선택지: 검증 (값: \"staging\") / 운영 (값: \"prod\"), 여러 이름 (d_multi_plain) (필수) · 목록 · 선택지 (여러 개): \"alpha\" / \"beta\", 여러 기능 (e_multi_titled) · 목록 · 선택지 (여러 개): 로그 (값: \"log\") / 캐시 (값: \"cache\"), 개수 (f_count) · 숫자, 확인 (g_flag) · 예/아니요";
        assert_eq!(
            describe_schema_with_language(
                &params["requestedSchema"],
                crate::i18n::Language::Korean
            ),
            expected_schema
        );
        let expected = format!(
            "MCP 도구가 추가 정보를 요청했습니다.\n{message}\n요청한 항목: {expected_schema}"
        );
        for mode in ["form", "openai/form", "openaiForm"] {
            let mut mode_params = params.clone();
            mode_params["mode"] = json!(mode);
            let builders = server_request_messages_with_language(
                method,
                key,
                &mode_params,
                crate::i18n::Language::Korean,
            );
            assert!(builders.len() > 1);
            let expected_actions = serde_json::to_value(approval_actions_with_language(
                method,
                key,
                &mode_params,
                crate::i18n::Language::Korean,
            ))
            .unwrap();
            let mut reconstructed = String::new();
            for (index, builder) in builders.iter().enumerate() {
                let serialized = serde_json::to_value(builder).unwrap();
                let content = serialized["content"].as_str().unwrap();
                assert!(content.len() <= 1800);
                reconstructed.push_str(content);
                assert_eq!(
                    serialized["allowed_mentions"],
                    serde_json::to_value(no_mentions()).unwrap()
                );
                assert!(serialized["attachments"].as_array().unwrap().is_empty());
                if index + 1 == builders.len() {
                    assert_eq!(serialized["components"], expected_actions);
                    assert_eq!(
                        serialized["components"][0]["components"][0]["custom_id"],
                        json!(format!("codex:approval:{key}:answer"))
                    );
                    assert_eq!(
                        serialized["components"][0]["components"][1]["custom_id"],
                        json!(format!("codex:approval:{key}:decline"))
                    );
                    assert_eq!(
                        serialized["components"][0]["components"][2]["custom_id"],
                        json!(format!("codex:approval:{key}:cancel"))
                    );
                } else {
                    assert!(serialized.get("components").is_none());
                }
            }
            assert_eq!(reconstructed.as_bytes(), expected.as_bytes());
            assert!(reconstructed.find("배포 지역 (a_plain)").unwrap() > 1800);
            for private in [
                "private-server-name",
                "private-thread-id",
                "private-turn-id",
                "private-debug-metadata",
                "requestedSchema",
                "enumNames",
                "oneOf",
                "anyOf",
                "\"properties\"",
            ] {
                assert!(!reconstructed.contains(private));
            }
            let mut short_params = mode_params.clone();
            short_params["message"] = json!("지역을 선택하세요.");
            let short_builders = server_request_messages_with_language(
                method,
                key,
                &short_params,
                crate::i18n::Language::Korean,
            );
            assert_eq!(short_builders.len(), 1);
            assert_eq!(
                serde_json::to_value(&short_builders[0]).unwrap()["content"],
                json!(format!(
                    "MCP 도구가 추가 정보를 요청했습니다.\n지역을 선택하세요.\n요청한 항목: {expected_schema}"
                ))
            );
        }
        assert_eq!(
            describe_schema_with_language(
                &json!({"type": "object", "properties": {"region": {"type": "string", "description": "배포 지역 이름"}}, "required": ["region"]}),
                crate::i18n::Language::Korean
            ),
            "region (필수) · 문자 — 배포 지역 이름"
        );
    }

    #[test]
    fn mcp_tool_approval_buttons_responses_and_claim_state_follow_offered_scope() {
        let params = json!({
            "mode": "form",
            "message": "Allow the blender MCP server to run tool execute_blender_code?",
            "requestedSchema": {"type":"object", "properties":{}},
            "_meta": {"codex_approval_kind":"mcp_tool_call", "persist":["session", "always"]}
        });
        let pending = PendingApproval {
            native_id: json!(17),
            method: "mcpServer/elicitation/request".to_owned(),
            params: params.clone(),
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
            claimed: false,
            connection_generation: 1,
        };
        let rows = serde_json::to_value(approval_actions_with_language(
            &pending.method,
            "request",
            &params,
            crate::i18n::Language::Korean,
        ))
        .unwrap();
        let buttons = rows[0]["components"].as_array().unwrap();
        let aliases = buttons
            .iter()
            .map(|button| {
                button["custom_id"]
                    .as_str()
                    .unwrap()
                    .rsplit(':')
                    .next()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            aliases,
            vec![
                "mcp-allow",
                "mcp-session",
                "mcp-always",
                "decline",
                "cancel"
            ]
        );
        assert_eq!(
            buttons
                .iter()
                .map(|button| button["label"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["이번만 허용", "세션 허용", "항상 허용", "거절", "취소"]
        );
        assert_eq!(
            describe_server_request_with_language(
                &pending.method,
                &params,
                crate::i18n::Language::Korean
            ),
            "MCP 도구 실행 승인이 필요합니다.\nAllow the blender MCP server to run tool execute_blender_code?"
        );
        let expected = [
            json!({"action":"accept"}),
            json!({"action":"accept", "_meta":{"persist":"session"}}),
            json!({"action":"accept", "_meta":{"persist":"always"}}),
            json!({"action":"decline"}),
            json!({"action":"cancel"}),
        ];
        for (alias, expected_response) in aliases.iter().zip(expected) {
            assert_eq!(
                approval_result(&pending.method, alias, &pending).unwrap(),
                expected_response
            );
        }
        for mode in ["openai/form", "openaiForm"] {
            let mut alias_params = params.clone();
            alias_params["mode"] = json!(mode);
            assert_eq!(
                mcp_tool_approval_persist_options(&alias_params),
                Some(vec!["session", "always"])
            );
            let alias_actions = serde_json::to_value(approval_actions_with_language(
                &pending.method,
                mode,
                &alias_params,
                crate::i18n::Language::Korean,
            ))
            .unwrap();
            assert_eq!(alias_actions[0]["components"].as_array().unwrap().len(), 5);
            let alias_pending = PendingApproval {
                params: alias_params,
                ..pending.clone()
            };
            assert_eq!(
                approval_result(&pending.method, "mcp-session", &alias_pending).unwrap(),
                json!({"action":"accept", "_meta":{"persist":"session"}})
            );
        }
        let mut requests = std::collections::HashMap::from([("request".to_owned(), pending)]);
        assert!(
            claim_approval(&mut requests, "request", "mcp-always", 1)
                .unwrap()
                .1["_meta"]["persist"]
                == "always"
        );
        assert!(requests["request"].claimed);
        assert!(claim_approval(&mut requests, "request", "mcp-always", 1).is_err());
    }

    #[test]
    fn mcp_tool_approval_rejects_unoffered_scopes_and_modal_answers_without_claiming() {
        let params = json!({
            "mode": "form",
            "message": "Allow the blender MCP server to run tool execute_blender_code?",
            "requestedSchema": {"type":"object", "properties":{}},
            "_meta": {"codex_approval_kind":"mcp_tool_call", "persist":"session"}
        });
        let pending = PendingApproval {
            native_id: json!(17),
            method: "mcpServer/elicitation/request".to_owned(),
            params: params.clone(),
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
            claimed: false,
            connection_generation: 1,
        };
        assert!(
            input_modal_with_language(
                "mcpServer/elicitation/request",
                "request",
                &params,
                crate::i18n::Language::Korean
            )
            .is_err()
        );
        assert!(elicitation_result(&params, serde_json::Map::new()).is_err());
        let buttons = serde_json::to_value(approval_actions_with_language(
            &pending.method,
            "request",
            &params,
            crate::i18n::Language::Korean,
        ))
        .unwrap();
        let aliases = buttons[0]["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|button| {
                button["custom_id"]
                    .as_str()
                    .unwrap()
                    .rsplit(':')
                    .next()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            aliases,
            vec!["mcp-allow", "mcp-session", "decline", "cancel"]
        );
        let mut requests = std::collections::HashMap::from([("request".to_owned(), pending)]);
        for forged in ["mcp-always", "answer", "accept"] {
            assert!(claim_approval(&mut requests, "request", forged, 1).is_err());
            assert!(!requests["request"].claimed);
        }
    }

    #[test]
    fn ordinary_forms_and_non_form_elicitations_are_not_reclassified_as_tool_approval() {
        let ordinary_form = json!({
            "mode":"form", "message":"문자열 답변을 입력하세요.",
            "requestedSchema":{"type":"object", "properties":{"name":{"type":"string"}}},
            "_meta":{"codex_approval_kind":"other"}
        });
        assert_eq!(mcp_tool_approval_persist_options(&ordinary_form), None);
        let actions = serde_json::to_value(approval_actions_with_language(
            "mcpServer/elicitation/request",
            "form",
            &ordinary_form,
            crate::i18n::Language::Korean,
        ))
        .unwrap();
        assert_eq!(actions[0]["components"].as_array().unwrap().len(), 3);
        assert_eq!(actions[0]["components"][0]["label"], "응답 작성");
        let parsed = elicitation_result(
            &ordinary_form,
            serde_json::Map::from_iter([(
                "elicitation_json".to_owned(),
                json!("{\"name\":\"Ada\"}"),
            )]),
        )
        .unwrap();
        assert_eq!(parsed, json!({"action":"accept", "content":{"name":"Ada"}}));
        for request in [
            json!({"mode":"url", "url":"https://example.test", "elicitationId":"id", "_meta":{"codex_approval_kind":"mcp_tool_call"}}),
            json!({"mode":"openai/userVerification", "title":"승인", "challenge":"proof", "_meta":{"codex_approval_kind":"mcp_tool_call"}}),
        ] {
            assert_eq!(mcp_tool_approval_persist_options(&request), None);
            assert!(
                input_modal_with_language(
                    "mcpServer/elicitation/request",
                    "other",
                    &request,
                    crate::i18n::Language::Korean
                )
                .is_err()
            );
        }
        let marked_ordinary_form = json!({
            "mode":"form",
            "_meta":{"codex_approval_kind":"mcp_tool_call"},
            "requestedSchema":{"type":"object", "properties":{"name":{"type":"string"}}}
        });
        assert_eq!(
            mcp_tool_approval_persist_options(&marked_ordinary_form),
            None
        );
        assert!(
            input_modal_with_language(
                "mcpServer/elicitation/request",
                "ordinary",
                &marked_ordinary_form,
                crate::i18n::Language::Korean
            )
            .is_ok()
        );
        let user_input = json!({"mode":"url", "questions":[{"id":"q","question":"질문"}]});
        assert!(
            input_modal_with_language(
                "item/tool/requestUserInput",
                "question",
                &user_input,
                crate::i18n::Language::Korean
            )
            .is_ok()
        );
        let url_buttons = serde_json::to_value(approval_actions_with_language(
            "mcpServer/elicitation/request",
            "url",
            &json!({"mode":"url", "url":"https://example.test", "elicitationId":"id"}),
            crate::i18n::Language::Korean,
        ))
        .unwrap();
        assert_eq!(url_buttons[0]["components"].as_array().unwrap().len(), 2);
        assert_eq!(url_buttons[0]["components"][0]["label"], "거절");
        assert_eq!(url_buttons[0]["components"][1]["label"], "취소");
    }

    #[test]
    fn mcp_form_modal_json_result_preserves_original_keys_and_typed_user_values() {
        let params = json!({
            "mode": "form", "serverName": "private-server-name", "threadId": "private-thread-id",
            "message": "요청한 정보를 JSON으로 입력하세요.",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "deployment_region": {"type": "string", "title": "배포 지역", "enum": ["서울", "부산"]},
                    "names": {"type": "array", "title": "이름", "items": {"type": "string", "enum": ["alpha", "beta"]}},
                    "count": {"type": "number", "title": "개수"},
                    "confirmed": {"type": "boolean", "title": "확인"},
                    "note": {"type": "string", "title": "메모"}
                },
                "required": ["deployment_region", "names", "count", "confirmed", "note"]
            }
        });
        let modal = serde_json::to_value(
            input_modal_with_language(
                "mcpServer/elicitation/request",
                "mcp-answer-routing-key",
                &params,
                crate::i18n::Language::Korean,
            )
            .unwrap(),
        )
        .unwrap();
        let field = modal["components"][0]["components"][0]["custom_id"]
            .as_str()
            .unwrap();
        assert_eq!(field, "elicitation_json");
        let raw = r#"{"deployment_region":"부산","names":["alpha","beta"],"count":2.75,"confirmed":true,"note":" \t코드: 🙂\n끝  "}"#;
        let components =
            json!({"components": [{"components": [{"custom_id": field, "value": raw}]}]});
        let mut submitted = serde_json::Map::new();
        collect_modal_values(&components, &mut submitted);
        let expected_content = json!({"deployment_region": "부산", "names": ["alpha", "beta"], "count": 2.75, "confirmed": true, "note": " \t코드: 🙂\n끝  "});
        assert_eq!(
            elicitation_result(&params, submitted).unwrap(),
            json!({"action": "accept", "content": expected_content})
        );
        assert!(elicitation_result(&params, serde_json::Map::new()).is_err());
        assert!(
            elicitation_result(
                &params,
                serde_json::Map::from_iter([("elicitation_json".to_owned(), json!("{broken"))])
            )
            .is_err()
        );
    }

    #[test]
    fn individual_user_answers_preserve_reserved_id_and_exact_text() {
        let params = json!({
            "questions": [{"id": "answers_text", "question": "답변을 입력하세요."}]
        });
        for text in ["alpha", "1:alpha", " \n  alpha\n\n    beta \n "] {
            let components = json!({"components": [{"components": [{
                "custom_id": "answers_text", "value": text
            }]}]});
            let mut submitted = serde_json::Map::new();
            collect_modal_values(&components, &mut submitted);
            assert_eq!(
                user_input_result(&params, submitted).unwrap(),
                json!({"answers": {"answers_text": {"answers": [text]}}})
            );
        }
    }

    #[test]
    fn numbered_user_answers_use_question_count_and_preserve_native_ids() {
        let ids = [
            "answers_text",
            "second",
            "third",
            "fourth",
            "fifth",
            "sixth",
        ];
        let questions = ids
            .iter()
            .map(|id| json!({"id": id, "question": "답변을 입력하세요."}))
            .collect::<Vec<_>>();
        let params = json!({"questions": questions});
        let submitted = serde_json::Map::from_iter([(
            "answers_text".to_owned(),
            json!("1:alpha\n2:beta\n3:gamma\n4:delta\n5:epsilon\n6:zeta"),
        )]);
        let expected = ids
            .iter()
            .zip(["alpha", "beta", "gamma", "delta", "epsilon", "zeta"])
            .map(|(id, answer)| ((*id).to_owned(), json!({"answers": [answer]})))
            .collect::<serde_json::Map<_, _>>();
        assert_eq!(
            user_input_result(&params, submitted).unwrap(),
            json!({"answers": expected})
        );
        assert_eq!(
            user_input_result(&params, serde_json::Map::new())
                .unwrap_err()
                .to_string(),
            "모든 질문에 답변해 주세요"
        );
        for text in ["alpha", "1:alpha"] {
            let submitted = serde_json::Map::from_iter([("answers_text".to_owned(), json!(text))]);
            assert!(user_input_result(&params, submitted).is_err());
        }
    }

    #[test]
    fn numbered_user_answers_preserve_exact_body_after_each_separator() {
        let ids = [
            "answers_text",
            "indent-private",
            "empty-private",
            "spaces-private",
            "unicode-private",
            "last-private",
        ];
        let questions = ids
            .iter()
            .map(|id| json!({"id": id, "question": "답변을 입력하세요."}))
            .collect::<Vec<_>>();
        let params = json!({"questions": questions});
        let components = json!({"components": [{"components": [{
            "custom_id": "answers_text",
            "value": "  1 :    let value: i32 = 1;  \n2：\thttps://example.test/경로🙂  \n3:\n4：   \n5:e\u{301}　끝 \t\n6：「原文」\u{00a0}　 "
        }]}]});
        let mut submitted = serde_json::Map::new();
        collect_modal_values(&components, &mut submitted);
        let expected = ids
            .iter()
            .zip([
                "    let value: i32 = 1;  ",
                "\thttps://example.test/경로🙂  ",
                "",
                "   ",
                "e\u{301}　끝 \t",
                "「原文」\u{00a0}　 ",
            ])
            .map(|(id, answer)| ((*id).to_owned(), json!({"answers": [answer]})))
            .collect::<serde_json::Map<_, _>>();
        assert_eq!(
            user_input_result(&params, submitted).unwrap(),
            json!({"answers": expected})
        );
    }

    #[test]
    fn numbered_human_answers_keep_native_question_keys() {
        let questions = vec![
            json!({"id":"first-private-key","question":"첫 질문"}),
            json!({"id":"second-private-key","question":"둘째 질문"}),
        ];
        let answers = parse_numbered_answers("1:첫 답변\n2:두 번째 답변", &questions).unwrap();
        assert_eq!(answers["first-private-key"], json!({"answers":["첫 답변"]}));
        assert_eq!(
            answers["second-private-key"],
            json!({"answers":["두 번째 답변"]})
        );
    }

    #[test]
    fn command_approval_explains_reason_and_location_without_echoing_command() {
        let params = json!({
            "threadId":"thread-private-123",
            "command":["sh","-c","secret-command-body"],
            "reason":"테스트 환경의 빌드 결과를 확인합니다.",
            "cwd":"/workspace/example"
        });
        let card = describe_server_request_with_language(
            "item/commandExecution/requestApproval",
            &params,
            crate::i18n::Language::Korean,
        );
        assert!(card.contains("테스트 환경의 빌드 결과를 확인합니다."));
        assert!(card.contains("/workspace/example"));
        assert!(!card.contains("thread-private-123"));
        assert!(!card.contains("secret-command-body"));
        assert!(!card.contains("\"command\""));
    }

    #[test]
    fn permission_profiles_show_and_resolve_friendly_names() {
        let catalog = json!({"data":[
            {"id":":read-only","allowed":true,"description":null},
            {"id":":danger-full-access","allowed":true},
            {"id":":workspace","allowed":true}
        ]});
        let display =
            format_permission_catalog_with_language(&catalog, crate::i18n::Language::Korean);
        assert!(display.contains("읽기 전용 (Read Only)"));
        assert!(display.contains("전체 접근 (Full Access)"));
        assert!(display.contains("작업 공간 (Workspace)"));
        assert!(!display.contains(":danger-full-access"));
        assert!(!display.contains(":workspace"));

        assert_eq!(
            resolve_permission_profile_with_language(
                &catalog,
                "Full Access",
                crate::i18n::Language::Korean
            )
            .unwrap(),
            (
                ":danger-full-access".to_owned(),
                "전체 접근 (Full Access)".to_owned()
            )
        );
        assert_eq!(
            resolve_permission_profile_with_language(
                &catalog,
                "Workspace",
                crate::i18n::Language::Korean
            )
            .unwrap()
            .0,
            ":workspace"
        );
        assert_eq!(
            resolve_permission_profile_with_language(
                &catalog,
                "Read Only",
                crate::i18n::Language::Korean
            )
            .unwrap(),
            (":read-only".to_owned(), "읽기 전용 (Read Only)".to_owned())
        );
        // Existing operators may still use a remembered native value; the list never reveals it.
        assert_eq!(
            resolve_permission_profile_with_language(
                &catalog,
                ":danger-full-access",
                crate::i18n::Language::Korean
            )
            .unwrap()
            .0,
            ":danger-full-access"
        );
    }

    #[test]
    fn current_permission_profile_never_displays_machine_key() {
        let settings = format_settings_with_language(
            &json!({
                "activePermissionProfile":{"id":":danger-full-access"}
            }),
            crate::i18n::Language::Korean,
        );
        assert!(settings.contains("전체 접근 (Full Access)"));
        assert!(!settings.contains(":danger-full-access"));
    }

    #[test]
    fn current_read_only_profile_displays_human_label_without_exposing_native_id() {
        let settings = format_settings_with_language(
            &json!({
                "activePermissionProfile":{"id":":read-only"}
            }),
            crate::i18n::Language::Korean,
        );
        assert!(settings.contains("읽기 전용 (Read Only)"));
        assert!(!settings.contains(":read-only"));
    }

    #[test]
    fn offered_command_buttons_preserve_order_and_exact_decision_payloads() {
        let available = vec![
            json!("cancel"),
            json!({"acceptWithExecpolicyAmendment": {
                "execpolicy_amendment": ["cargo", "PRIVATE_COMMAND_PAYLOAD"]
            }}),
            json!("decline"),
            json!({"applyNetworkPolicyAmendment": {"network_policy_amendment": {
                "host": "first.example", "action": "allow", "opaque": "PRIVATE_RULE_PAYLOAD"
            }}}),
            json!("acceptForSession"),
            json!({"applyNetworkPolicyAmendment": {"network_policy_amendment": {
                "host": "second.example", "action": "deny"
            }}}),
            json!("accept"),
        ];
        let pending = PendingApproval {
            native_id: json!(17),
            method: "item/commandExecution/requestApproval".to_owned(),
            params: json!({
                "availableDecisions": available.clone(),
                "proposedExecpolicyAmendment": ["unselected-proposal"],
                "proposedNetworkPolicyAmendments": [{"host": "unselected.example", "action": "deny"}],
                "threadId": "PRIVATE_NATIVE_ID"
            }),
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
            claimed: false,
            connection_generation: 1,
        };
        let rows = serde_json::to_value(approval_actions_with_language(
            &pending.method,
            "request",
            &pending.params,
            crate::i18n::Language::Korean,
        ))
        .unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row["components"].as_array().unwrap().len())
                .collect::<Vec<_>>(),
            vec![5, 2]
        );
        let buttons = rows
            .iter()
            .flat_map(|row| row["components"].as_array().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(buttons[0]["label"], "거절·중단");
        assert_eq!(buttons[2]["label"], "거절 후 계속");
        for (index, (button, offered)) in buttons.iter().zip(&available).enumerate() {
            let custom_id = button["custom_id"].as_str().unwrap();
            assert_eq!(custom_id, format!("codex:approval:request:offered-{index}"));
            let alias = custom_id.rsplit(':').next().unwrap();
            assert_eq!(
                approval_result(&pending.method, alias, &pending).unwrap(),
                json!({"decision": offered})
            );
        }
        let rendered = serde_json::to_string(rows).unwrap();
        for private in [
            "PRIVATE_COMMAND_PAYLOAD",
            "PRIVATE_RULE_PAYLOAD",
            "PRIVATE_NATIVE_ID",
            "unselected-proposal",
        ] {
            assert!(!rendered.contains(private));
        }
    }

    #[test]
    fn fallback_command_buttons_preserve_basics_and_each_proposed_amendment() {
        for available in [None, Some(Value::Null)] {
            let mut params = json!({
                "proposedExecpolicyAmendment": ["git", "status"],
                "proposedNetworkPolicyAmendments": [
                    {"host": "first.example", "action": "allow", "opaque": "retained"},
                    {"host": "second.example", "action": "deny"}
                ]
            });
            if let Some(available) = available {
                params["availableDecisions"] = available;
            }
            let pending = PendingApproval {
                native_id: json!(17),
                method: "item/commandExecution/requestApproval".to_owned(),
                params: params.clone(),
                channel_id: ChannelId::new(42),
                message_id: serenity::all::MessageId::new(76),
                claimed: false,
                connection_generation: 1,
            };
            let expected = vec![
                json!("accept"),
                json!("acceptForSession"),
                json!("decline"),
                json!("cancel"),
                json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": params["proposedExecpolicyAmendment"]}}),
                json!({"applyNetworkPolicyAmendment": {"network_policy_amendment": params["proposedNetworkPolicyAmendments"][0]}}),
                json!({"applyNetworkPolicyAmendment": {"network_policy_amendment": params["proposedNetworkPolicyAmendments"][1]}}),
            ];
            let rows = serde_json::to_value(approval_actions_with_language(
                &pending.method,
                "request",
                &params,
                crate::i18n::Language::Korean,
            ))
            .unwrap();
            let rows = rows.as_array().unwrap();
            assert_eq!(
                rows.iter()
                    .map(|row| row["components"].as_array().unwrap().len())
                    .collect::<Vec<_>>(),
                vec![5, 2]
            );
            let buttons = rows
                .iter()
                .flat_map(|row| row["components"].as_array().unwrap())
                .collect::<Vec<_>>();
            assert!(
                buttons[5]["label"]
                    .as_str()
                    .unwrap()
                    .contains("first.example")
            );
            assert!(
                buttons[6]["label"]
                    .as_str()
                    .unwrap()
                    .contains("second.example")
            );
            for (button, decision) in buttons.iter().zip(&expected) {
                let alias = button["custom_id"]
                    .as_str()
                    .unwrap()
                    .rsplit(':')
                    .next()
                    .unwrap();
                assert_eq!(
                    approval_result(&pending.method, alias, &pending).unwrap(),
                    json!({"decision": decision})
                );
            }
            let ordinary = serde_json::to_value(approval_actions_with_language(
                &pending.method,
                "request",
                &json!({}),
                crate::i18n::Language::Korean,
            ))
            .unwrap();
            assert_eq!(ordinary[0]["components"].as_array().unwrap().len(), 4);
        }
    }

    #[test]
    fn approval_validation_precedes_claim_and_valid_choice_is_single_use() {
        let pending = PendingApproval {
            native_id: json!(17),
            method: "item/commandExecution/requestApproval".to_owned(),
            params: json!({"availableDecisions": ["decline", "cancel"]}),
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
            claimed: false,
            connection_generation: 1,
        };
        let rows = serde_json::to_value(approval_actions_with_language(
            &pending.method,
            "request",
            &pending.params,
            crate::i18n::Language::Korean,
        ))
        .unwrap();
        let alias = rows[0]["components"][0]["custom_id"]
            .as_str()
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap();
        let mut requests = std::collections::HashMap::from([("request".to_owned(), pending)]);
        for invalid in ["accept", "offered-99", "unknown"] {
            assert!(claim_approval(&mut requests, "request", invalid, 1).is_err());
            assert!(!requests["request"].claimed);
        }
        let (claimed, result) = claim_approval(&mut requests, "request", alias, 1).unwrap();
        assert!(claimed.claimed && requests["request"].claimed);
        assert_eq!(result, json!({"decision": "decline"}));
        assert!(claim_approval(&mut requests, "request", alias, 1).is_err());
        assert!(requests["request"].claimed);
    }

    #[test]
    fn approval_choices_preserve_native_scope_and_permissions() {
        let pending = PendingApproval {
            native_id: json!(17),
            method: "item/permissions/requestApproval".to_owned(),
            params: json!({"permissions":{"network":true,"read":true}}),
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
            claimed: false,
            connection_generation: 1,
        };

        assert_eq!(
            approval_result("item/permissions/requestApproval", "turn", &pending).unwrap(),
            json!({"permissions":{"network":true,"read":true},"scope":"turn"})
        );
        assert_eq!(
            approval_result("item/permissions/requestApproval", "session", &pending).unwrap(),
            json!({"permissions":{"network":true,"read":true},"scope":"session"})
        );
        assert_eq!(
            approval_result("item/permissions/requestApproval", "none", &pending).unwrap(),
            json!({"permissions":{},"scope":"turn"})
        );
        assert!(approval_result("item/permissions/requestApproval", "cancel", &pending).is_err());
    }

    #[test]
    fn pending_request_claim_is_single_use_until_delivery_finishes() {
        let pending = PendingApproval {
            native_id: json!(17),
            method: "item/commandExecution/requestApproval".to_owned(),
            params: json!({}),
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(76),
            claimed: false,
            connection_generation: 1,
        };
        let mut requests = std::collections::HashMap::from([("request".to_owned(), pending)]);

        assert!(claim_pending(&mut requests, "request", 1).is_ok());
        assert!(claim_pending(&mut requests, "request", 1).is_err());
        requests.get_mut("request").unwrap().claimed = false;
        assert!(claim_pending(&mut requests, "request", 1).is_ok());
        requests.get_mut("request").unwrap().claimed = false;
        assert!(claim_pending(&mut requests, "request", 2).is_err());
    }

    #[test]
    fn resolved_request_claims_only_matching_generation_and_retains_cards() {
        let request = |generation| PendingApproval {
            native_id: json!(17),
            method: "item/commandExecution/requestApproval".to_owned(),
            params: json!({}),
            channel_id: ChannelId::new(42),
            message_id: serenity::all::MessageId::new(generation),
            claimed: false,
            connection_generation: generation,
        };
        let mut requests = std::collections::HashMap::from([
            ("old".to_owned(), request(1)),
            ("new".to_owned(), request(2)),
            ("duplicate".to_owned(), request(2)),
        ]);

        let resolved = claim_resolved_pending(&mut requests, &json!(17), 1);
        assert_eq!(resolved.len(), 1);
        assert!(requests.get("old").is_some_and(|request| request.claimed));
        assert!(requests.contains_key("new"));
        assert!(requests.contains_key("duplicate"));

        let resolved = claim_resolved_pending(&mut requests, &json!(17), 2);
        assert_eq!(resolved.len(), 2);
        assert!(requests.values().all(|request| request.claimed));

        let mut requests = std::collections::HashMap::from([
            ("old".to_owned(), request(1)),
            ("new".to_owned(), request(2)),
        ]);
        let closed = claim_generation_pending(&mut requests, 1);
        assert_eq!(closed.len(), 1);
        assert!(requests.get("old").is_some_and(|request| request.claimed));
        assert!(requests.contains_key("new"));
    }

    #[test]
    fn live_settings_native_status_preserves_separate_future_default_readback() {
        let defaults = json!({"model":"actual-default", "effort":"low", "serviceTier":"priority"});
        let applied: anyhow::Result<Value> = Ok(json!({"status":"applied"}));
        let reply = format_settings_update_result_with_language(
            "requested-model",
            Some(&defaults),
            Some(&applied),
            crate::i18n::Language::Korean,
        );
        assert!(reply.contains("요청한 값: `requested-model`"));
        assert!(reply.contains("이후 대화 기본값:\n모델: actual-default"));
        assert!(reply.contains("이후 작업이 사용할 설정을 갱신했습니다"));
        assert!(reply.contains("이미 시작된 작업은 바꾸지 않습니다"));
        let unavailable: anyhow::Result<Value> = Ok(json!({"status":"targetUnavailable"}));
        let failed_live = format_settings_update_result_with_language(
            "requested-model",
            Some(&defaults),
            Some(&unavailable),
            crate::i18n::Language::Korean,
        );
        assert!(failed_live.contains("진행 중인 응답에 설정을 적용할 수 없었습니다"));
        assert!(!failed_live.contains("이후 작업이 사용할 설정을 갱신했습니다"));
        assert_eq!(
            reply.split("현재 응답:").next(),
            failed_live.split("현재 응답:").next()
        );
        for value in [
            json!({"status":"future-status"}),
            json!({}),
            json!({"status":null}),
            json!({"status":7}),
        ] {
            let unknown: anyhow::Result<Value> = Ok(value);
            let reply = format_settings_update_result_with_language(
                "requested-model",
                Some(&defaults),
                Some(&unknown),
                crate::i18n::Language::Korean,
            );
            assert!(reply.contains("설정 적용 결과를 확인하지 못했습니다"));
            assert!(!reply.contains("이후 작업이 사용할 설정을 갱신했습니다"));
            assert!(reply.contains("모델: actual-default"));
        }
    }

    #[test]
    fn settings_rpc_failure_idle_and_default_readback_failure_remain_distinct() {
        let defaults = json!({"model":"actual-default"});
        let failure: anyhow::Result<Value> = Err(anyhow!("native RPC failed"));
        let failed = format_settings_update_result_with_language(
            "requested-model",
            Some(&defaults),
            Some(&failure),
            crate::i18n::Language::Korean,
        );
        assert!(failed.contains("설정 변경 요청을 전달하지 못했습니다"));
        assert!(failed.contains("모델: actual-default"));
        assert!(!failed.contains("진행 중인 응답이 없습니다"));
        let idle = format_settings_update_result_with_language(
            "requested-model",
            Some(&defaults),
            None,
            crate::i18n::Language::Korean,
        );
        assert!(idle.contains("진행 중인 응답이 없습니다"));
        assert!(!idle.contains("설정 변경 요청을 전달하지 못했습니다"));
        let applied: anyhow::Result<Value> = Ok(json!({"status":"applied"}));
        let unread = format_settings_update_result_with_language(
            "requested-model",
            None,
            Some(&applied),
            crate::i18n::Language::Korean,
        );
        assert!(unread.contains("이후 대화 기본값:\n조회하지 못했습니다."));
        assert!(unread.contains("이후 작업이 사용할 설정을 갱신했습니다"));
    }

    #[test]
    fn permission_confirmation_distinguishes_current_active_and_idle_turns() {
        let confirmed =
            json!({"model":"future-model", "activePermissionProfile":{"id":":read-only"}});
        let active = format_permission_settings_result_with_language(
            "읽기 전용",
            &confirmed,
            true,
            crate::i18n::Language::Korean,
        );
        assert!(active.contains("이후 대화 기본값을 확인했습니다"));
        assert!(active.contains("모델: future-model"));
        assert!(active.contains("권한 프로필: 읽기 전용 (Read Only)"));
        assert!(active.contains("진행 중인 응답의 권한 적용은 확인하지 않았습니다"));
        assert!(!active.contains("진행 중인 응답이 없습니다"));
        let idle = format_permission_settings_result_with_language(
            "읽기 전용",
            &confirmed,
            false,
            crate::i18n::Language::Korean,
        );
        assert!(idle.contains("진행 중인 응답이 없습니다"));
        assert!(!idle.contains("진행 중인 응답의 권한 적용은 확인하지 않았습니다"));
        assert_eq!(
            active.split("현재 응답:").next(),
            idle.split("현재 응답:").next()
        );
    }

    #[tokio::test]
    async fn setting_choices_require_actual_current_model_without_default_substitution() {
        let catalog = json!({"data":[
            {"id":"gpt-default","isDefault":true,
             "supportedReasoningEfforts":[{"reasoningEffort":"low"}],
             "serviceTiers":[{"id":"priority"}]}
        ]});
        for current in [Some("gpt-running".to_owned()), None] {
            assert!(
                validate_setting_choice("effort", "low", &catalog, &current)
                    .await
                    .is_err()
            );
            assert!(
                validate_setting_choice("fast", "priority", &catalog, &current)
                    .await
                    .is_err()
            );
            assert!(
                validate_setting_choice("fast", "off", &catalog, &current)
                    .await
                    .is_ok()
            );
            assert!(
                validate_setting_choice("model", "gpt-default", &catalog, &current)
                    .await
                    .is_ok()
            );
        }
        let current = Some("gpt-default".to_owned());
        assert!(
            validate_setting_choice("effort", "low", &catalog, &current)
                .await
                .is_ok()
        );
        assert!(
            validate_setting_choice("fast", "priority", &catalog, &current)
                .await
                .is_ok()
        );
        assert!(
            validate_setting_choice("effort", "high", &catalog, &current)
                .await
                .is_err()
        );
        assert!(
            validate_setting_choice("fast", "standard", &catalog, &current)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn setting_choices_use_current_catalog_without_stale_fallbacks() {
        let catalog = json!({"data":[
            {"id":"gpt-current","isDefault":true,
             "supportedReasoningEfforts":[{"reasoningEffort":"low"}],
             "serviceTiers":[{"id":"priority"}]}
        ]});
        let current = Some("gpt-current".to_owned());

        validate_setting_choice("model", "gpt-current", &catalog, &current)
            .await
            .unwrap();
        validate_setting_choice("effort", "low", &catalog, &current)
            .await
            .unwrap();
        validate_setting_choice("fast", "priority", &catalog, &current)
            .await
            .unwrap();
        assert!(
            validate_setting_choice("model", "gpt-removed", &catalog, &current)
                .await
                .is_err()
        );
        assert!(
            validate_setting_choice("effort", "high", &catalog, &current)
                .await
                .is_err()
        );
        assert!(
            validate_setting_choice("fast", "standard", &catalog, &current)
                .await
                .is_err()
        );
    }

    #[test]
    fn settings_menu_paginates_choices_and_serializes_opaque_button_ids() {
        let choices = (0..45)
            .map(|index| SettingsChoice {
                value: format!("native-model-choice-{index:02}"),
                label: if index == 3 {
                    format!("A long display label {} {}", "label".repeat(18), index)
                } else {
                    format!("Model display {index:02}")
                },
            })
            .collect::<Vec<_>>();
        let menu_for_page = |page| SettingsMenuState {
            claimed: false,
            requester_id: 31,
            connection_generation: 7,
            thread_id: "thread-opaque".to_owned(),
            kind: SettingsKind::Model,
            page,
            choices: choices.clone(),
            current: "Model display 20".to_owned(),
            current_value: Some("native-model-choice-20".to_owned()),
            model_scope: Some("native-model-choice-20".to_owned()),
        };

        for page in 0..3 {
            let menu = menu_for_page(page);
            let (content, rows) =
                render_settings_menu("menu-opaque", &menu, crate::i18n::Language::English);
            assert!(content.contains("Model display 20"));
            assert!(rows.len() <= 5);
            let payload = serde_json::to_value(&rows).unwrap();
            let rows_payload = payload.as_array().unwrap();
            assert!(rows_payload.iter().all(|row| {
                row["components"]
                    .as_array()
                    .is_some_and(|buttons| buttons.len() <= 5)
            }));
            let choice_start = page * SETTINGS_PAGE_SIZE;
            let choice_end = (choice_start + SETTINGS_PAGE_SIZE).min(choices.len());
            let choice_ids = rows_payload
                .iter()
                .flat_map(|row| row["components"].as_array().unwrap())
                .filter_map(|button| button["custom_id"].as_str())
                .filter(|id| id.contains(":c"))
                .collect::<Vec<_>>();
            assert_eq!(choice_ids.len(), choice_end - choice_start);
            if page == 1 {
                let selected = rows_payload
                    .iter()
                    .flat_map(|row| row["components"].as_array().unwrap())
                    .find(|button| button["custom_id"] == "codex:settings:menu-opaque:c20")
                    .unwrap();
                assert_eq!(selected["style"], 3);
            }
            for id in choice_ids {
                assert!(id.len() <= 100);
                assert!(id.starts_with("codex:settings:menu-opaque:c"));
                assert!(!choices.iter().any(|choice| id.contains(&choice.value)));
            }
        }

        let last_page = menu_for_page(2);
        let (content, rows) =
            render_settings_menu("menu-opaque", &last_page, crate::i18n::Language::English);
        assert!(content.contains("Model display 20"));
        assert!(rows.len() <= 5);

        let mut long = menu_for_page(0);
        long.choices[0].value = format!("native-value-{}", "x".repeat(120));
        long.choices[0].label = format!("Display label {}", "y".repeat(100));
        let (content, _) =
            render_settings_menu("menu-opaque", &long, crate::i18n::Language::English);
        assert!(content.contains(&long.choices[0].value));
        assert!(content.contains(&long.choices[0].label));
    }

    #[test]
    fn settings_content_chunks_recombine_without_losing_unicode() {
        let content = "한🙂日本語".repeat(1_200);
        let chunks = split_discord_content(&content);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 2_000));
        assert_eq!(chunks.concat(), content);
    }

    #[test]
    fn generated_commands_use_settings_panels_without_retired_aliases() {
        let commands = definitions(crate::i18n::Language::English);
        let payload = serde_json::to_value(commands).unwrap();
        let names = payload[0]["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|option| option["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "settings",
                "voice",
                "status",
                "interrupt",
                "new",
                "voice-leave",
                "voice-join"
            ]
        );
    }

    fn menu_fixture() -> SettingsMenuState {
        SettingsMenuState {
            claimed: false,
            requester_id: 31,
            connection_generation: 7,
            thread_id: "thread-menu".to_owned(),
            kind: SettingsKind::Hub,
            page: 0,
            choices: vec![],
            current: String::new(),
            current_value: None,
            model_scope: None,
        }
    }

    #[test]
    fn failed_settings_refresh_preserves_clickable_menu() {
        let mut menus = HashMap::from([("old".to_owned(), menu_fixture())]);
        assert!(claim_settings_menu(&mut menus, "old", 31).is_ok());
        assert_eq!(
            claim_settings_menu(&mut menus, "old", 31).unwrap_err(),
            SettingsClaimError::Busy
        );
        release_settings_menu(&mut menus, "old");
        assert!(claim_settings_menu(&mut menus, "old", 31).is_ok());
        assert_eq!(menus.len(), 1);
    }

    #[test]
    fn failed_settings_delivery_keeps_both_possible_visible_versions_usable() {
        let mut menus = HashMap::from([("old".to_owned(), menu_fixture())]);
        claim_settings_menu(&mut menus, "old", 31).unwrap();
        let mut replacement = menu_fixture();
        replacement.kind = SettingsKind::Model;
        replacement.claimed = true;
        menus.insert("new".to_owned(), replacement);
        finish_settings_update(&mut menus, "old", "new", false);
        assert_eq!(
            claim_settings_menu(&mut menus, "old", 31).unwrap().kind,
            SettingsKind::Hub
        );
        assert_eq!(
            claim_settings_menu(&mut menus, "new", 31).unwrap().kind,
            SettingsKind::Model
        );
    }

    #[test]
    fn successful_settings_delivery_retires_only_previous_buttons() {
        let mut menus = HashMap::from([
            ("old".to_owned(), menu_fixture()),
            ("new".to_owned(), menu_fixture()),
        ]);
        finish_settings_update(&mut menus, "old", "new", true);
        assert_eq!(
            claim_settings_menu(&mut menus, "old", 31).unwrap_err(),
            SettingsClaimError::Missing
        );
        assert_eq!(
            claim_settings_menu(&mut menus, "new", 99).unwrap_err(),
            SettingsClaimError::OtherUser
        );
        assert!(claim_settings_menu(&mut menus, "new", 31).is_ok());
    }
}
