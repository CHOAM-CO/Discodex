//! One cancellable, user-driven AGY onboarding flow per worker profile.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use anyhow::{Result, bail};
use serenity::{
    all::{
        ButtonStyle, ComponentInteraction, Context, CreateActionRow, CreateButton,
        CreateInteractionResponse, CreateInteractionResponseMessage, InputTextStyle,
        ModalInteraction,
    },
    builder::{CreateAllowedMentions, CreateInputText, CreateModal, EditInteractionResponse},
};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use uuid::Uuid;

use crate::{
    agy_setup::{InputDisposition, SetupConfig, SetupInput, SetupProgress, SetupReady},
    commands::SettingsMenuState,
    discord::BridgeState,
    i18n::{Language, MessageKey},
};

#[derive(Default)]
pub(crate) struct Controller {
    active: Mutex<Option<Arc<Active>>>,
}

struct Active {
    id: String,
    requester: u64,
    actions: mpsc::UnboundedSender<Action>,
    cancel: watch::Sender<bool>,
    closed: AtomicBool,
    attempt: AtomicU64,
    message_id: AtomicU64,
    page: AtomicUsize,
    stage: std::sync::Mutex<Stage>,
    input: std::sync::Mutex<Option<mpsc::Sender<SetupInput>>>,
}

impl Controller {
    pub async fn cancel(&self) {
        if let Some(active) = self.active.lock().await.as_ref() {
            active.cancel.send_replace(true);
            let _ = active.actions.send(Action::Cancel);
        }
    }
}

enum Action {
    Attach(ComponentInteraction),
    Choose(ComponentInteraction, bool),
    Retry(ComponentInteraction),
    Back(ComponentInteraction),
    Cancel,
    Exit,
    Page(bool),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Stage {
    Sharing,
    Progress(SetupProgress),
    Stopping,
    Finished(String),
}

fn button(id: &str, action: &str, label: &str) -> CreateButton {
    CreateButton::new(format!("discodex:agy-setup:{id}:{action}"))
        .label(label)
        .style(ButtonStyle::Secondary)
}

fn panel(id: &str, stage: &Stage, language: Language) -> (String, Vec<CreateActionRow>) {
    let mut rows = Vec::new();
    let body = match stage {
        Stage::Sharing => {
            rows.push(CreateActionRow::Buttons(vec![
                button(
                    id,
                    "separate",
                    language.message(MessageKey::DiscordAgySetupSeparateSettings),
                ),
                button(
                    id,
                    "shared",
                    language.message(MessageKey::DiscordAgySetupShareExistingSettings),
                ),
            ]));
            language
                .message(MessageKey::DiscordAgySetupInstallDiscodexManagedAntigravityCliChoose)
                .to_owned()
        }
        Stage::Progress(SetupProgress::Authentication {
            output,
            url,
            accepts_code,
            accepts_navigation,
        }) => {
            if *accepts_navigation {
                rows.push(CreateActionRow::Buttons(vec![
                    button(id, "up", "↑"),
                    button(id, "down", "↓"),
                    button(
                        id,
                        "enter",
                        language.message(MessageKey::DiscordAgySetupContinue),
                    ),
                ]));
            }
            if *accepts_code {
                rows.push(CreateActionRow::Buttons(vec![button(
                    id,
                    "code",
                    language.message(MessageKey::DiscordAgySetupEnterSignCode),
                )]));
            }
            let instruction =
                language.message(MessageKey::DiscordAgySetupFollowAntigravitySignPromptBelow);
            // Keep the complete native URL as text: OAuth URLs may exceed a link button's limit.
            format!(
                "{instruction}\n{output}{}",
                url.as_ref()
                    .filter(|url| !output.contains(url.as_str()))
                    .map(|url| format!("\n{url}"))
                    .unwrap_or_default()
            )
        }
        Stage::Progress(progress) => match progress {
            SetupProgress::Checking => {
                language.message(MessageKey::DiscordAgySetupCheckingManagedCli)
            }
            SetupProgress::Installing => {
                language.message(MessageKey::DiscordAgySetupInstallingOfficialPrebuiltCli)
            }
            SetupProgress::CheckingAccount => {
                language.message(MessageKey::DiscordAgySetupCheckingSign)
            }
            SetupProgress::Verifying => {
                language.message(MessageKey::DiscordAgySetupVerifyingAvailableModels)
            }
            SetupProgress::Authentication { .. } => unreachable!(),
        }
        .to_owned(),
        Stage::Stopping => language
            .message(MessageKey::DiscordAgySetupCancellingSetup)
            .to_owned(),
        Stage::Finished(notice) => {
            rows.push(CreateActionRow::Buttons(vec![
                button(
                    id,
                    "retry",
                    language.message(MessageKey::DiscordAgySetupSetUpAgain),
                ),
                button(
                    id,
                    "back",
                    language.message(MessageKey::DiscordAgySetupBack),
                ),
            ]));
            notice.clone()
        }
    };
    let mut controls = Vec::new();
    if !matches!(stage, Stage::Finished(_)) {
        controls.push(
            button(
                id,
                "cancel",
                language.message(MessageKey::DiscordAgySetupCancel),
            )
            .disabled(matches!(stage, Stage::Stopping)),
        );
    }
    controls.push(button(
        id,
        "exit",
        language.message(MessageKey::DiscordAgySetupExit),
    ));
    rows.push(CreateActionRow::Buttons(controls));
    (
        format!(
            "**{}**\n{body}",
            language.message(MessageKey::CommandsAgyTitle)
        ),
        rows,
    )
}

async fn render(
    ctx: &Context,
    target: &ComponentInteraction,
    active: &Active,
    stage: &Stage,
    language: Language,
) -> Result<()> {
    if active.closed.load(Ordering::Acquire) {
        return Ok(());
    }
    *active.stage.lock().unwrap() = stage.clone();
    let (content, mut rows) = panel(&active.id, stage, language);
    let chunks = crate::commands::split_discord_content(&content);
    let page = active
        .page
        .load(Ordering::Acquire)
        .min(chunks.len().saturating_sub(1));
    active.page.store(page, Ordering::Release);
    if chunks.len() > 1 {
        rows.push(CreateActionRow::Buttons(vec![
            button(
                &active.id,
                "previous",
                language.message(MessageKey::DiscordAgySetupPrevious),
            )
            .disabled(page == 0),
            button(
                &active.id,
                "page",
                &format!("{}/{}", page + 1, chunks.len()),
            )
            .disabled(true),
            button(
                &active.id,
                "next",
                language.message(MessageKey::DiscordAgySetupNext),
            )
            .disabled(page + 1 == chunks.len()),
        ]));
    }
    target
        .edit_response(
            ctx,
            EditInteractionResponse::new()
                .content(chunks.get(page).cloned().unwrap_or_default())
                .components(rows)
                .allowed_mentions(CreateAllowedMentions::new()),
        )
        .await?;
    Ok(())
}

pub(crate) async fn begin(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
    previous_id: &str,
    menu: SettingsMenuState,
    choose_sharing: bool,
) -> Result<()> {
    let mut current = state.agy_setup.active.lock().await;
    if let Some(active) = current.as_ref() {
        if !active.actions.is_closed() {
            if active.requester != interaction.user.id.get() {
                bail!(
                    state
                        .config
                        .language
                        .message(MessageKey::DiscordAgySetupAnotherOperatorSettingUpAntigravity)
                );
            }
            if active.closed.load(Ordering::Acquire) {
                bail!(
                    state
                        .config
                        .language
                        .message(MessageKey::DiscordAgySetupSetupStoppingOpenSettingsAgainWhen)
                );
            }
            active.actions.send(Action::Attach(interaction.clone()))?;
            state.settings_menus.lock().await.remove(previous_id);
            return Ok(());
        }
    }
    let preferences = state.native.read_native_feature_preferences()?;
    let share = if choose_sharing {
        None
    } else {
        preferences.antigravity_share_settings
    };
    let (actions, rx) = mpsc::unbounded_channel();
    let (cancel, cancellation) = watch::channel(false);
    let active = Arc::new(Active {
        id: Uuid::new_v4().simple().to_string(),
        requester: interaction.user.id.get(),
        actions,
        cancel,
        closed: AtomicBool::new(false),
        attempt: AtomicU64::new(0),
        message_id: AtomicU64::new(interaction.message.id.get()),
        page: AtomicUsize::new(0),
        stage: std::sync::Mutex::new(Stage::Sharing),
        input: std::sync::Mutex::new(None),
    });
    let stage = if share.is_some() {
        Stage::Progress(SetupProgress::Checking)
    } else {
        Stage::Sharing
    };
    render(ctx, interaction, &active, &stage, state.config.language).await?;
    state.settings_menus.lock().await.remove(previous_id);
    *current = Some(active.clone());
    let ctx = ctx.clone();
    let state = state.clone();
    let target = interaction.clone();
    tokio::spawn(async move {
        run(&ctx, target, &state, &active, menu, share, rx, cancellation).await;
        let mut current = state.agy_setup.active.lock().await;
        if current
            .as_ref()
            .is_some_and(|current| current.id == active.id)
        {
            *current = None;
        }
    });
    Ok(())
}

fn setup_config(state: &BridgeState, share: bool) -> Result<SetupConfig> {
    crate::agy_settings::prepare(&state.config, share)?;
    let runtime = crate::agy::Runtime::for_config(&state.config)?;
    Ok(SetupConfig {
        executable: runtime.executable,
        arguments: runtime.arguments,
        environment: runtime.environment,
        cwd: state.config.project_dir.clone(),
    })
}

async fn run(
    ctx: &Context,
    mut target: ComponentInteraction,
    state: &Arc<BridgeState>,
    active: &Arc<Active>,
    menu: SettingsMenuState,
    mut share: Option<bool>,
    mut actions: mpsc::UnboundedReceiver<Action>,
    cancellation: watch::Receiver<bool>,
) {
    let language = state.config.language;
    let (events, mut progress) = mpsc::unbounded_channel();
    let mut running: Option<tokio::task::JoinHandle<Result<SetupReady>>> = None;
    let mut stage = Stage::Sharing;
    let mut start = share.is_some();
    loop {
        if start && *cancellation.borrow() {
            start = false;
            stage = Stage::Finished(cancelled_notice(language).to_owned());
        }
        if start {
            start = false;
            while progress.try_recv().is_ok() {}
            match setup_config(state, share.expect("selected settings mode")) {
                Ok(config) => {
                    let (tx, rx) = mpsc::channel(8);
                    *active.input.lock().unwrap() = Some(tx);
                    active.attempt.fetch_add(1, Ordering::AcqRel);
                    let events = events.clone();
                    let cancel = cancellation.clone();
                    running = Some(tokio::spawn(crate::agy_setup::prepare(
                        config, events, rx, cancel,
                    )));
                    stage = Stage::Progress(SetupProgress::Checking);
                }
                Err(error) => {
                    stage = Stage::Finished(crate::discord::sanitize_discord_error(
                        &state.config,
                        &format!("{error:#}"),
                    ));
                    if let Err(render_error) = render(ctx, &target, active, &stage, language).await
                    {
                        let detail = crate::discord::sanitize_discord_error(
                            &state.config,
                            &format!("{render_error:#}"),
                        );
                        tracing::error!(%detail, "could not show Antigravity setup error");
                        crate::discord::report_discord_failure(
                            &ctx.http,
                            &state.config,
                            serenity::all::ChannelId::new(state.config.home_channel_id),
                            &format!("Antigravity setup failed: {error:#}; Discord delivery failed: {detail}"),
                        ).await;
                    }
                }
            }
        }
        tokio::select! {
            biased;
            action = actions.recv() => {
                let Some(action) = action else { active.cancel.send_replace(true); break; };
                match action {
                    Action::Attach(new_target) => {
                        if target.message.id != new_target.message.id {
                            if let Err(error) = target.edit_response(ctx, EditInteractionResponse::new().components(Vec::new())).await {
                                let detail = crate::discord::sanitize_discord_error(&state.config, &format!("{error:#}"));
                                tracing::warn!(%detail, "could not remove controls from previous Antigravity setup panel");
                                crate::discord::report_discord_failure(
                                    &ctx.http,
                                    &state.config,
                                    serenity::all::ChannelId::new(state.config.home_channel_id),
                                    &detail,
                                ).await;
                            }
                            active.message_id.store(new_target.message.id.get(), Ordering::Release);
                        }
                        target = new_target;
                    }
                    Action::Page(next) => {
                        if next { active.page.fetch_add(1, Ordering::AcqRel); }
                        else { let _ = active.page.fetch_update(Ordering::AcqRel, Ordering::Acquire, |page| Some(page.saturating_sub(1))); }
                    }
                    Action::Choose(new_target, selected) if running.is_none() && stage == Stage::Sharing => {
                        target = new_target; share = Some(selected); start = true; active.page.store(0, Ordering::Release);
                    }
                    Action::Retry(new_target) if running.is_none() => { target = new_target; stage = Stage::Sharing; }
                    Action::Back(new_target) if running.is_none() => {
                        if let Err(error) = crate::commands::return_from_agy_setup(ctx, &new_target, state, &menu).await {
                            stage = Stage::Finished(crate::discord::sanitize_discord_error(
                                &state.config,
                                &format!("{error:#}"),
                            ));
                            target = new_target;
                        } else { break; }
                    }
                    Action::Cancel => {
                        active.cancel.send_replace(true);
                        stage = if running.is_some() { Stage::Stopping } else { Stage::Finished(cancelled_notice(language).to_owned()) };
                    }
                    Action::Exit => {
                        active.cancel.send_replace(true);
                        active.closed.store(true, Ordering::Release);
                        if running.is_none() { break; }
                        stage = Stage::Stopping;
                    }
                    _ => {}
                }
            }
            result = async { running.as_mut().expect("running setup").await }, if running.is_some() => {
                running = None;
                *active.input.lock().unwrap() = None;
                if active.closed.load(Ordering::Acquire) { break; }
                let result = result.map_err(anyhow::Error::from).and_then(|result| result);
                stage = match result {
                    Ok(ready) => {
                        // Serialize the final preference write with other settings; cancellation
                        // checked after acquiring the lock wins over a late verification result.
                        let _settings = state.settings_change_lock.lock().await;
                        if *cancellation.borrow() {
                            Stage::Finished(cancelled_notice(language).to_owned())
                        } else {
                            let result = async {
                                let mut preferences = state.native.read_native_feature_preferences()?;
                                preferences.antigravity_enabled = true;
                                preferences.antigravity_share_settings = share;
                                preferences.antigravity_path = None;
                                state.native.apply_native_feature_preferences(&preferences).await
                            }.await;
                            match result {
                                Ok(_) => Stage::Finished(format!("{} ({})", language.message(MessageKey::DiscordAgySetupAntigravityReadyAvailableModels), ready.catalog.models.len())),
                                Err(error) => Stage::Finished(crate::discord::sanitize_discord_error(
                                    &state.config,
                                    &format!("{error:#}"),
                                )),
                            }
                        }
                    }
                    Err(error) if error.downcast_ref::<crate::agy_setup::SetupCancelled>().is_some() || *cancellation.borrow() => Stage::Finished(cancelled_notice(language).to_owned()),
                    Err(error) => Stage::Finished(format!(
                        "{}\n{}",
                        language.message(MessageKey::DiscordAgySetupCouldNotFinishAntigravitySetup),
                        crate::discord::sanitize_discord_error(&state.config, &format!("{error:#}")),
                    )),
                };
            }
            event = progress.recv(), if running.is_some() => {
                if let Some(mut event) = event {
                    while let Ok(newer) = progress.try_recv() { event = newer; }
                    if !*cancellation.borrow() { stage = Stage::Progress(event); }
                }
            }
        }
        if let Err(error) = render(ctx, &target, active, &stage, language).await {
            // A new settings panel can attach to this flow and refresh its interaction token.
            let detail =
                crate::discord::sanitize_discord_error(&state.config, &format!("{error:#}"));
            tracing::error!(%detail, "could not refresh Antigravity setup panel");
            crate::discord::report_discord_failure(
                &ctx.http,
                &state.config,
                serenity::all::ChannelId::new(state.config.home_channel_id),
                &detail,
            )
            .await;
        }
    }
    active.cancel.send_replace(true);
    if let Some(running) = running {
        if let Err(error) = running.await {
            let detail =
                crate::discord::sanitize_discord_error(&state.config, &format!("{error:#}"));
            tracing::error!(%detail, "Antigravity setup worker terminated unexpectedly");
            crate::discord::report_discord_failure(
                &ctx.http,
                &state.config,
                serenity::all::ChannelId::new(state.config.home_channel_id),
                &detail,
            )
            .await;
        }
    }
}

fn cancelled_notice(language: Language) -> &'static str {
    language.message(MessageKey::DiscordAgySetupSetupCancelledAntigravityNotEnabledAttempt)
}

fn code_receipt_notice(disposition: InputDisposition, language: Language) -> &'static str {
    match disposition {
        InputDisposition::Delivered => {
            language.message(MessageKey::DiscordAgySetupSignCodeSubmitted)
        }
        InputDisposition::NoLongerNeeded => {
            language.message(MessageKey::DiscordAgySetupAntigravityAlreadySignedNoCodeNeeded)
        }
        InputDisposition::NotAccepting => unreachable!("not accepting is returned as an error"),
    }
}

async fn active_for(state: &BridgeState, id: &str, requester: u64) -> Result<Arc<Active>> {
    let active = state.agy_setup.active.lock().await;
    match active.as_ref() {
        Some(active) if active.id == id && active.requester == requester => Ok(active.clone()),
        _ => bail!(
            state
                .config
                .language
                .message(MessageKey::DiscordAgySetupSetupPanelNoLongerActiveOpen)
        ),
    }
}

pub(crate) async fn handle_component(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
) -> Result<()> {
    let parts = interaction.data.custom_id.split(':').collect::<Vec<_>>();
    if parts.len() != 4 {
        bail!(
            state
                .config
                .language
                .message(MessageKey::DiscordAgySetupInvalidAction)
        );
    }
    let active = active_for(state, parts[2], interaction.user.id.get()).await?;
    if active.message_id.load(Ordering::Acquire) != interaction.message.id.get() {
        bail!(
            state
                .config
                .language
                .message(MessageKey::DiscordAgySetupUseLatestSetupPanel)
        );
    }
    let action = parts[3];
    if action == "code" {
        ensure_input_stage(&active, true, state.config.language)?;
        let language = state.config.language;
        interaction
            .create_response(
                ctx,
                CreateInteractionResponse::Modal(
                    CreateModal::new(
                        format!(
                            "discodex:agy-code:{}:{}",
                            active.id,
                            active.attempt.load(Ordering::Acquire)
                        ),
                        language.message(MessageKey::DiscordAgySetupAntigravitySign),
                    )
                    .components(vec![CreateActionRow::InputText(
                        CreateInputText::new(
                            InputTextStyle::Short,
                            language.message(MessageKey::DiscordAgySetupCodeSignPage),
                            "code",
                        )
                        .required(true),
                    )]),
                ),
            )
            .await?;
        return Ok(());
    }
    interaction
        .create_response(ctx, CreateInteractionResponse::Acknowledge)
        .await?;
    active.actions.send(Action::Attach(interaction.clone()))?;
    let action = match action {
        "shared" | "separate" => {
            if *active.stage.lock().unwrap() != Stage::Sharing {
                bail!(
                    state
                        .config
                        .language
                        .message(MessageKey::DiscordAgySetupModeAlreadySelected)
                );
            }
            active.cancel.send_replace(false);
            Action::Choose(interaction.clone(), action == "shared")
        }
        "previous" => Action::Page(false),
        "next" => Action::Page(true),
        "retry" => Action::Retry(interaction.clone()),
        "back" => Action::Back(interaction.clone()),
        "cancel" => {
            active.cancel.send_replace(true);
            Action::Cancel
        }
        "exit" => {
            active.cancel.send_replace(true);
            active.closed.store(true, Ordering::Release);
            // Signal first so HTTP failure cannot leave a login/installer running.
            active.actions.send(Action::Exit)?;
            interaction.delete_response(ctx).await?;
            return Ok(());
        }
        "up" | "down" | "enter" => {
            let key = match action {
                "up" => "\x1b[A",
                "down" => "\x1b[B",
                _ => "\r",
            };
            submit_input(&active, key.to_owned(), false, state.config.language).await?;
            return Ok(());
        }
        _ => bail!(
            state
                .config
                .language
                .message(MessageKey::DiscordAgySetupInvalidAction)
        ),
    };
    active.actions.send(action)?;
    Ok(())
}

fn input_available(stage: &Stage, code: bool) -> bool {
    match stage {
        Stage::Progress(SetupProgress::Authentication {
            accepts_code,
            accepts_navigation,
            ..
        }) => {
            if code {
                *accepts_code
            } else {
                *accepts_navigation
            }
        }
        _ => false,
    }
}

fn ensure_input_stage(active: &Active, code: bool, language: Language) -> Result<()> {
    if active.closed.load(Ordering::Acquire)
        || *active.cancel.borrow()
        || !input_available(&active.stage.lock().unwrap(), code)
    {
        bail!(language.message(MessageKey::DiscordAgySetupSignPromptChangedUseCurrentSetup));
    }
    Ok(())
}

async fn submit_input(
    active: &Active,
    text: String,
    code: bool,
    language: Language,
) -> Result<InputDisposition> {
    ensure_input_stage(active, code, language)?;
    let input = active.input.lock().unwrap().clone().ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            language.message(MessageKey::DiscordAgySetupSigninEnded)
        )
    })?;
    let mut cancel = active.cancel.subscribe();
    let (acknowledgement, received) = oneshot::channel();
    let response = async {
        input
            .send(SetupInput {
                data: text,
                acknowledgement,
            })
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "{}",
                    language.message(MessageKey::DiscordAgySetupSigninEnded)
                )
            })?;
        match received.await? {
            disposition @ (InputDisposition::Delivered | InputDisposition::NoLongerNeeded) => {
                Ok(disposition)
            }
            InputDisposition::NotAccepting => {
                bail!(language.message(MessageKey::DiscordAgySetupSignPromptChangedUseCurrentPanel))
            }
        }
    };
    tokio::select! {
        biased;
        _ = cancel.changed() => bail!(cancelled_notice(language)),
        result = response => result,
    }
}

pub(crate) async fn handle_modal(
    ctx: &Context,
    interaction: &ModalInteraction,
    state: &Arc<BridgeState>,
) -> Result<()> {
    let identity = interaction
        .data
        .custom_id
        .strip_prefix("discodex:agy-code:")
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                state
                    .config
                    .language
                    .message(MessageKey::DiscordAgySetupInvalidModal)
            )
        })?;
    let (id, attempt) = identity.split_once(':').ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            state
                .config
                .language
                .message(MessageKey::DiscordAgySetupInvalidAttempt)
        )
    })?;
    let active = active_for(state, id, interaction.user.id.get()).await?;
    if attempt.parse::<u64>()? != active.attempt.load(Ordering::Acquire) {
        bail!(
            state
                .config
                .language
                .message(MessageKey::DiscordAgySetupSignAttemptEndedUseCurrentSetup)
        );
    }
    let code = interaction
        .data
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|component| match component {
            serenity::all::ActionRowComponent::InputText(input) if input.custom_id == "code" => {
                input.value.as_deref()
            }
            _ => None,
        })
        .unwrap_or_default();
    ensure_input_stage(&active, true, state.config.language)?;
    interaction
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    let disposition = submit_input(
        &active,
        format!("{}\r", code.trim()),
        true,
        state.config.language,
    )
    .await?;
    let notice = code_receipt_notice(disposition, state.config.language);
    interaction
        .edit_response(ctx, EditInteractionResponse::new().content(notice))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controls(stage: Stage, language: Language) -> serde_json::Value {
        serde_json::to_value(panel("setup-test", &stage, language).1).unwrap()
    }

    #[test]
    fn all_setup_stages_offer_exit_and_running_stages_offer_cancel() {
        for language in [
            Language::English,
            Language::Korean,
            Language::Chinese,
            Language::Japanese,
        ] {
            for stage in [
                Stage::Sharing,
                Stage::Progress(SetupProgress::Installing),
                Stage::Progress(SetupProgress::CheckingAccount),
                Stage::Progress(SetupProgress::Verifying),
                Stage::Stopping,
                Stage::Finished("Complete".into()),
            ] {
                let rows = controls(stage.clone(), language);
                let buttons = rows
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|row| row["components"].as_array().unwrap())
                    .collect::<Vec<_>>();
                let exit = buttons
                    .iter()
                    .find(|button| button["custom_id"] == "discodex:agy-setup:setup-test:exit")
                    .unwrap();
                assert_eq!(
                    exit["label"],
                    language.message(MessageKey::DiscordAgySetupExit)
                );
                assert_eq!(buttons.iter().any(|button| button["custom_id"] == "discodex:agy-setup:setup-test:cancel"), !matches!(stage, Stage::Finished(_)));
            }
        }
    }

    #[test]
    fn sharing_is_explicit_and_auth_controls_follow_native_prompt() {
        let rows = controls(Stage::Sharing, Language::English);
        assert_eq!(
            rows[0]["components"][0]["custom_id"],
            "discodex:agy-setup:setup-test:separate"
        );
        assert_eq!(
            rows[0]["components"][1]["custom_id"],
            "discodex:agy-setup:setup-test:shared"
        );
        let stage = Stage::Progress(SetupProgress::Authentication {
            output: "Paste code".into(),
            url: Some("https://example.test/login".into()),
            accepts_code: true,
            accepts_navigation: false,
        });
        let (text, rows) = panel("setup-test", &stage, Language::English);
        assert!(text.contains("https://example.test/login"));
        assert_eq!(
            serde_json::to_value(rows).unwrap()[0]["components"][0]["custom_id"],
            "discodex:agy-setup:setup-test:code"
        );
    }

    #[test]
    fn code_receipt_notice_distinguishes_delivery_from_already_signed_in() {
        for (language, delivered, unnecessary) in [
            (
                Language::English,
                "Sign-in code submitted.",
                "Antigravity is already signed in; no code was needed.",
            ),
            (
                Language::Korean,
                "로그인 코드를 전달했습니다.",
                "Antigravity가 이미 로그인되어 있어 코드를 입력하지 않았습니다.",
            ),
            (
                Language::Chinese,
                "已提交登录代码。",
                "Antigravity 已登录，无需输入代码。",
            ),
            (
                Language::Japanese,
                "ログインコードを送信しました。",
                "Antigravity はログイン済みのため、コードは不要でした。",
            ),
        ] {
            assert_eq!(
                code_receipt_notice(InputDisposition::Delivered, language),
                delivered
            );
            assert_eq!(
                code_receipt_notice(InputDisposition::NoLongerNeeded, language),
                unnecessary
            );
        }
    }

    fn active_input_fixture() -> (
        Arc<Active>,
        mpsc::Receiver<SetupInput>,
        watch::Receiver<bool>,
    ) {
        let (actions, _) = mpsc::unbounded_channel();
        let (cancel, cancellation) = watch::channel(false);
        let (input, rx) = mpsc::channel(1);
        (
            Arc::new(Active {
                id: "setup-test".into(),
                requester: 7,
                actions,
                cancel,
                closed: AtomicBool::new(false),
                attempt: AtomicU64::new(1),
                message_id: AtomicU64::new(42),
                page: AtomicUsize::new(0),
                stage: std::sync::Mutex::new(Stage::Progress(SetupProgress::Authentication {
                    output: "Paste code".into(),
                    url: None,
                    accepts_code: true,
                    accepts_navigation: true,
                })),
                input: std::sync::Mutex::new(Some(input)),
            }),
            rx,
            cancellation,
        )
    }

    #[tokio::test]
    async fn login_receipt_waits_for_actual_backend_delivery() {
        let (active, mut inputs, _cancellation) = active_input_fixture();
        let task = tokio::spawn({
            let active = active.clone();
            async move { submit_input(&active, "code\r".into(), true, Language::English).await }
        });
        let received = inputs.recv().await.unwrap();
        assert_eq!(received.data, "code\r");
        assert!(!task.is_finished());
        received
            .acknowledgement
            .send(InputDisposition::Delivered)
            .unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn closed_or_stale_login_input_is_not_reported_as_submitted() {
        let (active, mut inputs, _cancellation) = active_input_fixture();
        let task = tokio::spawn({
            let active = active.clone();
            async move { submit_input(&active, "code\r".into(), true, Language::English).await }
        });
        inputs
            .recv()
            .await
            .unwrap()
            .acknowledgement
            .send(InputDisposition::NotAccepting)
            .unwrap();
        assert!(task.await.unwrap().is_err());
        *active.stage.lock().unwrap() = Stage::Finished("Cancelled".into());
        assert!(
            submit_input(&active, "old code\r".into(), true, Language::English)
                .await
                .is_err()
        );
        assert!(inputs.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancel_releases_pending_code_submission() {
        let (active, mut inputs, _cancellation) = active_input_fixture();
        let task = tokio::spawn({
            let active = active.clone();
            async move { submit_input(&active, "code\r".into(), true, Language::English).await }
        });
        let _pending = inputs.recv().await.unwrap();
        active.cancel.send_replace(true);
        assert!(task.await.unwrap().is_err());
        assert!(
            submit_input(&active, "late code\r".into(), true, Language::English)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn external_disable_cancels_pending_setup_and_notifies_panel() {
        let (active, _inputs, cancellation) = active_input_fixture();
        let (actions, mut rx) = mpsc::unbounded_channel();
        let mut active = Arc::try_unwrap(active).ok().unwrap();
        active.actions = actions;
        let controller = Controller {
            active: Mutex::new(Some(Arc::new(active))),
        };
        controller.cancel().await;
        assert!(*cancellation.borrow());
        assert!(matches!(rx.recv().await, Some(Action::Cancel)));
    }
}
