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
    i18n::Language,
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
                    language.text("Separate settings", "별도 설정", "独立设置", "個別の設定"),
                ),
                button(
                    id,
                    "shared",
                    language.text(
                        "Share existing settings",
                        "기존 설정 공유",
                        "共享现有设置",
                        "既存の設定を共有",
                    ),
                ),
            ]));
            language.text(
                "Install a Discodex-managed Antigravity CLI. Choose separate settings or share your existing AGY settings. AGY's OS credential store is used in either case. Browser sign-in may require your confirmation.",
                "Discodex 전용 Antigravity CLI를 설치합니다. 별도 설정 또는 기존 AGY 설정 공유를 선택하세요. 로그인은 두 경우 모두 AGY의 OS 자격증명 저장소를 사용합니다. 브라우저 로그인 시 직접 확인이 필요할 수 있습니다.",
                "安装由 Discodex 管理的 Antigravity CLI。请选择独立设置或共享现有 AGY 设置。两种方式均使用 AGY 的操作系统凭据存储。浏览器登录可能需要您确认。",
                "Discodex 管理の Antigravity CLI をインストールします。個別の設定か既存の AGY 設定の共有を選んでください。どちらも AGY の OS 資格情報ストアを使います。ブラウザーでのログインには確認が必要な場合があります。",
            ).to_owned()
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
                        language.text("Continue", "계속", "继续", "続ける"),
                    ),
                ]));
            }
            if *accepts_code {
                rows.push(CreateActionRow::Buttons(vec![button(
                    id,
                    "code",
                    language.text(
                        "Enter sign-in code",
                        "로그인 코드 입력",
                        "输入登录代码",
                        "ログインコードを入力",
                    ),
                )]));
            }
            let instruction = language.text(
                "Follow the Antigravity sign-in prompt below.",
                "아래 Antigravity 로그인 안내에 따라 진행하세요.",
                "请按照下方 Antigravity 登录提示操作。",
                "以下の Antigravity のログイン案内に従ってください。",
            );
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
            SetupProgress::Checking => language.text(
                "Checking the managed CLI…",
                "전용 CLI를 확인하고 있어요…",
                "正在检查托管 CLI…",
                "管理対象 CLI を確認中…",
            ),
            SetupProgress::Installing => language.text(
                "Installing the official prebuilt CLI…",
                "공식 빌드된 CLI를 설치하고 있어요…",
                "正在安装官方预编译 CLI…",
                "公式ビルド済み CLI をインストール中…",
            ),
            SetupProgress::CheckingAccount => language.text(
                "Checking sign-in…",
                "로그인 상태를 확인하고 있어요…",
                "正在检查登录状态…",
                "ログイン状態を確認中…",
            ),
            SetupProgress::Verifying => language.text(
                "Verifying available models…",
                "사용 가능한 모델을 확인하고 있어요…",
                "正在验证可用模型…",
                "利用可能なモデルを確認中…",
            ),
            SetupProgress::Authentication { .. } => unreachable!(),
        }
        .to_owned(),
        Stage::Stopping => language
            .text(
                "Cancelling setup…",
                "설치를 취소하고 있어요…",
                "正在取消设置…",
                "セットアップをキャンセル中…",
            )
            .to_owned(),
        Stage::Finished(notice) => {
            rows.push(CreateActionRow::Buttons(vec![
                button(
                    id,
                    "retry",
                    language.text("Set up again", "설정 다시 진행", "重新设置", "再設定"),
                ),
                button(
                    id,
                    "back",
                    language.text("Back", "뒤로가기", "返回", "戻る"),
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
                language.text("Cancel", "취소", "取消", "キャンセル"),
            )
            .disabled(matches!(stage, Stage::Stopping)),
        );
    }
    controls.push(button(
        id,
        "exit",
        language.text("Exit", "나가기", "退出", "終了"),
    ));
    rows.push(CreateActionRow::Buttons(controls));
    (format!("**Antigravity**\n{body}"), rows)
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
                language.text("Previous", "이전", "上一页", "前へ"),
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
                language.text("Next", "다음", "下一页", "次へ"),
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
                bail!(state.config.language.text(
                    "Another operator is setting up Antigravity.",
                    "다른 관리자가 Antigravity를 설정 중입니다.",
                    "另一位管理员正在设置 Antigravity。",
                    "別の管理者が Antigravity を設定中です。"
                ));
            }
            if active.closed.load(Ordering::Acquire) {
                bail!(state.config.language.text(
                    "Setup is stopping. Open settings again when it has stopped.",
                    "설정을 종료하고 있습니다. 종료 후 설정을 다시 여세요.",
                    "正在停止设置，请停止后重新打开。",
                    "セットアップを終了中です。終了後に設定を開き直してください。"
                ));
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
    let runtime = crate::agy::Runtime::for_config(&state.config, share)?;
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
                    stage = Stage::Finished(format!("{error:#}"));
                    if let Err(render_error) = render(ctx, &target, active, &stage, language).await
                    {
                        tracing::warn!(%render_error, "could not show Antigravity setup error");
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
                            let _ = target.edit_response(ctx, EditInteractionResponse::new().components(Vec::new())).await;
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
                            stage = Stage::Finished(format!("{error:#}"));
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
                                Ok(_) => Stage::Finished(format!("{} ({})", language.text("Antigravity is ready. Available models", "Antigravity를 사용할 수 있어요. 사용 가능한 모델", "Antigravity 已就绪。可用模型", "Antigravity の準備ができました。利用可能なモデル"), ready.catalog.models.len())),
                                Err(error) => Stage::Finished(format!("{error:#}")),
                            }
                        }
                    }
                    Err(error) if error.downcast_ref::<crate::agy_setup::SetupCancelled>().is_some() || *cancellation.borrow() => Stage::Finished(cancelled_notice(language).to_owned()),
                    Err(error) => Stage::Finished(format!("{}\n{error:#}", language.text("Could not finish Antigravity setup.", "Antigravity 설정을 완료하지 못했습니다.", "无法完成 Antigravity 设置。", "Antigravity のセットアップを完了できませんでした。"))),
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
            tracing::warn!(%error, "could not refresh Antigravity setup panel");
        }
    }
    active.cancel.send_replace(true);
    if let Some(running) = running {
        let _ = running.await;
    }
}

fn cancelled_notice(language: Language) -> &'static str {
    language.text(
        "Setup cancelled. Antigravity was not enabled by this attempt.",
        "설정을 취소했습니다. 이번 시도로 Antigravity를 활성화하지 않았습니다.",
        "设置已取消。本次操作未启用 Antigravity。",
        "セットアップをキャンセルしました。今回の操作では Antigravity を有効にしていません。",
    )
}

fn code_receipt_notice(disposition: InputDisposition, language: Language) -> &'static str {
    match disposition {
        InputDisposition::Delivered => language.text(
            "Sign-in code submitted.",
            "로그인 코드를 전달했습니다.",
            "已提交登录代码。",
            "ログインコードを送信しました。",
        ),
        InputDisposition::NoLongerNeeded => language.text(
            "Antigravity is already signed in; no code was needed.",
            "Antigravity가 이미 로그인되어 있어 코드를 입력하지 않았습니다.",
            "Antigravity 已登录，无需输入代码。",
            "Antigravity はログイン済みのため、コードは不要でした。",
        ),
        InputDisposition::NotAccepting => unreachable!("not accepting is returned as an error"),
    }
}

async fn active_for(state: &BridgeState, id: &str, requester: u64) -> Result<Arc<Active>> {
    let active = state.agy_setup.active.lock().await;
    match active.as_ref() {
        Some(active) if active.id == id && active.requester == requester => Ok(active.clone()),
        _ => bail!(state.config.language.text(
            "This setup panel is no longer active. Open settings again.",
            "설정 화면이 만료되었습니다. 설정을 다시 여세요.",
            "此设置面板已失效，请重新打开设置。",
            "この設定画面は無効です。設定を開き直してください。"
        )),
    }
}

pub(crate) async fn handle_component(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
) -> Result<()> {
    let parts = interaction.data.custom_id.split(':').collect::<Vec<_>>();
    if parts.len() != 4 {
        bail!("Invalid AGY setup action");
    }
    let active = active_for(state, parts[2], interaction.user.id.get()).await?;
    if active.message_id.load(Ordering::Acquire) != interaction.message.id.get() {
        bail!(state.config.language.text(
            "Use the latest setup panel.",
            "최신 설정 화면을 사용하세요.",
            "请使用最新设置面板。",
            "最新の設定画面を使ってください。"
        ));
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
                        language.text(
                            "Antigravity sign-in",
                            "Antigravity 로그인",
                            "Antigravity 登录",
                            "Antigravity ログイン",
                        ),
                    )
                    .components(vec![CreateActionRow::InputText(
                        CreateInputText::new(
                            InputTextStyle::Short,
                            language.text(
                                "Code from the sign-in page",
                                "로그인 페이지의 코드",
                                "登录页面中的代码",
                                "ログインページのコード",
                            ),
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
                bail!("Settings mode has already been selected");
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
        _ => bail!("Invalid AGY setup action"),
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
        bail!(language.text(
            "This sign-in prompt has changed. Use the current setup panel.",
            "로그인 단계가 변경되었습니다. 현재 설정 화면을 사용하세요.",
            "登录提示已更改，请使用当前设置面板。",
            "ログイン手順が変わりました。現在の設定画面を使ってください。"
        ));
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
    let input = active
        .input
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Antigravity sign-in has ended"))?;
    let mut cancel = active.cancel.subscribe();
    let (acknowledgement, received) = oneshot::channel();
    let response = async {
        input
            .send(SetupInput {
                data: text,
                acknowledgement,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Antigravity sign-in has ended"))?;
        match received.await? {
            disposition @ (InputDisposition::Delivered | InputDisposition::NoLongerNeeded) => {
                Ok(disposition)
            }
            InputDisposition::NotAccepting => bail!(language.text(
                "The sign-in prompt has changed. Use the current panel.",
                "로그인 단계가 변경되었습니다. 현재 화면을 사용하세요.",
                "登录提示已更改，请使用当前面板。",
                "ログイン手順が変わりました。現在の画面を使ってください。"
            )),
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
        .ok_or_else(|| anyhow::anyhow!("Invalid AGY setup modal"))?;
    let (id, attempt) = identity
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("Invalid AGY setup attempt"))?;
    let active = active_for(state, id, interaction.user.id.get()).await?;
    if attempt.parse::<u64>()? != active.attempt.load(Ordering::Acquire) {
        bail!(state.config.language.text(
            "This sign-in attempt has ended. Use the current setup panel.",
            "로그인 시도가 종료되었습니다. 현재 설정 화면을 사용하세요.",
            "此登录尝试已结束，请使用当前设置面板。",
            "このログイン試行は終了しました。現在の設定画面を使ってください。"
        ));
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
                    language.text("Exit", "나가기", "退出", "終了")
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
