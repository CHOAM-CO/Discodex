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
        CreateInteractionResponseMessage, CreateModal, GuildChannel, GuildId, InputTextStyle,
        Member, ModalInteraction, PartialGuild, Permissions, UserId,
    },
    builder::{CreateAllowedMentions, CreateMessage},
};
use uuid::Uuid;

use crate::discord::{BridgeState, PendingApproval, ReplyTarget, StateKey};
use crate::discord_plugins::{
    OAuthCompletion, PluginApp, PluginDetail, PluginInstallTarget, SuggestionDecision,
    begin_oauth_login, install_confirmed, is_plugin_suggestion, list_catalog, list_installed,
    plugin_suggestion, plugin_suggestion_response, read_plugin, suggested_plugin_install_verified,
    suggested_plugin_is_installed, suggested_plugin_target, suggestion_labels, uninstall_confirmed,
    verified_suggestion_accept,
};
use crate::i18n::MessageKey;
use crate::message_access::{MessageAccess, MessagePolicy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsKind {
    Hub,
    Model,
    SubagentModel,
    Effort,
    Fast,
    Permissions,
    ApprovalPolicy,
    Input,
    Voice,
    Updates,
    MessageAccess,
    PairedAccounts,
    Whitelist,
    Blacklist,
    RegisteredChannels,
    NativeFeatures,
    Antigravity,
    AntigravityAutoApprove,
    AntigravitySetting(AgySettingKey),
    NativeFeature(NativeFeatureKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgySettingKey {
    ToolPermission,
    ArtifactReview,
    TerminalSandbox,
    NonWorkspaceAccess,
    Allow,
    Ask,
    Deny,
}

impl AgySettingKey {
    const ALL: [Self; 7] = [
        Self::ToolPermission,
        Self::ArtifactReview,
        Self::TerminalSandbox,
        Self::NonWorkspaceAccess,
        Self::Allow,
        Self::Ask,
        Self::Deny,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::ToolPermission => "tool-permission",
            Self::ArtifactReview => "artifact-review",
            Self::TerminalSandbox => "sandbox",
            Self::NonWorkspaceAccess => "non-workspace",
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::ToolPermission => "toolPermission",
            Self::ArtifactReview => "artifactReviewPolicy",
            Self::TerminalSandbox => "enableTerminalSandbox",
            Self::NonWorkspaceAccess => "allowNonWorkspaceAccess",
            Self::Allow => "/permissions/allow",
            Self::Ask => "/permissions/ask",
            Self::Deny => "/permissions/deny",
        }
    }

    fn label(self, language: crate::i18n::Language) -> &'static str {
        language.message(match self {
            Self::ToolPermission => MessageKey::CommandsAgyToolPermission,
            Self::ArtifactReview => MessageKey::CommandsAgyArtifactReview,
            Self::TerminalSandbox => MessageKey::CommandsAgyTerminalSandbox,
            Self::NonWorkspaceAccess => MessageKey::CommandsAgyNonWorkspaceAccess,
            Self::Allow => MessageKey::CommandsAgyAllowRules,
            Self::Ask => MessageKey::CommandsAgyAskRules,
            Self::Deny => MessageKey::CommandsAgyDenyRules,
        })
    }

    fn is_rule_list(self) -> bool {
        matches!(self, Self::Allow | Self::Ask | Self::Deny)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeFeatureKey {
    CodeMode,
    CodeModeHost,
    MultiAgentV2,
    WaitAgent,
    AsyncNotifications,
    RequestUserInput,
    Clock,
    TokenBudget,
    HistoryNotes,
    SubagentModel,
}

impl NativeFeatureKey {
    const ALL: [Self; 9] = [
        Self::CodeMode,
        Self::CodeModeHost,
        Self::MultiAgentV2,
        Self::WaitAgent,
        Self::AsyncNotifications,
        Self::RequestUserInput,
        Self::Clock,
        Self::TokenBudget,
        Self::HistoryNotes,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::CodeMode => "codeMode",
            Self::CodeModeHost => "codeModeHost",
            Self::MultiAgentV2 => "multiAgentV2",
            Self::WaitAgent => "waitAgent",
            Self::AsyncNotifications => "asyncNotifications",
            Self::RequestUserInput => "requestUserInput",
            Self::Clock => "clock",
            Self::TokenBudget => "tokenBudget",
            Self::HistoryNotes => "historyNotes",
            Self::SubagentModel => "subagentModel",
        }
    }

    fn from_key(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.key() == value)
    }

    fn label(self, language: crate::i18n::Language) -> &'static str {
        match self {
            Self::CodeMode => language.message(MessageKey::CommandsCodeMode004c354e),
            Self::CodeModeHost => language.message(MessageKey::CommandsCodeModeHost4cb2e89d),
            Self::MultiAgentV2 => language.message(MessageKey::CommandsMultiAgentV26dd758e9),
            Self::WaitAgent => language.message(MessageKey::CommandsWaitForAgents032f9048),
            Self::AsyncNotifications => {
                language.message(MessageKey::CommandsAsyncNotifications0c673297)
            }
            Self::RequestUserInput => language.message(MessageKey::CommandsNativeQuestionsD61baa04),
            Self::Clock => language.message(MessageKey::CommandsClockAndTimeReminders4246b655),
            Self::TokenBudget => language.message(MessageKey::CommandsTokenBudget6a6282bf),
            Self::HistoryNotes => language.message(MessageKey::CommandsHistoryNotes234724c3),
            Self::SubagentModel => {
                language.message(MessageKey::CommandsDefaultSubagentModel9811e108)
            }
        }
    }

    fn config_paths(self) -> &'static [&'static str] {
        match self {
            Self::CodeMode => &["features.code_mode.enabled"],
            Self::CodeModeHost => &["features.code_mode_host.enabled"],
            Self::MultiAgentV2 => &["features.multi_agent_v2.enabled"],
            Self::WaitAgent => &["features.multi_agent_v2.wait_agent_enabled"],
            Self::AsyncNotifications => &["features.send_message_to_user_async"],
            Self::RequestUserInput => &[
                "tools.experimental_request_user_input.enabled",
                "features.default_mode_request_user_input",
            ],
            Self::Clock => &["features.current_time_reminder.enabled"],
            Self::TokenBudget => &["features.token_budget.enabled"],
            Self::HistoryNotes => &["features.token_budget.use_history_notes_extension"],
            Self::SubagentModel => &[
                "agents.default_subagent_model",
                "agents.default_subagent_model_provider",
                "agents.default_subagent_executor",
            ],
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PluginMenuMode {
    Catalog,
    Installed,
}

#[derive(Clone, Debug)]
pub(crate) enum PluginMenuEntry {
    Install(PluginInstallTarget),
    Uninstall {
        plugin_id: String,
        display_name: String,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct PluginMenuState {
    pub claimed: bool,
    pub requester_id: u64,
    pub connection_generation: u64,
    pub thread_id: String,
    pub mode: PluginMenuMode,
    pub page: usize,
    pub entries: Vec<PluginMenuEntry>,
    pub selected: Option<PluginMenuEntry>,
    pub selected_detail: Option<PluginDetail>,
    pub apps_needing_auth: Vec<PluginApp>,
    pub oauth_servers: Vec<String>,
    pub auth_view: bool,
    pub install_notice: Option<String>,
}

pub fn definitions(language: crate::i18n::Language) -> Vec<CreateCommand> {
    let mut command = CreateCommand::new("discodex").description(
        language.message(MessageKey::CommandsManageTheCurrentDiscodexConversationAndSet831a7659),
    );
    command = command
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "update",
            language.message(MessageKey::CommandsCheckForTheLatestDiscodexUpdateAndConfirm68b53c7f),
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "settings",
            language.message(MessageKey::CommandsChooseConversationSettingsWithButtons70876a9a),
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "voice",
            language.message(MessageKey::CommandsChooseAVoiceProfileWithButtons89280fb4),
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "status",
            language.message(MessageKey::CommandsShowConversationConnectionAndSettings181cc827),
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "interrupt",
            language.message(MessageKey::CommandsStopTheCurrentResponseCa339947),
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "compact",
            language
                .message(MessageKey::CommandsCompactConversationContextReplacingAnyActiv6fcab920),
        ))
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "new",
                language.message(MessageKey::CommandsStartANewCodexConversation0a33cf04),
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "active-turn",
                    language.message(MessageKey::CommandsHowToHandleTheActiveResponse0bfb7119),
                )
                .required(false)
                .add_string_choice(
                    language.message(MessageKey::CommandsFinishThenStartANewConversationB9003cb8),
                    "finish",
                )
                .add_string_choice(
                    language.message(MessageKey::CommandsInterruptFirst84068a6d),
                    "interrupt",
                ),
            ),
        )
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "voice-leave",
            language.message(MessageKey::CommandsLeaveTheCurrentVoiceCall04af42c5),
        ))
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "voice-join",
                language.message(MessageKey::CommandsJoinAnAllowedVoiceChannelC1945831),
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::Channel,
                    "channel",
                    language.message(MessageKey::CommandsAllowedVoiceChannelToJoin4e5d0bf6),
                )
                .required(true),
            ),
        );
    command = command.add_option(
        CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "plugins",
            language.message(MessageKey::CommandsBrowseInstallOrRemoveNativeCodexPlugins7a3ef886),
        )
        .add_sub_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "view",
                language.message(MessageKey::CommandsCatalogOrInstalledPluginsD3704ecb),
            )
            .required(false)
            .add_string_choice(
                language.message(MessageKey::CommandsCatalogFe5df316),
                "catalog",
            )
            .add_string_choice(
                language.message(MessageKey::CommandsInstalledA4c359ee),
                "installed",
            ),
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
        let detail = crate::discord::sanitize_discord_error(&state.config, &format!("{error:#}"));
        let chunks = split_discord_content(&detail);
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
    if interaction
        .data
        .options
        .first()
        .is_some_and(|sub| sub.name == "update")
    {
        let (content, rows) =
            match crate::discord_updates::manual_update(&state, interaction.user.id.get()).await {
                Ok(response) => response,
                Err(error) => {
                    let detail =
                        settings_error_detail(&error, state.config.language, Some(&state.config));
                    tracing::warn!(error = %detail, "could not check manual update");
                    (
                        localized_failure_message(
                            state.config.language.message(
                                MessageKey::CommandsCouldNotCheckForUpdatesPleaseTryAgainAf2aa53e,
                            ),
                            &detail,
                        ),
                        Vec::new(),
                    )
                }
            };
        interaction
            .edit_response(
                ctx,
                serenity::builder::EditInteractionResponse::new()
                    .content(content)
                    .components(rows)
                    .allowed_mentions(no_mentions()),
            )
            .await?;
        return Ok(());
    }
    if settings_ui_requested(interaction) {
        let response = match settings_menu_from_command(ctx, interaction, &state).await {
            Ok((content, rows)) => {
                send_command_settings_panel(ctx, interaction, &content, rows).await
            }
            Err(error) => {
                let detail =
                    settings_error_detail(&error, state.config.language, Some(&state.config));
                tracing::warn!(error = %detail, "failed to build Discord settings menu");
                interaction
                    .edit_response(
                        ctx,
                        serenity::builder::EditInteractionResponse::new()
                            .content(localized_failure_message(
                            state.config.language.message(
                                MessageKey::CommandsCouldNotLoadTheCurrentSettingsChoices0a0afc4d,
                            ),
                            &detail,
                        )),
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
    if plugins_ui_requested(interaction) {
        let response = match plugin_menu_from_command(interaction, &state).await {
            Ok((id, menu)) => {
                let rendered = render_plugin_menu(&id, &menu, state.config.language);
                let current_generation = state.native.current_generation();
                let current_thread_id = state.native.thread_id();
                let mut menus = state.plugin_menus.lock().await;
                menus.retain(|_, previous| {
                    previous.connection_generation == current_generation
                        && previous.thread_id == current_thread_id
                        && previous.requester_id != interaction.user.id.get()
                });
                menus.insert(id, menu);
                drop(menus);
                send_command_settings_panel(ctx, interaction, &rendered.0, rendered.1).await
            }
            Err(error) => {
                let detail =
                    settings_error_detail(&error, state.config.language, Some(&state.config));
                tracing::warn!(error = %detail, "failed to build Discord plugin menu");
                interaction.edit_response(ctx, serenity::builder::EditInteractionResponse::new().content(
                    localized_failure_message(
                        state.config.language.message(MessageKey::CommandsCouldNotLoadTheNativePluginListCheckCodeC2348bb5),
                        &detail,
                    ),
                )).await.map(|_| ())
            }
        };
        response?;
        return Ok(());
    }

    let content = match handle_authorized(ctx, interaction, state.clone()).await {
        Ok(content) => content,
        Err(error) => {
            let detail = settings_error_detail(&error, state.config.language, Some(&state.config));
            tracing::warn!(error = %detail, "Discord Codex command failed");
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
            localized_failure_message(
                state
                    .config
                    .language
                    .message(MessageKey::CommandsTheDiscodexRequestFailedCheckTheInputAnd71994868),
                &detail,
            )
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
            .message(MessageKey::CommandsUseDiscodexStatus29fe8772)
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
                        language
                            .message(MessageKey::CommandsCurrentCodexConversationStatus3da97baf),
                        language.message(MessageKey::CommandsVoiceCall7fee09b5),
                        if state
                            .voice
                            .is_active(GuildId::new(state.config.guild_id))
                            .await
                        {
                            language.message(MessageKey::CommandsConnected1881bcc7)
                        } else {
                            language.message(MessageKey::CommandsNotConnected97df72d5)
                        },
                        language.message(MessageKey::CommandsSettings2dd4ab86),
                        format_settings_with_language(&settings_snapshot(&value), language)
                    )
                }
                Err(error) => {
                    tracing::warn!(error = ?error, "failed to refresh native thread state for Discord status");
                    output = language
                        .message(
                            MessageKey::CommandsCouldNotReadTheCurrentConversationStatusA4f36a29,
                        )
                        .to_owned()
                }
            }
            output.push_str(&format!(
                "\n{}: {}",
                language.message(MessageKey::CommandsInputDelivery47ae354d),
                input_mode(*state.input_mode.read().await, language)
            ));
            output.push_str(&format!(
                "\n{}: {}",
                language.message(MessageKey::CommandsActiveResponse007ef48b),
                if state.current_turn.lock().await.is_some() {
                    language.message(MessageKey::CommandsYes528feda9)
                } else {
                    language.message(MessageKey::CommandsNoA97f680d)
                }
            ));
            output.push_str(&format!(
                "\n{}: {}",
                language.message(MessageKey::CommandsQueuedInputs61de5fed),
                crate::discord::pending_input_count(&*state.queued_inputs.lock().await)
            ));
            if let Some(checks) = &state.installation_checks {
                output.push_str(&format!(
                    "\n{}\n",
                    language.message(MessageKey::CommandsInstallationFeatureChecksC6ab7b32)
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
                .message(MessageKey::CommandsAskedTheCurrentResponseToStopCc19104e)
                .to_owned();
        }
        "compact" => {
            let _settings_change = state.settings_change_lock.lock().await;
            output = request_compaction_with_config(
                &state.native.thread_id(),
                language,
                Some(&state.config),
                |method, params| state.native.request(method, params),
            )
            .await;
        }
        "new" => {
            let active_turn = state.current_turn.lock().await.clone();
            let expected_active_turn = active_turn.clone();
            let active_choice = arg_string();
            if active_turn.is_some() && active_choice.is_none() {
                bail!(
                    language.message(
                        MessageKey::CommandsAResponseIsInProgressChooseHowToHandleID71652a8
                    )
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
                        output.push_str(
                            language.message(
                                MessageKey::CommandsAskedTheExistingResponseToStop31ecef20,
                            ),
                        );
                    }
                    Some("finish") => output.push_str(language.message(
                        MessageKey::CommandsTheExistingResponseWillContinueInThePrevi758e7673,
                    )),
                    _ => {
                        bail!(language.message(
                            MessageKey::CommandsChooseHowToHandleTheActiveResponse8f647f6b
                        ))
                    }
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
                bail!(
                    language.message(
                        MessageKey::CommandsWaitForTheVoiceCallToEndBeforeStartingA051a4271
                    )
                );
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
                bail!(language.message(
                    MessageKey::CommandsWaitForQueuedMessagesInThisConversationTo64053425
                ));
            }
            // Input observation must not pair the new thread with the previous turn.
            let mut current = state.current_turn.lock().await;
            if current
                .as_ref()
                .is_some_and(|turn| Some(turn) != expected_active_turn.as_ref())
            {
                bail!(
                    language.message(
                        MessageKey::CommandsAQueuedMessageHasStartedChooseAgainHowTo117bdf62
                    )
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
            output.push_str(
                language.message(MessageKey::CommandsStartedANewCodexConversation5b85b8f3),
            );
        }
        "voice-join" => {
            let channel_id = arg
                .and_then(option_channel)
                .ok_or_else(|| anyhow!("음성 채널을 지정하세요"))?;
            if !state.config.voice_channel_ids.contains(&channel_id.get()) {
                bail!(
                    language.message(MessageKey::CommandsThisVoiceChannelIsNotAllowlisted62b54e80)
                );
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
                language.message(MessageKey::CommandsJoinedVoiceChannelB6c9416a),
                channel_id.get(),
                language.message(MessageKey::CommandsMessage3a07098d)
            );
        }
        "voice-leave" => {
            state
                .voice
                .leave(GuildId::new(state.config.guild_id))
                .await?;
            output = language
                .message(MessageKey::CommandsLeftTheVoiceCallFb14decf)
                .to_owned();
        }
        _ => bail!(
            "{}: {sub}",
            language.message(MessageKey::CommandsUnknownDiscodexCommandC126ecfd)
        ),
    }
    Ok(output)
}

async fn request_compaction<F, Fut>(
    thread_id: &str,
    language: crate::i18n::Language,
    request: F,
) -> String
where
    F: FnOnce(&'static str, Value) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Value>>,
{
    request_compaction_with_config(thread_id, language, None, request).await
}

async fn request_compaction_with_config<F, Fut>(
    thread_id: &str,
    language: crate::i18n::Language,
    config: Option<&crate::config::Config>,
    request: F,
) -> String
where
    F: FnOnce(&'static str, Value) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Value>>,
{
    match request("thread/compact/start", json!({ "threadId": thread_id })).await {
        Ok(_) => language
            .message(MessageKey::CommandsRequestedCompactionOfTheCurrentConversationEfa9704c)
            .to_owned(),
        Err(error) => {
            let detail = settings_error_detail(&error, language, config);
            tracing::warn!(error = %detail, "could not request native conversation compaction");
            format!(
                "{}\n{detail}",
                language.message(MessageKey::CommandsCouldNotRequestConversationCompaction092a77b5)
            )
        }
    }
}

const SETTINGS_PAGE_SIZE: usize = 20;
const MESSAGE_ACCESS_PAGE_SIZE: usize = 10;

fn settings_page_size(kind: SettingsKind) -> usize {
    if is_access_list(kind) {
        MESSAGE_ACCESS_PAGE_SIZE
    } else {
        SETTINGS_PAGE_SIZE
    }
}

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
    ctx: &Context,
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
        ctx,
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
    ctx: &Context,
    state: &BridgeState,
    kind: SettingsKind,
    page: usize,
    requester_id: u64,
    connection_generation: u64,
) -> anyhow::Result<SettingsMenuState> {
    build_settings_menu_with_catalog(
        ctx,
        state,
        kind,
        page,
        requester_id,
        connection_generation,
        None,
    )
    .await
}

fn settings_kind_uses_model_catalog(kind: SettingsKind) -> bool {
    matches!(
        kind,
        SettingsKind::Model
            | SettingsKind::SubagentModel
            | SettingsKind::Effort
            | SettingsKind::Fast
            | SettingsKind::NativeFeature(NativeFeatureKey::SubagentModel)
    )
}

async fn build_settings_menu_with_catalog(
    ctx: &Context,
    state: &BridgeState,
    kind: SettingsKind,
    page: usize,
    requester_id: u64,
    connection_generation: u64,
    model_catalog: Option<&Value>,
) -> anyhow::Result<SettingsMenuState> {
    let language = state.config.language;
    if state.native.current_generation() != connection_generation {
        bail!("Codex connection changed while settings were opening");
    }
    let target_thread_id = state.native.thread_id();
    let thread = if kind == SettingsKind::MessageAccess
        || is_access_list(kind)
        || matches!(
            kind,
            SettingsKind::Antigravity
                | SettingsKind::AntigravityAutoApprove
                | SettingsKind::AntigravitySetting(_)
        ) {
        json!({})
    } else {
        state.native.refresh_thread_state().await?
    };
    let model_scope = thread_model_scope(&thread);
    let model_catalog = match model_catalog {
        Some(catalog) => Some(catalog.clone()),
        None if settings_kind_uses_model_catalog(kind) => Some(state.native.model_catalog().await?),
        None => None,
    };
    let (current_value, current, choices) = match kind {
        SettingsKind::Hub => settings_home_values(
            &thread,
            *state.input_mode.read().await,
            state.voice.selected_voice()?,
            language,
        ),
        SettingsKind::SubagentModel => {
            let snapshot = state.native.read_native_feature_snapshot().await?;
            let catalog = model_catalog.as_ref().ok_or_else(|| {
                anyhow!(language.message(MessageKey::CommandsModelCatalogUnavailable))
            })?;
            subagent_model_settings_values(&thread, &snapshot, &catalog, language)
        }
        SettingsKind::NativeFeatures => {
            advanced_settings_values(&state.native.read_native_feature_preferences()?, language)
        }
        SettingsKind::Antigravity => {
            let preferences = state.native.read_native_feature_preferences()?;
            antigravity_settings_values(&preferences, language)
        }
        SettingsKind::AntigravityAutoApprove => {
            let preferences = state.native.read_native_feature_preferences()?;
            (
                Some(
                    if preferences.antigravity_auto_approve {
                        "on"
                    } else {
                        "off"
                    }
                    .to_owned(),
                ),
                language
                    .message(MessageKey::CommandsAgyAutoApproveDescription)
                    .to_owned(),
                vec![
                    SettingsChoice {
                        value: "on".to_owned(),
                        label: language.message(MessageKey::CommandsOn65845774).to_owned(),
                    },
                    SettingsChoice {
                        value: "off".to_owned(),
                        label: language.message(MessageKey::CommandsOff5677591d).to_owned(),
                    },
                ],
            )
        }
        SettingsKind::AntigravitySetting(key) => {
            let preferences = state.native.read_native_feature_preferences()?;
            let settings = crate::agy_settings::read(
                &state.config,
                preferences.antigravity_share_settings.unwrap_or(false),
            )?;
            agy_setting_values(key, &settings, language)?
        }
        SettingsKind::NativeFeature(key) => {
            let snapshot = state.native.read_native_feature_snapshot().await?;
            let saved = native_feature_saved_value(&snapshot.preferences, key);
            let effective = effective_native_feature_text(&snapshot.native_config, key, language);
            let subagent_catalog = if key == NativeFeatureKey::SubagentModel {
                model_catalog.as_ref()
            } else {
                None
            };
            let choices = if let Some(catalog) = subagent_catalog {
                subagent_model_choices(catalog, language)
            } else {
                vec![
                    SettingsChoice {
                        value: "on".to_owned(),
                        label: language.message(MessageKey::CommandsOn65845774).to_owned(),
                    },
                    SettingsChoice {
                        value: "off".to_owned(),
                        label: language.message(MessageKey::CommandsOff5677591d).to_owned(),
                    },
                    SettingsChoice {
                        value: "native-default".to_owned(),
                        label: language
                            .message(MessageKey::CommandsNativeDefault308250fc)
                            .to_owned(),
                    },
                ]
            };
            let current_value = if key == NativeFeatureKey::SubagentModel {
                subagent_catalog
                    .and_then(|catalog| {
                        subagent_preference_scope(&snapshot.preferences, catalog).and_then(
                            |value| normalized_model_value_with_language(catalog, &value, language),
                        )
                    })
                    .or_else(|| saved.clone())
                    .unwrap_or_else(|| "native-default".to_owned())
            } else {
                match saved.as_deref() {
                    Some("true") => "on".to_owned(),
                    Some("false") => "off".to_owned(),
                    _ => "native-default".to_owned(),
                }
            };
            let selected = choices.iter().find(|choice| choice.value == current_value);
            let desired = selected
                .map(|choice| choice.label.as_str())
                .unwrap_or(current_value.as_str());
            let current = format!(
                "{}: {}\n{}: {}\n{}",
                language.message(MessageKey::CommandsSavedPreference47ce2cf1),
                desired,
                language.message(MessageKey::CommandsEffectiveCodexConfigEeb899f3),
                effective,
                if key == NativeFeatureKey::SubagentModel {
                    child_default_timing(language)
                } else {
                    language
                        .message(MessageKey::CommandsRestartRequiredToApplyThisPreference4ac207b7)
                }
            );
            (Some(current_value), current, choices)
        }
        SettingsKind::MessageAccess => {
            let access = state.message_access.read().await.clone();
            let selected = format!("policy:{}", message_policy_key(access.policy));
            let current = format!(
                "{}: {}\n{}: {} · {}: {} · {}: {} · {}: {}",
                language.message(MessageKey::CommandsCurrentPolicy7ed3f9e2),
                message_policy_label(access.policy, language),
                language.message(MessageKey::CommandsPairedDb2c2b0b),
                access.paired_user_ids.len(),
                language.message(MessageKey::CommandsWhitelist4df98d9d),
                access.whitelist.len(),
                language.message(MessageKey::CommandsBlacklistD5332ba7),
                access.blacklist.len(),
                language.message(MessageKey::CommandsRegisteredChannels5a818eab),
                access.channel_ids.len()
            );
            let choices = [
                (
                    "policy:everyone",
                    message_policy_label(MessagePolicy::Everyone, language),
                ),
                (
                    "policy:registered-server",
                    message_policy_label(MessagePolicy::RegisteredServer, language),
                ),
                (
                    "policy:registered-channels",
                    message_policy_label(MessagePolicy::RegisteredChannels, language),
                ),
                (
                    "policy:paired",
                    message_policy_label(MessagePolicy::Paired, language),
                ),
                (
                    "list:paired",
                    language.message(MessageKey::CommandsEditPairedAccountsAceb36c6),
                ),
                (
                    "list:whitelist",
                    language.message(MessageKey::CommandsEditWhitelist82750902),
                ),
                (
                    "list:blacklist",
                    language.message(MessageKey::CommandsEditBlocklistF37f514b),
                ),
                (
                    "list:channels",
                    language.message(MessageKey::CommandsEditRegisteredChannelsCca7da5c),
                ),
            ]
            .into_iter()
            .map(|(value, label)| SettingsChoice {
                value: value.to_owned(),
                label: label.to_owned(),
            })
            .collect();
            (Some(selected.to_owned()), current, choices)
        }
        SettingsKind::PairedAccounts
        | SettingsKind::Whitelist
        | SettingsKind::Blacklist
        | SettingsKind::RegisteredChannels => {
            let access = state.message_access.read().await.clone();
            let ids = access_list(&access, kind).to_vec();
            let page = if ids.is_empty() {
                0
            } else {
                page.min((ids.len() - 1) / MESSAGE_ACCESS_PAGE_SIZE)
            };
            let start = page * MESSAGE_ACCESS_PAGE_SIZE;
            let end = (start + MESSAGE_ACCESS_PAGE_SIZE).min(ids.len());
            let mut choices = ids
                .iter()
                .map(|id| SettingsChoice {
                    value: id.to_string(),
                    label: String::new(),
                })
                .collect::<Vec<_>>();
            for (index, id) in ids.get(start..end).unwrap_or_default().iter().enumerate() {
                let label = if kind == SettingsKind::RegisteredChannels {
                    match ctx.http.get_channel(ChannelId::new(*id)).await {
                        Ok(channel) => channel
                            .guild()
                            .map(|channel| channel.name.clone())
                            .unwrap_or_else(|| {
                                language
                                    .message(MessageKey::CommandsUnavailable9a54161b)
                                    .to_owned()
                            }),
                        Err(_) => language
                            .message(MessageKey::CommandsUnavailableInThisServerEb964050)
                            .to_owned(),
                    }
                } else {
                    account_display_name(
                        &ctx.http,
                        GuildId::new(state.config.guild_id),
                        UserId::new(*id),
                        language,
                    )
                    .await
                };
                choices[start + index].label = label;
            }
            let current = format!(
                "{}: {}",
                language.message(MessageKey::CommandsEntries9281e703),
                ids.len()
            );
            (None, current, choices)
        }
        SettingsKind::Updates => {
            let muted = state.updates.muted().await;
            let choices = [
                ("on", language.message(MessageKey::CommandsOn65845774)),
                ("off", language.message(MessageKey::CommandsOff5677591d)),
            ]
            .into_iter()
            .map(|(value, label)| SettingsChoice {
                value: value.to_owned(),
                label: label.to_owned(),
            })
            .collect::<Vec<_>>();
            let value = if muted { "off" } else { "on" };
            let current = choice_current_label(&choices, Some(value), language);
            (Some(value.to_owned()), current, choices)
        }
        SettingsKind::Model => {
            let catalog = model_catalog.as_ref().ok_or_else(|| {
                anyhow!(language.message(MessageKey::CommandsModelCatalogUnavailable))
            })?;
            let choices = catalog_model_choices(&catalog);
            let selected = model_scope
                .as_deref()
                .and_then(|value| normalized_model_value_with_language(&catalog, value, language));
            let provider = thread_model_provider(&thread)
                .unwrap_or_else(|| language.message(MessageKey::CommandsModelProviderUnknown));
            let current = format!(
                "{}\n{}: {provider}",
                choice_current_label(&choices, selected.as_deref(), language),
                language.message(MessageKey::CommandsActiveModelProviderEa54b5ed)
            );
            (selected, model_catalog_warnings(current, &catalog), choices)
        }
        SettingsKind::Effort | SettingsKind::Fast => {
            let catalog = model_catalog.as_ref().ok_or_else(|| {
                anyhow!(language.message(MessageKey::CommandsModelCatalogUnavailable))
            })?;
            let model = selected_model_with_language(&catalog, &model_scope, language)?;
            if kind == SettingsKind::Effort {
                let choices = model_effort_settings_choices(model, language);
                let selected = thread
                    .get("effort")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        thread
                            .get("model")
                            .and_then(Value::as_str)
                            .and_then(|id| agy_variant_effort(model, id))
                    });
                let effort_value = selected.as_ref().map(|effort| json!(effort));
                let current = effort_current_label(&choices, effort_value.as_ref(), language);
                (selected, model_catalog_warnings(current, &catalog), choices)
            } else {
                let mut choices = vec![SettingsChoice {
                    value: "off".to_owned(),
                    label: language.message(MessageKey::CommandsOff4d374cce).to_owned(),
                }];
                choices.extend(
                    model
                        .get("serviceTiers")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|item| service_tier_settings_choice(item, language)),
                );
                let selected = thread
                    .get("serviceTier")
                    .filter(|value| !value.is_null())
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| "off".to_owned());
                let current = choice_current_label(&choices, Some(&selected), language);
                (
                    Some(selected),
                    model_catalog_warnings(current, &catalog),
                    choices,
                )
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
        SettingsKind::ApprovalPolicy => {
            let requirements = state
                .native
                .request("configRequirements/read", json!({}))
                .await?;
            let saved = state
                .native
                .read_native_feature_preferences()?
                .approval_policy;
            approval_policy_settings_values(&thread, &saved, &requirements, language)
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
                .message(MessageKey::CommandsCodexDefault3029befd)
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
        page.min((choices.len() - 1) / settings_page_size(kind))
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

fn effort_settings_choice(item: &Value, language: crate::i18n::Language) -> Option<SettingsChoice> {
    let value = item
        .get("reasoningEffort")
        .and_then(Value::as_str)
        .or_else(|| item.as_str())?;
    let label = match value {
        "none" => language.message(MessageKey::CommandsNone45e9a015),
        "minimal" => language.message(MessageKey::CommandsMinimalB4fec1ec),
        "low" => language.message(MessageKey::CommandsLowCbc3febe),
        "medium" => language.message(MessageKey::CommandsMediumEd9c6849),
        "high" => language.message(MessageKey::CommandsHigh454ac2f1),
        "xhigh" => language.message(MessageKey::CommandsExtraHighFd13615e),
        "max" => language.message(MessageKey::CommandsMaximumAe40202b),
        "ultra" => language.message(MessageKey::CommandsUltra0f57cfc1),
        _ => value,
    };
    Some(SettingsChoice {
        value: value.to_owned(),
        label: label.to_owned(),
    })
}

fn model_effort_settings_choice(
    item: &Value,
    language: crate::i18n::Language,
) -> Option<SettingsChoice> {
    let mut choice = effort_settings_choice(item, language)?;
    if item.get("agyOrchestration").and_then(Value::as_bool) == Some(true) {
        choice.label = if let Some(actual) = item.get("agyEffort").and_then(Value::as_str) {
            language
                .message(MessageKey::CommandsAgyUltraEffortAliasLabel)
                .replace(
                    "{actual}",
                    &effort_settings_choice(&json!(actual), language)?.label,
                )
        } else {
            language
                .message(MessageKey::CommandsUltraCodexProactiveOrchestrationAgyDefault5b74f52a)
                .to_owned()
        };
    } else if let Some(actual) = item.get("agyEffort").and_then(Value::as_str) {
        choice.label = language
            .message(MessageKey::CommandsAgyEffortAliasLabel)
            .replace(
                "{actual}",
                &effort_settings_choice(&json!(actual), language)?.label,
            );
    }
    Some(choice)
}

fn model_effort_settings_choices(
    model: &Value,
    language: crate::i18n::Language,
) -> Vec<SettingsChoice> {
    let mut items = model
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let aliases = model
        .get("reasoningEffortAliases")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for alias in aliases {
        let (Some(requested), Some(actual)) = (
            alias.get("requested").and_then(Value::as_str),
            alias.get("actual").and_then(Value::as_str),
        ) else {
            continue;
        };
        if items.iter().any(|item| {
            item.get("reasoningEffort")
                .and_then(Value::as_str)
                .or_else(|| item.as_str())
                == Some(requested)
        }) {
            continue;
        }
        let alias_item = json!({
            "reasoningEffort": requested,
            "agyEffort": actual,
            "source": "agy-effort-alias"
        });
        let insert_at = items
            .iter()
            .position(|item| {
                item.get("reasoningEffort")
                    .and_then(Value::as_str)
                    .or_else(|| item.as_str())
                    == Some(actual)
            })
            .map(|index| index + 1)
            .unwrap_or(items.len());
        items.insert(insert_at, alias_item);
    }
    items
        .iter()
        .filter_map(|item| model_effort_settings_choice(item, language))
        .collect()
}

fn agy_variant_effort(model: &Value, model_id: &str) -> Option<String> {
    model
        .get("agyVariants")
        .and_then(Value::as_array)?
        .iter()
        .find(|variant| variant.get("id").and_then(Value::as_str) == Some(model_id))?
        .get("effort")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn service_tier_settings_choice(
    item: &Value,
    language: crate::i18n::Language,
) -> Option<SettingsChoice> {
    let value = item
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| item.as_str())?;
    let label = match value {
        "priority" => language.message(MessageKey::CommandsPriorityE9c80c39),
        "standard" => language.message(MessageKey::CommandsStandard0dd9d07b),
        "flex" => language.message(MessageKey::CommandsFlex0f24853f),
        _ => item.get("name").and_then(Value::as_str).unwrap_or(value),
    };
    Some(SettingsChoice {
        value: value.to_owned(),
        label: label.to_owned(),
    })
}

fn effort_current_label(
    choices: &[SettingsChoice],
    effort: Option<&Value>,
    language: crate::i18n::Language,
) -> String {
    // A null native value can be unset or unavailable; do not invent a default.
    choice_current_label(choices, effort.and_then(Value::as_str), language)
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
                .message(MessageKey::CommandsNotReported0a1fa855)
                .to_owned()
        })
}

fn model_catalog_warnings(mut current: String, catalog: &Value) -> String {
    for warning in catalog
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        current.push('\n');
        current.push_str(warning);
    }
    current
}

fn settings_home_values(
    thread: &Value,
    mode: crate::config::InputMode,
    selected_voice: Option<String>,
    language: crate::i18n::Language,
) -> (Option<String>, String, Vec<SettingsChoice>) {
    let mut current = format_settings_with_language(&settings_snapshot(thread), language);
    current.push_str(&format!(
        "\n{}: {}",
        language.message(MessageKey::CommandsInputDelivery47ae354d),
        input_mode(mode, language)
    ));
    if let Some(provider) = thread_model_provider(thread) {
        current.push_str(&format!(
            "\n{}: {provider}",
            language.message(MessageKey::CommandsActiveModelProviderEa54b5ed)
        ));
    }
    let voice = selected_voice.unwrap_or_else(|| {
        language
            .message(MessageKey::CommandsCodexDefault3029befd)
            .to_owned()
    });
    current.push_str(&format!(
        "\n{}: {voice}",
        language.message(MessageKey::CommandsVoiceProfile2f25af45)
    ));
    let choices = [
        ("model", MessageKey::CommandsSettingsHomeModel),
        (
            "subagent-model",
            MessageKey::CommandsSettingsHomeSubagentModel,
        ),
        ("effort", MessageKey::CommandsSettingsHomeEffort),
        ("fast", MessageKey::CommandsSettingsHomeFast),
        ("updates", MessageKey::CommandsSettingsHomeUpdates),
        ("permissions", MessageKey::CommandsSettingsHomePermissions),
        (
            "approval-policy",
            MessageKey::CommandsSettingsHomeApprovalPolicy,
        ),
        ("input", MessageKey::CommandsSettingsHomeInput),
        (
            "message-access",
            MessageKey::CommandsSettingsHomeMessageAccess,
        ),
        ("voice", MessageKey::CommandsSettingsHomeVoice),
        ("antigravity", MessageKey::CommandsAgyTitle),
        (
            "native-features",
            MessageKey::CommandsSettingsHomeNativeFeatures,
        ),
    ]
    .into_iter()
    .map(|(value, label)| SettingsChoice {
        value: value.to_owned(),
        label: language.message(label).to_owned(),
    })
    .collect();
    (None, current, choices)
}

fn subagent_model_settings_values(
    thread: &Value,
    snapshot: &crate::native::NativeFeatureSnapshot,
    catalog: &Value,
    language: crate::i18n::Language,
) -> (Option<String>, String, Vec<SettingsChoice>) {
    let saved = snapshot.preferences.subagent_model.clone();
    let effective = effective_native_feature_text(
        &snapshot.native_config,
        NativeFeatureKey::SubagentModel,
        language,
    );
    let selected = subagent_preference_scope(&snapshot.preferences, catalog)
        .and_then(|value| normalized_model_value_with_language(catalog, &value, language));
    let saved_label = saved.as_ref().map(|model| {
        format!(
            "{} — {} / {}",
            model,
            snapshot
                .preferences
                .subagent_model_executor
                .as_deref()
                .unwrap_or("codex"),
            snapshot
                .preferences
                .subagent_model_provider
                .as_deref()
                .unwrap_or_else(|| language.message(MessageKey::CommandsNativeDefault308250fc)),
        )
    });
    let current = format!(
        "{}: {}\n{}: {}\n{}: {}\n{}",
        language.message(MessageKey::CommandsSavedPreference47ce2cf1),
        saved_label
            .as_deref()
            .unwrap_or_else(|| language.message(MessageKey::CommandsNativeDefault308250fc)),
        language.message(MessageKey::CommandsActiveModelProviderEa54b5ed),
        thread_model_provider(thread)
            .unwrap_or_else(|| language.message(MessageKey::CommandsModelProviderUnknown)),
        language.message(MessageKey::CommandsEffectiveCodexConfigEeb899f3),
        effective,
        child_default_timing(language)
    );
    let current_value = selected
        .or(saved)
        .unwrap_or_else(|| "native-default".to_owned());
    let choices = subagent_model_choices(catalog, language);
    (
        Some(current_value),
        model_catalog_warnings(current, catalog),
        choices,
    )
}

fn child_default_timing(language: crate::i18n::Language) -> &'static str {
    language.message(MessageKey::CommandsTheSavedChildDefaultAppliesOnTheNextThre8f9e7706)
}

fn child_default_saved_notice(language: crate::i18n::Language) -> String {
    format!(
        "{}\n{}",
        language.message(MessageKey::CommandsSaved7b6b2c9c),
        child_default_timing(language)
    )
}

fn advanced_settings_values(
    preferences: &crate::native::NativeFeaturePreferences,
    language: crate::i18n::Language,
) -> (Option<String>, String, Vec<SettingsChoice>) {
    let descriptors = preferences.settings();
    let mut current = language
        .message(MessageKey::CommandsNativeFeatureSwitchesApplyAfterTheDiscodex1ea6deb2)
        .to_owned();
    for key in NativeFeatureKey::ALL {
        let value = native_feature_saved_value(preferences, key);
        current.push_str(&format!(
            "\n• {}: {}",
            key.label(language),
            native_preference_label(value.as_ref(), language)
        ));
    }
    let choices = descriptors
        .into_iter()
        .filter_map(|(descriptor, _)| {
            NativeFeatureKey::from_key(descriptor.key).map(|key| SettingsChoice {
                value: format!("native-feature:{}", key.key()),
                label: key.label(language).to_owned(),
            })
        })
        .collect::<Vec<_>>();
    (None, current, choices)
}

fn settings_kind_title(kind: SettingsKind, language: crate::i18n::Language) -> &'static str {
    match kind {
        SettingsKind::Hub => language.message(MessageKey::CommandsSettings6f9676eb),
        SettingsKind::NativeFeatures => {
            language.message(MessageKey::CommandsAdvancedSettingsC1402b86)
        }
        SettingsKind::NativeFeature(key) => key.label(language),
        SettingsKind::Antigravity => language.message(MessageKey::CommandsAgyTitle),
        SettingsKind::AntigravityAutoApprove => {
            language.message(MessageKey::CommandsAgyAutoApprove)
        }
        SettingsKind::AntigravitySetting(key) => key.label(language),
        SettingsKind::Updates => language.message(MessageKey::CommandsUpdateNotifications6a160463),
        SettingsKind::Model => language.message(MessageKey::CommandsModel704caa01),
        SettingsKind::SubagentModel => {
            language.message(MessageKey::CommandsDefaultSubagentModel9811e108)
        }
        SettingsKind::Effort => language.message(MessageKey::CommandsReasoningEffortCeade6e1),
        SettingsKind::Fast => language.message(MessageKey::CommandsFastMode86774341),
        SettingsKind::Permissions => {
            language.message(MessageKey::CommandsPermissionProfileC7e4bb80)
        }
        SettingsKind::ApprovalPolicy => {
            language.message(MessageKey::CommandsApprovalPolicyBe6ee113)
        }
        SettingsKind::Input => language.message(MessageKey::CommandsInputDelivery47ae354d),
        SettingsKind::MessageAccess => {
            language.message(MessageKey::CommandsMessageReceptionPolicy13952d44)
        }
        SettingsKind::PairedAccounts => {
            language.message(MessageKey::CommandsPairedAccounts263399a8)
        }
        SettingsKind::Whitelist => language.message(MessageKey::CommandsWhitelist4df98d9d),
        SettingsKind::Blacklist => language.message(MessageKey::CommandsBlacklistD5332ba7),
        SettingsKind::RegisteredChannels => {
            language.message(MessageKey::CommandsRegisteredChannels5a818eab)
        }
        SettingsKind::Voice => language.message(MessageKey::CommandsVoiceProfile2f25af45),
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
        language.message(MessageKey::CommandsCurrent46b1c35d),
        menu.current
    );
    if menu.kind != SettingsKind::Hub {
        let pages = menu
            .choices
            .len()
            .div_ceil(settings_page_size(menu.kind))
            .max(1);
        if pages > 1 {
            content.push_str(&format!(
                "\n{} {}/{}",
                language.message(MessageKey::CommandsChooseAnOptionPage9bb1942d),
                menu.page + 1,
                pages
            ));
        } else {
            content.push_str(&format!(
                "\n{}",
                language.message(MessageKey::CommandsChooseAnOption6254cba9)
            ));
        }
    } else {
        content.push_str(&format!(
            "\n{}",
            language.message(MessageKey::CommandsChooseASettingA7d20000)
        ));
    }

    if menu.kind != SettingsKind::Hub {
        let start = menu.page * settings_page_size(menu.kind);
        let end = (start + settings_page_size(menu.kind)).min(menu.choices.len());
        for choice in &menu.choices[start..end] {
            if is_access_list(menu.kind) {
                content.push_str(&format!(
                    "\n• {} (ID: {})",
                    choice.label.strip_prefix("ID: ").unwrap_or(&choice.label),
                    choice.value
                ));
            } else {
                content.push_str(&format!("\n• {} — {}", choice.label, choice.value));
            }
        }
    }

    let is_access_list = is_access_list(menu.kind);
    let start = if menu.kind == SettingsKind::Hub {
        0
    } else {
        menu.page * settings_page_size(menu.kind)
    };
    let end = (start
        + if menu.kind == SettingsKind::Hub {
            menu.choices.len()
        } else {
            settings_page_size(menu.kind)
        })
    .min(menu.choices.len());
    let mut rows = Vec::new();
    if !is_access_list {
        for (offset, group) in menu.choices[start..end].chunks(5).enumerate() {
            let buttons = group
                .iter()
                .enumerate()
                .map(|(within, choice)| {
                    let index = start + offset * 5 + within;
                    let is_hub = menu.kind == SettingsKind::Hub;
                    let marker = if !is_hub
                        && menu.current_value.as_deref() == Some(choice.value.as_str())
                    {
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
    }
    let mut controls = Vec::new();
    if menu.kind != SettingsKind::Hub {
        let pages = menu
            .choices
            .len()
            .div_ceil(settings_page_size(menu.kind))
            .max(1);
        if pages > 1 {
            controls.push(
                CreateButton::new(format!(
                    "codex:settings:{menu_id}:p{}",
                    menu.page.saturating_sub(1)
                ))
                .label(language.message(MessageKey::CommandsPrevious9be71e5f))
                .style(ButtonStyle::Secondary)
                .disabled(menu.page == 0),
            );
        }
        if is_access_list {
            controls.push(
                CreateButton::new(format!("codex:settings:{menu_id}:a"))
                    .label(access_list_action_label(menu.kind, true, language))
                    .style(ButtonStyle::Success),
            );
            controls.push(
                CreateButton::new(format!("codex:settings:{menu_id}:r"))
                    .label(access_list_action_label(menu.kind, false, language))
                    .style(ButtonStyle::Danger),
            );
        }
        controls.push(
            CreateButton::new(format!(
                "codex:settings:{menu_id}:{}",
                if is_access_list { "b" } else { "h" }
            ))
            .label(language.message(MessageKey::CommandsBackC2e16a99))
            .style(ButtonStyle::Primary),
        );
        if pages > 1 {
            controls.push(
                CreateButton::new(format!(
                    "codex:settings:{menu_id}:n{}",
                    (menu.page + 1).min(pages - 1)
                ))
                .label(language.message(MessageKey::CommandsNext8a0c8241))
                .style(ButtonStyle::Secondary)
                .disabled(menu.page + 1 >= pages),
            );
        }
        controls.push(
            CreateButton::new(format!("codex:settings:{menu_id}:x"))
                .label(language.message(MessageKey::CommandsExit82c65349))
                .style(ButtonStyle::Secondary),
        );
    }
    if menu.kind == SettingsKind::Hub {
        controls.push(
            CreateButton::new(format!("codex:settings:{menu_id}:x"))
                .label(language.message(MessageKey::CommandsExit82c65349))
                .style(ButtonStyle::Secondary),
        );
    }
    for group in controls.chunks(5) {
        rows.push(CreateActionRow::Buttons(group.to_vec()));
    }
    (content, rows)
}

fn settings_page_from_action(action: &str) -> Option<usize> {
    action
        .strip_prefix('p')
        .or_else(|| action.strip_prefix('n'))?
        .parse()
        .ok()
}

pub(crate) fn split_discord_content(content: &str) -> Vec<String> {
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

fn message_policy_key(policy: MessagePolicy) -> &'static str {
    match policy {
        MessagePolicy::Everyone => "everyone",
        MessagePolicy::RegisteredServer => "registered-server",
        MessagePolicy::RegisteredChannels => "registered-channels",
        MessagePolicy::Paired => "paired",
    }
}

fn message_policy_label(policy: MessagePolicy, language: crate::i18n::Language) -> &'static str {
    match policy {
        MessagePolicy::Everyone => {
            language.message(MessageKey::CommandsEveryoneIncludingDms3d00363e)
        }
        MessagePolicy::RegisteredServer => {
            language.message(MessageKey::CommandsAllMembersInRegisteredServerE81e3e9f)
        }
        MessagePolicy::RegisteredChannels => {
            language.message(MessageKey::CommandsRegisteredChannels5a818eab)
        }
        MessagePolicy::Paired => language.message(MessageKey::CommandsPairedAccounts263399a8),
    }
}

fn is_access_list(kind: SettingsKind) -> bool {
    matches!(
        kind,
        SettingsKind::PairedAccounts
            | SettingsKind::Whitelist
            | SettingsKind::Blacklist
            | SettingsKind::RegisteredChannels
    )
}

fn access_list(access: &MessageAccess, kind: SettingsKind) -> &[u64] {
    match kind {
        SettingsKind::PairedAccounts => &access.paired_user_ids,
        SettingsKind::Whitelist => &access.whitelist,
        SettingsKind::Blacklist => &access.blacklist,
        SettingsKind::RegisteredChannels => &access.channel_ids,
        _ => &[],
    }
}

fn access_list_key(kind: SettingsKind) -> Option<&'static str> {
    match kind {
        SettingsKind::PairedAccounts => Some("paired"),
        SettingsKind::Whitelist => Some("whitelist"),
        SettingsKind::Blacklist => Some("blacklist"),
        SettingsKind::RegisteredChannels => Some("channels"),
        _ => None,
    }
}

fn access_list_kind(key: &str) -> Option<SettingsKind> {
    match key {
        "paired" => Some(SettingsKind::PairedAccounts),
        "whitelist" => Some(SettingsKind::Whitelist),
        "blacklist" => Some(SettingsKind::Blacklist),
        "channels" => Some(SettingsKind::RegisteredChannels),
        _ => None,
    }
}

async fn account_display_name(
    http: &serenity::http::Http,
    guild: GuildId,
    user: UserId,
    language: crate::i18n::Language,
) -> String {
    if let Ok(member) = http.get_member(guild, user).await {
        return member_display_name(
            member.nick.as_deref(),
            member.user.global_name.as_deref(),
            &member.user.name,
        )
        .to_owned();
    }
    match http.get_user(user).await {
        Ok(user) => member_display_name(None, user.global_name.as_deref(), &user.name).to_owned(),
        Err(_) => language
            .message(MessageKey::CommandsAccountUnavailableDfa33901)
            .to_owned(),
    }
}

fn member_display_name<'a>(
    nick: Option<&'a str>,
    global_name: Option<&'a str>,
    username: &'a str,
) -> &'a str {
    nick.or(global_name).unwrap_or(username)
}

fn access_list_action_label(
    kind: SettingsKind,
    add: bool,
    language: crate::i18n::Language,
) -> String {
    match (kind, add) {
        (SettingsKind::PairedAccounts, true) => language
            .message(MessageKey::CommandsAddIdD1e7598c)
            .to_owned(),
        (SettingsKind::PairedAccounts, false) => language
            .message(MessageKey::CommandsRemoveIdB4666e69)
            .to_owned(),
        (SettingsKind::Whitelist, true) => language
            .message(MessageKey::CommandsAllowId0e9d741b)
            .to_owned(),
        (SettingsKind::Whitelist, false) => language
            .message(MessageKey::CommandsRemoveIdB4666e69)
            .to_owned(),
        (SettingsKind::Blacklist, true) => language
            .message(MessageKey::CommandsBlockId396144c0)
            .to_owned(),
        (SettingsKind::Blacklist, false) => language
            .message(MessageKey::CommandsRemoveIdB4666e69)
            .to_owned(),
        (_, true) => language
            .message(MessageKey::CommandsAddChannelIdAea94df4)
            .to_owned(),
        (_, false) => language
            .message(MessageKey::CommandsRemoveChannelId72d40c77)
            .to_owned(),
    }
}

async fn save_message_policy(state: &BridgeState, key: &str) -> anyhow::Result<()> {
    let policy = match key {
        "everyone" => MessagePolicy::Everyone,
        "registered-server" => MessagePolicy::RegisteredServer,
        "registered-channels" => MessagePolicy::RegisteredChannels,
        "paired" => MessagePolicy::Paired,
        _ => bail!("unknown message policy"),
    };
    let _change = state.settings_change_lock.lock().await;
    let mut current = state.message_access.write().await;
    let mut staged = current.clone();
    staged.policy = policy;
    staged.save(&state.message_access_path)?;
    *current = staged;
    Ok(())
}

fn apply_message_access_list_edit(
    access: &mut MessageAccess,
    kind: SettingsKind,
    add: bool,
    id: u64,
) -> anyhow::Result<()> {
    let list = match kind {
        SettingsKind::PairedAccounts => &mut access.paired_user_ids,
        SettingsKind::Whitelist => &mut access.whitelist,
        SettingsKind::Blacklist => &mut access.blacklist,
        SettingsKind::RegisteredChannels => &mut access.channel_ids,
        _ => bail!("not a message access list"),
    };
    if add {
        if list.contains(&id) {
            bail!("ID {id} is already registered");
        }
        list.push(id);
    } else {
        let Some(index) = list.iter().position(|value| *value == id) else {
            bail!("ID {id} is not registered in this list");
        };
        list.remove(index);
    }
    Ok(())
}

async fn update_message_access_list(
    state: &BridgeState,
    kind: SettingsKind,
    add: bool,
    id: u64,
) -> anyhow::Result<()> {
    let _change = state.settings_change_lock.lock().await;
    let mut current = state.message_access.write().await;
    let mut staged = current.clone();
    apply_message_access_list_edit(&mut staged, kind, add, id)?;
    staged.save(&state.message_access_path)?;
    *current = staged;
    Ok(())
}

fn native_feature_saved_value(
    preferences: &crate::native::NativeFeaturePreferences,
    key: NativeFeatureKey,
) -> Option<String> {
    match key {
        NativeFeatureKey::CodeMode => preferences.code_mode.map(|value| value.to_string()),
        NativeFeatureKey::CodeModeHost => preferences.code_mode_host.map(|value| value.to_string()),
        NativeFeatureKey::MultiAgentV2 => preferences.multi_agent_v2.map(|value| value.to_string()),
        NativeFeatureKey::WaitAgent => preferences.wait_agent.map(|value| value.to_string()),
        NativeFeatureKey::AsyncNotifications => preferences
            .async_notifications
            .map(|value| value.to_string()),
        NativeFeatureKey::RequestUserInput => preferences
            .request_user_input
            .map(|value| value.to_string()),
        NativeFeatureKey::Clock => preferences.clock.map(|value| value.to_string()),
        NativeFeatureKey::TokenBudget => preferences.token_budget.map(|value| value.to_string()),
        NativeFeatureKey::HistoryNotes => preferences.history_notes.map(|value| value.to_string()),
        NativeFeatureKey::SubagentModel => preferences.subagent_model.clone(),
    }
}

fn native_preference_label(value: Option<&String>, language: crate::i18n::Language) -> String {
    match value.map(String::as_str) {
        Some("true") => language.message(MessageKey::CommandsOn65845774).to_owned(),
        Some("false") => language.message(MessageKey::CommandsOff5677591d).to_owned(),
        Some(value) => value.to_owned(),
        None => language
            .message(MessageKey::CommandsNativeDefault308250fc)
            .to_owned(),
    }
}

fn effective_native_feature_text(
    native_config: &Value,
    key: NativeFeatureKey,
    language: crate::i18n::Language,
) -> String {
    key.config_paths()
        .iter()
        .map(|path| {
            let value = effective_config_value(native_config, path)
                .map(Value::to_string)
                .unwrap_or_else(|| {
                    language
                        .message(MessageKey::CommandsNotReportedEb1c723f)
                        .to_owned()
                });
            let origin = native_config
                .pointer("/origins")
                .and_then(|origins| origins.get(*path));
            match origin {
                Some(origin) => format!("{path} = {value} ({})", origin),
                None => format!("{path} = {value}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn effective_config_value<'a>(native_config: &'a Value, path: &str) -> Option<&'a Value> {
    let mut value = native_config.get("config")?;
    for segment in path.split('.') {
        let camel = snake_to_camel(segment);
        value = value.get(segment).or_else(|| value.get(camel))?;
    }
    Some(value)
}

fn snake_to_camel(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut uppercase = false;
    for character in value.chars() {
        if character == '_' {
            uppercase = true;
        } else if uppercase {
            output.extend(character.to_uppercase());
            uppercase = false;
        } else {
            output.push(character);
        }
    }
    output
}

fn set_native_feature_preference(
    preferences: &mut crate::native::NativeFeaturePreferences,
    key: NativeFeatureKey,
    selected: Option<&str>,
) -> anyhow::Result<()> {
    if key == NativeFeatureKey::SubagentModel {
        preferences.subagent_model = selected.map(str::to_owned);
        return Ok(());
    }
    let value = match selected {
        Some("true") | Some("on") => Some(true),
        Some("false") | Some("off") => Some(false),
        None => None,
        _ => bail!("unknown native feature preference"),
    };
    match key {
        NativeFeatureKey::CodeMode => preferences.code_mode = value,
        NativeFeatureKey::CodeModeHost => preferences.code_mode_host = value,
        NativeFeatureKey::MultiAgentV2 => preferences.multi_agent_v2 = value,
        NativeFeatureKey::WaitAgent => preferences.wait_agent = value,
        NativeFeatureKey::AsyncNotifications => preferences.async_notifications = value,
        NativeFeatureKey::RequestUserInput => preferences.request_user_input = value,
        NativeFeatureKey::Clock => preferences.clock = value,
        NativeFeatureKey::TokenBudget => preferences.token_budget = value,
        NativeFeatureKey::HistoryNotes => preferences.history_notes = value,
        NativeFeatureKey::SubagentModel => unreachable!(),
    }
    Ok(())
}

fn set_subagent_model_selection(
    preferences: &mut crate::native::NativeFeaturePreferences,
    selection: Option<&crate::model_catalog::ModelSelection>,
) {
    preferences.subagent_model = selection.map(|value| value.model.clone());
    preferences.subagent_model_provider = selection.map(|value| value.provider.clone());
    preferences.subagent_model_executor = selection.map(|value| value.executor.as_str().to_owned());
}

fn settings_back_target(kind: SettingsKind) -> SettingsKind {
    match kind {
        SettingsKind::NativeFeature(_) => SettingsKind::NativeFeatures,
        SettingsKind::AntigravityAutoApprove | SettingsKind::AntigravitySetting(_) => {
            SettingsKind::Antigravity
        }
        _ => SettingsKind::Hub,
    }
}

fn settings_kind_from_key(value: &str) -> Option<SettingsKind> {
    match value {
        "model" => Some(SettingsKind::Model),
        "subagent-model" => Some(SettingsKind::SubagentModel),
        "effort" => Some(SettingsKind::Effort),
        "fast" => Some(SettingsKind::Fast),
        "permissions" => Some(SettingsKind::Permissions),
        "approval-policy" => Some(SettingsKind::ApprovalPolicy),
        "input" => Some(SettingsKind::Input),
        "voice" => Some(SettingsKind::Voice),
        "updates" => Some(SettingsKind::Updates),
        "message-access" => Some(SettingsKind::MessageAccess),
        "native-features" => Some(SettingsKind::NativeFeatures),
        "antigravity" => Some(SettingsKind::Antigravity),
        _ => None,
    }
}

fn antigravity_settings_values(
    preferences: &crate::native::NativeFeaturePreferences,
    language: crate::i18n::Language,
) -> (Option<String>, String, Vec<SettingsChoice>) {
    let enabled = if preferences.antigravity_enabled {
        "on"
    } else {
        "off"
    };
    let status = if preferences.antigravity_enabled {
        language.message(MessageKey::CommandsOn2666c335)
    } else {
        language.message(MessageKey::CommandsOff38c695fb)
    };
    let models = if preferences.antigravity_models.is_empty() {
        language
            .message(MessageKey::CommandsNoModelIdsConfiguredA4c9e21b)
            .to_owned()
    } else {
        preferences.antigravity_models.join("\n")
    };
    let sharing = match preferences.antigravity_share_settings {
        Some(true) => language.message(MessageKey::CommandsSharedExistingSettingsAfa9effe),
        Some(false) => language.message(MessageKey::CommandsSeparateSettings016de9e5),
        None => language.message(MessageKey::CommandsChooseDuringSetup9558c853),
    };
    let current = format!(
        "{status}\n{sharing}\n{}:\n{models}\n{}",
        language.message(MessageKey::CommandsModelIdsF17651aa),
        language.message(MessageKey::CommandsDiscodexManagesItsOwnAgyCliEnablingItSta9ab1e631),
    );
    let mut choices = [
        ("on", language.message(MessageKey::CommandsOn65845774)),
        ("off", language.message(MessageKey::CommandsOff5677591d)),
        (
            "sharing",
            language.message(MessageKey::CommandsSettingsSharing2e85b807),
        ),
        (
            "models",
            language.message(MessageKey::CommandsModelIdsF17651aa),
        ),
        (
            "refresh-models",
            language.message(MessageKey::CommandsUpdateModelList3e0facf2),
        ),
    ]
    .into_iter()
    .map(|(value, label)| SettingsChoice {
        value: value.to_owned(),
        label: label.to_owned(),
    })
    .collect::<Vec<_>>();
    choices.push(SettingsChoice {
        value: "auto-approve".to_owned(),
        label: language
            .message(MessageKey::CommandsAgyAutoApprove)
            .to_owned(),
    });
    choices.extend(AgySettingKey::ALL.into_iter().map(|key| SettingsChoice {
        value: format!("agy-setting:{}", key.id()),
        label: key.label(language).to_owned(),
    }));
    (Some(enabled.to_owned()), current, choices)
}

fn agy_setting_values(
    key: AgySettingKey,
    settings: &Value,
    language: crate::i18n::Language,
) -> anyhow::Result<(Option<String>, String, Vec<SettingsChoice>)> {
    let choices = match key {
        AgySettingKey::ToolPermission => vec![
            ("request-review", MessageKey::CommandsAgyRequestReview),
            (
                "proceed-in-sandbox",
                MessageKey::CommandsAgyProceedInSandbox,
            ),
            ("strict", MessageKey::CommandsAgyStrict),
            ("always-proceed", MessageKey::CommandsAgyAlwaysProceed),
        ],
        AgySettingKey::ArtifactReview => vec![
            ("asks-for-review", MessageKey::CommandsAgyAsksForReview),
            ("agent-decides", MessageKey::CommandsAgyAgentDecides),
            ("always-proceed", MessageKey::CommandsAgyAlwaysProceed),
        ],
        AgySettingKey::TerminalSandbox | AgySettingKey::NonWorkspaceAccess => vec![
            ("true", MessageKey::CommandsOn65845774),
            ("false", MessageKey::CommandsOff5677591d),
        ],
        AgySettingKey::Allow | AgySettingKey::Ask | AgySettingKey::Deny => vec![
            ("add", MessageKey::CommandsAgyAddRule),
            ("remove", MessageKey::CommandsAgyRemoveRule),
        ],
    }
    .into_iter()
    .map(|(value, label)| SettingsChoice {
        value: value.to_owned(),
        label: language.message(label).to_owned(),
    })
    .collect::<Vec<_>>();
    if key.is_rule_list() {
        let rules = agy_permission_rules(settings, key, language)?;
        let mut current = if rules.is_empty() {
            language.message(MessageKey::CommandsAgyNoRules).to_owned()
        } else {
            rules
                .iter()
                .map(|rule| {
                    format!(
                        "• {}",
                        rule.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| rule.to_string())
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        current.push_str(&format!(
            "\n{}",
            language.message(MessageKey::CommandsAgyRulePrecedence)
        ));
        if key == AgySettingKey::Allow {
            current.push_str(&format!(
                "\n{} `{}`",
                language.message(MessageKey::CommandsAgyManagedRule),
                crate::agy_settings::MANAGED_MCP_ALLOW_RULE,
            ));
        }
        return Ok((None, current, choices));
    }
    let default = match key {
        AgySettingKey::ToolPermission => json!("request-review"),
        AgySettingKey::ArtifactReview => json!("asks-for-review"),
        _ => json!(false),
    };
    let saved = settings.get(key.key()).filter(|value| !value.is_null());
    let effective = saved.unwrap_or(&default);
    let value = effective
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| effective.to_string());
    let label = choices
        .iter()
        .find(|choice| choice.value == value)
        .map(|choice| choice.label.as_str())
        .unwrap_or(&value);
    let current = format!(
        "{}: {label}\n{}\n{}",
        if saved.is_some() {
            language.message(MessageKey::CommandsAgyEffectivePreference)
        } else {
            language.message(MessageKey::CommandsAgyInheritedDefault)
        },
        language.message(MessageKey::CommandsAgyNativePolicyNotice),
        language.message(MessageKey::CommandsAgyAppliesNextSession),
    );
    Ok((Some(value), current, choices))
}

fn agy_permission_rules(
    settings: &Value,
    key: AgySettingKey,
    language: crate::i18n::Language,
) -> anyhow::Result<Vec<Value>> {
    match settings.pointer(key.key()) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(rules)) => Ok(rules.clone()),
        Some(_) => bail!(
            "{}: {}",
            language.message(MessageKey::CommandsAgyInvalidRuleList),
            key.key()
        ),
    }
}

fn agy_permission_rule_modal(
    menu_id: &str,
    key: AgySettingKey,
    action: &str,
    language: crate::i18n::Language,
) -> CreateModal {
    CreateModal::new(
        format!("codex:agy-settings:{menu_id}:{action}"),
        key.label(language),
    )
    .components(vec![CreateActionRow::InputText(
        serenity::builder::CreateInputText::new(
            InputTextStyle::Short,
            language.message(if action == "add" {
                MessageKey::CommandsAgyRuleToAdd
            } else {
                MessageKey::CommandsAgyRuleToRemove
            }),
            "value",
        )
        .required(true)
        .placeholder(language.message(MessageKey::CommandsAgyRulePlaceholder)),
    )])
}

async fn refresh_agy_catalog_notice<F, Fut>(
    refresh: F,
    language: crate::i18n::Language,
) -> anyhow::Result<(Value, String)>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Value>>,
{
    let catalog = refresh().await?;
    let count = catalog
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| row.get("executor").and_then(Value::as_str) == Some("antigravity"))
        .count();
    let notice = format!(
        "{} ({count})",
        language.message(MessageKey::CommandsUpdatedTheAntigravityModelListD912f16d)
    );
    Ok((catalog, notice))
}

fn antigravity_text_field<'a>(menu: &'a SettingsMenuState, action: &str) -> Option<&'a str> {
    let index = action.strip_prefix('c')?.parse::<usize>().ok()?;
    let value = menu.choices.get(index)?.value.as_str();
    match menu.kind {
        SettingsKind::Antigravity if value == "models" => Some(value),
        SettingsKind::AntigravitySetting(key)
            if key.is_rule_list() && matches!(value, "add" | "remove") =>
        {
            Some(value)
        }
        _ => None,
    }
}

fn antigravity_text_modal(
    menu_id: &str,
    field: &str,
    preferences: &crate::native::NativeFeaturePreferences,
    language: crate::i18n::Language,
) -> anyhow::Result<CreateModal> {
    let (title, label, placeholder, value, style) = match field {
        "models" => (
            language.message(MessageKey::CommandsAntigravityModels977cb98b),
            language.message(MessageKey::CommandsModelIdsOnePerLine5bc970c7),
            language.message(MessageKey::CommandsExactModelIdsFromYourAgyCliAc57b51e),
            preferences.antigravity_models.join("\n"),
            InputTextStyle::Paragraph,
        ),
        _ => bail!("unknown Antigravity text setting"),
    };
    Ok(
        CreateModal::new(format!("codex:agy-settings:{menu_id}:{field}"), title).components(vec![
            CreateActionRow::InputText(
                serenity::builder::CreateInputText::new(style, label, "value")
                    .required(false)
                    .placeholder(placeholder)
                    .value(value),
            ),
        ]),
    )
}

fn set_antigravity_text_preference(
    preferences: &mut crate::native::NativeFeaturePreferences,
    field: &str,
    value: &str,
) -> anyhow::Result<()> {
    match field {
        "models" => {
            preferences.antigravity_models = value
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        }
        _ => bail!("unknown Antigravity text setting"),
    }
    Ok(())
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

fn settings_menu_is_current(menu: &SettingsMenuState, generation: u64, thread_id: &str) -> bool {
    menu.connection_generation == generation && menu.thread_id == thread_id
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsChoiceFreshness {
    Current,
    ModelChanged,
    Unavailable,
}

#[derive(Debug)]
struct SettingsModelScopeChanged;

impl std::fmt::Display for SettingsModelScopeChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("settings model changed before the choice was applied")
    }
}

impl std::error::Error for SettingsModelScopeChanged {}

fn ensure_settings_model_scope(
    kind: SettingsKind,
    expected_model_scope: &Option<String>,
    current_model_scope: Option<String>,
) -> anyhow::Result<()> {
    if matches!(kind, SettingsKind::Effort | SettingsKind::Fast)
        && *expected_model_scope != current_model_scope
    {
        return Err(SettingsModelScopeChanged.into());
    }
    Ok(())
}

fn ensure_settings_apply_target(
    expected_generation: Option<u64>,
    current_generation: u64,
    expected_thread_id: Option<&str>,
    current_thread_id: &str,
) -> anyhow::Result<()> {
    if expected_generation.is_some_and(|generation| generation != current_generation) {
        bail!("Codex connection changed before the setting could be applied");
    }
    if expected_thread_id.is_some_and(|thread| thread != current_thread_id) {
        bail!("Codex conversation changed before the setting could be applied");
    }
    Ok(())
}

fn settings_choice_freshness(
    kind: SettingsKind,
    previous_model_scope: &Option<String>,
    current_model_scope: &Option<String>,
    choices: &[SettingsChoice],
    requested: &str,
) -> SettingsChoiceFreshness {
    if matches!(kind, SettingsKind::Effort | SettingsKind::Fast)
        && previous_model_scope != current_model_scope
    {
        SettingsChoiceFreshness::ModelChanged
    } else if kind == SettingsKind::Model || choices.iter().any(|choice| choice.value == requested)
    {
        SettingsChoiceFreshness::Current
    } else {
        SettingsChoiceFreshness::Unavailable
    }
}

fn settings_apply_notice(
    result: anyhow::Result<String>,
    language: crate::i18n::Language,
    config: Option<&crate::config::Config>,
) -> String {
    match result {
        Ok(result) => result,
        Err(error) => {
            let detail = settings_error_detail(&error, language, config);
            tracing::warn!(error = %detail, "failed to apply Discord settings choice");
            format!(
                "{}\n{}",
                language
                    .message(MessageKey::CommandsCouldNotApplyThatSettingTheCurrentChoices1c140555),
                detail
            )
        }
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
                SettingsClaimError::Missing => language
                    .message(MessageKey::CommandsThisPanelIsNoLongerActiveOpenDiscodexSetA4e5b13c),
                SettingsClaimError::OtherUser => language
                    .message(MessageKey::CommandsOnlyThePersonWhoOpenedThisSettingsPanelC4f357438),
                SettingsClaimError::Busy => language
                    .message(MessageKey::CommandsThisSettingIsStillBeingProcessedPleaseWaiCd1fb4ef),
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
    if let Some(field) = antigravity_text_field(&menu, action) {
        let response = async {
            let modal = if let SettingsKind::AntigravitySetting(key) = menu.kind {
                agy_permission_rule_modal(menu_id, key, field, state.config.language)
            } else {
                let preferences = state.native.read_native_feature_preferences()?;
                antigravity_text_modal(menu_id, field, &preferences, state.config.language)?
            };
            interaction
                .create_response(ctx, CreateInteractionResponse::Modal(modal))
                .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        release_settings_menu(&mut *state.settings_menus.lock().await, menu_id);
        return response.context("open Antigravity setting input");
    }
    if matches!(action, "a" | "r") && is_access_list(menu.kind) {
        let key =
            access_list_key(menu.kind).ok_or_else(|| anyhow!("invalid message access list"))?;
        let add = action == "a";
        let title = if menu.kind == SettingsKind::RegisteredChannels {
            state
                .config
                .language
                .message(MessageKey::CommandsEditRegisteredChannelsCca7da5c)
        } else {
            settings_kind_title(menu.kind, state.config.language)
        };
        let label = if menu.kind == SettingsKind::RegisteredChannels {
            state
                .config
                .language
                .message(MessageKey::CommandsChannelId7e46e03a)
        } else {
            state
                .config
                .language
                .message(MessageKey::CommandsAccountIdCb6e9f3a)
        };
        let placeholder = state
            .config
            .language
            .message(MessageKey::CommandsEnterADiscordNumericId03937d9b);
        let modal_id = format!(
            "codex:access:{menu_id}:{key}:{}",
            if add { "add" } else { "remove" }
        );
        let modal = CreateModal::new(modal_id, discord_button_label(title)).components(vec![
            CreateActionRow::InputText(
                serenity::builder::CreateInputText::new(InputTextStyle::Short, label, "id")
                    .placeholder(placeholder)
                    .min_length(1)
                    .max_length(20),
            ),
        ]);
        let response = interaction
            .create_response(ctx, CreateInteractionResponse::Modal(modal))
            .await;
        release_settings_menu(&mut *state.settings_menus.lock().await, menu_id);
        response.context("open message access ID modal")?;
        return Ok(());
    }

    let result = async {
        interaction
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await
            .context("acknowledge settings button")?;
        if action == "x" {
            interaction
                .delete_response(ctx)
                .await
                .context("close settings panel")?;
            state.settings_menus.lock().await.remove(menu_id);
            return Ok(());
        }
        if !settings_menu_is_current(
            &menu,
            state.native.current_generation(),
            &state.native.thread_id(),
        ) {
            interaction
                .edit_response(
                    ctx,
                    serenity::builder::EditInteractionResponse::new()
                        .content(state.config.language.message(
                            MessageKey::CommandsTheCodexConnectionChangedOpenSettingsAgainE9c70d30,
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
        let notice = if action == "x" {
            state
                .config
                .language
                .message(MessageKey::CommandsCouldNotCloseSettingsYouCanRetryExitAd305cd1)
        } else {
            state
                .config
                .language
                .message(MessageKey::CommandsCouldNotUpdateSettingsYouCanRetryTheExisD59486a7)
        };
        // An edit error can mean the new panel reached Discord but its response
        // was lost. Never overwrite either visible version with a recovery edit.
        release_settings_menu(&mut *state.settings_menus.lock().await, menu_id);
        let detail = settings_error_detail(&error, state.config.language, Some(&state.config));
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

fn settings_error_detail(
    error: &anyhow::Error,
    language: crate::i18n::Language,
    config: Option<&crate::config::Config>,
) -> String {
    // HTTP errors can contain interaction-token URLs. The API error itself is
    // enough to explain a failed edit without displaying those URLs.
    for cause in error.chain() {
        if let Some(serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(
            response,
        ))) = cause.downcast_ref::<serenity::Error>()
        {
            let detail = format!(
                "{} {}: {} ({})",
                language.message(MessageKey::CommandsDiscordErrorLabel),
                response.status_code,
                response.error.message,
                response.error.code
            );
            return config
                .map(|config| crate::discord::sanitize_discord_error(config, &detail))
                .unwrap_or(detail);
        }
        if cause.downcast_ref::<serenity::Error>().is_some() {
            let detail = format!(
                "{}: {error}",
                language.message(MessageKey::CommandsDiscordErrorLabel)
            );
            return config
                .map(|config| crate::discord::sanitize_discord_error(config, &detail))
                .unwrap_or(detail);
        }
    }
    let detail = format!("{error:#}");
    config
        .map(|config| crate::discord::sanitize_discord_error(config, &detail))
        .unwrap_or(detail)
}

fn localized_failure_message(prefix: &str, detail: &str) -> String {
    format!("{prefix}\n{detail}")
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
        menu.kind = if let Some(key) = selected.value.strip_prefix("native-feature:") {
            NativeFeatureKey::from_key(key)
                .map(SettingsKind::NativeFeature)
                .ok_or_else(|| anyhow!("unknown native feature choice"))?
        } else {
            settings_kind_from_key(&selected.value)
                .ok_or_else(|| anyhow!("unknown settings choice"))?
        };
        menu.page = 0;
    } else if prefix == 'h' && suffix.is_empty() {
        menu.kind = settings_back_target(menu.kind);
        menu.page = 0;
    } else if prefix == 'b' && suffix.is_empty() && is_access_list(menu.kind) {
        menu.kind = SettingsKind::MessageAccess;
        menu.page = 0;
    } else if let Some(page) = settings_page_from_action(action) {
        menu.page = page;
    } else if prefix == 'c' {
        let index = index.ok_or_else(|| anyhow!("invalid settings choice"))?;
        let selected = menu
            .choices
            .get(index)
            .cloned()
            .ok_or_else(|| anyhow!("unknown settings choice"))?;
        if menu.kind == SettingsKind::Antigravity {
            if selected.value == "auto-approve" {
                let refreshed = build_settings_menu(
                    ctx,
                    state,
                    SettingsKind::AntigravityAutoApprove,
                    0,
                    menu.requester_id,
                    menu.connection_generation,
                )
                .await?;
                return update_settings_panel(ctx, interaction, state, menu_id, refreshed).await;
            }
            if let Some(key) = selected.value.strip_prefix("agy-setting:") {
                menu.kind = SettingsKind::AntigravitySetting(
                    AgySettingKey::ALL
                        .into_iter()
                        .find(|candidate| candidate.id() == key)
                        .ok_or_else(|| {
                            anyhow!(
                                state
                                    .config
                                    .language
                                    .message(MessageKey::CommandsAgyUnknownSetting)
                            )
                        })?,
                );
                let refreshed = build_settings_menu(
                    ctx,
                    state,
                    menu.kind,
                    0,
                    menu.requester_id,
                    menu.connection_generation,
                )
                .await?;
                return update_settings_panel(ctx, interaction, state, menu_id, refreshed).await;
            }
            if selected.value == "refresh-models" {
                let (_catalog, notice) = {
                    let _settings_change = state.settings_change_lock.lock().await;
                    refresh_agy_catalog_notice(
                        || state.native.refresh_agy_model_catalog(),
                        state.config.language,
                    )
                    .await?
                };
                let refreshed = build_settings_menu(
                    ctx,
                    state,
                    SettingsKind::Antigravity,
                    0,
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
                    &notice,
                )
                .await;
            }
            if matches!(selected.value.as_str(), "on" | "sharing") {
                return crate::discord_agy_setup::begin(
                    ctx,
                    interaction,
                    state,
                    menu_id,
                    menu,
                    selected.value == "sharing",
                )
                .await;
            }
            if selected.value == "off" {
                state.agy_setup.cancel().await;
            }
        }
        if menu.kind == SettingsKind::NativeFeatures {
            let feature = selected
                .value
                .strip_prefix("native-feature:")
                .and_then(NativeFeatureKey::from_key)
                .ok_or_else(|| anyhow!("unknown native feature choice"))?;
            menu.kind = SettingsKind::NativeFeature(feature);
            let refreshed = build_settings_menu(
                ctx,
                state,
                menu.kind,
                0,
                menu.requester_id,
                menu.connection_generation,
            )
            .await?;
            return update_settings_panel(ctx, interaction, state, menu_id, refreshed).await;
        }
        if menu.kind == SettingsKind::MessageAccess {
            let notice = match selected.value.as_str() {
                "policy:everyone"
                | "policy:registered-server"
                | "policy:registered-channels"
                | "policy:paired" => {
                    save_message_policy(state, &selected.value[7..]).await?;
                    format!(
                        "{}: {}",
                        state
                            .config
                            .language
                            .message(MessageKey::CommandsPolicySaved6bb6f2da),
                        selected.label
                    )
                }
                "list:paired" => {
                    menu.kind = SettingsKind::PairedAccounts;
                    String::new()
                }
                "list:whitelist" => {
                    menu.kind = SettingsKind::Whitelist;
                    String::new()
                }
                "list:blacklist" => {
                    menu.kind = SettingsKind::Blacklist;
                    String::new()
                }
                "list:channels" => {
                    menu.kind = SettingsKind::RegisteredChannels;
                    String::new()
                }
                _ => bail!("unknown message access choice"),
            };
            menu.page = 0;
            let refreshed = build_settings_menu(
                ctx,
                state,
                menu.kind,
                0,
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
                &notice,
            )
            .await;
        }
        let model_catalog = if settings_kind_uses_model_catalog(menu.kind) {
            Some(state.native.model_catalog().await?)
        } else {
            None
        };
        let fresh = build_settings_menu_with_catalog(
            ctx,
            state,
            menu.kind,
            menu.page,
            menu.requester_id,
            menu.connection_generation,
            model_catalog.as_ref(),
        )
        .await?;
        match settings_choice_freshness(
            menu.kind,
            &menu.model_scope,
            &fresh.model_scope,
            &fresh.choices,
            &selected.value,
        ) {
            SettingsChoiceFreshness::ModelChanged => {
                return update_settings_panel(ctx, interaction, state, menu_id, fresh).await;
            }
            SettingsChoiceFreshness::Unavailable => {
                let stale = state
                    .config
                    .language
                    .message(MessageKey::CommandsThatChoiceIsNoLongerAvailableCurrentChoicB53f72ff);
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
            SettingsChoiceFreshness::Current => {}
        }
        let applied = apply_setting_choice(
            state,
            interaction.id.get(),
            menu.kind,
            &selected.value,
            Some(&menu.thread_id),
            Some(menu.connection_generation),
            menu.model_scope.clone(),
            model_catalog.as_ref(),
        )
        .await;
        if applied
            .as_ref()
            .err()
            .is_some_and(|error| error.is::<SettingsModelScopeChanged>())
        {
            let refreshed = build_settings_menu_with_catalog(
                ctx,
                state,
                menu.kind,
                menu.page,
                menu.requester_id,
                menu.connection_generation,
                model_catalog.as_ref(),
            )
            .await?;
            return update_settings_panel(ctx, interaction, state, menu_id, refreshed).await;
        }
        let outcome = match applied {
            Ok(outcome) => outcome,
            Err(error) => {
                let notice =
                    settings_apply_notice(Err(error), state.config.language, Some(&state.config));
                let refreshed = build_settings_menu_with_catalog(
                    ctx,
                    state,
                    menu.kind,
                    menu.page,
                    menu.requester_id,
                    menu.connection_generation,
                    model_catalog.as_ref(),
                )
                .await?;
                return update_settings_panel_with_notice(
                    ctx,
                    interaction,
                    state,
                    menu_id,
                    refreshed,
                    &notice,
                )
                .await;
            }
        };
        match outcome {
            SettingChoiceOutcome::OperationStarted { request, requested } => {
                let pending_notice = format!(
                    "{}: `{requested}`\n{}",
                    state
                        .config
                        .language
                        .message(MessageKey::CommandsRequestedValue9eb15821),
                    state
                        .config
                        .language
                        .message(MessageKey::SettingsOperationPending),
                );
                let (panel_result_tx, panel_result_rx) = tokio::sync::oneshot::channel();
                spawn_settings_confirmation(
                    ctx.clone(),
                    interaction.clone(),
                    state.clone(),
                    menu.kind,
                    menu.page,
                    menu.requester_id,
                    menu.connection_generation,
                    menu.thread_id.clone(),
                    model_catalog.clone(),
                    request,
                    requested,
                    interaction.id.get(),
                    panel_result_rx,
                );
                let refreshed = match build_settings_menu_with_catalog(
                    ctx,
                    state,
                    menu.kind,
                    menu.page,
                    menu.requester_id,
                    menu.connection_generation,
                    model_catalog.as_ref(),
                )
                .await
                {
                    Ok(menu) => menu,
                    Err(error) => {
                        let detail = settings_error_detail(
                            &error,
                            state.config.language,
                            Some(&state.config),
                        );
                        let _ = panel_result_tx.send(PanelDelivery::Failed(detail));
                        return Err(error);
                    }
                };
                let panel_update = update_settings_panel_with_notice_and_id(
                    ctx,
                    interaction,
                    state,
                    menu_id,
                    refreshed.clone(),
                    &pending_notice,
                )
                .await;
                match &panel_update {
                    Ok(pending_menu_id) => {
                        let _ =
                            panel_result_tx.send(PanelDelivery::Delivered(pending_menu_id.clone()));
                    }
                    Err(error) => {
                        let detail = settings_error_detail(
                            error,
                            state.config.language,
                            Some(&state.config),
                        );
                        let _ = panel_result_tx.send(PanelDelivery::Failed(detail));
                    }
                }
                if let Err(error) = panel_update {
                    return Err(error);
                }
                return Ok(());
            }
            SettingChoiceOutcome::Applied(notice) => {
                let refreshed = build_settings_menu_with_catalog(
                    ctx,
                    state,
                    menu.kind,
                    menu.page,
                    menu.requester_id,
                    menu.connection_generation,
                    model_catalog.as_ref(),
                )
                .await?;
                return update_settings_panel_with_notice(
                    ctx,
                    interaction,
                    state,
                    menu_id,
                    refreshed,
                    &notice,
                )
                .await;
            }
        }
    } else {
        bail!("unknown settings action");
    }

    let refreshed = build_settings_menu(
        ctx,
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

pub(crate) async fn return_from_agy_setup(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
    previous: &SettingsMenuState,
) -> anyhow::Result<()> {
    let menu = build_settings_menu(
        ctx,
        state,
        SettingsKind::Antigravity,
        0,
        previous.requester_id,
        state.native.current_generation(),
    )
    .await?;
    update_settings_panel(ctx, interaction, state, "", menu).await
}

async fn update_settings_panel_with_notice(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
    previous_id: &str,
    menu: SettingsMenuState,
    notice: &str,
) -> anyhow::Result<()> {
    update_settings_panel_with_notice_and_id(ctx, interaction, state, previous_id, menu, notice)
        .await
        .map(|_| ())
}

async fn update_settings_panel_with_notice_and_id(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
    previous_id: &str,
    mut menu: SettingsMenuState,
    notice: &str,
) -> anyhow::Result<String> {
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
            let error = anyhow::Error::new(error);
            tracing::warn!(error = %settings_error_detail(&error, state.config.language, Some(&state.config)), "settings panel updated but continuation delivery failed");
            return Err(error);
        }
    }
    Ok(menu_id)
}

enum SettingChoiceOutcome {
    Applied(String),
    OperationStarted {
        request: crate::settings_operation::SettingRequest,
        requested: String,
    },
}

async fn apply_setting_choice(
    state: &BridgeState,
    interaction_id: u64,
    kind: SettingsKind,
    requested: &str,
    expected_thread_id: Option<&str>,
    expected_generation: Option<u64>,
    expected_model_scope: Option<String>,
    model_catalog: Option<&Value>,
) -> anyhow::Result<SettingChoiceOutcome> {
    // Match new-session admission order: input gate, settings, then active turn.
    // Keep incoming Discord input outside a provider detach/resume transition.
    let input_phase = if kind == SettingsKind::Model {
        Some(state.voice.input_gate.state.lock().await)
    } else {
        None
    };
    let _settings_change = state.settings_change_lock.lock().await;
    ensure_settings_apply_target(
        expected_generation,
        state.native.current_generation(),
        expected_thread_id,
        &state.native.thread_id(),
    )?;
    let thread_id = expected_thread_id
        .map(str::to_owned)
        .unwrap_or_else(|| state.native.thread_id());
    let language = state.config.language;
    match kind {
        SettingsKind::AntigravityAutoApprove => {
            let mut preferences = state.native.read_native_feature_preferences()?;
            preferences.antigravity_auto_approve = match requested {
                "on" => true,
                "off" => false,
                _ => bail!(
                    "{}",
                    language.message(MessageKey::CommandsAgyUnknownSetting)
                ),
            };
            state
                .native
                .apply_native_feature_preferences(&preferences)
                .await?;
            Ok(SettingChoiceOutcome::Applied(
                language
                    .message(MessageKey::CommandsAgyAppliesNextSession)
                    .to_owned(),
            ))
        }
        SettingsKind::AntigravitySetting(key) => {
            let preferences = state.native.read_native_feature_preferences()?;
            let value = match key {
                AgySettingKey::TerminalSandbox | AgySettingKey::NonWorkspaceAccess => {
                    Value::Bool(requested.parse::<bool>()?)
                }
                AgySettingKey::ToolPermission | AgySettingKey::ArtifactReview => {
                    Value::String(requested.to_owned())
                }
                _ => bail!(
                    "{}",
                    language.message(MessageKey::CommandsAgyUseRuleButtons)
                ),
            };
            crate::agy_settings::set(
                &state.config,
                preferences.antigravity_share_settings.unwrap_or(false),
                key.key(),
                value,
            )?;
            Ok(SettingChoiceOutcome::Applied(
                language
                    .message(MessageKey::CommandsAgyAppliesNextSession)
                    .to_owned(),
            ))
        }
        SettingsKind::Antigravity => {
            let mut preferences = state.native.read_native_feature_preferences()?;
            preferences.antigravity_enabled = match requested {
                "off" => false,
                _ => bail!("unknown Antigravity switch"),
            };
            state
                .native
                .apply_native_feature_preferences(&preferences)
                .await?;
            Ok(SettingChoiceOutcome::Applied(
                language
                    .message(MessageKey::CommandsAntigravityPreferenceSavedItAppliesOnTheN85aacfc8)
                    .to_owned(),
            ))
        }
        SettingsKind::Updates => {
            let muted = match requested {
                "on" => false,
                "off" => true,
                _ => bail!("unknown update notification choice"),
            };
            state.updates.set_muted(muted, language).await?;
            Ok(SettingChoiceOutcome::Applied(
                language
                    .message(
                        MessageKey::CommandsUpdateNotificationPreferenceSavedManualUpda807290ea,
                    )
                    .to_owned(),
            ))
        }
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
            Ok(SettingChoiceOutcome::Applied(
                format_permission_settings_result_with_language(
                    &display_name,
                    &settings,
                    active_turn,
                    language,
                ),
            ))
        }
        SettingsKind::ApprovalPolicy => {
            let policy: Value =
                serde_json::from_str(requested).context("parse native approval policy choice")?;
            let settings = state.native.set_approval_policy_and_confirm(policy).await?;
            Ok(SettingChoiceOutcome::Applied(format!(
                "{}: {}",
                language.message(MessageKey::CommandsApprovalPolicyAppliedAndSavedA6d52828),
                format_approval_policy_with_language(&settings["approvalPolicy"], language),
            )))
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
            Ok(SettingChoiceOutcome::Applied(format!(
                "{} `{}`",
                language.message(MessageKey::CommandsInputDeliverySetToDd4696ee),
                input_mode(mode, language)
            )))
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
                bail!(
                    "{}",
                    language.message(MessageKey::CommandsVoiceNoLongerAvailable)
                );
            }
            let selected = if requested.is_empty() {
                None
            } else {
                Some(requested)
            };
            state.voice.set_voice(selected)?;
            Ok(SettingChoiceOutcome::Applied(
                language
                    .message(MessageKey::CommandsVoiceProfileSavedForTheNextCallTheActive468592de)
                    .to_owned(),
            ))
        }
        SettingsKind::Model => {
            let thread = state.native.refresh_thread_state().await?;
            ensure_settings_apply_target(
                expected_generation,
                state.native.current_generation(),
                expected_thread_id,
                &state.native.thread_id(),
            )?;
            let catalog = match model_catalog {
                Some(catalog) => catalog.clone(),
                None => state.native.model_catalog().await?,
            };
            let selection = model_selection_for_choice(&catalog, requested, &thread, language)?;
            let active_provider = thread_model_provider(&thread)
                .or_else(|| catalog.get("provider").and_then(Value::as_str));
            let active_executor = thread
                .get("modelExecutor")
                .cloned()
                .map(serde_json::from_value::<crate::model_catalog::ModelExecutor>)
                .transpose()
                .context("read current model executor")?
                .unwrap_or_default();
            if active_provider.is_none_or(|provider| {
                model_change_requires_resume(active_executor, provider, &selection)
            }) {
                let current_turn = state.current_turn.lock().await;
                ensure_provider_switch_idle(
                    input_phase
                        .as_ref()
                        .is_some_and(|phase| phase.allow_ordinary()),
                    current_turn.is_some(),
                    language,
                )?;
                let snapshot = state.native.set_model_selection(&selection).await?;
                return Ok(SettingChoiceOutcome::Applied(
                    format_settings_update_result_with_language(
                        &format!("{} — {}", selection.model, selection.provider),
                        Some(&snapshot),
                        None,
                        language,
                    ),
                ));
            }
            let requested = format!("{} — {}", selection.model, selection.provider);
            let expected = Value::String(selection.model.clone());
            let current_turn_params = state.current_turn.lock().await.clone().map(|turn_id| {
                json!({
                    "threadId":thread_id,
                    "turnId":turn_id,
                    "model":selection.model
                })
            });
            Ok(SettingChoiceOutcome::OperationStarted {
                request: crate::settings_operation::SettingRequest {
                    generation: expected_generation
                        .unwrap_or_else(|| state.native.current_generation()),
                    thread_id: thread_id.clone(),
                    field: "model",
                    params: json!({"threadId":thread_id,"model":selection.model}),
                    expected,
                    expected_model_scope: None,
                    model_selection: Some(selection),
                    current_turn_params,
                },
                requested,
            })
        }
        SettingsKind::SubagentModel | SettingsKind::Effort | SettingsKind::Fast => {
            let sub = match kind {
                SettingsKind::Model => "model",
                SettingsKind::SubagentModel => "model",
                SettingsKind::Effort => "effort",
                SettingsKind::Fast => "fast",
                _ => unreachable!(),
            };
            let thread = state.native.refresh_thread_state().await?;
            ensure_settings_apply_target(
                expected_generation,
                state.native.current_generation(),
                expected_thread_id,
                &state.native.thread_id(),
            )?;
            ensure_settings_model_scope(kind, &expected_model_scope, thread_model_scope(&thread))?;
            let current_model = thread
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let catalog = match model_catalog {
                Some(catalog) => catalog.clone(),
                None => state.native.model_catalog().await?,
            };
            if kind == SettingsKind::SubagentModel {
                let selection = if requested == "native-default" {
                    None
                } else {
                    Some(subagent_model_selection(&catalog, requested, language)?)
                };
                let mut preferences = state.native.read_native_feature_preferences()?;
                set_subagent_model_selection(&mut preferences, selection.as_ref());
                state
                    .native
                    .apply_native_feature_preferences_with_catalog(&preferences, Some(&catalog))
                    .await?;
                return Ok(SettingChoiceOutcome::Applied(child_default_saved_notice(
                    language,
                )));
            }
            validate_setting_choice_with_language(
                sub,
                requested,
                &catalog,
                &current_model,
                language,
            )
            .await?;
            let (field, value) = match kind {
                SettingsKind::Model => ("model", Value::String(requested.to_owned())),
                SettingsKind::SubagentModel => unreachable!(),
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
            let live_value = if kind == SettingsKind::Fast && requested.eq_ignore_ascii_case("off")
            {
                Value::Null
            } else {
                Value::String(requested.to_owned())
            };
            let current_turn_params = state.current_turn.lock().await.clone().map(|turn_id| {
                let mut live = json!({"threadId":thread_id,"turnId":turn_id});
                live[field] = live_value;
                live
            });
            Ok(SettingChoiceOutcome::OperationStarted {
                request: crate::settings_operation::SettingRequest {
                    generation: expected_generation
                        .unwrap_or_else(|| state.native.current_generation()),
                    thread_id: thread_id.clone(),
                    field,
                    params,
                    expected: value,
                    expected_model_scope: matches!(kind, SettingsKind::Effort | SettingsKind::Fast)
                        .then_some(expected_model_scope.clone()),
                    model_selection: None,
                    current_turn_params,
                },
                requested: requested.to_owned(),
            })
        }
        SettingsKind::NativeFeatures => bail!(
            "{}",
            language.message(MessageKey::CommandsChooseNativeFeatureFirst)
        ),
        SettingsKind::NativeFeature(key) => {
            let mut preferences = state.native.read_native_feature_preferences()?;
            let selection =
                if key == NativeFeatureKey::SubagentModel && requested != "native-default" {
                    Some(subagent_model_selection(
                        &match model_catalog {
                            Some(catalog) => catalog.clone(),
                            None => state.native.model_catalog().await?,
                        },
                        requested,
                        language,
                    )?)
                } else {
                    None
                };
            let selected = if requested == "native-default" {
                None
            } else {
                Some(
                    selection
                        .as_ref()
                        .map_or(requested, |value| value.model.as_str()),
                )
            };
            set_native_feature_preference(&mut preferences, key, selected)?;
            if key == NativeFeatureKey::SubagentModel {
                set_subagent_model_selection(&mut preferences, selection.as_ref());
            }
            state
                .native
                .apply_native_feature_preferences_with_catalog(
                    &preferences,
                    (key == NativeFeatureKey::SubagentModel)
                        .then_some(model_catalog)
                        .flatten(),
                )
                .await?;
            if key == NativeFeatureKey::SubagentModel {
                return Ok(SettingChoiceOutcome::Applied(child_default_saved_notice(
                    language,
                )));
            }
            Ok(SettingChoiceOutcome::Applied(
                language
                    .message(MessageKey::CommandsSavedForTheNextDiscodexWorkerStartRestart0f175de2)
                    .to_owned(),
            ))
        }
        SettingsKind::Hub => bail!(
            "{}",
            language.message(MessageKey::CommandsChooseSettingFirst)
        ),
        SettingsKind::MessageAccess
        | SettingsKind::PairedAccounts
        | SettingsKind::Whitelist
        | SettingsKind::Blacklist
        | SettingsKind::RegisteredChannels => {
            bail!(
                "{}",
                language.message(MessageKey::CommandsMessageAccessSettingsHaveASeparateMenu)
            )
        }
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
    // Viewing an already-visible table is not an operator-only settings action.
    if crate::table_messages::is_source_button(&interaction.data.custom_id) {
        return crate::table_messages::handle_component(ctx, interaction, state.config.language)
            .await;
    }
    authorize(
        &state,
        interaction.guild_id,
        interaction.user.id,
        state.config.language,
    )?;
    if interaction.data.custom_id.starts_with("asyncq:") {
        return crate::async_questions::handle_component(ctx, interaction, &state).await;
    }
    if interaction
        .data
        .custom_id
        .starts_with("discodex:agy-setup:")
    {
        return crate::discord_agy_setup::handle_component(ctx, interaction, &state).await;
    }
    if interaction
        .data
        .custom_id
        .starts_with("discodex:update-confirm:")
    {
        return crate::discord_updates::handle_confirmation(ctx, interaction, &state).await;
    }
    if interaction.data.custom_id.starts_with("discodex:update:") {
        return crate::discord_updates::handle_component(ctx, interaction, &state).await;
    }
    if interaction.data.custom_id.starts_with("codex:plugins:") {
        return handle_plugin_component(ctx, interaction, &state).await;
    }
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
    if matches!(decision, "plugin-install" | "plugin-check")
        && plugin_suggestion(
            &json!({"method": pending.method.clone(), "params": pending.params.clone()}),
        )
        .is_some()
    {
        return handle_plugin_suggestion_component(
            ctx,
            interaction,
            &state,
            key,
            decision,
            pending,
        )
        .await;
    }
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
                approval_delivery_failure_response(
                    &error,
                    state.config.language,
                    Some(&state.config),
                ),
            )
            .await?;
        return Ok(());
    }
    let response_edit = interaction
        .edit_response(
            ctx,
            serenity::builder::EditInteractionResponse::new().content(
                state
                    .config
                    .language
                    .message(MessageKey::CommandsChoiceSentToTheRequestB2a9b18b),
            ),
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

async fn handle_antigravity_settings_modal(
    ctx: &Context,
    interaction: &ModalInteraction,
    state: &Arc<BridgeState>,
) -> anyhow::Result<()> {
    let parts = interaction.data.custom_id.split(':').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "codex" || parts[1] != "agy-settings" {
        bail!("unknown Antigravity settings modal id");
    }
    let menu_id = parts[2];
    let field = parts[3];
    let menu = claim_settings_menu(
        &mut *state.settings_menus.lock().await,
        menu_id,
        interaction.user.id.get(),
    )
    .map_err(|_| anyhow!("This settings panel is no longer available to this user"))?;
    let result = async {
        interaction
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await?;
        let valid_field = match menu.kind {
            SettingsKind::Antigravity => field == "models",
            SettingsKind::AntigravitySetting(key) => {
                key.is_rule_list() && matches!(field, "add" | "remove")
            }
            _ => false,
        };
        if !valid_field
            || menu.connection_generation != state.native.current_generation()
            || menu.thread_id != state.native.thread_id()
        {
            bail!(
                state
                    .config
                    .language
                    .message(MessageKey::CommandsThisSettingsPanelIsNoLongerActiveOpenSet7d065fd5)
            );
        }
        let mut values = serde_json::Map::new();
        collect_modal_values(
            &serde_json::to_value(&interaction.data.components)?,
            &mut values,
        );
        {
            let _settings_change = state.settings_change_lock.lock().await;
            let mut preferences = state.native.read_native_feature_preferences()?;
            let value = values
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let SettingsKind::AntigravitySetting(key) = menu.kind {
                let value = value.trim();
                if value.is_empty() {
                    bail!(
                        "{}",
                        state
                            .config
                            .language
                            .message(MessageKey::CommandsAgyRuleRequired)
                    );
                }
                if field == "remove"
                    && key == AgySettingKey::Allow
                    && value == crate::agy_settings::MANAGED_MCP_ALLOW_RULE
                {
                    bail!(
                        "{}",
                        state
                            .config
                            .language
                            .message(MessageKey::CommandsAgyManagedRuleRequired)
                    );
                }
                let sharing = preferences.antigravity_share_settings.unwrap_or(false);
                let settings = crate::agy_settings::read(&state.config, sharing)?;
                let mut rules = agy_permission_rules(&settings, key, state.config.language)?;
                if field == "add" {
                    if !rules.iter().any(|rule| rule.as_str() == Some(value)) {
                        rules.push(Value::String(value.to_owned()));
                    }
                } else {
                    if !rules.iter().any(|rule| rule.as_str() == Some(value)) {
                        bail!(
                            "{}",
                            state
                                .config
                                .language
                                .message(MessageKey::CommandsAgyRuleNotFound)
                        );
                    }
                    rules.retain(|rule| rule.as_str() != Some(value));
                }
                crate::agy_settings::set(&state.config, sharing, key.key(), Value::Array(rules))?;
            } else {
                set_antigravity_text_preference(&mut preferences, field, value)?;
                state
                    .native
                    .apply_native_feature_preferences(&preferences)
                    .await?;
            }
        }
        let mut fresh = build_settings_menu(
            ctx,
            state,
            menu.kind,
            0,
            menu.requester_id,
            menu.connection_generation,
        )
        .await?;
        fresh.claimed = false;
        let (content, rows) = render_settings_menu(menu_id, &fresh, state.config.language);
        let chunks = split_discord_content(&content);
        interaction
            .edit_response(
                ctx,
                serenity::builder::EditInteractionResponse::new()
                    .content(chunks.first().cloned().unwrap_or_default())
                    .components(rows)
                    .allowed_mentions(no_mentions()),
            )
            .await?;
        state
            .settings_menus
            .lock()
            .await
            .insert(menu_id.to_owned(), fresh);
        for chunk in chunks.into_iter().skip(1) {
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
        Ok::<(), anyhow::Error>(())
    }
    .await;
    release_settings_menu(&mut *state.settings_menus.lock().await, menu_id);
    if let Err(error) = result {
        let detail = settings_error_detail(&error, state.config.language, Some(&state.config));
        tracing::warn!(%detail, "Antigravity settings modal failed");
        for chunk in split_discord_content(&format!(
            "{}\n{detail}",
            state
                .config
                .language
                .message(MessageKey::CommandsAgyCouldNotSaveSetting),
        )) {
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

async fn handle_message_access_modal(
    ctx: &Context,
    interaction: &ModalInteraction,
    state: &Arc<BridgeState>,
) -> anyhow::Result<()> {
    let parts = interaction.data.custom_id.split(':').collect::<Vec<_>>();
    if parts.len() != 5 || parts[0] != "codex" || parts[1] != "access" {
        bail!("unknown message access modal id");
    }
    let menu_id = parts[2];
    let kind = access_list_kind(parts[3]).ok_or_else(|| anyhow!("unknown message access list"))?;
    let add = match parts[4] {
        "add" => true,
        "remove" => false,
        _ => bail!("unknown message access operation"),
    };
    let menu = claim_settings_menu(
        &mut *state.settings_menus.lock().await,
        menu_id,
        interaction.user.id.get(),
    )
    .map_err(|_| anyhow!("This settings panel is no longer available to this user"))?;
    if menu.kind != kind
        || menu.connection_generation != state.native.current_generation()
        || menu.thread_id != state.native.thread_id()
    {
        release_settings_menu(&mut *state.settings_menus.lock().await, menu_id);
        interaction
            .create_response(
                ctx,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(state.config.language.message(
                            MessageKey::CommandsThisMessageAccessPanelIsNoLongerActiveOpB96a7437,
                        ))
                        .ephemeral(true),
                ),
            )
            .await?;
        return Ok(());
    }
    let raw_components = serde_json::to_value(&interaction.data.components)?;
    let mut values = serde_json::Map::new();
    collect_modal_values(&raw_components, &mut values);
    let raw_id = values.get("id").and_then(Value::as_str).unwrap_or_default();
    let id =
        match parse_message_access_id(raw_id) {
            Ok(id) => id,
            Err(()) => {
                release_settings_menu(&mut *state.settings_menus.lock().await, menu_id);
                interaction
                    .create_response(
                        ctx,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(state.config.language.message(
                                    MessageKey::CommandsEnterANonzeroDiscordNumericId5a3a739d,
                                ))
                                .ephemeral(true),
                        ),
                    )
                    .await?;
                return Ok(());
            }
        };
    if let Err(error) = interaction
        .create_response(ctx, CreateInteractionResponse::Acknowledge)
        .await
    {
        release_settings_menu(&mut *state.settings_menus.lock().await, menu_id);
        return Err(error.into());
    }
    let update = update_message_access_list(state, kind, add, id).await;
    if let Err(error) = update {
        release_settings_menu(&mut *state.settings_menus.lock().await, menu_id);
        let detail = crate::discord::sanitize_discord_error(&state.config, &error.to_string());
        let notice = localized_access_update_error(detail, state.config.language);
        interaction
            .edit_response(
                ctx,
                serenity::builder::EditInteractionResponse::new()
                    .content(notice)
                    .allowed_mentions(no_mentions()),
            )
            .await?;
        return Ok(());
    }
    let mut fresh = match build_settings_menu(
        ctx,
        state,
        kind,
        menu.page,
        menu.requester_id,
        menu.connection_generation,
    )
    .await
    {
        Ok(menu) => menu,
        Err(error) => {
            release_settings_menu(&mut *state.settings_menus.lock().await, menu_id);
            let detail = settings_error_detail(&error, state.config.language, Some(&state.config));
            interaction
                .edit_response(
                    ctx,
                    serenity::builder::EditInteractionResponse::new()
                        .content(localized_failure_message(
                            state.config.language.message(MessageKey::CommandsSavedButCouldNotRefreshTheListReopenSett05e90f73),
                            &detail,
                        ))
                        .allowed_mentions(no_mentions()),
                )
                .await?;
            return Ok(());
        }
    };
    fresh.claimed = false;
    let (content, rows) = render_settings_menu(menu_id, &fresh, state.config.language);
    let notice = state
        .config
        .language
        .message(MessageKey::CommandsAccountListUpdatedE892e513);
    let result = interaction
        .edit_response(
            ctx,
            serenity::builder::EditInteractionResponse::new()
                .content(format!("{notice}\n\n{content}"))
                .components(rows)
                .allowed_mentions(no_mentions()),
        )
        .await;
    if result.is_ok() {
        state
            .settings_menus
            .lock()
            .await
            .insert(menu_id.to_owned(), fresh);
    } else {
        release_settings_menu(&mut *state.settings_menus.lock().await, menu_id);
    }
    result.context("refresh message access settings panel")?;
    Ok(())
}

fn parse_message_access_id(input: &str) -> Result<u64, ()> {
    let id = input.trim().parse::<u64>().map_err(|_| ())?;
    if id == 0 {
        return Err(());
    }
    Ok(id)
}

fn localized_access_update_error(error: String, language: crate::i18n::Language) -> String {
    if error.contains("already registered") {
        language
            .message(MessageKey::CommandsThatIdIsAlreadyInThisList70f3b901)
            .to_owned()
    } else if error.contains("not registered") {
        language
            .message(MessageKey::CommandsThatIdIsNotInThisList15844bf5)
            .to_owned()
    } else {
        format!(
            "{}: {error}",
            language.message(MessageKey::CommandsCouldNotSaveMessageAccessSettingsDfe7562c)
        )
    }
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
    if interaction.data.custom_id.starts_with("asyncq:") {
        return crate::async_questions::handle_modal(ctx, interaction, &state).await;
    }
    if interaction.data.custom_id.starts_with("discodex:agy-code:") {
        return crate::discord_agy_setup::handle_modal(ctx, interaction, &state).await;
    }
    if interaction.data.custom_id.starts_with("codex:access:") {
        return handle_message_access_modal(ctx, interaction, &state).await;
    }
    if interaction
        .data
        .custom_id
        .starts_with("codex:agy-settings:")
    {
        return handle_antigravity_settings_modal(ctx, interaction, &state).await;
    }
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
            let detail = settings_error_detail(&error, state.config.language, Some(&state.config));
            tracing::warn!(error = %detail, "failed to read Discord modal answer");
            interaction
                .edit_response(
                    ctx,
                    serenity::builder::EditInteractionResponse::new()
                        .content(localized_failure_message(
                        state.config.language.message(
                            MessageKey::CommandsCouldNotReadTheAnswerUseTheDisplayedFormBe586209,
                        ),
                        &detail,
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
                approval_delivery_failure_response(
                    &error,
                    state.config.language,
                    Some(&state.config),
                ),
            )
            .await?;
        return Ok(());
    }
    let response_edit = interaction
        .edit_response(
            ctx,
            serenity::builder::EditInteractionResponse::new().content(
                state
                    .config
                    .language
                    .message(MessageKey::CommandsAnswerSentToTheRequestDaffc8b1),
            ),
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
            source_guild: None,
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

fn approval_delivery_failure_message(
    error: &anyhow::Error,
    language: crate::i18n::Language,
    config: Option<&crate::config::Config>,
) -> String {
    format!(
        "{}\n{}",
        language.message(MessageKey::CommandsTheApprovalResponseCouldNotBeConfirmedAs36c4b12d),
        settings_error_detail(error, language, config)
    )
}

fn approval_delivery_failure_response(
    error: &anyhow::Error,
    language: crate::i18n::Language,
    config: Option<&crate::config::Config>,
) -> serenity::builder::EditInteractionResponse {
    serenity::builder::EditInteractionResponse::new()
        .content(approval_delivery_failure_message(error, language, config))
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
                language.message(MessageKey::CommandsAllowHost420b5fac),
                ButtonStyle::Primary,
            ),
            Some("deny") => (
                language.message(MessageKey::CommandsDenyHostFd5846c5),
                ButtonStyle::Secondary,
            ),
            _ => (
                language.message(MessageKey::CommandsHostRule795adbfa),
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
            language.message(MessageKey::CommandsAllow8557860a),
            ButtonStyle::Success,
        ),
        Some("acceptForSession") => (
            language.message(MessageKey::CommandsAllowForSession5daec59d),
            ButtonStyle::Primary,
        ),
        Some("decline") => (
            language.message(MessageKey::CommandsDeclineAndContinue2f0154e7),
            ButtonStyle::Danger,
        ),
        Some("cancel") => (
            language.message(MessageKey::CommandsDeclineAndStopFa4203c2),
            ButtonStyle::Secondary,
        ),
        _ if decision.get("acceptWithExecpolicyAmendment").is_some() => (
            language.message(MessageKey::CommandsAllowWithPolicyChangeC36cdbe2),
            ButtonStyle::Primary,
        ),
        _ => (
            language.message(MessageKey::CommandsChoose92fc12fb),
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
        && let Some(suggestion) = plugin_suggestion(&json!({"method": method, "params": params}))
    {
        let labels = suggestion_labels(language);
        let install_label = if is_plugin_suggestion(&suggestion) {
            language.message(MessageKey::CommandsInstall92cf4534)
        } else {
            language.message(MessageKey::CommandsOpenSetup5b5190a9)
        };
        let install = CreateButton::new(format!("codex:approval:{key}:plugin-install"))
            .label(install_label)
            .style(ButtonStyle::Success);
        let install = if !is_plugin_suggestion(&suggestion) {
            suggestion
                .install_url
                .as_deref()
                .map(|url| CreateButton::new_link(url).label(install_label))
                .unwrap_or(install)
        } else {
            install
        };
        let mut buttons = vec![install];
        if !is_plugin_suggestion(&suggestion) && suggestion.install_url.is_some() {
            buttons.push(
                CreateButton::new(format!("codex:approval:{key}:plugin-check"))
                    .label(language.message(MessageKey::CommandsSetupCompletedCheckAccess11750254))
                    .style(ButtonStyle::Primary),
            );
        }
        buttons.extend([
            CreateButton::new(format!("codex:approval:{key}:plugin-decline"))
                .label(labels.decline)
                .style(ButtonStyle::Secondary),
            CreateButton::new(format!("codex:approval:{key}:plugin-disable"))
                .label(labels.decline_and_remember)
                .style(ButtonStyle::Danger),
            CreateButton::new(format!("codex:approval:{key}:cancel"))
                .label(labels.cancel)
                .style(ButtonStyle::Secondary),
        ]);
        return vec![CreateActionRow::Buttons(buttons)];
    }
    if method == "mcpServer/elicitation/request"
        && let Some(persist_options) = mcp_tool_approval_persist_options(params)
    {
        let mut buttons = vec![
            CreateButton::new(format!("codex:approval:{key}:mcp-allow"))
                .label(language.message(MessageKey::CommandsAllowOnce3fb701fe))
                .style(ButtonStyle::Success),
        ];
        if persist_options.contains(&"session") {
            buttons.push(
                CreateButton::new(format!("codex:approval:{key}:mcp-session"))
                    .label(language.message(MessageKey::CommandsAllowForSession5daec59d))
                    .style(ButtonStyle::Primary),
            );
        }
        if persist_options.contains(&"always") {
            buttons.push(
                CreateButton::new(format!("codex:approval:{key}:mcp-always"))
                    .label(language.message(MessageKey::CommandsAlwaysAllow73a593b2))
                    .style(ButtonStyle::Primary),
            );
        }
        buttons.extend([
            CreateButton::new(format!("codex:approval:{key}:decline"))
                .label(language.message(MessageKey::CommandsDecline4a93815a))
                .style(ButtonStyle::Danger),
            CreateButton::new(format!("codex:approval:{key}:cancel"))
                .label(language.message(MessageKey::CommandsCancel4973805f))
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
                .label(language.message(MessageKey::CommandsDecline4a93815a))
                .style(ButtonStyle::Danger),
            CreateButton::new(format!("codex:approval:{key}:cancel"))
                .label(language.message(MessageKey::CommandsCancel4973805f))
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
                language.message(MessageKey::CommandsAllow8557860a),
                ButtonStyle::Success,
            ),
            (
                "acceptForSession",
                language.message(MessageKey::CommandsAllowForSession5daec59d),
                ButtonStyle::Primary,
            ),
            (
                "decline",
                language.message(MessageKey::CommandsDecline4a93815a),
                ButtonStyle::Danger,
            ),
            (
                "cancel",
                language.message(MessageKey::CommandsDeclineAndStopFa4203c2),
                ButtonStyle::Secondary,
            ),
        ],
        "item/permissions/requestApproval" => &[
            (
                "turn",
                language.message(MessageKey::CommandsAllowThisResponseBab75358),
                ButtonStyle::Success,
            ),
            (
                "session",
                language.message(MessageKey::CommandsAllowForSession5daec59d),
                ButtonStyle::Primary,
            ),
            (
                "none",
                language.message(MessageKey::CommandsDoNotGrantMorePermissions850c5a39),
                ButtonStyle::Danger,
            ),
        ],
        "item/tool/requestUserInput" => &[(
            "answer",
            language.message(MessageKey::CommandsWriteAResponseB91cf891),
            ButtonStyle::Primary,
        )],
        "mcpServer/elicitation/request" => &[
            (
                "answer",
                language.message(MessageKey::CommandsWriteAResponseB91cf891),
                ButtonStyle::Primary,
            ),
            (
                "decline",
                language.message(MessageKey::CommandsDecline4a93815a),
                ButtonStyle::Danger,
            ),
            (
                "cancel",
                language.message(MessageKey::CommandsCancel4973805f),
                ButtonStyle::Secondary,
            ),
        ],
        "execCommandApproval" => &[
            (
                "approved",
                language.message(MessageKey::CommandsAllow8557860a),
                ButtonStyle::Success,
            ),
            (
                "approved_for_session",
                language.message(MessageKey::CommandsAllowForSession5daec59d),
                ButtonStyle::Primary,
            ),
            (
                "denied",
                language.message(MessageKey::CommandsDeclineAndContinue2f0154e7),
                ButtonStyle::Danger,
            ),
            (
                "abort",
                language.message(MessageKey::CommandsCancelResponse19bbaa6e),
                ButtonStyle::Secondary,
            ),
        ],
        "applyPatchApproval" => &[
            (
                "approved",
                language.message(MessageKey::CommandsAllow8557860a),
                ButtonStyle::Success,
            ),
            (
                "approved_for_session",
                language.message(MessageKey::CommandsAllowForSession5daec59d),
                ButtonStyle::Primary,
            ),
            (
                "denied",
                language.message(MessageKey::CommandsDeclineAndContinue2f0154e7),
                ButtonStyle::Danger,
            ),
            (
                "abort",
                language.message(MessageKey::CommandsCancelResponse19bbaa6e),
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
            if plugin_suggestion(&json!({"method": method, "params": pending.params.clone()}))
                .is_some()
            {
                let decision = match decision {
                    "plugin-install" => bail!(
                        "plugin install choices must use the native install and verification flow"
                    ),
                    "plugin-decline" => SuggestionDecision::Decline,
                    "plugin-disable" => SuggestionDecision::DeclineAndRemember,
                    "cancel" => SuggestionDecision::Cancel,
                    _ => bail!("unsupported plugin suggestion choice: {decision}"),
                };
                Ok(plugin_suggestion_response(decision))
            } else if mcp_tool_approval_persist_options(&pending.params).is_some() {
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

fn thread_model_executor(thread: &Value) -> Option<crate::model_catalog::ModelExecutor> {
    thread
        .get("modelExecutor")
        .or_else(|| thread.get("model_executor"))
        .cloned()
        .map(serde_json::from_value::<crate::model_catalog::ModelExecutor>)
        .transpose()
        .ok()
        .map(Option::unwrap_or_default)
}

fn thread_model_provider(thread: &Value) -> Option<&str> {
    if thread_model_executor(thread)? == crate::model_catalog::ModelExecutor::Antigravity {
        Some("antigravity")
    } else {
        thread.get("modelProvider").and_then(Value::as_str)
    }
}

fn thread_model_scope(thread: &Value) -> Option<String> {
    crate::settings_operation::model_scope(thread)
}

fn normalized_model_value(catalog: &Value, value: &str) -> Option<String> {
    normalized_model_value_with_language(catalog, value, crate::i18n::Language::English)
}

fn normalized_model_value_with_language(
    catalog: &Value,
    value: &str,
    language: crate::i18n::Language,
) -> Option<String> {
    let model = selected_model_with_language(catalog, &Some(value.to_owned()), language).ok()?;
    crate::model_catalog::selection_value(model)
        .or_else(|| crate::model_catalog::model_id(model))
        .map(str::to_owned)
}

fn subagent_preference_scope(
    preferences: &crate::native::NativeFeaturePreferences,
    catalog: &Value,
) -> Option<String> {
    let model = preferences.subagent_model.as_deref()?;
    let executor = preferences
        .subagent_model_executor
        .as_deref()
        .map(|executor| {
            serde_json::from_value::<crate::model_catalog::ModelExecutor>(json!(executor))
        })
        .transpose()
        .ok()?
        .unwrap_or_default();
    let provider = preferences
        .subagent_model_provider
        .as_deref()
        .or_else(|| catalog.get("catalogProvider").and_then(Value::as_str))
        .or_else(|| catalog.get("provider").and_then(Value::as_str));
    Some(match provider {
        Some(provider) => crate::model_catalog::ModelSelection {
            executor,
            provider: provider.to_owned(),
            model: model.to_owned(),
        }
        .value(),
        None => model.to_owned(),
    })
}

fn subagent_model_selection(
    catalog: &Value,
    requested: &str,
    _language: crate::i18n::Language,
) -> anyhow::Result<crate::model_catalog::ModelSelection> {
    crate::model_catalog::resolve_selection(catalog, requested)
}

fn model_change_requires_resume(
    active_executor: crate::model_catalog::ModelExecutor,
    active_provider: &str,
    selection: &crate::model_catalog::ModelSelection,
) -> bool {
    selection.executor != active_executor || selection.provider != active_provider
}

fn model_selection_for_choice(
    catalog: &Value,
    requested: &str,
    current_thread: &Value,
    language: crate::i18n::Language,
) -> anyhow::Result<crate::model_catalog::ModelSelection> {
    if let Ok(selection) = crate::model_catalog::resolve_selection(catalog, requested) {
        return Ok(selection);
    }

    // Stale settings buttons can carry the full provider-qualified selection
    // created from the earlier catalog. Preserve it and let Native validate it.
    if let Ok(parts) = serde_json::from_str::<Vec<String>>(requested) {
        let selection = match parts.as_slice() {
            [provider, model] if !provider.is_empty() && !model.is_empty() => {
                Some(crate::model_catalog::ModelSelection {
                    executor: crate::model_catalog::ModelExecutor::Codex,
                    provider: provider.clone(),
                    model: model.clone(),
                })
            }
            [executor, provider, model] if !provider.is_empty() && !model.is_empty() => {
                let executor = match executor.as_str() {
                    "codex" => Some(crate::model_catalog::ModelExecutor::Codex),
                    "antigravity" => Some(crate::model_catalog::ModelExecutor::Antigravity),
                    _ => None,
                };
                executor.map(|executor| crate::model_catalog::ModelSelection {
                    executor,
                    provider: provider.clone(),
                    model: model.clone(),
                })
            }
            _ => None,
        };
        if let Some(selection) = selection {
            return Ok(selection);
        }
    }

    // Legacy catalog entries can contain only a model ID. Reuse the active
    // executor/provider for that explicitly selected ID; Native remains the
    // authority on whether the model can actually be applied.
    let provider = thread_model_provider(current_thread)
        .or_else(|| catalog.get("provider").and_then(Value::as_str));
    let Some(provider) = provider else {
        bail!(
            "{}",
            language.message(MessageKey::CommandsModelProviderUnavailable)
        );
    };
    let executor = current_thread
        .get("modelExecutor")
        .cloned()
        .map(serde_json::from_value::<crate::model_catalog::ModelExecutor>)
        .transpose()
        .context("read current model executor")?
        .unwrap_or_default();
    Ok(crate::model_catalog::ModelSelection {
        executor,
        provider: provider.to_owned(),
        model: requested.to_owned(),
    })
}

fn ensure_provider_switch_idle(
    voice_idle: bool,
    turn_active: bool,
    language: crate::i18n::Language,
) -> anyhow::Result<()> {
    if !voice_idle || turn_active {
        bail!(
            "{}",
            language.message(MessageKey::CommandsWaitForTheActiveResponseOrVoiceCallToFiA5ef13df)
        );
    }
    Ok(())
}

fn catalog_model_choices(catalog: &Value) -> Vec<SettingsChoice> {
    crate::model_catalog::model_choices(catalog)
        .into_iter()
        .map(|(value, label)| SettingsChoice { value, label })
        .collect()
}

fn subagent_model_choices(catalog: &Value, language: crate::i18n::Language) -> Vec<SettingsChoice> {
    let mut choices = vec![SettingsChoice {
        value: "native-default".to_owned(),
        label: language
            .message(MessageKey::CommandsNativeDefault308250fc)
            .to_owned(),
    }];
    choices.extend(catalog_model_choices(catalog));
    choices
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
    validate_setting_choice_with_language(
        sub,
        requested,
        catalog,
        current_model,
        crate::i18n::Language::English,
    )
    .await
}

async fn validate_setting_choice_with_language(
    sub: &str,
    requested: &str,
    catalog: &Value,
    current_model: &Option<String>,
    language: crate::i18n::Language,
) -> anyhow::Result<()> {
    match sub {
        // Model choices are presentation data. Native is the authority for
        // whether an explicitly selected exact ID can be applied.
        "model" => {}
        "effort" => {
            let model = selected_model_with_language(catalog, current_model, language)?;
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
                })
                || model
                    .get("reasoningEffortAliases")
                    .and_then(Value::as_array)
                    .is_some_and(|aliases| {
                        aliases.iter().any(|alias| {
                            alias.get("requested").and_then(Value::as_str) == Some(requested)
                        })
                    });
            if !valid {
                bail!(
                    "{}",
                    language
                        .message(MessageKey::CommandsEffortUnavailableForModel)
                        .replace("{value}", requested)
                );
            }
        }
        "fast" => {
            if requested.eq_ignore_ascii_case("off") {
                return Ok(());
            }
            let model = selected_model_with_language(catalog, current_model, language)?;
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
                    "{}",
                    language
                        .message(MessageKey::CommandsTierUnavailableForModel)
                        .replace("{value}", requested)
                );
            }
        }
        _ => {}
    }
    Ok(())
}

fn selected_model<'a>(
    catalog: &'a Value,
    current_model: &Option<String>,
) -> anyhow::Result<&'a Value> {
    selected_model_with_language(catalog, current_model, crate::i18n::Language::English)
}

fn selected_model_with_language<'a>(
    catalog: &'a Value,
    current_model: &Option<String>,
    language: crate::i18n::Language,
) -> anyhow::Result<&'a Value> {
    let data = catalog
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!(language.message(MessageKey::CommandsModelListUnreadable)))?;
    let current_model = current_model
        .as_deref()
        .ok_or_else(|| anyhow!(language.message(MessageKey::CommandsCurrentModelUnavailable)))?;
    let selected =
        crate::model_catalog::resolve_selection(catalog, current_model).map_err(|_| {
            anyhow!(language.message(MessageKey::CommandsCurrentModelMissingFromCatalog))
        })?;
    data.iter()
        .find(|model| {
            crate::model_catalog::model_id(model) == Some(selected.model.as_str())
                && model
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or(selected.provider.as_str())
                    == selected.provider
        })
        .ok_or_else(|| {
            anyhow!(language.message(MessageKey::CommandsCurrentModelMissingFromCatalog))
        })
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
            Some("applied") => language
                .message(MessageKey::CommandsUpdatedSettingsForLaterWorkInTheActiveReC44c334f),
            Some("targetUnavailable") => language
                .message(MessageKey::CommandsCouldNotApplySettingsToTheActiveResponseCeec6b12),
            _ => language
                .message(MessageKey::CommandsCouldNotConfirmWhetherSettingsWereAppliedAce88396),
        },
        Some(Err(_)) => {
            language.message(MessageKey::CommandsCouldNotSendTheSettingsUpdateToTheActiveF50c2a86)
        }
        None => language.message(MessageKey::CommandsNoResponseIsInProgress16302856),
    }
}

fn current_turn_notice(
    result: Option<&anyhow::Result<Value>>,
    language: crate::i18n::Language,
    config: Option<&crate::config::Config>,
) -> String {
    let mut notice = current_turn_settings_result_with_language(result, language).to_owned();
    if let Some(Err(error)) = result {
        notice.push('\n');
        notice.push_str(&settings_error_detail(error, language, config));
    }
    notice
}

fn format_pending_settings_update(
    requested: &str,
    current_response: &str,
    language: crate::i18n::Language,
) -> String {
    format!(
        "{}: `{requested}`\n{}\n{}: {current_response}",
        language.message(MessageKey::CommandsRequestedValue9eb15821),
        language.message(MessageKey::CommandsSettingsDefaultPending),
        language.message(MessageKey::CommandsCurrentResponse5c4a065c),
    )
}

fn format_confirmed_settings_update(
    requested: &str,
    snapshot: &Value,
    current_response: &str,
    language: crate::i18n::Language,
) -> String {
    format!(
        "{}: `{requested}`\n{}:\n{}\n{}: {current_response}",
        language.message(MessageKey::CommandsRequestedValue9eb15821),
        language.message(MessageKey::CommandsConversationDefaultsAfterUpdate4806be50),
        format_settings_with_language(&settings_snapshot(snapshot), language),
        language.message(MessageKey::CommandsCurrentResponse5c4a065c),
    )
}

fn format_setting_operation_result(
    requested: &str,
    outcome: &crate::settings_operation::SettingOutcome,
    state: &BridgeState,
) -> (String, bool, bool) {
    let language = state.config.language;
    let current_response = match &outcome.current_turn {
        crate::settings_operation::CurrentTurnOutcome::NotRequested => {
            current_turn_notice(None, language, Some(&state.config))
        }
        crate::settings_operation::CurrentTurnOutcome::Applied(response)
        | crate::settings_operation::CurrentTurnOutcome::Unconfirmed(response) => {
            let result: anyhow::Result<Value> = Ok(response.clone());
            current_turn_notice(Some(&result), language, Some(&state.config))
        }
        crate::settings_operation::CurrentTurnOutcome::Failed(error) => {
            let result: anyhow::Result<Value> = Err(anyhow!("{error:#}"));
            current_turn_notice(Some(&result), language, Some(&state.config))
        }
    };
    let confirmed = matches!(
        &outcome.application,
        crate::settings_operation::ApplicationOutcome::Applied(_)
    );
    let terminal_only = outcome.superseded
        || !outcome.target_current
        || matches!(
            &outcome.application,
            crate::settings_operation::ApplicationOutcome::Superseded
                | crate::settings_operation::ApplicationOutcome::StaleTarget
        );
    let mut notice = match &outcome.application {
        crate::settings_operation::ApplicationOutcome::Applied(snapshot) => {
            format_confirmed_settings_update(requested, snapshot, &current_response, language)
        }
        crate::settings_operation::ApplicationOutcome::Superseded => language
            .message(MessageKey::SettingsOperationSuperseded)
            .to_owned(),
        crate::settings_operation::ApplicationOutcome::StaleTarget => language
            .message(MessageKey::SettingsOperationStaleTarget)
            .to_owned(),
        crate::settings_operation::ApplicationOutcome::Failed(error) => localized_failure_message(
            language.message(MessageKey::CommandsCouldNotConfirmWhetherSettingsWereAppliedAce88396),
            &settings_error_detail(error, language, Some(&state.config)),
        ),
    };
    if let crate::settings_operation::PersistenceOutcome::Failed(error) = &outcome.persistence {
        notice = format!(
            "{}\n{}\n{}",
            language.message(MessageKey::CommandsModelDefaultConfirmedButSavedSelectionFailed),
            notice,
            settings_error_detail(error, language, Some(&state.config))
        );
    }
    if outcome.superseded {
        notice.push('\n');
        notice.push_str(language.message(MessageKey::SettingsOperationSuperseded));
    } else if !outcome.target_current && confirmed {
        notice.push('\n');
        notice.push_str(language.message(MessageKey::SettingsOperationStaleTarget));
    }
    if let crate::settings_operation::CurrentTurnOutcome::Failed(error) = &outcome.current_turn {
        tracing::error!(
            error = %settings_error_detail(error, language, Some(&state.config)),
            "current-turn settings update failed"
        );
    }
    (notice, confirmed, terminal_only)
}

async fn deliver_settings_operation_fallback(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &BridgeState,
    notice: &str,
) -> SettingsFallbackDelivery {
    let mut delivery = SettingsFallbackDelivery::default();
    for chunk in split_discord_content(notice) {
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
            let error = anyhow::Error::new(error);
            let detail = settings_error_detail(&error, state.config.language, Some(&state.config));
            delivery.record_failure(detail.clone());
            tracing::error!(
                error = %detail,
                "settings terminal result follow-up delivery failed"
            );
            let fallback = localized_failure_message(
                state
                    .config
                    .language
                    .message(MessageKey::CommandsDiscordErrorLabel),
                &format!("{notice}\n{detail}"),
            );
            crate::discord::report_discord_failure(
                &ctx.http,
                &state.config,
                ChannelId::new(state.config.home_channel_id),
                &fallback,
            )
            .await;
            break;
        }
        delivery.record_success();
    }
    delivery
}

enum PanelDelivery {
    Delivered(String),
    Failed(String),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SettingsFallbackDelivery {
    delivered_chunks: usize,
    failure: Option<String>,
}

impl SettingsFallbackDelivery {
    fn record_success(&mut self) {
        self.delivered_chunks += 1;
    }

    fn record_failure(&mut self, diagnostic: String) {
        self.failure = Some(diagnostic);
    }

    fn is_fully_delivered(&self, expected_chunks: usize) -> bool {
        self.failure.is_none() && self.delivered_chunks == expected_chunks
    }
}

enum SettingsPanelDecision {
    Update(String),
    Fallback {
        notice: String,
        panel_failure: Option<String>,
        observer_ended: bool,
    },
}

fn settings_panel_decision(
    panel: Result<PanelDelivery, ()>,
    completed_notice: &str,
    update_allowed: bool,
    language: crate::i18n::Language,
) -> SettingsPanelDecision {
    match panel {
        Ok(PanelDelivery::Delivered(menu_id)) if update_allowed => {
            SettingsPanelDecision::Update(menu_id)
        }
        Ok(PanelDelivery::Delivered(_)) => SettingsPanelDecision::Fallback {
            notice: completed_notice.to_owned(),
            panel_failure: None,
            observer_ended: false,
        },
        Ok(PanelDelivery::Failed(panel_error)) => {
            let notice = format!(
                "{completed_notice}\n{}: {panel_error}",
                language.message(MessageKey::CommandsDiscordErrorLabel)
            );
            SettingsPanelDecision::Fallback {
                notice,
                panel_failure: Some(panel_error),
                observer_ended: false,
            }
        }
        Err(()) => {
            let observer_error = language.message(MessageKey::SettingsOperationPanelObserverEnded);
            let notice = format!(
                "{completed_notice}\n{}: {observer_error}",
                language.message(MessageKey::CommandsDiscordErrorLabel)
            );
            SettingsPanelDecision::Fallback {
                notice,
                panel_failure: None,
                observer_ended: true,
            }
        }
    }
}

fn spawn_settings_confirmation(
    ctx: Context,
    interaction: ComponentInteraction,
    state: Arc<BridgeState>,
    kind: SettingsKind,
    page: usize,
    requester_id: u64,
    generation: u64,
    thread_id: String,
    model_catalog: Option<Value>,
    request: crate::settings_operation::SettingRequest,
    requested: String,
    interaction_id: u64,
    panel_result: tokio::sync::oneshot::Receiver<PanelDelivery>,
) {
    tokio::spawn(async move {
        let requested_field = request.field;
        let expected_value = request.expected.clone();
        let _input_phase = if kind == SettingsKind::Model {
            Some(state.voice.input_gate.state.lock().await)
        } else {
            None
        };
        let _settings_change = state.settings_change_lock.lock().await;
        let (dispatched_tx, dispatched_rx) = tokio::sync::oneshot::channel();
        let apply = state.settings_operations.apply_setting_with_dispatch(
            &state.native,
            request,
            Some(dispatched_tx),
        );
        tokio::pin!(apply);
        let outcome = tokio::select! {
            outcome = &mut apply => {
                drop(_input_phase);
                drop(_settings_change);
                outcome
            }
            _ = dispatched_rx => {
                drop(_input_phase);
                drop(_settings_change);
                apply.await
            }
        };
        let language = state.config.language;
        let (update_confirmed, current_applied, mut notice, terminal_only, target_current) =
            match outcome {
                Ok(outcome) => {
                    let current_applied = matches!(
                        &outcome.current_turn,
                        crate::settings_operation::CurrentTurnOutcome::NotRequested
                            | crate::settings_operation::CurrentTurnOutcome::Applied(_)
                    );
                    let (notice, confirmed, terminal_only) =
                        format_setting_operation_result(&requested, &outcome, &state);
                    let error_detail = match (
                        &outcome.application,
                        &outcome.persistence,
                        &outcome.current_turn,
                    ) {
                        (crate::settings_operation::ApplicationOutcome::Failed(error), _, _)
                        | (_, crate::settings_operation::PersistenceOutcome::Failed(error), _)
                        | (_, _, crate::settings_operation::CurrentTurnOutcome::Failed(error)) => {
                            Some(settings_error_detail(error, language, Some(&state.config)))
                        }
                        _ => None,
                    };
                    if let Some(detail) = error_detail.as_deref() {
                        tracing::error!(
                            event = "settings_operation_diagnostic",
                            error = %detail,
                            "settings operation contains a failed stage"
                        );
                    }
                    if let Some(checks) = &state.installation_checks {
                        let defaults_match = match &outcome.application {
                            crate::settings_operation::ApplicationOutcome::Applied(snapshot) => {
                                snapshot.get(requested_field) == Some(&expected_value)
                            }
                            _ => false,
                        };
                        checks.settings_confirmed(
                            interaction_id,
                            &thread_id,
                            defaults_match,
                            current_applied,
                            error_detail.as_deref(),
                        );
                    }
                    (
                        confirmed,
                        current_applied,
                        notice,
                        terminal_only,
                        outcome.target_current,
                    )
                }
                Err(error) => {
                    let detail = settings_error_detail(&error, language, Some(&state.config));
                    tracing::error!(
                        event = "settings_operation_worker_error",
                        error = %detail,
                        "settings operation completed without an application outcome"
                    );
                    if let Some(checks) = &state.installation_checks {
                        checks.settings_confirmed(
                            interaction_id,
                            &thread_id,
                            false,
                            false,
                            Some(&detail),
                        );
                    }
                    (
                    false,
                    false,
                    localized_failure_message(
                        language.message(
                            MessageKey::CommandsCouldNotConfirmWhetherSettingsWereAppliedAce88396,
                        ),
                        &detail,
                    ),
                    false,
                    false,
                )
                }
            };

        tracing::info!(
            event = "settings_operation_completed",
            applied = update_confirmed,
            current_turn_applied = current_applied,
            "native settings operation reached a terminal result"
        );

        // The operation is worker-owned before the Discord panel edit starts.
        // A failed/dropped panel observer cannot cancel native confirmation.
        let panel_decision = settings_panel_decision(
            panel_result.await.map_err(|_| ()),
            &notice,
            !terminal_only && target_current,
            language,
        );
        let pending_menu_id = match panel_decision {
            SettingsPanelDecision::Update(id) => id,
            SettingsPanelDecision::Fallback {
                notice,
                panel_failure,
                observer_ended,
            } => {
                if let Some(panel_error) = panel_failure {
                    tracing::error!(
                        event = "settings_pending_panel_delivery_failed",
                        error = %panel_error,
                        "settings operation continued after its pending panel could not be delivered"
                    );
                } else if observer_ended {
                    tracing::error!(
                        error = language.message(MessageKey::SettingsOperationPanelObserverEnded),
                        "settings panel result observer ended"
                    );
                } else {
                    tracing::info!(
                        event = "settings_terminal_result_using_fallback",
                        "settings terminal result is being delivered through the fallback channel"
                    );
                }
                let _ =
                    deliver_settings_operation_fallback(&ctx, &interaction, &state, &notice).await;
                return;
            }
        };

        if state.native.current_generation() != generation || state.native.thread_id() != thread_id
        {
            deliver_settings_operation_fallback(&ctx, &interaction, &state, &notice).await;
            return;
        }

        let current = {
            let menus = state.settings_menus.lock().await;
            menus.get(&pending_menu_id).is_some_and(|menu| {
                !menu.claimed
                    && menu.connection_generation == generation
                    && menu.thread_id == thread_id
            })
        };
        if !current {
            deliver_settings_operation_fallback(&ctx, &interaction, &state, &notice).await;
            return;
        }
        let refreshed = match build_settings_menu_with_catalog(
            &ctx,
            &state,
            kind,
            page,
            requester_id,
            generation,
            model_catalog.as_ref(),
        )
        .await
        {
            Ok(menu) => menu,
            Err(error) => {
                report_settings_confirmation_delivery_failure(
                    &ctx,
                    &interaction,
                    &state,
                    &error,
                    update_confirmed,
                    &notice,
                )
                .await;
                return;
            }
        };
        let still_current = state.native.current_generation() == generation
            && state.native.thread_id() == thread_id
            && {
                let menus = state.settings_menus.lock().await;
                menus.get(&pending_menu_id).is_some_and(|menu| {
                    !menu.claimed
                        && menu.connection_generation == generation
                        && menu.thread_id == thread_id
                })
            };
        if !still_current {
            deliver_settings_operation_fallback(&ctx, &interaction, &state, &notice).await;
            return;
        }
        if let Err(error) = update_settings_panel_with_notice(
            &ctx,
            &interaction,
            &state,
            &pending_menu_id,
            refreshed,
            &notice,
        )
        .await
        {
            report_settings_confirmation_delivery_failure(
                &ctx,
                &interaction,
                &state,
                &error,
                update_confirmed,
                &notice,
            )
            .await;
        }
    });
}

async fn report_settings_confirmation_delivery_failure(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &BridgeState,
    error: &anyhow::Error,
    update_confirmed: bool,
    setting_notice: &str,
) {
    let language = state.config.language;
    let detail = settings_error_detail(error, language, Some(&state.config));
    let prefix = if update_confirmed {
        language.message(MessageKey::CommandsSettingWasConfirmedButSettingsPanelCouldNotBeUpdated)
    } else {
        language.message(MessageKey::CommandsCouldNotConfirmWhetherSettingsWereAppliedAce88396)
    };
    let message = localized_failure_message(prefix, &format!("{setting_notice}\n{detail}"));
    let mut followup_failed = None;
    for chunk in split_discord_content(&message) {
        if let Err(delivery_error) = interaction
            .create_followup(
                ctx,
                serenity::builder::CreateInteractionResponseFollowup::new()
                    .content(chunk)
                    .ephemeral(true)
                    .allowed_mentions(no_mentions()),
            )
            .await
        {
            followup_failed = Some(anyhow::Error::new(delivery_error));
            break;
        }
    }
    if let Some(delivery_error) = followup_failed {
        let delivery_detail = settings_error_detail(&delivery_error, language, Some(&state.config));
        let fallback = localized_failure_message(
            prefix,
            &format!(
                "{setting_notice}\n{detail}\n{}: {delivery_detail}",
                language.message(MessageKey::CommandsDiscordErrorLabel)
            ),
        );
        crate::discord::report_discord_failure(
            &ctx.http,
            &state.config,
            ChannelId::new(state.config.home_channel_id),
            &fallback,
        )
        .await;
    }
}

#[cfg(test)]
mod settings_terminal_delivery_tests {
    use super::*;

    const COMPLETED_NOTICE: &str =
        "Model applied: example-model\nSaved selection failed: disk detail";

    #[test]
    fn pending_panel_failure_falls_back_with_completed_and_persistence_diagnostics() {
        let panel_error = "Discord panel edit failed: status 503";
        let decision = settings_panel_decision(
            Ok(PanelDelivery::Failed(panel_error.to_owned())),
            COMPLETED_NOTICE,
            true,
            crate::i18n::Language::English,
        );
        match decision {
            SettingsPanelDecision::Fallback {
                notice,
                panel_failure,
                observer_ended,
            } => {
                assert!(notice.contains("Model applied: example-model"));
                assert!(notice.contains("Saved selection failed: disk detail"));
                assert!(notice.contains(panel_error));
                assert_eq!(panel_failure.as_deref(), Some(panel_error));
                assert!(!observer_ended);
            }
            SettingsPanelDecision::Update(_) => panic!("failed panel must use fallback"),
        }
    }

    #[test]
    fn dropped_panel_observer_falls_back_with_completed_result() {
        let decision = settings_panel_decision(
            Err(()),
            COMPLETED_NOTICE,
            true,
            crate::i18n::Language::English,
        );
        match decision {
            SettingsPanelDecision::Fallback {
                notice,
                panel_failure,
                observer_ended,
            } => {
                assert!(notice.contains(COMPLETED_NOTICE));
                assert!(
                    notice.contains(
                        crate::i18n::Language::English
                            .message(MessageKey::SettingsOperationPanelObserverEnded)
                    )
                );
                assert!(panel_failure.is_none());
                assert!(observer_ended);
            }
            SettingsPanelDecision::Update(_) => panic!("dropped observer must use fallback"),
        }
    }

    #[test]
    fn stale_or_closed_panel_falls_back_without_resurrecting_its_menu() {
        let decision = settings_panel_decision(
            Ok(PanelDelivery::Delivered("closed-menu".to_owned())),
            COMPLETED_NOTICE,
            false,
            crate::i18n::Language::English,
        );
        match decision {
            SettingsPanelDecision::Fallback { notice, .. } => {
                assert_eq!(notice, COMPLETED_NOTICE);
            }
            SettingsPanelDecision::Update(_) => {
                panic!("stale/closed panel must not be updated")
            }
        }
    }

    #[test]
    fn current_delivered_panel_keeps_the_normal_panel_update_path() {
        let menu_id = "current-menu";
        let decision = settings_panel_decision(
            Ok(PanelDelivery::Delivered(menu_id.to_owned())),
            COMPLETED_NOTICE,
            true,
            crate::i18n::Language::English,
        );
        match decision {
            SettingsPanelDecision::Update(id) => assert_eq!(id, menu_id),
            SettingsPanelDecision::Fallback { .. } => {
                panic!("current delivered panel should be updated")
            }
        }
    }

    #[test]
    fn fallback_failure_keeps_diagnostic_and_does_not_claim_delivery() {
        let diagnostic = "Discord follow-up failed: status 503".to_owned();
        let mut delivery = SettingsFallbackDelivery::default();
        delivery.record_failure(diagnostic.clone());
        assert_eq!(delivery.delivered_chunks, 0);
        assert_eq!(delivery.failure.as_deref(), Some(diagnostic.as_str()));
        assert!(!delivery.is_fully_delivered(1));
    }

    #[test]
    fn fallback_success_reports_only_observed_delivered_chunks() {
        let mut delivery = SettingsFallbackDelivery::default();
        delivery.record_success();
        assert_eq!(delivery.delivered_chunks, 1);
        assert!(delivery.failure.is_none());
        assert!(delivery.is_fully_delivered(1));
        assert!(!delivery.is_fully_delivered(2));
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
    format_settings_update_result_with_config(requested, defaults, live_result, language, None)
}

fn format_settings_update_result_with_config(
    requested: &str,
    defaults: Option<&Value>,
    live_result: Option<&anyhow::Result<Value>>,
    language: crate::i18n::Language,
    config: Option<&crate::config::Config>,
) -> String {
    let defaults = defaults
        .map(|value| format_settings_with_language(&settings_snapshot(value), language))
        .unwrap_or_else(|| {
            language
                .message(MessageKey::CommandsCouldNotReadSettings23412011)
                .to_owned()
        });
    let mut result = format!(
        "{}: `{requested}`\n{}\n{}:\n{defaults}\n{}: {}",
        language.message(MessageKey::CommandsRequestedValue9eb15821),
        language.message(MessageKey::CommandsSentTheConversationDefaultUpdateRequest7097b23b),
        language.message(MessageKey::CommandsConversationDefaultsAfterUpdate4806be50),
        language.message(MessageKey::CommandsCurrentResponse5c4a065c),
        current_turn_settings_result_with_language(live_result, language),
    );
    if let Some(Err(error)) = live_result {
        result.push('\n');
        result.push_str(&settings_error_detail(error, language, config));
    }
    result
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
        language.message(MessageKey::CommandsPermissionUseInTheActiveResponseWasNotCo485a99f0)
    } else {
        language.message(MessageKey::CommandsNoResponseIsInProgress16302856)
    };
    let prefix = language.message(MessageKey::CommandsSetPermissionProfileTo1589e2c0);
    let suffix = language.message(MessageKey::CommandsMessage6c09cf1f);
    format!(
        "{prefix} `{display_name}`{suffix}\n{}:\n{}\n{}: {current}",
        language.message(MessageKey::CommandsConversationDefaultsAfterUpdate4806be50),
        format_settings_with_language(settings, language),
        language.message(MessageKey::CommandsCurrentResponse5c4a065c),
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
                    .message(MessageKey::CommandsReadOnlyBcb1d1ea)
                    .to_owned(),
            ),
            Some(":danger-full-access") => Some(
                language
                    .message(MessageKey::CommandsFullAccess6238ce12)
                    .to_owned(),
            ),
            Some(":workspace") => Some(
                language
                    .message(MessageKey::CommandsWorkspace8128b964)
                    .to_owned(),
            ),
            _ => None,
        })
        .unwrap_or_else(|| {
            format!(
                "{} {}",
                language.message(MessageKey::CommandsPermissionProfileC7e4bb80),
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
            .message(MessageKey::CommandsReadOnlyBcb1d1ea)
            .to_owned(),
        ":danger-full-access" => language
            .message(MessageKey::CommandsFullAccess6238ce12)
            .to_owned(),
        ":workspace" => language
            .message(MessageKey::CommandsWorkspace8128b964)
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
        ("model", language.message(MessageKey::CommandsModel704caa01)),
        (
            "effort",
            language.message(MessageKey::CommandsReasoningEffortCeade6e1),
        ),
        (
            "serviceTier",
            language.message(MessageKey::CommandsServiceTierB86474e3),
        ),
        (
            "activePermissionProfile",
            language.message(MessageKey::CommandsPermissionProfileC7e4bb80),
        ),
        (
            "approvalPolicy",
            language.message(MessageKey::CommandsApprovalPolicyBe6ee113),
        ),
        (
            "sandboxPolicy",
            language.message(MessageKey::CommandsFileAccess65b1775e),
        ),
        (
            "approvalsReviewer",
            language.message(MessageKey::CommandsApprovalReviewer1bb5dad9),
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
                } else if *key == "effort" {
                    let choices = effort_settings_choice(value, language)
                        .into_iter()
                        .collect::<Vec<_>>();
                    Some(effort_current_label(&choices, Some(value), language))
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
        Some("readOnly") => language.message(MessageKey::CommandsReadOnlyAabd721a),
        Some("workspaceWrite") => {
            language.message(MessageKey::CommandsReadAndWriteInTheWorkspaceCd2cf07f)
        }
        Some("dangerFullAccess") => {
            language.message(MessageKey::CommandsUnrestrictedFileAccess9720c6c4)
        }
        _ => language.message(MessageKey::CommandsCurrentNativeSetting32c855ae),
    };
    let mut details = vec![access.to_owned()];
    for (key, label) in [
        (
            "readableRoots",
            language.message(MessageKey::CommandsReadableLocations99fdcca7),
        ),
        (
            "writableRoots",
            language.message(MessageKey::CommandsWritableLocationsDd96deb1),
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
            language.message(MessageKey::CommandsNetworkAccessEec1571b),
            if network {
                language.message(MessageKey::CommandsAllowed0d02047a)
            } else {
                language.message(MessageKey::CommandsBlocked1b94f0d3)
            }
        ));
    }
    details.join(" · ")
}

fn format_approval_policy(value: &Value) -> String {
    format_approval_policy_with_language(value, crate::i18n::Language::English)
}

fn format_approval_policy_with_language(value: &Value, language: crate::i18n::Language) -> String {
    let label = match value.as_str() {
        Some("on-request") => Some(language.message(MessageKey::CommandsAskWhenNeeded998a5eef)),
        Some("untrusted") => Some(language.message(MessageKey::CommandsAskUnlessTrustedD339f046)),
        Some("never") => Some(language.message(MessageKey::CommandsNeverAskE7783a8c)),
        _ => None,
    };
    if let Some(label) = label {
        format!("{label} ({})", value.as_str().unwrap())
    } else if value.is_null() || value.as_str() == Some("native snapshot did not report this field")
    {
        language
            .message(MessageKey::CommandsNotReportedByCodex9ba3ffef)
            .to_owned()
    } else {
        value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string())
    }
}

fn approval_policy_settings_values(
    thread: &Value,
    saved: &Value,
    requirements: &Value,
    language: crate::i18n::Language,
) -> (Option<String>, String, Vec<SettingsChoice>) {
    let actual = thread.get("approvalPolicy");
    let current = format!(
        "{}: {}\n{}: {}",
        language.message(MessageKey::CommandsEffectivePolicyB2e092d3),
        format_approval_policy_with_language(actual.unwrap_or(&Value::Null), language),
        language.message(MessageKey::CommandsProfilePolicyAdaebde1),
        format_approval_policy_with_language(saved, language),
    );
    (
        actual.map(Value::to_string),
        current,
        approval_policy_choices(requirements, language),
    )
}

fn approval_policy_choices(
    requirements: &Value,
    language: crate::i18n::Language,
) -> Vec<SettingsChoice> {
    let policies = requirements
        .pointer("/requirements/allowedApprovalPolicies")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![json!("on-request"), json!("untrusted"), json!("never")]);
    policies
        .into_iter()
        .map(|policy| SettingsChoice {
            value: policy.to_string(),
            label: format_approval_policy_with_language(&policy, language),
        })
        .collect()
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
            .message(MessageKey::CommandsUser8116d237)
            .to_owned(),
        Some("auto_review") => language
            .message(MessageKey::CommandsAutomaticReviewAutoReview25c10795)
            .to_owned(),
        _ if value.is_null()
            || value.as_str() == Some("native snapshot did not report this field") =>
        {
            language
                .message(MessageKey::CommandsNotReportedByCodex9ba3ffef)
                .to_owned()
        }
        _ => value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
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
                .message(MessageKey::CommandsNoProfileSelected25f65a7d)
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
                .message(MessageKey::CommandsReadOnlyBcb1d1ea)
                .to_owned(),
        ),
        Some(":danger-full-access") => Some(
            language
                .message(MessageKey::CommandsFullAccess6238ce12)
                .to_owned(),
        ),
        Some(":workspace") => Some(
            language
                .message(MessageKey::CommandsWorkspace8128b964)
                .to_owned(),
        ),
        Some(_) => Some(
            language
                .message(MessageKey::CommandsCurrentPermissionProfile9e74fa66)
                .to_owned(),
        ),
        None => Some(
            setting_value_with_language(value, language).unwrap_or_else(|| {
                language
                    .message(MessageKey::CommandsCurrentPermissionProfile9e74fa66)
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
        Value::Null => Some(language.message(MessageKey::CommandsOff9616c89c).to_owned()),
        Value::String(text) if text == "native snapshot did not report this field" => None,
        Value::String(text) => Some(match text.as_str() {
            ":danger-full-access" => language
                .message(MessageKey::CommandsFullAccess6238ce12)
                .to_owned(),
            ":workspace" => language
                .message(MessageKey::CommandsWorkspace8128b964)
                .to_owned(),
            _ => text.clone(),
        }),
        Value::Bool(value) => Some(
            if *value {
                language.message(MessageKey::CommandsEnabled222f5de9)
            } else {
                language.message(MessageKey::CommandsDisabledA5d631b5)
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
        "read" => language.message(MessageKey::CommandsRead0c3244ec),
        "write" => language.message(MessageKey::CommandsWriteD2cfe0e4),
        "network" => language.message(MessageKey::CommandsNetwork0e84aac9),
        "roots" => language.message(MessageKey::CommandsAllowedPaths8c5120c2),
        "mode" | "scope" => language.message(MessageKey::CommandsScope60298d24),
        _ => value,
    }
}

fn format_model_catalog(catalog: &Value) -> String {
    format_model_catalog_with_language(catalog, crate::i18n::Language::English)
}

fn format_model_catalog_with_language(catalog: &Value, language: crate::i18n::Language) -> String {
    let Some(models) = catalog.get("data").and_then(Value::as_array) else {
        return language
            .message(MessageKey::CommandsCouldNotReadTheAvailableModelsC7cb825b)
            .to_owned();
    };
    if models.is_empty() {
        return language
            .message(MessageKey::CommandsNoModelsAreAvailable648adafd)
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
                .unwrap_or(language.message(MessageKey::CommandsUnnamedModel906cf8f6));
            let value = crate::model_catalog::model_id(model).unwrap_or(name);
            let mut details = Vec::new();
            if let Some(efforts) = model
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
            {
                let values = efforts
                    .iter()
                    .filter_map(|item| {
                        let effort = item
                            .get("reasoningEffort")
                            .and_then(Value::as_str)
                            .or_else(|| item.as_str())?;
                        Some(
                            if item.get("agyOrchestration").and_then(Value::as_bool) == Some(true) {
                                format!(
                                    "{effort} (Codex proactive orchestration; AGY default effort)"
                                )
                            } else {
                                effort.to_owned()
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                if !values.is_empty() {
                    details.push(format!(
                        "{}: {}",
                        language.message(MessageKey::CommandsReasoningEffortCeade6e1),
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
                        language.message(MessageKey::CommandsServiceTierB86474e3),
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
        language.message(MessageKey::CommandsAvailableModelsAndSettings54e331f3),
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
            .message(MessageKey::CommandsCouldNotReadTheAvailablePermissionProfiles0dd5ae6c)
            .to_owned();
    };
    if profiles.is_empty() {
        return language
            .message(MessageKey::CommandsNoPermissionProfilesAreAvailableAf94b34d)
            .to_owned();
    }
    let entries = profiles
        .iter()
        .enumerate()
        .map(|(index, profile)| {
            let name = permission_profile_display_name_with_language(profile, index, language);
            let allowed = profile.get("allowed").and_then(Value::as_bool);
            let status = match allowed {
                Some(true) => language.message(MessageKey::CommandsAvailable64e2ae33),
                Some(false) => language.message(MessageKey::CommandsCurrentlyUnavailable6ed28c28),
                None => language.message(MessageKey::CommandsStatusNotReportedB5c12e4b),
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
        language.message(MessageKey::CommandsPermissionProfilesEnterTheDisplayNameToSe2ca03a87),
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
                        language.message(MessageKey::CommandsSourceMessage28a4f745)
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
                .unwrap_or(language.message(
                    MessageKey::CommandsCodexRequestedPermissionToRunATerminalCom26660fce,
                ));
            let location = params
                .get("cwd")
                .and_then(Value::as_str)
                .map(|value| {
                    format!(
                        "\n{}: {value}",
                        language.message(MessageKey::CommandsWorkingDirectory1ef220d7)
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
                        language.message(MessageKey::CommandsNetworkTarget025d41e4)
                    )
                })
                .unwrap_or_default();
            format!(
                "{reason}{location}{network_scope}\n{}",
                language.message(MessageKey::CommandsChooseThePermissionScopeBelowE618ebe0)
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
                    language.message(MessageKey::CommandsFilesB5f72d90),
                    files.join(", ")
                )
            };
            let reason = params
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or(language.message(MessageKey::CommandsCodexRequestedFileChanges2b51e425));
            format!(
                "{reason}{details}\n{}",
                language.message(MessageKey::CommandsChooseThePermissionScopeBelowE618ebe0)
            )
        }
        "item/permissions/requestApproval" => {
            let permission = params
                .get("permissions")
                .map(|value| setting_value_with_language(value, language).unwrap_or_default())
                .unwrap_or_default();
            format!(
                "{}\n{}: {}\n{}",
                language.message(MessageKey::CommandsCodexRequestedAdditionalPermissionsAc468166),
                language.message(MessageKey::CommandsRequestedPermissions82d992f3),
                if permission.is_empty() {
                    language.message(MessageKey::CommandsNoDetailsWereProvidedD84630b6)
                } else {
                    &permission
                },
                language.message(MessageKey::CommandsChooseThePermissionScopeBelowE618ebe0)
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
                    let prompt = question
                        .get("question")
                        .and_then(Value::as_str)
                        .unwrap_or(language.message(MessageKey::CommandsEnterYourAnswer694089c6));
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
                            language.message(MessageKey::CommandsOptionsD0bca22f)
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            format!(
                "{}\n\n{details}\n\n{}",
                language
                    .message(MessageKey::CommandsCodexAskedYouToAnswerTheFollowingQuestion03e6cccf),
                language.message(MessageKey::CommandsPressWriteAResponseToAnswer49aa6928)
            )
        }
        "mcpServer/elicitation/request" => {
            if let Some(suggestion) =
                plugin_suggestion(&json!({"method": method, "params": params}))
            {
                let plugin_name = suggestion
                    .tool_name
                    .as_deref()
                    .unwrap_or_else(|| language.message(MessageKey::CommandsPluginFallbackName));
                return format!(
                    "{}
{}
{}",
                    if is_plugin_suggestion(&suggestion) {
                        language.message(MessageKey::CommandsCodexSuggestsInstallingAPluginCb61c9dc)
                    } else {
                        language.message(MessageKey::CommandsCodexSuggestsConnectingThisAppC9f49f9d)
                    },
                    plugin_name,
                    format!(
                        "{}{}",
                        suggestion.message,
                        suggestion
                            .install_url
                            .as_deref()
                            .map(|url| format!("\n<{url}>"))
                            .unwrap_or_default()
                    ),
                );
            }
            let message = params.get("message").and_then(Value::as_str).unwrap_or(
                language
                    .message(MessageKey::CommandsTheMcpToolRequestedAdditionalInformation7ffe761a),
            );
            if mcp_tool_approval_persist_options(params).is_some() {
                format!(
                    "{}\n{message}",
                    language
                        .message(MessageKey::CommandsMcpToolExecutionApprovalIsRequired76b9e25c)
                )
            } else {
                let schema = describe_schema_with_language(
                    params.get("requestedSchema").unwrap_or(&Value::Null),
                    language,
                );
                format!(
                    "{}\n{message}\n{}: {schema}",
                    language.message(
                        MessageKey::CommandsTheMcpToolRequestedAdditionalInformation7ffe761a
                    ),
                    language.message(MessageKey::CommandsRequestedFields70ec5151)
                )
            }
        }
        _ => language
            .message(MessageKey::CommandsThisRequestHasNoChoicesThatCanBeHandled5a27bdcf)
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
                    .message(MessageKey::CommandsRequestedInformation5885acc4)
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
                    language.message(MessageKey::CommandsRequiredE306ce88)
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
                            language.message(MessageKey::CommandsMultipleA0fe33e9)
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
                                language.message(MessageKey::CommandsValue76361ed6)
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
        "string" => language.message(MessageKey::CommandsText8dac6330),
        "number" | "integer" => language.message(MessageKey::CommandsNumberDe58f6aa),
        "boolean" => language.message(MessageKey::CommandsYesNo19b2b691),
        "array" => language.message(MessageKey::CommandsList38b51386),
        "object" => language.message(MessageKey::CommandsMultipleFields5d939658),
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
                language.message(MessageKey::CommandsQuestionNumbersAndAnswersCf643d88),
                "answers_text",
            )
            .placeholder(
                language.message(MessageKey::CommandsOneNumberAnswerPerLineExample1AnswerD4bd5012),
            )
            .required(true);
            return Ok(CreateModal::new(
                format!("codex:input:{key}"),
                language.message(MessageKey::CommandsAnswerRequestedByCodexFd12fd68),
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
                label = language
                    .message(MessageKey::CommandsAnswer21d69914)
                    .to_owned();
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
            language.message(MessageKey::CommandsAnswerRequestedByCodexFd12fd68),
        )
        .components(components));
    }
    let schema = params.get("requestedSchema").unwrap_or(params);
    let schema_summary = describe_schema_with_language(schema, language);
    let input = serenity::builder::CreateInputText::new(
        serenity::all::InputTextStyle::Paragraph,
        language.message(MessageKey::CommandsRequestedInformation5885acc4),
        "elicitation_json",
    )
    .placeholder(schema_summary.chars().take(100).collect::<String>())
    .required(true);
    Ok(CreateModal::new(
        format!("codex:input:{key}"),
        language.message(MessageKey::CommandsEnterAdditionalInformation42ca01e5),
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
            language.message(MessageKey::CommandsThisCommandIsAvailableOnlyInTheConfiguredBd74104f)
        );
    }
    if !state.config.operator_ids.contains(&user_id.get()) {
        bail!(
            "{}",
            language.message(MessageKey::CommandsThisDiscordUserIsNotConfiguredAsAnOperat97197eb8)
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
        crate::config::InputMode::Steering => {
            language.message(MessageKey::CommandsSendToTheActiveResponse32141a0b)
        }
        crate::config::InputMode::NextTurn => {
            language.message(MessageKey::CommandsSendBeforeTheNextResponse9c3b5081)
        }
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

fn plugin_suggestion_waiting_actions(
    key: &str,
    language: crate::i18n::Language,
) -> Vec<CreateActionRow> {
    let labels = suggestion_labels(language);
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("codex:approval:{key}:plugin-check"))
            .label(language.message(MessageKey::CommandsFinishedSignInCheckAgainD61b0267))
            .style(ButtonStyle::Success),
        CreateButton::new(format!("codex:approval:{key}:plugin-decline"))
            .label(labels.decline)
            .style(ButtonStyle::Secondary),
        CreateButton::new(format!("codex:approval:{key}:plugin-disable"))
            .label(labels.decline_and_remember)
            .style(ButtonStyle::Danger),
        CreateButton::new(format!("codex:approval:{key}:cancel"))
            .label(labels.cancel)
            .style(ButtonStyle::Secondary),
    ])]
}

async fn handle_plugin_suggestion_component(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
    key: &str,
    action: &str,
    _pending: PendingApproval,
) -> anyhow::Result<()> {
    let pending = {
        let mut approvals = state.pending_approvals.lock().await;
        claim_pending(&mut approvals, key, state.native.current_generation())?
    };
    let suggestion = plugin_suggestion(&json!({
        "method": pending.method.clone(),
        "params": pending.params.clone()
    }))
    .ok_or_else(|| anyhow!("native request is not a plugin-install suggestion"))?;
    if let Err(error) = interaction
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
    {
        release_pending(state, key).await;
        return Err(error.into());
    }

    let mut setup_links = Vec::new();
    let operation = async {
        if action == "plugin-install" {
            if is_plugin_suggestion(&suggestion) {
                let catalog = list_catalog(&state.native, true).await?;
                let installed = list_installed(&state.native).await?;
                if !suggested_plugin_is_installed(&installed, &suggestion) {
                    let target =
                        suggested_plugin_target(&catalog, &suggestion, state.config.language)?;
                    let _detail =
                        read_plugin(&state.native, &target, state.config.language).await?;
                    let outcome =
                        install_confirmed(&state.native, &target, true, state.config.language)
                            .await?;
                    for app in outcome.apps_needing_auth {
                        if let Some(url) = app.install_url {
                            setup_links.push(format!("{} — <{}>", app.name, url));
                        }
                    }
                }
            } else if let Some(url) = suggestion.install_url.as_deref() {
                setup_links.push(format!(
                    "{} — <{url}>",
                    suggestion.tool_name.as_deref().unwrap_or_else(|| {
                        state
                            .config
                            .language
                            .message(MessageKey::CommandsPluginFallbackAppSetup)
                    })
                ));
            }
            // Preserve the live native request while the user finishes OAuth/app setup.
            let mut card = interaction.message.clone();
            let mut content = card.content.clone();
            if !setup_links.is_empty() {
                content.push_str("\n\n");
                content.push_str(
                    &state
                        .config
                        .language
                        .message(MessageKey::CommandsCodexReturnedSetupLinksB3940f5f),
                );
                content.push_str("\n");
                content.push_str(&setup_links.join("\n"));
            }
            card.edit(
                ctx,
                serenity::builder::EditMessage::new()
                    .content(content)
                    .components(plugin_suggestion_waiting_actions(
                        key,
                        state.config.language,
                    ))
                    .allowed_mentions(no_mentions()),
            )
            .await?;
        }

        let verified = verify_plugin_suggestion_native_state(state, &suggestion).await?;
        if !verified {
            let text = state
                .config
                .language
                .message(MessageKey::CommandsCodexHasNotReportedTheRequestedPluginAppsEbf234e2);
            interaction
                .edit_response(
                    ctx,
                    serenity::builder::EditInteractionResponse::new().content(text),
                )
                .await?;
            return Ok::<bool, anyhow::Error>(false);
        }
        let result = verified_suggestion_accept(verified)
            .ok_or_else(|| anyhow!("native suggestion is not verified"))?;
        state
            .native
            .respond_for_generation(
                pending.connection_generation,
                pending.native_id.clone(),
                result,
            )
            .await?;
        interaction
            .edit_response(
                ctx,
                serenity::builder::EditInteractionResponse::new().content(
                    state.config.language.message(
                        MessageKey::CommandsAcceptanceSentToCodexCodexWillReportWhethFdec8fc5,
                    ),
                ),
            )
            .await?;
        let mut card = interaction.message.clone();
        if let Err(error) = card
            .edit(
                ctx,
                serenity::builder::EditMessage::new().components(Vec::new()),
            )
            .await
        {
            tracing::warn!(?error, "failed to close verified plugin suggestion card");
        }
        remove_pending_if_same(state, key, &pending).await;
        if let Some(checks) = &state.installation_checks {
            checks.request_answered(pending.connection_generation, &pending.native_id);
        }
        Ok::<bool, anyhow::Error>(true)
    }
    .await;

    match operation {
        Ok(true) => Ok(()),
        Ok(false) => {
            release_pending(state, key).await;
            Ok(())
        }
        Err(error) => {
            tracing::warn!(error = ?error, "native plugin suggestion install/check failed");
            release_pending(state, key).await;
            interaction
                .edit_response(
                    ctx,
                    serenity::builder::EditInteractionResponse::new().content(format!(
                        "{}: {}",
                        state.config.language.message(
                            MessageKey::CommandsCodexCouldNotCompleteThePluginSetupCheck0cc616ac
                        ),
                        settings_error_detail(&error, state.config.language, Some(&state.config)),
                    )),
                )
                .await?;
            Ok(())
        }
    }
}

async fn verify_plugin_suggestion_native_state(
    state: &BridgeState,
    suggestion: &crate::discord_plugins::PluginSuggestion,
) -> anyhow::Result<bool> {
    let accessible_ids = if is_plugin_suggestion(suggestion) {
        suggestion.app_connector_ids.clone()
    } else {
        suggestion
            .app_connector_ids
            .clone()
            .into_iter()
            .chain(suggestion.tool_id.clone())
            .collect()
    };
    let mut accessible = std::collections::HashSet::new();
    if !accessible_ids.is_empty() {
        let mut cursor: Option<String> = None;
        let mut seen_cursors = std::collections::HashSet::new();
        loop {
            let mut params = json!({
                "threadId": state.native.thread_id(),
                "forceRefetch": true,
            });
            if let Some(value) = cursor.as_ref() {
                params["cursor"] = Value::String(value.clone());
                params["forceRefetch"] = Value::Bool(false);
            }
            let page = state.native.request("app/list", params).await?;
            for app in page
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if app.get("isAccessible").and_then(Value::as_bool) == Some(true)
                    && let Some(id) = app.get("id").and_then(Value::as_str)
                {
                    accessible.insert(id.to_owned());
                }
            }
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor
                .as_ref()
                .is_none_or(|value| !seen_cursors.insert(value.clone()))
            {
                break;
            }
        }
    }
    if is_plugin_suggestion(suggestion) {
        let installed = list_installed(&state.native).await?;
        Ok(suggested_plugin_install_verified(
            suggestion,
            &installed,
            &accessible,
        ))
    } else {
        Ok(!accessible_ids.is_empty() && accessible_ids.iter().all(|id| accessible.contains(id)))
    }
}

fn plugins_ui_requested(interaction: &CommandInteraction) -> bool {
    interaction
        .data
        .options
        .first()
        .is_some_and(|subcommand| subcommand.name == "plugins")
}

async fn plugin_menu_from_command(
    interaction: &CommandInteraction,
    state: &Arc<BridgeState>,
) -> anyhow::Result<(String, PluginMenuState)> {
    let subcommand = interaction
        .data
        .options
        .first()
        .ok_or_else(|| anyhow!("plugin command is missing"))?;
    let installed = match &subcommand.value {
        CommandDataOptionValue::SubCommand(options) => options
            .iter()
            .find(|option| option.name == "view")
            .and_then(option_string)
            .is_some_and(|view| view == "installed"),
        _ => false,
    };
    let mode = if installed {
        PluginMenuMode::Installed
    } else {
        PluginMenuMode::Catalog
    };
    let entries = load_plugin_menu_entries(state, mode).await?;
    let id = Uuid::new_v4().simple().to_string();
    Ok((
        id,
        PluginMenuState {
            claimed: false,
            requester_id: interaction.user.id.get(),
            connection_generation: state.native.current_generation(),
            thread_id: state.native.thread_id(),
            mode,
            page: 0,
            entries,
            selected: None,
            selected_detail: None,
            apps_needing_auth: Vec::new(),
            oauth_servers: Vec::new(),
            auth_view: false,
            install_notice: None,
        },
    ))
}

async fn load_plugin_menu_entries(
    state: &BridgeState,
    mode: PluginMenuMode,
) -> anyhow::Result<Vec<PluginMenuEntry>> {
    let catalog = match mode {
        PluginMenuMode::Catalog => list_catalog(&state.native, false).await?,
        PluginMenuMode::Installed => list_installed(&state.native).await?,
    };
    let mut entries = Vec::new();
    for marketplace in &catalog.marketplaces {
        for plugin in &marketplace.plugins {
            match mode {
                PluginMenuMode::Catalog => {
                    if let Ok(target) = PluginInstallTarget::from_catalog(
                        marketplace,
                        plugin,
                        state.config.language,
                    ) {
                        entries.push(PluginMenuEntry::Install(target));
                    }
                }
                PluginMenuMode::Installed if plugin.installed => {
                    entries.push(PluginMenuEntry::Uninstall {
                        plugin_id: plugin.id.clone(),
                        display_name: plugin.name.clone(),
                    })
                }
                PluginMenuMode::Installed => {}
            }
        }
    }
    Ok(entries)
}

const PLUGIN_MENU_PAGE_SIZE: usize = 20;

fn render_plugin_menu(
    menu_id: &str,
    menu: &PluginMenuState,
    language: crate::i18n::Language,
) -> (String, Vec<CreateActionRow>) {
    if let Some(selected) = menu.selected.as_ref() {
        let (title, description) = match selected {
            PluginMenuEntry::Install(target) => (
                target.display_name.as_str(),
                menu.selected_detail
                    .as_ref()
                    .and_then(|plugin| plugin.description.as_deref())
                    .unwrap_or(""),
            ),
            PluginMenuEntry::Uninstall { display_name, .. } => (display_name.as_str(), ""),
        };
        let content = match selected {
            PluginMenuEntry::Install(_) => format!(
                "{}\n{}\n{}",
                language.message(
                    MessageKey::CommandsReviewThisNativeCodexPluginBeforeInstallin5706ca16
                ),
                title,
                description
            ),
            PluginMenuEntry::Uninstall { .. } => format!(
                "{}\n{}",
                language
                    .message(MessageKey::CommandsConfirmRemovalOfThisInstalledCodexPluginAe4e33e6),
                title
            ),
        };
        let confirm = match selected {
            PluginMenuEntry::Install(_) => {
                language.message(MessageKey::CommandsConfirmInstallAe773718)
            }
            PluginMenuEntry::Uninstall { .. } => {
                language.message(MessageKey::CommandsConfirmRemovalD1a86560)
            }
        };
        return (
            content,
            vec![CreateActionRow::Buttons(vec![
                CreateButton::new(format!("codex:plugins:{menu_id}:confirm"))
                    .label(confirm)
                    .style(ButtonStyle::Danger),
                CreateButton::new(format!("codex:plugins:{menu_id}:back"))
                    .label(language.message(MessageKey::CommandsBackFf4b818b))
                    .style(ButtonStyle::Secondary),
            ])],
        );
    }

    let (heading, empty, instruction) = match menu.mode {
        PluginMenuMode::Catalog => (
            language.message(MessageKey::CommandsNativeCodexPluginCatalog247a5e12),
            language.message(MessageKey::CommandsNoInstallablePluginsWereReturnedByCodex730c5822),
            language.message(MessageKey::CommandsChooseAPluginToReviewThenConfirmInstallat11ac8020),
        ),
        PluginMenuMode::Installed => (
            language.message(MessageKey::CommandsInstalledCodexPluginsDd11cf68),
            language.message(MessageKey::CommandsCodexReturnedNoInstalledPlugins5efd4691),
            language.message(MessageKey::CommandsChooseAPluginToReviewThenConfirmRemoval3ad3c64e),
        ),
    };
    let mut content = heading.to_owned();
    if menu.entries.is_empty() {
        content.push_str("\n");
        content.push_str(empty);
    } else {
        content.push_str("\n");
        content.push_str(instruction);
    }
    if let Some(notice) = &menu.install_notice {
        content.push_str("\n\n");
        content.push_str(notice);
    }
    if !menu.apps_needing_auth.is_empty() {
        content.push_str("\n\n");
        content.push_str(
            language.message(MessageKey::CommandsCodexReportsTheseAppsNeedAuthenticationEf990cc6),
        );
        for app in &menu.apps_needing_auth {
            content.push_str(&format!("\n• {}", app.name));
            if let Some(url) = app.install_url.as_deref() {
                content.push_str(&format!(" — <{url}>"));
            }
        }
    }
    if !menu.oauth_servers.is_empty() {
        content.push_str("\n\n");
        content.push_str(
            language.message(MessageKey::CommandsStartNativeOauthForAPluginMcpServerE072fd5b),
        );
    }
    if menu.auth_view {
        let mut rows = Vec::new();
        let start = menu
            .page
            .saturating_mul(PLUGIN_MENU_PAGE_SIZE)
            .min(menu.oauth_servers.len());
        let end = (start + PLUGIN_MENU_PAGE_SIZE).min(menu.oauth_servers.len());
        if menu.oauth_servers.is_empty() {
            content.push_str("\n");
            content.push_str(
                language.message(MessageKey::CommandsNoPluginMcpServersWereReturnedByCodex3f464c0e),
            );
        }
        for (row_index, group) in menu.oauth_servers[start..end].chunks(5).enumerate() {
            let buttons = group
                .iter()
                .enumerate()
                .map(|(offset, server)| {
                    let index = start + row_index * 5 + offset;
                    CreateButton::new(format!("codex:plugins:{menu_id}:o{index}"))
                        .label(discord_button_label(server))
                        .style(ButtonStyle::Success)
                })
                .collect();
            rows.push(CreateActionRow::Buttons(buttons));
        }
        let pages = menu
            .oauth_servers
            .len()
            .div_ceil(PLUGIN_MENU_PAGE_SIZE)
            .max(1);
        let mut controls = Vec::new();
        if pages > 1 {
            controls.push(
                CreateButton::new(format!(
                    "codex:plugins:{menu_id}:p{}",
                    menu.page.saturating_sub(1)
                ))
                .label(language.message(MessageKey::CommandsPrevious9be71e5f))
                .disabled(menu.page == 0),
            );
        }
        controls.push(
            CreateButton::new(format!("codex:plugins:{menu_id}:plugins"))
                .label(language.message(MessageKey::CommandsPluginsB04ed055))
                .style(ButtonStyle::Primary),
        );
        if pages > 1 {
            controls.push(
                CreateButton::new(format!(
                    "codex:plugins:{menu_id}:p{}",
                    (menu.page + 1).min(pages - 1)
                ))
                .label(language.message(MessageKey::CommandsNext8a0c8241))
                .disabled(menu.page + 1 >= pages),
            );
        }
        controls.push(
            CreateButton::new(format!("codex:plugins:{menu_id}:close"))
                .label(language.message(MessageKey::CommandsCloseE91bd255))
                .style(ButtonStyle::Secondary),
        );
        rows.push(CreateActionRow::Buttons(controls));
        return (content, rows);
    }
    let start = menu
        .page
        .saturating_mul(PLUGIN_MENU_PAGE_SIZE)
        .min(menu.entries.len());
    let end = (start + PLUGIN_MENU_PAGE_SIZE).min(menu.entries.len());
    let mut rows = Vec::new();
    for (row_index, group) in menu.entries[start..end].chunks(5).enumerate() {
        let buttons = group
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                let index = start + row_index * 5 + offset;
                let (action, label) = match entry {
                    PluginMenuEntry::Install(target) => {
                        (format!("s{index}"), target.display_name.as_str())
                    }
                    PluginMenuEntry::Uninstall { display_name, .. } => {
                        (format!("u{index}"), display_name.as_str())
                    }
                };
                CreateButton::new(format!("codex:plugins:{menu_id}:{action}"))
                    .label(discord_button_label(label))
                    .style(ButtonStyle::Secondary)
            })
            .collect();
        rows.push(CreateActionRow::Buttons(buttons));
    }
    let pages = menu.entries.len().div_ceil(PLUGIN_MENU_PAGE_SIZE).max(1);
    let mut controls = Vec::new();
    if pages > 1 {
        controls.push(
            CreateButton::new(format!(
                "codex:plugins:{menu_id}:p{}",
                menu.page.saturating_sub(1)
            ))
            .label(language.message(MessageKey::CommandsPrevious9be71e5f))
            .disabled(menu.page == 0),
        );
    }
    let mode_action = if menu.mode == PluginMenuMode::Catalog {
        "installed"
    } else {
        "catalog"
    };
    let mode_label = if menu.mode == PluginMenuMode::Catalog {
        language.message(MessageKey::CommandsInstalledA4c359ee)
    } else {
        language.message(MessageKey::CommandsCatalogFe5df316)
    };
    controls.push(
        CreateButton::new(format!("codex:plugins:{menu_id}:{mode_action}"))
            .label(mode_label)
            .style(ButtonStyle::Primary),
    );
    if !menu.oauth_servers.is_empty() {
        controls.push(
            CreateButton::new(format!("codex:plugins:{menu_id}:auth"))
                .label(language.message(MessageKey::CommandsPluginOauthAf3b23e6))
                .style(ButtonStyle::Success),
        );
    }
    if pages > 1 {
        controls.push(
            CreateButton::new(format!(
                "codex:plugins:{menu_id}:p{}",
                (menu.page + 1).min(pages - 1)
            ))
            .label(language.message(MessageKey::CommandsNext8a0c8241))
            .disabled(menu.page + 1 >= pages),
        );
    }
    controls.push(
        CreateButton::new(format!("codex:plugins:{menu_id}:close"))
            .label(language.message(MessageKey::CommandsCloseE91bd255))
            .style(ButtonStyle::Secondary),
    );
    rows.push(CreateActionRow::Buttons(controls));
    (content, rows)
}

async fn handle_plugin_component(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
) -> anyhow::Result<()> {
    let parts = interaction.data.custom_id.split(':').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "codex" || parts[1] != "plugins" {
        bail!("unknown plugin menu component id");
    }
    let menu_id = parts[2];
    let action = parts[3];
    let current_generation = state.native.current_generation();
    let claimed = {
        let mut menus = state.plugin_menus.lock().await;
        claim_plugin_menu(
            &mut menus,
            menu_id,
            interaction.user.id.get(),
            current_generation,
            &state.native.thread_id(),
        )
    };
    let mut menu = match claimed {
        Ok(menu) => menu,
        Err(reason) => {
            let language = state.config.language;
            let text = match reason {
                PluginMenuClaimError::Missing => language
                    .message(MessageKey::CommandsThisPluginPanelExpiredRunDiscodexPluginsA3a629725),
                PluginMenuClaimError::OtherUser => language
                    .message(MessageKey::CommandsOnlyThePersonWhoOpenedThisPanelCanUseItA550ca64),
                PluginMenuClaimError::Busy => {
                    language.message(MessageKey::CommandsThisPluginActionIsStillProcessingE3038c8d)
                }
                PluginMenuClaimError::Stale => language.message(
                    MessageKey::CommandsTheCodexConnectionChangedRunDiscodexPluginB4158b22,
                ),
            };
            interaction
                .create_response(
                    ctx,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content(text)
                            .ephemeral(true),
                    ),
                )
                .await?;
            return Ok(());
        }
    };

    if let Some(index) = action
        .strip_prefix('o')
        .and_then(|value| value.parse::<usize>().ok())
    {
        let Some(server) = menu.oauth_servers.get(index).cloned() else {
            if let Some(current) = state.plugin_menus.lock().await.get_mut(menu_id) {
                current.claimed = false;
            }
            interaction
                .create_response(
                    ctx,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content(state.config.language.message(
                                MessageKey::CommandsThatOauthServerIsNoLongerListed924f32d6,
                            ))
                            .ephemeral(true),
                    ),
                )
                .await?;
            return Ok(());
        };
        interaction
            .create_response(
                ctx,
                CreateInteractionResponse::Defer(
                    CreateInteractionResponseMessage::new().ephemeral(true),
                ),
            )
            .await?;
        let login_result = begin_oauth_login(&state.native, &server, Some(&menu.thread_id)).await;
        let response = match login_result {
            Ok(login) => {
                let mut content = format!(
                    "{}\n<{}>",
                    state.config.language.message(
                        MessageKey::CommandsNativeOauthSignInStartedOpenTheAuthorizat2e3156cb
                    ),
                    login.authorization_url
                );
                if let Some(id) = login.login_id {
                    content.push_str(&format!(
                        "\n{}: `{id}`",
                        state
                            .config
                            .language
                            .message(MessageKey::CommandsLoginIdEae80152)
                    ));
                }
                content
            }
            Err(error) => format!(
                "{}: {}",
                state
                    .config
                    .language
                    .message(MessageKey::CommandsCodexCouldNotStartOauthSignIn908daee8),
                settings_error_detail(&error, state.config.language, Some(&state.config)),
            ),
        };
        interaction
            .edit_response(
                ctx,
                serenity::builder::EditInteractionResponse::new().content(response),
            )
            .await?;
        if let Some(current) = state.plugin_menus.lock().await.get_mut(menu_id) {
            current.claimed = false;
        }
        return Ok(());
    }

    interaction
        .create_response(ctx, CreateInteractionResponse::Acknowledge)
        .await?;
    let mut message = interaction.message.clone();

    let response_result = async {
        if action == "close" {
            message
                .edit(
                    ctx,
                    serenity::builder::EditMessage::new().components(Vec::new()),
                )
                .await?;
            state.plugin_menus.lock().await.remove(menu_id);
            return Ok::<(), anyhow::Error>(());
        }
        if action == "auth" {
            menu.auth_view = true;
            menu.page = 0;
        } else if action == "plugins" {
            menu.auth_view = false;
            menu.page = 0;
        } else if action == "catalog" || action == "installed" {
            menu.auth_view = false;
            menu.mode = if action == "catalog" {
                PluginMenuMode::Catalog
            } else {
                PluginMenuMode::Installed
            };
            menu.page = 0;
            menu.entries = load_plugin_menu_entries(state, menu.mode).await?;
            menu.selected = None;
            menu.selected_detail = None;
        } else if action == "back" {
            menu.selected = None;
            menu.selected_detail = None;
        } else if let Some(page) = action
            .strip_prefix('p')
            .and_then(|value| value.parse::<usize>().ok())
        {
            let count = if menu.auth_view {
                menu.oauth_servers.len()
            } else {
                menu.entries.len()
            };
            menu.page = page.min(count.div_ceil(PLUGIN_MENU_PAGE_SIZE).saturating_sub(1));
        } else if action == "confirm" {
            let entry = menu
                .selected
                .clone()
                .ok_or_else(|| anyhow!("plugin confirmation has no selection"))?;
            match entry {
                PluginMenuEntry::Install(target) => {
                    let outcome =
                        install_confirmed(&state.native, &target, true, state.config.language)
                            .await?;
                    menu.apps_needing_auth = outcome.apps_needing_auth.clone();
                    menu.oauth_servers = menu
                        .selected_detail
                        .as_ref()
                        .map(|detail| detail.mcp_servers.clone())
                        .unwrap_or_default();
                    menu.selected = None;
                    menu.selected_detail = None;
                    menu.mode = PluginMenuMode::Installed;
                    let refresh_error = match load_plugin_menu_entries(state, menu.mode).await {
                        Ok(entries) => {
                            menu.entries = entries;
                            None
                        }
                        Err(error) => Some(format!("{error:#}")),
                    };
                    menu.page = 0;
                    let auth_policy = outcome.auth_policy;
                    menu.install_notice = Some(format!(
                        "{} {} ({}){}",
                        state
                            .config
                            .language
                            .message(MessageKey::CommandsCodexConfirmedInstallationOf9ae67a2a),
                        target.display_name,
                        auth_policy,
                        refresh_error
                            .map(|error| format!(
                                "\n{}: {error}",
                                state.config.language.message(
                                    MessageKey::CommandsCouldNotRefreshTheInstalledList05716ecd
                                )
                            ))
                            .unwrap_or_default()
                    ));
                }
                PluginMenuEntry::Uninstall {
                    plugin_id,
                    display_name,
                } => {
                    uninstall_confirmed(&state.native, &plugin_id, true, state.config.language)
                        .await?;
                    menu.selected = None;
                    menu.selected_detail = None;
                    menu.apps_needing_auth.clear();
                    menu.oauth_servers.clear();
                    menu.mode = PluginMenuMode::Installed;
                    menu.auth_view = false;
                    menu.entries = load_plugin_menu_entries(state, menu.mode).await?;
                    menu.page = 0;
                    menu.install_notice = Some(format!(
                        "{} {display_name}.",
                        state
                            .config
                            .language
                            .message(MessageKey::CommandsCodexConfirmedRemovalOf6da0bc0c)
                    ));
                }
            }
        } else if let Some(index) = action
            .strip_prefix('s')
            .and_then(|value| value.parse::<usize>().ok())
        {
            let Some(PluginMenuEntry::Install(target)) = menu.entries.get(index).cloned() else {
                bail!("plugin selection is unavailable")
            };
            menu.selected_detail =
                Some(read_plugin(&state.native, &target, state.config.language).await?);
            menu.selected = Some(PluginMenuEntry::Install(target));
        } else if let Some(index) = action
            .strip_prefix('u')
            .and_then(|value| value.parse::<usize>().ok())
        {
            let Some(PluginMenuEntry::Uninstall {
                plugin_id,
                display_name,
            }) = menu.entries.get(index).cloned()
            else {
                bail!("installed plugin selection is unavailable")
            };
            menu.selected = Some(PluginMenuEntry::Uninstall {
                plugin_id,
                display_name,
            });
            menu.selected_detail = None;
        } else {
            bail!("unknown plugin menu action");
        }
        let rendered = render_plugin_menu(menu_id, &menu, state.config.language);
        message
            .edit(
                ctx,
                serenity::builder::EditMessage::new()
                    .content(rendered.0)
                    .components(rendered.1)
                    .allowed_mentions(no_mentions()),
            )
            .await?;
        state
            .plugin_menus
            .lock()
            .await
            .insert(menu_id.to_owned(), menu.clone());
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = response_result {
        if let Some(current) = state.plugin_menus.lock().await.get_mut(menu_id) {
            current.claimed = false;
        }
        interaction
            .create_followup(
                ctx,
                serenity::builder::CreateInteractionResponseFollowup::new()
                    .content(format!(
                        "{}: {}",
                        state.config.language.message(
                            MessageKey::CommandsCodexCouldNotCompleteThisPluginAction0111fad8
                        ),
                        settings_error_detail(&error, state.config.language, Some(&state.config)),
                    ))
                    .ephemeral(true)
                    .allowed_mentions(no_mentions()),
            )
            .await?;
        return Ok(());
    }
    if let Some(current) = state.plugin_menus.lock().await.get_mut(menu_id) {
        current.claimed = false;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PluginMenuClaimError {
    Missing,
    OtherUser,
    Busy,
    Stale,
}

fn claim_plugin_menu(
    menus: &mut HashMap<String, PluginMenuState>,
    menu_id: &str,
    requester_id: u64,
    generation: u64,
    thread_id: &str,
) -> Result<PluginMenuState, PluginMenuClaimError> {
    let menu = menus
        .get_mut(menu_id)
        .ok_or(PluginMenuClaimError::Missing)?;
    if menu.requester_id != requester_id {
        return Err(PluginMenuClaimError::OtherUser);
    }
    if menu.claimed {
        return Err(PluginMenuClaimError::Busy);
    }
    if menu.connection_generation != generation || menu.thread_id != thread_id {
        menus.remove(menu_id);
        return Err(PluginMenuClaimError::Stale);
    }
    menu.claimed = true;
    Ok(menu.clone())
}

pub(crate) fn oauth_completion_text(
    completion: &OAuthCompletion,
    language: crate::i18n::Language,
) -> String {
    if completion.success {
        format!(
            "{} {}.",
            language.message(MessageKey::CommandsOauthSignInCompletedFor149c7a44),
            completion.name
        )
    } else {
        format!(
            "{} {}{}",
            language.message(MessageKey::CommandsOauthSignInDidNotCompleteFor4d954f84),
            completion.name,
            completion
                .error
                .as_deref()
                .map(|error| format!(" — {error}"))
                .unwrap_or_default()
        )
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;

    #[test]
    fn settings_home_and_advanced_render_one_child_entry_without_changing_profiles() {
        use crate::i18n::Language;
        let mut profile_p = crate::native::NativeFeaturePreferences {
            subagent_model: Some("child-before".into()),
            subagent_model_provider: Some("provider-before".into()),
            subagent_model_executor: Some("codex".into()),
            history_notes: None,
            ..Default::default()
        };
        let profile_q = crate::native::NativeFeaturePreferences {
            subagent_model: Some("other-child".into()),
            code_mode: Some(false),
            approval_policy: json!("never"),
            ..Default::default()
        };
        let shared_config = json!({"model":"shared-parent","approval_policy":"never",
            "agents":{"default_subagent_model":"shared-child"}});
        let q_before = profile_q.clone();
        let shared_before = shared_config.clone();
        let thread = json!({"id":"profile-p-thread","model":"parent-example",
            "modelProvider":"parent-provider","approvalPolicy":"on-request"});
        let thread_before = thread.clone();
        let p_before = profile_p.clone();
        for (language, child, advanced, exit, back) in [
            (
                Language::English,
                "Default subagent model",
                "Advanced settings",
                "Exit",
                "Back",
            ),
            (
                Language::Korean,
                "기본 하위 에이전트 모델",
                "고급 설정",
                "나가기",
                "뒤로가기",
            ),
            (
                Language::Chinese,
                "默认子代理模型",
                "高级设置",
                "退出",
                "返回",
            ),
            (
                Language::Japanese,
                "既定のサブエージェントモデル",
                "詳細設定",
                "閉じる",
                "戻る",
            ),
        ] {
            let mut home = menu_fixture();
            home.kind = SettingsKind::Hub;
            (home.current_value, home.current, home.choices) =
                settings_home_values(&thread, crate::config::InputMode::Steering, None, language);
            assert_eq!(
                home.choices
                    .iter()
                    .filter(|c| c.value == "subagent-model")
                    .count(),
                1
            );
            assert_eq!(
                home.choices
                    .iter()
                    .find(|c| c.value == "subagent-model")
                    .unwrap()
                    .label,
                child
            );
            assert_eq!(
                home.choices
                    .iter()
                    .find(|c| c.value == "native-features")
                    .unwrap()
                    .label,
                advanced
            );
            let (home_content, home_rows) = render_settings_menu("profile-p", &home, language);
            let home_rows = serde_json::to_value(home_rows).unwrap();
            let home_buttons = home_rows
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|row| row["components"].as_array().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                home_buttons.iter().filter(|b| b["label"] == child).count(),
                1
            );
            assert_eq!(home_buttons.last().unwrap()["label"], exit);
            assert_eq!(
                home_buttons.last().unwrap()["custom_id"],
                "codex:settings:profile-p:x"
            );
            assert!(!home_content.contains(child)); // no duplicated model summary

            let mut detail = menu_fixture();
            detail.kind = settings_kind_from_key("native-features").unwrap();
            (detail.current_value, detail.current, detail.choices) =
                advanced_settings_values(&profile_p, language);
            let (detail_content, detail_rows) =
                render_settings_menu("profile-p", &detail, language);
            let detail_rows = serde_json::to_value(detail_rows).unwrap();
            assert!(detail_content.starts_with(advanced));
            assert!(!detail_content.contains(child));
            assert!(!detail_content.contains("child-before"));
            assert!(detail.choices.iter().all(|choice| !matches!(
                choice.value.as_str(),
                "subagent-model" | "native-feature:subagentModel"
            )));
            let controls = detail_rows.as_array().unwrap().last().unwrap()["components"]
                .as_array()
                .unwrap();
            assert_eq!(controls.len(), 2);
            assert_eq!(controls[0]["label"], back);
            assert_eq!(controls[0]["custom_id"], "codex:settings:profile-p:h");
            assert_eq!(controls[1]["label"], exit);
            assert_eq!(controls[1]["custom_id"], "codex:settings:profile-p:x");
            assert_eq!(settings_back_target(detail.kind), SettingsKind::Hub);
            assert_eq!(
                settings_back_target(SettingsKind::NativeFeature(NativeFeatureKey::Clock)),
                SettingsKind::NativeFeatures
            );
            let repeated = render_settings_menu("profile-p", &detail, language);
            assert_eq!(repeated.0, detail_content);
            assert_eq!(serde_json::to_value(repeated.1).unwrap(), detail_rows);
        }
        assert_eq!(profile_p, p_before);
        let selection = crate::model_catalog::ModelSelection {
            executor: crate::model_catalog::ModelExecutor::Codex,
            provider: "child-provider-after".into(),
            model: "child-after".into(),
        };
        set_subagent_model_selection(&mut profile_p, Some(&selection));
        set_native_feature_preference(&mut profile_p, NativeFeatureKey::CodeMode, Some("off"))
            .unwrap();
        let restored: crate::native::NativeFeaturePreferences =
            serde_json::from_value(serde_json::to_value(&profile_p).unwrap()).unwrap();
        assert_eq!(restored, profile_p);
        assert_eq!(
            subagent_preference_scope(&restored, &json!({})),
            Some(selection.value())
        );
        assert_eq!(
            native_feature_saved_value(&restored, NativeFeatureKey::CodeMode).as_deref(),
            Some("false")
        );
        let mut expected = p_before;
        expected.subagent_model = Some(selection.model);
        expected.subagent_model_provider = Some(selection.provider);
        expected.subagent_model_executor = Some("codex".into());
        expected.code_mode = Some(false);
        assert_eq!(profile_p, expected);
        assert_eq!(profile_q, q_before);
        assert_eq!(shared_config, shared_before);
        assert_eq!(thread, thread_before);
    }

    #[test]
    fn home_and_subagent_picker_report_the_running_executor_provider_in_every_language() {
        for (language, provider_label) in [
            (crate::i18n::Language::English, "Active model provider"),
            (crate::i18n::Language::Korean, "현재 모델 제공자"),
            (crate::i18n::Language::Chinese, "当前模型提供方"),
            (crate::i18n::Language::Japanese, "現在のモデルプロバイダー"),
        ] {
            for (executor, expected_provider) in [
                ("codex", "provider-example"),
                ("antigravity", "antigravity"),
            ] {
                let thread = json!({"id":"thread-example","model":"model-example","modelProvider":"provider-example","modelExecutor":executor});
                let before = thread.clone();
                let snapshot = crate::native::NativeFeatureSnapshot {
                    preferences: crate::native::NativeFeaturePreferences {
                        subagent_model: Some("child-model".into()),
                        subagent_model_provider: Some("child-provider".into()),
                        subagent_model_executor: Some("codex".into()),
                        ..Default::default()
                    },
                    native_config: json!({"config":{"agents":{"default_subagent_model":"child-model","default_subagent_model_provider":"child-provider"}}}),
                };
                let catalog = json!({"provider":"provider-example","data":[]});
                let snapshot_before = snapshot.clone();
                let catalog_before = catalog.clone();
                for kind in [SettingsKind::Hub, SettingsKind::SubagentModel] {
                    let mut menu = menu_fixture();
                    menu.kind = kind;
                    (menu.current_value, menu.current, menu.choices) = if kind == SettingsKind::Hub
                    {
                        settings_home_values(
                            &thread,
                            crate::config::InputMode::Steering,
                            None,
                            language,
                        )
                    } else {
                        subagent_model_settings_values(&thread, &snapshot, &catalog, language)
                    };
                    let (content, _) = render_settings_menu("provider-example", &menu, language);
                    let provider_lines = content
                        .lines()
                        .filter(|line| line.starts_with(provider_label))
                        .collect::<Vec<_>>();
                    assert_eq!(
                        provider_lines,
                        vec![format!("{provider_label}: {expected_provider}")]
                    );
                }
                assert_eq!(snapshot, snapshot_before);
                assert_eq!(catalog, catalog_before);
                assert_eq!(thread, before);
            }
        }
    }

    #[test]
    fn settings_timing_distinguishes_child_resume_native_restart_and_agy_start() {
        for (language, saved, child, advanced) in [
            (
                crate::i18n::Language::English,
                "Saved.",
                "The saved child default applies on the next thread creation or resume, including after a worker restart. The current thread keeps its effective settings until resumed.",
                "Native feature switches apply after the Discodex worker restarts. Antigravity settings apply on the next model selection or AGY session start.",
            ),
            (
                crate::i18n::Language::Korean,
                "저장했습니다.",
                "저장한 하위 에이전트 기본값은 다음 스레드 생성·재개 시 적용되며, 워커 재시작 후 재개할 때도 적용됩니다. 현재 스레드의 실효 설정은 재개 전까지 유지됩니다.",
                "네이티브 기능 스위치는 Discodex 워커 재시작 후 적용됩니다. Antigravity 설정은 다음 모델 선택 또는 AGY 세션 시작 때 적용됩니다.",
            ),
            (
                crate::i18n::Language::Chinese,
                "已保存。",
                "保存的子代理默认值在下次创建或恢复线程时生效，包括工作进程重启后的恢复。当前线程在恢复前保持其有效设置。",
                "原生功能开关在 Discodex 工作进程重启后生效。Antigravity 设置在下次模型选择或 AGY 会话启动时生效。",
            ),
            (
                crate::i18n::Language::Japanese,
                "保存しました。",
                "保存した子エージェントの既定値は、ワーカー再起動後を含め、次のスレッド作成・再開時に適用されます。現在のスレッドは再開するまで有効な設定を保持します。",
                "ネイティブ機能の切り替えは Discodex ワーカーの再起動後に適用されます。Antigravity 設定は次のモデル選択または AGY セッション開始時に適用されます。",
            ),
        ] {
            assert_eq!(child_default_timing(language), child);
            assert_eq!(
                child_default_saved_notice(language),
                format!("{saved}\n{child}")
            );
            let mut menu = menu_fixture();
            menu.kind = SettingsKind::NativeFeatures;
            (menu.current_value, menu.current, menu.choices) = advanced_settings_values(
                &crate::native::NativeFeaturePreferences::default(),
                language,
            );
            assert_eq!(menu.current.lines().next(), Some(advanced));
            let (content, _) = render_settings_menu("advanced-example", &menu, language);
            assert!(content.contains(advanced));
        }
    }

    #[test]
    fn approval_panel_separates_effective_saved_and_permission_values_in_every_language() {
        use crate::i18n::Language;
        let thread = json!({"approvalPolicy":"on-request","activePermissionProfile":{"id":"workspace-write","displayName":"Workspace"}});
        let before = thread.clone();
        for (language, policy_label, permission_label, effective_label, saved_label) in [
            (
                Language::English,
                "Approval policy",
                "Permission profile",
                "Effective policy",
                "Profile policy",
            ),
            (
                Language::Korean,
                "승인 방식",
                "권한 프로필",
                "실효 승인 방식",
                "프로필 승인 방식",
            ),
            (
                Language::Chinese,
                "审批策略",
                "权限配置",
                "当前生效策略",
                "配置策略",
            ),
            (
                Language::Japanese,
                "承認ポリシー",
                "権限プロファイル",
                "有効なポリシー",
                "プロファイルポリシー",
            ),
        ] {
            let mut menu = menu_fixture();
            menu.kind = SettingsKind::ApprovalPolicy;
            (menu.current_value, menu.current, menu.choices) =
                approval_policy_settings_values(&thread, &json!("never"), &json!({}), language);
            assert_eq!(menu.current_value, Some(json!("on-request").to_string()));
            let effective = format_approval_policy_with_language(&json!("on-request"), language);
            let saved = format_approval_policy_with_language(&json!("never"), language);
            assert_eq!(
                menu.current,
                format!("{effective_label}: {effective}\n{saved_label}: {saved}")
            );
            let (content, rows) = render_settings_menu("policy-p", &menu, language);
            assert!(content.starts_with(policy_label));
            let rows = serde_json::to_value(rows).unwrap();
            assert_eq!(rows[0]["components"][0]["label"], format!("✓ {effective}"));
            assert_eq!(rows[0]["components"][2]["label"], saved);
            let home =
                settings_home_values(&thread, crate::config::InputMode::Steering, None, language);
            assert!(home.1.contains(&format!("{policy_label}: {effective}")));
            assert!(home.1.contains(&format!("{permission_label}:")));
            let missing =
                approval_policy_settings_values(&json!({}), &json!("never"), &json!({}), language);
            assert_eq!(missing.0, None);
            assert_eq!(
                missing.1,
                format!(
                    "{effective_label}: {}\n{saved_label}: {saved}",
                    format_approval_policy_with_language(&Value::Null, language)
                )
            );
        }
        assert_eq!(thread, before);
    }

    #[test]
    fn model_catalog_diagnostics_preserve_the_cause_and_existing_settings() {
        let catalog = json!({"warnings":["AGY model discovery failed: command exited with code 7", "model metadata unavailable"], "data":[{"model":"example"}]});
        let before = catalog.clone();
        assert_eq!(
            model_catalog_warnings("Current model: example".into(), &catalog),
            "Current model: example\nAGY model discovery failed: command exited with code 7\nmodel metadata unavailable"
        );
        assert_eq!(
            model_catalog_warnings("Current model: example".into(), &json!({"data":[]})),
            "Current model: example"
        );
        assert_eq!(catalog, before);
    }

    #[test]
    fn approval_choice_and_delivery_error_keep_request_identity_and_unrelated_cards() {
        let snapshot = |request: &PendingApproval| {
            (
                request.native_id.clone(),
                request.method.clone(),
                request.params.clone(),
                request.channel_id,
                request.message_id,
                request.claimed,
                request.connection_generation,
            )
        };
        for (method, params, action, expected) in [
            (
                "item/commandExecution/requestApproval",
                json!({"availableDecisions":["accept","decline"],"threadId":"thread-r1","turnId":"turn-r1","itemId":"item-r1","callId":"call-r1","reason":"provided reason","command":"example-command"}),
                "offered-1",
                json!({"decision":"decline"}),
            ),
            (
                "mcpServer/elicitation/request",
                json!({"mode":"form","threadId":"thread-r1","turnId":"turn-r1","callId":"call-r1","reason":"provided reason","message":"Example input"}),
                "decline",
                json!({"action":"decline"}),
            ),
        ] {
            let r1 = PendingApproval {
                native_id: json!("native-r1"),
                method: method.into(),
                params,
                channel_id: ChannelId::new(10),
                message_id: serenity::all::MessageId::new(11),
                claimed: false,
                connection_generation: 3,
            };
            let r2 = PendingApproval {
                native_id: json!("native-r2"),
                method: "item/commandExecution/requestApproval".into(),
                params: json!({"threadId":"thread-r2","turnId":"turn-r2","itemId":"item-r2","reason":"other reason"}),
                channel_id: ChannelId::new(20),
                message_id: serenity::all::MessageId::new(21),
                claimed: false,
                connection_generation: 3,
            };
            let before_r2 = snapshot(&r2);
            let mut requests =
                std::collections::HashMap::from([("r1".into(), r1.clone()), ("r2".into(), r2)]);
            let (claimed, response) = claim_approval(&mut requests, "r1", action, 3).unwrap();
            assert_eq!(response, expected);
            let mut expected_claim = r1;
            expected_claim.claimed = true;
            assert_eq!(snapshot(&claimed), snapshot(&expected_claim));
            assert_eq!(snapshot(&requests["r2"]), before_r2);
            let error = anyhow!("socket closed before delivery acknowledgment")
                .context("approval transport");
            for (language, prefix) in [
                (
                    crate::i18n::Language::English,
                    "The approval response could not be confirmed as sent.",
                ),
                (
                    crate::i18n::Language::Korean,
                    "승인 응답 전달을 확인하지 못했습니다.",
                ),
                (crate::i18n::Language::Chinese, "无法确认审批响应已发送。"),
                (
                    crate::i18n::Language::Japanese,
                    "承認応答の送信を確認できませんでした。",
                ),
            ] {
                let message = approval_delivery_failure_message(&error, language, None);
                assert!(message.starts_with(prefix));
                assert!(
                    message.ends_with(
                        "approval transport: socket closed before delivery acknowledgment"
                    )
                );
            }
            assert_eq!(snapshot(&requests["r1"]), snapshot(&expected_claim));
            assert_eq!(snapshot(&requests["r2"]), before_r2);
            let resolved = claim_resolved_pending(&mut requests, &json!("native-r2"), 3);
            assert_eq!(resolved.len(), 1);
            assert_eq!(resolved[0].0, "r2");
            assert_eq!(snapshot(&requests["r1"]), snapshot(&expected_claim));
        }
    }

    #[test]
    fn modal_delivery_error_reports_uncertainty_and_keeps_the_request_claimed() {
        let snapshot = |request: &PendingApproval| {
            (
                request.native_id.clone(),
                request.method.clone(),
                request.params.clone(),
                request.channel_id,
                request.message_id,
                request.claimed,
                request.connection_generation,
            )
        };
        for (method, native_id, params, field, answer, expected) in [
            (
                "item/tool/requestUserInput",
                json!("native-question"),
                json!({"threadId":"thread-r1","turnId":"turn-r1","itemId":"item-r1","questions":[{"id":"question-r1","question":"Choose a scope","options":[{"label":"Current operation","description":"Use the current operation only"}]}]}),
                "question-r1",
                "Current operation",
                json!({"answers":{"question-r1":{"answers":["Current operation"]}}}),
            ),
            (
                "mcpServer/elicitation/request",
                json!(17),
                json!({"mode":"form","threadId":"thread-r1","turnId":null,"serverName":"example-server","message":"Choose a scope","requestedSchema":{"type":"object","properties":{"scope":{"type":"string","enum":["current","session"]}},"required":["scope"]},"_meta":{"persist":"session"}}),
                "elicitation_json",
                "{\"scope\":\"current\"}",
                json!({"action":"accept","content":{"scope":"current"}}),
            ),
        ] {
            let pending = PendingApproval {
                native_id,
                method: method.into(),
                params,
                channel_id: ChannelId::new(10),
                message_id: serenity::all::MessageId::new(11),
                claimed: false,
                connection_generation: 3,
            };
            let other = PendingApproval {
                native_id: json!("native-other"),
                method: "item/commandExecution/requestApproval".into(),
                params: json!({"threadId":"thread-other","turnId":"turn-other","itemId":"item-other","availableDecisions":["decline"]}),
                channel_id: ChannelId::new(20),
                message_id: serenity::all::MessageId::new(21),
                claimed: false,
                connection_generation: 3,
            };
            let before_other = snapshot(&other);
            let components = json!([{ "components":[{"custom_id":field,"value":answer}] }]);
            let mut submitted = serde_json::Map::new();
            collect_modal_values(&components, &mut submitted);
            let result = match pending.method.as_str() {
                "item/tool/requestUserInput" => user_input_result(&pending.params, submitted),
                "mcpServer/elicitation/request" => elicitation_result(&pending.params, submitted),
                _ => unreachable!(),
            }
            .unwrap();
            assert_eq!(result, expected);
            let mut requests =
                HashMap::from([("r1".into(), pending.clone()), ("r2".into(), other)]);
            let claimed = claim_pending(&mut requests, "r1", 3).unwrap();
            let mut expected_claim = pending;
            expected_claim.claimed = true;
            assert_eq!(snapshot(&claimed), snapshot(&expected_claim));
            let error = anyhow::Error::new(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "pipe closed after write",
            ))
            .context("native response transport");
            for (language, notice) in [
                (
                    crate::i18n::Language::English,
                    "The approval response could not be confirmed as sent. This does not establish whether the command ran.",
                ),
                (
                    crate::i18n::Language::Korean,
                    "승인 응답 전달을 확인하지 못했습니다. 이 오류만으로 명령 실행 여부를 판단할 수 없습니다.",
                ),
                (
                    crate::i18n::Language::Chinese,
                    "无法确认审批响应已发送。此错误不能证明命令是否已执行。",
                ),
                (
                    crate::i18n::Language::Japanese,
                    "承認応答の送信を確認できませんでした。このエラーだけではコマンドが実行されたかは判断できません。",
                ),
            ] {
                let response = serde_json::to_value(approval_delivery_failure_response(
                    &error, language, None,
                ))
                .unwrap();
                assert_eq!(
                    response["content"],
                    format!("{notice}\nnative response transport: pipe closed after write")
                );
                assert!(response.get("components").is_none());
                assert!(claim_pending(&mut requests, "r1", 3).is_err());
                assert_eq!(snapshot(&requests["r1"]), snapshot(&expected_claim));
                assert_eq!(snapshot(&requests["r2"]), before_other);
                assert_eq!(result, expected);
            }
        }
    }

    #[test]
    fn approval_policy_menu_localizes_buttons_and_marks_the_actual_policy() {
        use crate::i18n::Language;
        for (language, title, never, back, advanced) in [
            (
                Language::English,
                "Approval policy",
                "Never ask (never)",
                "Back",
                "Advanced settings",
            ),
            (
                Language::Korean,
                "승인 방식",
                "승인 요청 안 함 (never)",
                "뒤로가기",
                "고급 설정",
            ),
            (
                Language::Chinese,
                "审批策略",
                "不请求审批 (never)",
                "返回",
                "高级设置",
            ),
            (
                Language::Japanese,
                "承認ポリシー",
                "承認を要求しない (never)",
                "戻る",
                "詳細設定",
            ),
        ] {
            assert_eq!(
                settings_kind_title(SettingsKind::ApprovalPolicy, language),
                title
            );
            assert_eq!(
                settings_kind_title(SettingsKind::NativeFeatures, language),
                advanced
            );
            assert_eq!(
                settings_kind_from_key("approval-policy"),
                Some(SettingsKind::ApprovalPolicy)
            );
            let mut menu = menu_fixture();
            menu.kind = SettingsKind::ApprovalPolicy;
            menu.choices = approval_policy_choices(&json!({"requirements":null}), language);
            menu.current_value = Some(json!("never").to_string());
            menu.current = format_approval_policy_with_language(&json!("never"), language);
            let (content, rows) = render_settings_menu("approval-example", &menu, language);
            assert!(content.contains(never));
            let payload = serde_json::to_value(rows).unwrap();
            assert_eq!(payload[0]["components"][2]["label"], format!("✓ {never}"));
            assert_eq!(payload[1]["components"][0]["label"], back);
            assert_eq!(
                menu.choices
                    .iter()
                    .map(|choice| choice.value.as_str())
                    .collect::<Vec<_>>(),
                vec!["\"on-request\"", "\"untrusted\"", "\"never\""]
            );
        }
    }

    #[test]
    fn approval_policy_choices_use_native_requirements_and_preserve_granular_values() {
        use crate::i18n::Language;
        let granular = json!({"granular":{
            "sandbox_approval":true,"rules":false,"mcp_elicitations":true,
            "skill_approval":false,"request_permissions":true
        }});
        let choices = approval_policy_choices(
            &json!({"requirements":{
                "allowedApprovalPolicies":["on-request",granular]
            }}),
            Language::English,
        );
        assert_eq!(choices.len(), 2);
        assert_eq!(
            serde_json::from_str::<Value>(&choices[1].value).unwrap(),
            granular
        );
        assert!(choices.iter().all(|choice| choice.value != "\"never\""));
        assert!(
            approval_policy_choices(
                &json!({"requirements":{"allowedApprovalPolicies":[]}}),
                Language::English
            )
            .is_empty()
        );
        assert_eq!(
            format_approval_policy_with_language(&granular, Language::English),
            granular.to_string()
        );
        assert_eq!(
            format_approvals_reviewer_with_language(&json!("custom-reviewer"), Language::English),
            "custom-reviewer"
        );
        assert_eq!(
            format_approvals_reviewer_with_language(&json!("auto_review"), Language::English),
            "Automatic review (auto_review)"
        );
    }

    #[test]
    fn advanced_settings_has_no_duplicate_child_model_route() {
        assert_eq!(
            settings_kind_from_key("subagent-model"),
            Some(SettingsKind::SubagentModel)
        );
        assert!(
            NativeFeatureKey::ALL
                .iter()
                .all(|key| *key != NativeFeatureKey::SubagentModel)
        );
        assert_eq!(NativeFeatureKey::from_key("subagentModel"), None);
    }

    #[test]
    fn antigravity_settings_are_opt_in_and_localized_with_text_modals() {
        use crate::i18n::Language;
        let preferences = crate::native::NativeFeaturePreferences::default();
        for (language, off, sharing, models) in [
            (Language::English, "Off", "Settings sharing", "Model IDs"),
            (Language::Korean, "끄기", "설정 공유", "모델 ID"),
            (Language::Chinese, "关闭", "设置共享", "模型 ID"),
            (Language::Japanese, "オフ", "設定の共有", "モデル ID"),
        ] {
            let mut menu = menu_fixture();
            menu.kind = SettingsKind::Antigravity;
            (menu.current_value, menu.current, menu.choices) =
                antigravity_settings_values(&preferences, language);
            assert_eq!(menu.current_value.as_deref(), Some("off"));
            assert!(menu.current.contains("AGY"));
            let (_, rows) = render_settings_menu("agy-example", &menu, language);
            let payload = serde_json::to_value(rows).unwrap();
            assert_eq!(payload[0]["components"][1]["label"], format!("✓ {off}"));
            assert_eq!(payload[0]["components"][2]["label"], sharing);
            assert_eq!(payload[0]["components"][3]["label"], models);
            assert_eq!(
                payload[0]["components"][4]["custom_id"],
                "codex:settings:agy-example:c4"
            );
            assert_eq!(antigravity_text_field(&menu, "c0"), None);
            assert_eq!(antigravity_text_field(&menu, "c2"), None);
            assert_eq!(antigravity_text_field(&menu, "c3"), Some("models"));
            for field in ["models"] {
                let modal =
                    antigravity_text_modal("agy-example", field, &preferences, language).unwrap();
                let value = serde_json::to_value(modal).unwrap();
                assert_eq!(
                    value["custom_id"],
                    format!("codex:agy-settings:agy-example:{field}")
                );
                assert_eq!(
                    value["components"][0]["components"][0]["custom_id"],
                    "value"
                );
                assert_eq!(value["components"][0]["components"][0]["required"], false);
            }
        }
    }

    #[test]
    fn antigravity_text_settings_preserve_exact_model_ids_and_other_preferences() {
        let mut preferences = crate::native::NativeFeaturePreferences::default();
        preferences.subagent_model = Some("child-example".into());
        preferences.subagent_model_provider = Some("provider-example".into());
        preferences.subagent_model_executor = Some("codex".into());
        preferences.antigravity_share_settings = Some(false);
        set_antigravity_text_preference(
            &mut preferences,
            "models",
            "  exact/id-v2\n\nother:model+custom \n",
        )
        .unwrap();
        assert_eq!(preferences.antigravity_share_settings, Some(false));
        assert_eq!(
            preferences.antigravity_models,
            vec!["exact/id-v2", "other:model+custom"]
        );
        assert!(!preferences.antigravity_enabled);
        assert_eq!(preferences.subagent_model.as_deref(), Some("child-example"));
        assert_eq!(
            preferences.subagent_model_executor.as_deref(),
            Some("codex")
        );
        let before = preferences.clone();
        assert!(set_antigravity_text_preference(&mut preferences, "unknown", "anything").is_err());
        assert_eq!(preferences, before);
        assert!(set_antigravity_text_preference(&mut preferences, "path", "agy").is_err());
        set_antigravity_text_preference(&mut preferences, "models", "").unwrap();
        assert_eq!(preferences.antigravity_path, None);
        assert!(preferences.antigravity_models.is_empty());
    }

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
    fn connector_setup_link_keeps_native_access_check_action_available() {
        let params = json!({
            "mode": "form",
            "message": "Connect the calendar app.",
            "requestedSchema": {"type": "object", "properties": {}},
            "_meta": {
                "codex_approval_kind": "tool_suggestion",
                "tool_id": "calendar",
                "tool_type": "connector",
                "tool_name": "Calendar",
                "install_url": "https://example.invalid/setup",
                "app_connector_ids": ["calendar"]
            }
        });
        let rows = serde_json::to_value(approval_actions_with_language(
            "mcpServer/elicitation/request",
            "request",
            &params,
            crate::i18n::Language::Korean,
        ))
        .unwrap();
        let buttons = rows[0]["components"].as_array().unwrap();
        assert_eq!(buttons.len(), 5);
        assert!(buttons[0]["url"].as_str().unwrap().ends_with("/setup"));
        assert_eq!(
            buttons[1]["custom_id"],
            "codex:approval:request:plugin-check"
        );
        assert_eq!(buttons[1]["label"], "설정 완료 / 접근 확인");
        assert_eq!(buttons[1]["style"], 1);
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
    fn queued_default_change_is_reported_as_pending_separately_from_current_turn() {
        let pending = format_pending_settings_update(
            "requested-model",
            "The active response was updated.",
            crate::i18n::Language::English,
        );
        assert!(pending.contains("The conversation-default update was queued"));
        assert!(pending.contains("Current response: The active response was updated."));
        assert!(!pending.contains("Conversation defaults after update:"));

        let confirmed = format_confirmed_settings_update(
            "requested-model",
            &json!({"model":"requested-model"}),
            "The active response was updated.",
            crate::i18n::Language::English,
        );
        assert!(confirmed.contains("Conversation defaults after update:\nModel: requested-model"));
        assert!(confirmed.contains("Current response: The active response was updated."));
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

    #[test]
    fn stale_settings_model_scope_and_choice_are_rejected_before_apply() {
        let choices = vec![SettingsChoice {
            value: "high".to_owned(),
            label: "High".to_owned(),
        }];
        let prior_scope = Some("[\"antigravity\",\"antigravity\",\"model-a\"]".to_owned());
        let changed_scope = Some("[\"antigravity\",\"antigravity\",\"model-b\"]".to_owned());
        assert_eq!(
            settings_choice_freshness(
                SettingsKind::Effort,
                &prior_scope,
                &changed_scope,
                &choices,
                "high"
            ),
            SettingsChoiceFreshness::ModelChanged
        );
        assert_eq!(
            settings_choice_freshness(
                SettingsKind::Effort,
                &prior_scope,
                &prior_scope,
                &choices,
                "low"
            ),
            SettingsChoiceFreshness::Unavailable
        );
        assert_eq!(
            settings_choice_freshness(
                SettingsKind::Effort,
                &prior_scope,
                &prior_scope,
                &choices,
                "high"
            ),
            SettingsChoiceFreshness::Current
        );

        let menu = menu_fixture();
        assert!(settings_menu_is_current(&menu, 7, "thread-menu"));
        assert!(!settings_menu_is_current(&menu, 8, "thread-menu"));
        assert!(!settings_menu_is_current(&menu, 7, "thread-other"));

        assert!(
            ensure_settings_apply_target(Some(7), 7, Some("thread-menu"), "thread-menu").is_ok()
        );
        assert!(
            ensure_settings_apply_target(Some(8), 7, Some("thread-menu"), "thread-menu").is_err()
        );
        assert!(
            ensure_settings_apply_target(Some(7), 7, Some("thread-old"), "thread-menu").is_err()
        );
        assert!(
            ensure_settings_model_scope(SettingsKind::Effort, &prior_scope, changed_scope.clone())
                .is_err()
        );
    }

    #[test]
    fn failed_model_selection_persistence_cannot_be_reported_as_success() {
        let notice = settings_apply_notice(
            Err(anyhow!("could not persist selected model")),
            crate::i18n::Language::Korean,
            None,
        );
        assert!(notice.starts_with("설정을 적용하지 못했습니다."));
        assert!(notice.contains("could not persist selected model"));
        assert!(!notice.contains("설정을 적용했습니다"));
    }

    #[test]
    fn failed_model_resume_notice_includes_native_error_cause() {
        let native_error = crate::native::NativeRequestError::from_response(
            "thread/resume".to_owned(),
            &json!({"code":-32000,"message":"selected model is unavailable"}),
        );
        let error = Err(anyhow::Error::new(native_error)
            .context("could not switch model/provider; restored the previous conversation"));
        let notice = settings_apply_notice(error, crate::i18n::Language::Korean, None);

        assert!(
            notice.contains("could not switch model/provider; restored the previous conversation")
        );
        assert!(notice.contains(
            "Native request failed (thread/resume; code -32000): selected model is unavailable"
        ));
        assert!(!notice.contains("설정을 적용했습니다"));
    }

    #[test]
    fn top_level_command_failure_keeps_native_code_and_message() {
        let error = anyhow::Error::new(crate::native::NativeRequestError::from_response(
            "thread/resume".to_owned(),
            &json!({"code":-32000,"message":"selected model is unavailable"}),
        ));
        let language = crate::i18n::Language::Korean;
        let notice = localized_failure_message(
            language.message(MessageKey::CommandsTheDiscodexRequestFailedCheckTheInputAnd71994868),
            &settings_error_detail(&error, language, None),
        );

        assert!(notice.starts_with(
            language.message(MessageKey::CommandsTheDiscodexRequestFailedCheckTheInputAnd71994868)
        ));
        assert!(notice.contains("code -32000"));
        assert!(notice.contains("selected model is unavailable"));
        assert!(!notice.contains("요청을 성공적으로 처리했습니다"));
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
        // The catalog supplies menu choices, but a stale exact model choice
        // is passed to Native for authoritative validation during apply.
        assert!(
            validate_setting_choice("model", "gpt-removed", &catalog, &current)
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

    #[test]
    fn stale_provider_qualified_model_choice_preserves_its_explicit_provider() {
        use crate::model_catalog::{ModelExecutor, ModelSelection};

        let stale_choice = serde_json::to_string(&[
            "antigravity",
            "antigravity",
            "exact-model-no-longer-in-catalog",
        ])
        .unwrap();
        let current_thread = json!({
            "model": "current-model",
            "modelExecutor": "codex",
            "modelProvider": "different-provider"
        });
        let selection = model_selection_for_choice(
            &json!({"data": []}),
            &stale_choice,
            &current_thread,
            crate::i18n::Language::English,
        )
        .unwrap();

        assert_eq!(
            selection,
            ModelSelection {
                executor: ModelExecutor::Antigravity,
                provider: "antigravity".to_owned(),
                model: "exact-model-no-longer-in-catalog".to_owned(),
            }
        );

        let plain_selection = model_selection_for_choice(
            &json!({"provider":"catalog-provider","data": []}),
            "exact-model-id",
            &json!({"model":"current-model","modelExecutor":"codex","modelProvider":"active-provider"}),
            crate::i18n::Language::English,
        )
        .unwrap();
        assert_eq!(plain_selection.provider, "active-provider");
        assert_eq!(plain_selection.model, "exact-model-id");
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
    fn settings_menu_ids_are_unique_even_when_navigation_is_disabled() {
        for count in [0usize, 1, 10, 20, 21, 40, 41] {
            let pages = count.div_ceil(SETTINGS_PAGE_SIZE).max(1);
            for page in 0..pages {
                let mut menu = menu_fixture();
                menu.kind = SettingsKind::Voice;
                menu.page = page;
                menu.choices = (0..count)
                    .map(|index| SettingsChoice {
                        value: format!("voice-{index}"),
                        label: format!("Voice {index}"),
                    })
                    .collect();
                let (_, rows) =
                    render_settings_menu("menu-regression", &menu, crate::i18n::Language::English);
                let payload = serde_json::to_value(rows).unwrap();
                let ids = payload
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|row| row["components"].as_array().unwrap())
                    .map(|button| button["custom_id"].as_str().unwrap())
                    .collect::<Vec<_>>();
                let unique = ids
                    .iter()
                    .copied()
                    .collect::<std::collections::HashSet<_>>();
                assert_eq!(
                    ids.len(),
                    unique.len(),
                    "duplicate ID for {count} choices, page {page}"
                );
                let nav_rows = payload.as_array().unwrap();
                let nav = nav_rows
                    .iter()
                    .flat_map(|row| row["components"].as_array().unwrap())
                    .filter(|button| {
                        let id = button["custom_id"].as_str().unwrap();
                        id.ends_with(":h")
                            || id.ends_with(":x")
                            || id.rsplit(':').next().is_some_and(|action| {
                                action.starts_with('p') || action.starts_with('n')
                            })
                    })
                    .collect::<Vec<_>>();
                if pages == 1 {
                    assert_eq!(nav.len(), 2);
                    assert_eq!(nav[0]["custom_id"], "codex:settings:menu-regression:h");
                    assert_eq!(nav[1]["custom_id"], "codex:settings:menu-regression:x");
                    continue;
                }
                assert_eq!(nav.len(), 4);
                for (button, expected_page, disabled) in [
                    (&nav[0], page.saturating_sub(1), page == 0),
                    (&nav[2], (page + 1).min(pages - 1), page + 1 >= pages),
                ] {
                    let action = button["custom_id"]
                        .as_str()
                        .unwrap()
                        .rsplit(':')
                        .next()
                        .unwrap();
                    assert_eq!(settings_page_from_action(action), Some(expected_page));
                    assert_eq!(button["disabled"], disabled);
                }
                assert_eq!(nav[1]["custom_id"], "codex:settings:menu-regression:h");
                assert_eq!(nav[3]["custom_id"], "codex:settings:menu-regression:x");
            }
        }
    }

    #[test]
    fn current_effort_uses_native_value_without_inventing_a_default() {
        use crate::i18n::Language;
        let choices = ["low", "high"]
            .map(|value| effort_settings_choice(&json!(value), Language::Korean).unwrap());
        assert_eq!(
            effort_current_label(&choices, Some(&json!("high")), Language::Korean),
            "높음"
        );
        assert_eq!(
            effort_current_label(&choices, Some(&Value::Null), Language::Korean),
            "확인할 수 없음"
        );
        assert_eq!(
            effort_current_label(&choices, None, Language::Korean),
            "확인할 수 없음"
        );
        assert_eq!(
            format_settings_with_language(&json!({"effort":"high"}), Language::Korean),
            "추론 강도: 높음"
        );
        let mut menu = menu_fixture();
        menu.kind = SettingsKind::Effort;
        menu.choices = choices.to_vec();
        menu.current_value = Some("high".to_owned());
        menu.current = effort_current_label(&menu.choices, Some(&json!("high")), Language::Korean);
        let (content, rows) = render_settings_menu("current-effort", &menu, Language::Korean);
        assert!(content.contains("현재: 높음"));
        let payload = serde_json::to_value(rows).unwrap();
        assert_eq!(payload[0]["components"][0]["label"], "낮음");
        assert_eq!(payload[0]["components"][1]["label"], "✓ 높음");
    }

    #[test]
    fn native_setting_choices_translate_labels_without_changing_values() {
        use crate::i18n::Language;
        let efforts = [
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ];
        let tiers = ["priority", "standard", "flex"];
        for (language, effort_labels, tier_labels) in [
            (
                Language::English,
                [
                    "None",
                    "Minimal",
                    "Low",
                    "Medium",
                    "High",
                    "Extra high",
                    "Maximum",
                    "Ultra",
                ],
                ["Priority", "Standard", "Flex"],
            ),
            (
                Language::Korean,
                [
                    "없음",
                    "최소",
                    "낮음",
                    "보통",
                    "높음",
                    "매우 높음",
                    "최대",
                    "울트라",
                ],
                ["우선 처리", "표준", "유연 처리"],
            ),
            (
                Language::Chinese,
                ["无", "最低", "低", "中", "高", "很高", "最高", "超高"],
                ["优先", "标准", "灵活"],
            ),
            (
                Language::Japanese,
                [
                    "なし",
                    "最小",
                    "低",
                    "中",
                    "高",
                    "非常に高い",
                    "最大",
                    "ウルトラ",
                ],
                ["優先", "標準", "フレックス"],
            ),
        ] {
            for (value, label) in efforts.into_iter().zip(effort_labels) {
                for input in [json!(value), json!({"reasoningEffort": value})] {
                    let choice = effort_settings_choice(&input, language).unwrap();
                    assert_eq!(
                        (choice.value.as_str(), choice.label.as_str()),
                        (value, label)
                    );
                }
            }
            for (value, label) in tiers.into_iter().zip(tier_labels) {
                for input in [json!(value), json!({"id": value, "name": "Native name"})] {
                    let choice = service_tier_settings_choice(&input, language).unwrap();
                    assert_eq!(
                        (choice.value.as_str(), choice.label.as_str()),
                        (value, label)
                    );
                }
            }
            let future = effort_settings_choice(&json!("future-effort"), language).unwrap();
            assert_eq!(
                (future.value.as_str(), future.label.as_str()),
                ("future-effort", "future-effort")
            );
            let future = service_tier_settings_choice(
                &json!({"id":"future-tier", "name":"Native future tier"}),
                language,
            )
            .unwrap();
            assert_eq!(
                (future.value.as_str(), future.label.as_str()),
                ("future-tier", "Native future tier")
            );
            let unnamed = service_tier_settings_choice(&json!("unnamed-tier"), language).unwrap();
            assert_eq!(
                (unnamed.value.as_str(), unnamed.label.as_str()),
                ("unnamed-tier", "unnamed-tier")
            );
            assert!(effort_settings_choice(&json!({}), language).is_none());
            assert!(service_tier_settings_choice(&json!({}), language).is_none());
        }
    }

    #[test]
    fn settings_controls_follow_the_selected_language_on_every_panel() {
        use crate::i18n::Language;
        for (language, exit, back, previous, next) in [
            (Language::English, "Exit", "Back", "Previous", "Next"),
            (Language::Korean, "나가기", "뒤로가기", "이전", "다음"),
            (Language::Chinese, "退出", "返回", "上一页", "下一页"),
            (Language::Japanese, "閉じる", "戻る", "前へ", "次へ"),
        ] {
            for kind in [
                SettingsKind::Hub,
                SettingsKind::Model,
                SettingsKind::SubagentModel,
                SettingsKind::Effort,
                SettingsKind::Fast,
                SettingsKind::Permissions,
                SettingsKind::Input,
                SettingsKind::Voice,
                SettingsKind::Updates,
                SettingsKind::MessageAccess,
                SettingsKind::PairedAccounts,
                SettingsKind::Whitelist,
                SettingsKind::Blacklist,
                SettingsKind::RegisteredChannels,
                SettingsKind::NativeFeatures,
                SettingsKind::Antigravity,
                SettingsKind::NativeFeature(NativeFeatureKey::CodeMode),
            ] {
                for count in [1, 21] {
                    if kind == SettingsKind::Hub && count == 21 {
                        continue;
                    }
                    let mut menu = menu_fixture();
                    menu.kind = kind;
                    menu.choices = if kind == SettingsKind::Hub {
                        Vec::new()
                    } else {
                        (0..count)
                            .map(|index| SettingsChoice {
                                value: format!("choice-{index}"),
                                label: format!("Choice {index}"),
                            })
                            .collect()
                    };
                    let (_, rows) = render_settings_menu("localized", &menu, language);
                    let payload = serde_json::to_value(rows).unwrap();
                    let rows = payload.as_array().unwrap();
                    assert!(rows.len() <= 5);
                    let controls = rows
                        .iter()
                        .flat_map(|row| row["components"].as_array().unwrap())
                        .collect::<Vec<_>>();
                    let labels = controls
                        .iter()
                        .map(|button| button["label"].as_str().unwrap())
                        .collect::<Vec<_>>();
                    let expected = if is_access_list(kind) {
                        None
                    } else if kind == SettingsKind::Hub {
                        Some(vec![exit])
                    } else if count <= SETTINGS_PAGE_SIZE {
                        Some(vec![back, exit])
                    } else {
                        Some(vec![previous, back, next, exit])
                    };
                    if let Some(expected) = expected {
                        assert!(labels.ends_with(&expected));
                    } else {
                        assert!(labels.contains(&exit));
                    }
                    let ids = controls
                        .iter()
                        .map(|button| button["custom_id"].as_str().unwrap())
                        .collect::<Vec<_>>();
                    if kind == SettingsKind::Hub {
                        assert_eq!(ids, ["codex:settings:localized:x"]);
                    } else if is_access_list(kind) {
                        assert!(ids.contains(&"codex:settings:localized:b"));
                        assert!(ids.contains(&"codex:settings:localized:x"));
                    } else {
                        assert!(ids.contains(&"codex:settings:localized:h"));
                        assert!(ids.contains(&"codex:settings:localized:x"));
                    }
                }
            }
        }
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
                "update",
                "settings",
                "voice",
                "status",
                "interrupt",
                "compact",
                "new",
                "voice-leave",
                "voice-join",
                "plugins"
            ]
        );
    }

    #[tokio::test]
    async fn compact_command_sends_native_request_and_localizes_acceptance() {
        use crate::i18n::Language;
        for (language, expected) in [
            (
                Language::English,
                "Requested compaction of the current conversation.",
            ),
            (Language::Korean, "현재 대화의 문맥 압축을 요청했습니다."),
            (Language::Chinese, "已请求压缩当前对话的上下文。"),
            (
                Language::Japanese,
                "現在の会話のコンテキスト圧縮を要求しました。",
            ),
        ] {
            let mut requests = Vec::new();
            let response =
                request_compaction("conversation-example", language, |method, params| {
                    requests.push((method, params));
                    std::future::ready(Ok(json!({})))
                })
                .await;
            assert_eq!(response, expected);
            assert_eq!(
                requests,
                vec![(
                    "thread/compact/start",
                    json!({"threadId":"conversation-example"}),
                )]
            );

            let commands = serde_json::to_value(definitions(language)).unwrap();
            let compact = commands[0]["options"]
                .as_array()
                .unwrap()
                .iter()
                .find(|option| option["name"] == "compact")
                .unwrap();
            assert_eq!(compact["type"], 1);
            assert_eq!(
                compact["description"],
                language.text(
                    "Compact conversation context, replacing any active response",
                    "진행 중인 응답을 중단하고 대화 문맥을 압축합니다",
                    "压缩对话上下文，替换正在进行的回复",
                    "進行中の応答を中断して会話のコンテキストを圧縮します",
                )
            );
            assert!(compact["options"].as_array().is_none_or(Vec::is_empty));
        }
    }

    #[tokio::test]
    async fn compact_command_preserves_native_failure_without_retry_or_success_notice() {
        use crate::i18n::Language;
        for (language, expected_prefix) in [
            (
                Language::English,
                "Could not request conversation compaction:",
            ),
            (Language::Korean, "대화 문맥 압축을 요청하지 못했습니다:"),
            (Language::Chinese, "无法请求压缩对话上下文："),
            (
                Language::Japanese,
                "会話のコンテキスト圧縮を要求できませんでした：",
            ),
        ] {
            let mut calls = 0;
            let response =
                request_compaction("conversation-example", language, |method, params| {
                    calls += 1;
                    assert_eq!(method, "thread/compact/start");
                    assert_eq!(params, json!({"threadId":"conversation-example"}));
                    std::future::ready(Err(
                        anyhow!("provider rejected compaction").context("native request")
                    ))
                })
                .await;
            assert_eq!(calls, 1);
            assert_eq!(
                response,
                format!("{expected_prefix}\nnative request: provider rejected compaction")
            );
        }
    }

    #[test]
    fn message_access_policy_labels_and_keys_cover_all_four_modes() {
        use crate::i18n::Language;
        let policies = [
            (
                MessagePolicy::Everyone,
                "everyone",
                "Everyone, including DMs",
                "DM을 포함한 모든 사용자",
                "所有人（包括私信）",
                "DMを含む全員",
            ),
            (
                MessagePolicy::RegisteredServer,
                "registered-server",
                "All members in registered server",
                "등록 서버의 모든 구성원",
                "已登记服务器中的所有成员",
                "登録サーバーの全メンバー",
            ),
            (
                MessagePolicy::RegisteredChannels,
                "registered-channels",
                "Registered channels",
                "등록 채널",
                "已登记频道",
                "登録チャンネル",
            ),
            (
                MessagePolicy::Paired,
                "paired",
                "Paired accounts",
                "페어링 계정",
                "已配对账号",
                "ペアリング済みアカウント",
            ),
        ];
        for (policy, key, en, ko, zh, ja) in policies {
            assert_eq!(message_policy_key(policy), key);
            for (language, expected) in [
                (Language::English, en),
                (Language::Korean, ko),
                (Language::Chinese, zh),
                (Language::Japanese, ja),
            ] {
                assert_eq!(message_policy_label(policy, language), expected);
            }
        }
    }

    #[test]
    fn message_access_id_input_accepts_only_nonzero_discord_ids() {
        for input in ["123456789012345678", " 42 "] {
            assert!(parse_message_access_id(input).unwrap() > 0);
        }
        for input in ["", "0", " 0 ", "-1", "account-name", "18446744073709551616"] {
            assert!(parse_message_access_id(input).is_err());
        }
    }

    #[test]
    fn message_access_list_pages_render_first_and_last_rows_once_with_scoped_controls() {
        use crate::i18n::Language;
        let mut menu = menu_fixture();
        menu.kind = SettingsKind::Whitelist;
        menu.choices = (1..=25)
            .map(|id| SettingsChoice {
                value: id.to_string(),
                label: if id == 1 {
                    "Member Nickname".to_owned()
                } else if id == 2 {
                    "Unavailable in this server".to_owned()
                } else {
                    format!("Member {id}")
                },
            })
            .collect();
        menu.current_value = None;
        menu.page = 0;
        let (first, first_rows) = render_settings_menu("access-menu", &menu, Language::English);
        assert!(first.contains("Member Nickname (ID: 1)"));
        assert!(first.contains("Unavailable in this server (ID: 2)"));
        assert!(first.contains("Member 10 (ID: 10)"));
        assert!(!first.contains("Member 11 (ID: 11)"));
        assert!(!first.contains("ID: 1 (ID: 1)"));
        let payload = serde_json::to_value(first_rows).unwrap();
        let controls = payload
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row["components"].as_array().unwrap())
            .collect::<Vec<_>>();
        let ids = controls
            .iter()
            .map(|button| button["custom_id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"codex:settings:access-menu:a"));
        assert!(ids.contains(&"codex:settings:access-menu:r"));
        assert!(ids.contains(&"codex:settings:access-menu:b"));
        assert!(ids.contains(&"codex:settings:access-menu:x"));

        menu.page = 2;
        let (last, rows) = render_settings_menu("access-menu", &menu, Language::English);
        assert!(last.contains("Member 21 (ID: 21)"));
        assert!(last.contains("Member 25 (ID: 25)"));
        assert!(!last.contains("Member 20 (ID: 20)"));
        let controls = serde_json::to_value(rows).unwrap();
        let buttons = controls
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row["components"].as_array().unwrap())
            .collect::<Vec<_>>();
        assert!(
            buttons
                .iter()
                .any(|button| button["custom_id"] == "codex:settings:access-menu:p1")
        );
    }

    #[test]
    fn policy_selection_matches_the_emitted_choice_value() {
        let mut access = MessageAccess::default();
        access.policy = MessagePolicy::RegisteredChannels;
        let current_value = format!("policy:{}", message_policy_key(access.policy));
        let choice_value = format!(
            "policy:{}",
            message_policy_key(MessagePolicy::RegisteredChannels)
        );
        assert_eq!(current_value, choice_value);
    }

    #[test]
    fn account_display_name_tracks_current_server_nickname_precedence() {
        assert_eq!(
            member_display_name(Some("Nick"), Some("Global"), "User"),
            "Nick"
        );
        assert_eq!(member_display_name(None, Some("Global"), "User"), "Global");
        assert_eq!(member_display_name(None, None, "User"), "User");
        assert_eq!(
            member_display_name(Some("New Nick"), Some("Global"), "User"),
            "New Nick"
        );
    }

    #[tokio::test]
    async fn accounts_outside_server_use_discord_display_name_and_one_id() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v10/guilds/10/members/42"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(json!({"code":10007,"message":"Unknown Member"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut user = serenity::all::User::default();
        user.id = UserId::new(42);
        user.name = "test-account".into();
        user.global_name = Some("Display Name".into());
        Mock::given(method("GET"))
            .and(path("/api/v10/users/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(user))
            .expect(1)
            .mount(&server)
            .await;
        let http = serenity::http::HttpBuilder::new("test-token")
            .proxy(server.uri())
            .ratelimiter_disabled(true)
            .build();
        let label = account_display_name(
            &http,
            GuildId::new(10),
            UserId::new(42),
            crate::i18n::Language::English,
        )
        .await;
        assert_eq!(label, "Display Name");
        let mut menu = menu_fixture();
        menu.kind = SettingsKind::PairedAccounts;
        menu.choices = vec![SettingsChoice {
            value: "42".into(),
            label,
        }];
        let (content, _) =
            render_settings_menu("test-panel", &menu, crate::i18n::Language::English);
        assert!(content.contains("Display Name (ID: 42)"));
        assert_eq!(content.matches("ID: 42").count(), 1);
        server.verify().await;
    }

    #[test]
    fn message_access_list_edits_add_and_remove_ids_without_touching_other_lists_or_policy() {
        let mut access = MessageAccess {
            policy: MessagePolicy::RegisteredServer,
            paired_user_ids: vec![11],
            whitelist: vec![22],
            blacklist: vec![33],
            channel_ids: vec![44],
        };
        apply_message_access_list_edit(&mut access, SettingsKind::Whitelist, true, 55).unwrap();
        assert_eq!(access.whitelist, vec![22, 55]);
        assert_eq!(access.paired_user_ids, vec![11]);
        assert_eq!(access.blacklist, vec![33]);
        assert_eq!(access.channel_ids, vec![44]);
        assert_eq!(access.policy, MessagePolicy::RegisteredServer);
        apply_message_access_list_edit(&mut access, SettingsKind::Whitelist, false, 22).unwrap();
        assert_eq!(access.whitelist, vec![55]);
        assert_eq!(access.paired_user_ids, vec![11]);
        assert_eq!(access.blacklist, vec![33]);
        assert_eq!(access.channel_ids, vec![44]);
        assert_eq!(access.policy, MessagePolicy::RegisteredServer);
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

#[cfg(test)]
mod plugin_menu_tests {
    use super::*;

    fn menu_fixture() -> PluginMenuState {
        PluginMenuState {
            claimed: false,
            requester_id: 31,
            connection_generation: 7,
            thread_id: "thread-test".to_owned(),
            mode: PluginMenuMode::Catalog,
            page: 0,
            entries: Vec::new(),
            selected: None,
            selected_detail: None,
            apps_needing_auth: Vec::new(),
            oauth_servers: Vec::new(),
            auth_view: false,
            install_notice: None,
        }
    }

    #[test]
    fn plugin_panel_claim_tracks_user_generation_and_in_flight_state() {
        let mut menus = HashMap::from([("panel".to_owned(), menu_fixture())]);
        assert!(claim_plugin_menu(&mut menus, "panel", 31, 7, "thread-test").is_ok());
        assert_eq!(
            claim_plugin_menu(&mut menus, "panel", 31, 7, "thread-test").unwrap_err(),
            PluginMenuClaimError::Busy
        );
        assert_eq!(
            claim_plugin_menu(&mut menus, "panel", 99, 7, "thread-test").unwrap_err(),
            PluginMenuClaimError::OtherUser
        );
        menus.get_mut("panel").unwrap().claimed = false;
        assert_eq!(
            claim_plugin_menu(&mut menus, "panel", 31, 8, "thread-test").unwrap_err(),
            PluginMenuClaimError::Stale
        );
        assert!(!menus.contains_key("panel"));
    }

    #[test]
    fn plugin_panel_renders_native_app_auth_links_and_pages_server_auth_choices() {
        let mut menu = menu_fixture();
        menu.mode = PluginMenuMode::Installed;
        menu.install_notice = Some("Codex confirmed installation of Example (ON_USE)".to_owned());
        menu.apps_needing_auth.push(PluginApp {
            id: "calendar".to_owned(),
            name: "Calendar".to_owned(),
            description: None,
            install_url: Some("https://example.invalid/connect".to_owned()),
            category: None,
        });
        menu.oauth_servers = (0..26).map(|index| format!("server-{index}")).collect();
        let rendered = render_plugin_menu("menu", &menu, crate::i18n::Language::English);
        assert!(rendered.0.contains("Codex confirmed installation"));
        assert!(rendered.0.contains("<https://example.invalid/connect>"));
        assert!(rendered.1.len() <= 5);

        menu.auth_view = true;
        menu.page = 1;
        let auth_view = render_plugin_menu("menu", &menu, crate::i18n::Language::English);
        assert!(auth_view.1.len() <= 5);
        assert!(auth_view.0.contains("Calendar"));
    }

    #[test]
    fn oauth_completion_message_reflects_only_native_outcome() {
        let completed = OAuthCompletion {
            name: "calendar".to_owned(),
            thread_id: Some("thread-test".to_owned()),
            login_id: Some("login-test".to_owned()),
            success: true,
            error: None,
        };
        assert!(
            oauth_completion_text(&completed, crate::i18n::Language::English).contains("completed")
        );
        let failed = OAuthCompletion {
            success: false,
            error: Some("denied".to_owned()),
            ..completed
        };
        let message = oauth_completion_text(&failed, crate::i18n::Language::English);
        assert!(message.contains("did not complete"));
        assert!(message.contains("denied"));
    }
}

#[cfg(test)]
mod native_feature_settings_tests {
    use super::*;

    #[test]
    fn feature_preference_choices_round_trip_on_off_and_native_default() {
        let mut preferences = crate::native::NativeFeaturePreferences::default();
        set_native_feature_preference(&mut preferences, NativeFeatureKey::CodeMode, Some("off"))
            .unwrap();
        assert_eq!(
            native_feature_saved_value(&preferences, NativeFeatureKey::CodeMode).as_deref(),
            Some("false")
        );
        set_native_feature_preference(&mut preferences, NativeFeatureKey::CodeMode, Some("on"))
            .unwrap();
        assert_eq!(
            native_feature_saved_value(&preferences, NativeFeatureKey::CodeMode).as_deref(),
            Some("true")
        );
        set_native_feature_preference(&mut preferences, NativeFeatureKey::CodeMode, None).unwrap();
        assert_eq!(
            native_feature_saved_value(&preferences, NativeFeatureKey::CodeMode),
            None
        );
        assert!(
            set_native_feature_preference(
                &mut preferences,
                NativeFeatureKey::CodeMode,
                Some("surprise")
            )
            .is_err()
        );
    }

    #[test]
    fn subagent_model_can_inherit_or_select_native_catalog_slug() {
        let mut preferences = crate::native::NativeFeaturePreferences::default();
        set_native_feature_preference(
            &mut preferences,
            NativeFeatureKey::SubagentModel,
            Some("gpt-test"),
        )
        .unwrap();
        assert_eq!(
            native_feature_saved_value(&preferences, NativeFeatureKey::SubagentModel).as_deref(),
            Some("gpt-test")
        );
        set_native_feature_preference(&mut preferences, NativeFeatureKey::SubagentModel, None)
            .unwrap();
        assert_eq!(
            native_feature_saved_value(&preferences, NativeFeatureKey::SubagentModel),
            None
        );
    }

    #[test]
    fn saved_subagent_checkmark_preserves_its_provider_affinity() {
        let catalog = json!({"provider":"local","catalogProvider":"cloud"});
        let mut preferences = crate::native::NativeFeaturePreferences::default();
        preferences.subagent_model = Some("shared".into());
        assert_eq!(
            subagent_preference_scope(&preferences, &catalog),
            Some(serde_json::to_string(&["cloud", "shared"]).unwrap())
        );
        preferences.subagent_model_provider = Some("local".into());
        assert_eq!(
            subagent_preference_scope(&preferences, &catalog),
            Some(serde_json::to_string(&["local", "shared"]).unwrap())
        );
        preferences.subagent_model = None;
        assert_eq!(subagent_preference_scope(&preferences, &catalog), None);
    }

    #[test]
    fn model_menu_scope_changes_when_only_the_provider_changes() {
        let cloud = json!({"model":"shared-model","modelProvider":"cloud"});
        let local = json!({"model":"shared-model","modelProvider":"local"});
        assert_ne!(thread_model_scope(&cloud), thread_model_scope(&local));
        assert_eq!(
            thread_model_scope(&json!({"model":"legacy"})),
            Some("legacy".into())
        );
        assert_eq!(thread_model_scope(&json!({})), None);
    }

    #[test]
    fn agy_child_selection_retains_executor_provider_and_model_without_affecting_parent() {
        use crate::model_catalog::{ModelExecutor, ModelSelection};
        let mut catalog = json!({"provider":"local","data":[{
            "provider":"local", "model":"parent-example", "selectionValue":"[\"local\",\"parent-example\"]"
        }]});
        crate::model_catalog::add_antigravity_models(&mut catalog, true, &["agy-exact-id".into()]);
        let expected = ModelSelection {
            executor: ModelExecutor::Antigravity,
            provider: "antigravity".into(),
            model: "agy-exact-id".into(),
        };
        let selected =
            subagent_model_selection(&catalog, &expected.value(), crate::i18n::Language::English)
                .unwrap();
        assert_eq!(selected, expected);
        let mut preferences = crate::native::NativeFeaturePreferences::default();
        set_subagent_model_selection(&mut preferences, Some(&selected));
        assert_eq!(
            preferences.subagent_model_executor.as_deref(),
            Some("antigravity")
        );
        assert_eq!(
            preferences.subagent_model_provider.as_deref(),
            Some("antigravity")
        );
        assert_eq!(preferences.subagent_model.as_deref(), Some("agy-exact-id"));
        assert_eq!(
            subagent_preference_scope(&preferences, &catalog),
            Some(expected.value())
        );
        assert_eq!(
            normalized_model_value(&catalog, &expected.value()),
            Some(expected.value())
        );
        assert_eq!(catalog["provider"], "local");
        assert_eq!(
            thread_model_scope(&json!({
                "model":"agy-exact-id", "modelExecutor":"antigravity", "modelProvider":"host-provider"
            })),
            Some(expected.value())
        );
        set_subagent_model_selection(&mut preferences, None);
        assert_eq!(preferences.subagent_model, None);
        assert_eq!(preferences.subagent_model_provider, None);
        assert_eq!(preferences.subagent_model_executor, None);
    }

    #[test]
    fn main_model_changes_resume_only_when_executor_or_provider_changes() {
        use crate::model_catalog::{ModelExecutor, ModelSelection};
        let codex = ModelSelection {
            executor: ModelExecutor::Codex,
            provider: "provider-a".into(),
            model: "next-model".into(),
        };
        let agy = ModelSelection {
            executor: ModelExecutor::Antigravity,
            provider: "antigravity".into(),
            model: "agy-model".into(),
        };
        assert!(!model_change_requires_resume(
            ModelExecutor::Codex,
            "provider-a",
            &codex
        ));
        assert!(model_change_requires_resume(
            ModelExecutor::Codex,
            "provider-b",
            &codex
        ));
        assert!(model_change_requires_resume(
            ModelExecutor::Antigravity,
            "provider-a",
            &codex
        ));
        assert!(model_change_requires_resume(
            ModelExecutor::Codex,
            "provider-a",
            &agy
        ));
        assert!(!model_change_requires_resume(
            ModelExecutor::Antigravity,
            "antigravity",
            &agy
        ));
        assert!(model_change_requires_resume(
            ModelExecutor::Antigravity,
            "other-provider",
            &agy
        ));
    }

    #[test]
    fn legacy_antigravity_variant_resolves_to_family_without_resume_guard() {
        use crate::model_catalog::{ModelExecutor, resolve_selection};

        let catalog = json!({
            "data": [{
                "id": "family-model",
                "model": "family-model",
                "executor": "antigravity",
                "provider": "antigravity",
                "selectionValue": "[\"antigravity\",\"antigravity\",\"family-model\"]",
                "agyVariants": [
                    {"id": "family-model-low", "label": "Family Model Low", "effort": "low"},
                    {"id": "family-model-high", "label": "Family Model High", "effort": "high"}
                ]
            }]
        });
        let selection = resolve_selection(&catalog, "family-model").unwrap();
        let legacy_thread = json!({
            "model": "family-model-low",
            "modelExecutor": "antigravity",
            "modelProvider": "host-native-provider"
        });
        let legacy_scope = thread_model_scope(&legacy_thread);
        let legacy_provider = thread_model_provider(&legacy_thread);

        assert_eq!(selection.model, "family-model");
        assert_eq!(selection.executor, ModelExecutor::Antigravity);
        assert_ne!(legacy_scope, Some(selection.value()));
        assert_eq!(legacy_provider, Some("antigravity"));
        assert!(!model_change_requires_resume(
            ModelExecutor::Antigravity,
            legacy_provider.unwrap(),
            &selection,
        ));
    }

    #[test]
    fn provider_qualified_choices_resolve_current_model_and_subagent_provider() {
        use crate::i18n::Language;
        let catalog = crate::model_catalog::assemble_catalog(
            vec![json!({"model":"shared-model", "supportedReasoningEfforts":["low"]})],
            &json!({"model_provider":"cloud", "profiles":{"local":{"model":"shared-model","model_provider":"local"}}}),
            &json!({"model":"shared-model","modelProvider":"local"}),
            None, None,
        ).unwrap();
        let local = serde_json::to_string(&["local", "shared-model"]).unwrap();
        let cloud = serde_json::to_string(&["cloud", "shared-model"]).unwrap();
        assert_eq!(
            normalized_model_value(&catalog, "shared-model"),
            Some(local.clone())
        );
        assert_eq!(
            selected_model(&catalog, &Some(cloud.clone())).unwrap()["provider"],
            "cloud"
        );
        assert_eq!(
            subagent_model_selection(&catalog, &local, Language::English)
                .unwrap()
                .provider,
            "local"
        );
        for language in [
            Language::English,
            Language::Korean,
            Language::Chinese,
            Language::Japanese,
        ] {
            assert_eq!(
                subagent_model_selection(&catalog, &cloud, language)
                    .unwrap()
                    .provider,
                "cloud"
            );
            assert!(ensure_provider_switch_idle(true, false, language).is_ok());
            assert!(ensure_provider_switch_idle(false, false, language).is_err());
            assert!(ensure_provider_switch_idle(true, true, language).is_err());
        }
        assert_eq!(catalog_model_choices(&catalog).len(), 2);
        assert_eq!(subagent_model_choices(&catalog, Language::English).len(), 3);
    }

    #[test]
    fn model_and_subagent_choices_include_registered_provider_and_local_models() {
        let catalog = json!({
            "provider": "active-provider",
            "data": [
                {"model":"provider/model-a", "id":"legacy-a", "slug":"model-a", "displayName":"Provider model A"},
                {"slug":"local-model-b", "displayName":"Local model B"}
            ]
        });
        let primary = catalog_model_choices(&catalog);
        assert_eq!(
            primary
                .iter()
                .map(|choice| choice.value.as_str())
                .collect::<Vec<_>>(),
            vec!["provider/model-a", "local-model-b"]
        );
        assert!(primary[0].label.contains("Provider model A"));
        assert!(primary[1].label.contains("Local model B"));
        assert!(crate::model_catalog::catalog_has_model(
            &catalog,
            "provider/model-a"
        ));
        assert!(crate::model_catalog::model_matches(
            &catalog["data"][0],
            "legacy-a"
        ));

        let subagent = subagent_model_choices(&catalog, crate::i18n::Language::English);
        assert_eq!(
            subagent.first().map(|choice| choice.value.as_str()),
            Some("native-default")
        );
        assert_eq!(
            subagent
                .iter()
                .skip(1)
                .map(|choice| choice.value.as_str())
                .collect::<Vec<_>>(),
            vec!["provider/model-a", "local-model-b"]
        );
        let mut preferences = crate::native::NativeFeaturePreferences::default();
        set_native_feature_preference(
            &mut preferences,
            NativeFeatureKey::SubagentModel,
            Some(subagent[1].value.as_str()),
        )
        .unwrap();
        assert_eq!(
            preferences.subagent_model.as_deref(),
            Some("provider/model-a")
        );
    }

    #[tokio::test]
    async fn agy_family_menu_preserves_legacy_selection_and_marks_ultra_as_orchestration() {
        let mut catalog = json!({"data": []});
        crate::model_catalog::add_antigravity_catalog(
            &mut catalog,
            &crate::agy::Catalog {
                models: vec![
                    crate::agy::ModelOption {
                        id: "sample-family-high".into(),
                        label: "Sample Family (High)".into(),
                    },
                    crate::agy::ModelOption {
                        id: "sample-family-medium".into(),
                        label: "Sample Family (Medium)".into(),
                    },
                    crate::agy::ModelOption {
                        id: "sample-family-low".into(),
                        label: "Sample Family (Low)".into(),
                    },
                ],
                efforts: vec!["low".into(), "medium".into(), "high".into()],
            },
        );
        let models = catalog_model_choices(&catalog);
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].value,
            "[\"antigravity\",\"antigravity\",\"sample-family\"]"
        );
        assert!(models[0].label.contains("Sample Family"));
        assert_eq!(
            normalized_model_value(&catalog, "sample-family-high"),
            Some(models[0].value.clone())
        );
        let current = Some("sample-family-high".to_owned());
        let selected_model = selected_model(&catalog, &current).unwrap();
        let choices = model_effort_settings_choices(selected_model, crate::i18n::Language::English);
        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.value.as_str())
                .collect::<Vec<_>>(),
            vec!["high", "xhigh", "medium", "low", "ultra"]
        );
        assert_eq!(choices[1].label, "Extra high (AGY uses High)");
        assert_eq!(
            choices[4].label,
            "Ultra (Codex orchestration; AGY uses High)"
        );
        assert!(
            validate_setting_choice("effort", "ultra", &catalog, &current)
                .await
                .is_ok()
        );
        assert!(
            validate_setting_choice("effort", "xhigh", &catalog, &current)
                .await
                .is_ok()
        );
        let current_label = effort_current_label(
            &choices,
            Some(&json!("xhigh")),
            crate::i18n::Language::English,
        );
        assert_eq!(current_label, "Extra high (AGY uses High)");
        assert_eq!(
            agy_variant_effort(selected_model, "sample-family-high").as_deref(),
            Some("high")
        );

        let unsupported = json!({"data":[{
            "id":"sample-low-only",
            "executor":"antigravity",
            "supportedReasoningEfforts":[
                {"reasoningEffort":"low","source":"agy-model-variant"},
                {"reasoningEffort":"ultra","agyOrchestration":true}
            ],
            "reasoningEffortAliases":[]
        }]});
        let unsupported_current = Some("sample-low-only".to_owned());
        assert_eq!(
            model_effort_settings_choices(&unsupported["data"][0], crate::i18n::Language::English)
                .iter()
                .map(|choice| choice.value.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "ultra"]
        );
        assert!(
            validate_setting_choice("effort", "xhigh", &unsupported, &unsupported_current)
                .await
                .is_err()
        );
    }

    #[test]
    fn agy_effort_alias_labels_show_actual_effort_in_all_locales() {
        let ultra = json!({
            "reasoningEffort":"ultra",
            "agyOrchestration":true,
            "agyEffort":"high"
        });
        for (language, extra_high, ultra_label) in [
            (
                crate::i18n::Language::English,
                "Extra high (AGY uses High)",
                "Ultra (Codex orchestration; AGY uses High)",
            ),
            (
                crate::i18n::Language::Korean,
                "매우 높음 (AGY는 높음 사용)",
                "Ultra (Codex 오케스트레이션; AGY는 높음 사용)",
            ),
            (
                crate::i18n::Language::Chinese,
                "很高（AGY 使用高）",
                "Ultra（Codex 编排；AGY 使用高）",
            ),
            (
                crate::i18n::Language::Japanese,
                "非常に高い（AGY は「高」を使用）",
                "Ultra（Codex オーケストレーション、AGY は「高」を使用）",
            ),
        ] {
            let mapped_xhigh = json!({"reasoningEffort":"xhigh","agyEffort":"high"});
            assert_eq!(
                model_effort_settings_choice(&mapped_xhigh, language)
                    .unwrap()
                    .label,
                extra_high
            );
            assert_eq!(
                model_effort_settings_choice(&ultra, language)
                    .unwrap()
                    .label,
                ultra_label
            );
        }
    }

    #[tokio::test]
    async fn manual_agy_refresh_only_returns_success_notice_after_catalog_refresh_succeeds() {
        let (catalog, notice) = refresh_agy_catalog_notice(
            || async {
                Ok(json!({"data":[
                    {"executor":"antigravity","id":"sample-family"},
                    {"executor":"antigravity","id":"sample-other"},
                    {"executor":"codex","id":"native"}
                ]}))
            },
            crate::i18n::Language::English,
        )
        .await
        .unwrap();
        assert_eq!(catalog["data"].as_array().unwrap().len(), 3);
        assert_eq!(notice, "Updated the Antigravity model list (2)");

        let failure = refresh_agy_catalog_notice(
            || async { anyhow::bail!("discovery failed") },
            crate::i18n::Language::English,
        )
        .await;
        assert!(failure.is_err());
    }

    #[test]
    fn effective_config_display_reads_nested_snake_or_camel_case_and_origin() {
        let native_config = json!({
            "config": {"features": {"code_mode": {"enabled": true}}},
            "origins": {"features.code_mode.enabled": {"name": {"type": "user"}, "version": "1"}}
        });
        let text = effective_native_feature_text(
            &native_config,
            NativeFeatureKey::CodeMode,
            crate::i18n::Language::English,
        );
        assert!(text.contains("features.code_mode.enabled = true"));
        assert!(text.contains("\"version\":\"1\""));
        let camel = json!({"config": {"features": {"codeMode": {"enabled": false}}}});
        assert_eq!(
            effective_config_value(&camel, "features.code_mode.enabled"),
            Some(&Value::Bool(false))
        );
    }
}
