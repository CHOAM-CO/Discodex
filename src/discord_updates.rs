//! Discord-owned update notifications. No notification becomes a Codex turn.
use crate::{
    commands::split_discord_content,
    discord::{BridgeState, QueueDelivery},
    i18n::Language,
    install::updates::{self, UpdateSnapshot, UpdateStatus, UpdateStore},
};
use anyhow::{Context as _, Result, bail};
use serenity::{
    all::{
        ButtonStyle, ChannelId, ComponentInteraction, Context, CreateActionRow, CreateButton,
        CreateInteractionResponse, CreateInteractionResponseMessage, GuildId, MessageFlags,
        MessageId,
    },
    builder::{CreateAllowedMentions, CreateMessage, EditMessage},
    http::Http,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex as SyncMutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, Notify};

const IDLE_DELAY: Duration = Duration::from_secs(5 * 60);
const RETRY_DELAY: Duration = Duration::from_secs(15 * 60);
const INTERRUPT_TIMEOUT: Duration = Duration::from_secs(5);
const VOICE_LEAVE_TIMEOUT: Duration = Duration::from_secs(10);
const HANDLER_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

struct Activity {
    active: usize,
    last: Instant,
    applying: bool,
    updater_started: bool,
}

pub(crate) struct UpdateController {
    store: Mutex<UpdateStore>,
    state_dir: PathBuf,
    config_path: PathBuf,
    check_lock: Mutex<()>,
    manual_lock: Mutex<()>,
    confirmations: Mutex<HashMap<String, UpdateConfirmation>>,
    initialization_error: Option<String>,
    activity: SyncMutex<Activity>,
    wake: Notify,
    handlers_finished: Notify,
}

pub(crate) struct ActivityGuard {
    controller: Arc<UpdateController>,
    meaningful: bool,
}

impl ActivityGuard {
    pub(crate) fn mark_activity(&mut self) {
        self.meaningful = true;
        self.controller
            .activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last = Instant::now();
    }
}
impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let mut activity = self
            .controller
            .activity
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        activity.active = activity.active.saturating_sub(1);
        if self.meaningful {
            activity.last = Instant::now();
        }
        drop(activity);
        self.controller.handlers_finished.notify_waiters();
        self.controller.wake.notify_one();
    }
}

fn ready_activity(active: usize, elapsed: Duration, applying: bool, immediate: bool) -> bool {
    active == 0 && (immediate || elapsed >= IDLE_DELAY) && !applying
}

#[cfg(test)]
fn idle_activity(active: usize, elapsed: Duration, applying: bool) -> bool {
    ready_activity(active, elapsed, applying, false)
}

impl UpdateController {
    pub(crate) fn new(state_dir: PathBuf, config_path: PathBuf) -> Result<Self> {
        let (mut store, mut initialization_error) = match UpdateStore::load(&state_dir) {
            Ok(store) => (store, None),
            Err(error) => (
                UpdateStore::unavailable(&state_dir),
                Some(format!("{error:#}")),
            ),
        };
        if let Err(error) = store.reconcile_installed_release(
            updates::current_build_version(),
            updates::current_build_commit(),
        ) {
            initialization_error = Some(format!("{error:#}"));
        } else if let Err(error) = store.clear_unconfirmed_update() {
            initialization_error = Some(format!("{error:#}"));
        }
        let snapshot = store.snapshot();
        if snapshot.status == UpdateStatus::Applying {
            if let Some(target) = snapshot.pending {
                if target.commit == updates::current_build_commit() {
                    if let Err(error) = store.mark_applied(target.release_id) {
                        initialization_error = Some(format!("{error:#}"));
                    }
                } else if let Err(error) = store.mark_apply_failed(
                    target.release_id,
                    "Previous update was interrupted or restored the previous version".into(),
                ) {
                    initialization_error = Some(format!("{error:#}"));
                }
            }
        }
        Ok(Self {
            store: Mutex::new(store),
            state_dir,
            config_path,
            check_lock: Mutex::new(()),
            manual_lock: Mutex::new(()),
            confirmations: Mutex::new(HashMap::new()),
            initialization_error,
            activity: SyncMutex::new(Activity {
                active: 0,
                last: Instant::now(),
                applying: false,
                updater_started: false,
            }),
            wake: Notify::new(),
            handlers_finished: Notify::new(),
        })
    }

    pub(crate) fn admission(self: &Arc<Self>) -> ActivityGuard {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active += 1;
        ActivityGuard {
            controller: self.clone(),
            meaningful: false,
        }
    }

    pub(crate) fn activity(self: &Arc<Self>) -> ActivityGuard {
        let mut guard = self.admission();
        guard.mark_activity();
        guard
    }

    pub(crate) fn try_activity(self: &Arc<Self>) -> Option<ActivityGuard> {
        let mut state = self.activity.lock().unwrap_or_else(|e| e.into_inner());
        if state.applying {
            return None;
        }
        state.active += 1;
        state.last = Instant::now();
        Some(ActivityGuard {
            controller: self.clone(),
            meaningful: true,
        })
    }

    pub(crate) async fn wait_for_handlers(&self) {
        loop {
            let finished = self.handlers_finished.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            if self
                .activity
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active
                == 0
            {
                return;
            }
            finished.await;
        }
    }

    pub(crate) fn applying(&self) -> bool {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .applying
    }

    fn ready(&self, immediate: bool) -> bool {
        let state = self.activity.lock().unwrap_or_else(|e| e.into_inner());
        ready_activity(
            state.active,
            state.last.elapsed(),
            state.applying,
            immediate,
        )
    }

    fn claim_apply(&self, immediate: bool) -> bool {
        let mut state = self.activity.lock().unwrap_or_else(|e| e.into_inner());
        if state.applying
            || (!immediate
                && !ready_activity(state.active, state.last.elapsed(), state.applying, false))
        {
            return false;
        }
        state.applying = true;
        state.updater_started = false;
        true
    }

    fn updater_started(&self) {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .updater_started = true;
    }

    fn updater_is_started(&self) -> bool {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .updater_started
    }

    fn release_apply(&self) {
        let mut state = self.activity.lock().unwrap_or_else(|e| e.into_inner());
        state.applying = false;
        state.updater_started = false;
        state.last = Instant::now();
    }

    pub(crate) async fn muted(&self) -> bool {
        self.store.lock().await.snapshot().muted
    }

    pub(crate) async fn set_muted(&self, muted: bool) -> Result<()> {
        if let Some(error) = &self.initialization_error {
            bail!("update state is unavailable: {error}");
        }
        self.store.lock().await.set_muted(muted)?;
        self.wake.notify_one();
        Ok(())
    }

    async fn check_now(&self) -> Result<()> {
        if let Some(error) = &self.initialization_error {
            bail!("update state is unavailable: {error}");
        }
        let _check = self.check_lock.lock().await;
        let etag = self.store.lock().await.snapshot().etag;
        let result = tokio::task::spawn_blocking(move || {
            updates::check_release(
                etag,
                epoch(),
                updates::current_build_version(),
                updates::current_build_commit(),
            )
        })
        .await;
        match result {
            Ok(Ok(result)) => self.store.lock().await.apply_check(result)?,
            Ok(Err(error)) => {
                let message = format!("{error:#}");
                let _ = self
                    .store
                    .lock()
                    .await
                    .record_check_failure(epoch(), message.clone());
                bail!(message);
            }
            Err(error) => {
                let message = format!("release check task failed: {error}");
                let _ = self
                    .store
                    .lock()
                    .await
                    .record_check_failure(epoch(), message.clone());
                bail!(message);
            }
        }
        Ok(())
    }
}

fn version_is_newer(candidate: &str, installed: &str) -> Option<bool> {
    fn parts(value: &str) -> Option<[u64; 3]> {
        let value = value.trim_start_matches('v');
        let mut parts = value.split('.');
        Some([
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.split(['-', '+']).next()?.parse().ok()?,
        ])
    }
    match (parts(candidate), parts(installed)) {
        (Some(candidate), Some(installed)) => Some(candidate > installed),
        _ => None,
    }
}

fn has_pending_delivery(delivery: QueueDelivery) -> bool {
    matches!(
        delivery,
        QueueDelivery::Queued | QueueDelivery::Submitting | QueueDelivery::Unknown
    )
}

async fn idle(state: &BridgeState) -> bool {
    ready_to_apply(state, false).await
}

fn delivery_blocks_update(delivery: QueueDelivery, immediate: bool) -> bool {
    if immediate {
        matches!(delivery, QueueDelivery::Submitting | QueueDelivery::Unknown)
    } else {
        has_pending_delivery(delivery)
    }
}

async fn ready_to_apply(state: &BridgeState, immediate: bool) -> bool {
    if !state.updates.ready(immediate) || (!immediate && !state.native.is_connected()) {
        return false;
    }
    // Match admission lock order. Activity guards additionally cover downloads,
    // command handling and native output delivery before turn state is observable.
    let phase = state.voice.input_gate.state.lock().await;
    let turn = state.current_turn.lock().await;
    phase.allow_ordinary()
        && turn.is_none()
        && state.active_turns.lock().await.is_empty()
        && state.pending_approvals.lock().await.is_empty()
        && !state
            .queued_inputs
            .lock()
            .await
            .iter()
            .any(|input| delivery_blocks_update(input.delivery, immediate))
        && state.updates.ready(immediate)
}

fn check_due(snapshot: &UpdateSnapshot, now: u64) -> bool {
    let delay = if snapshot.last_check_error.is_some() {
        RETRY_DELAY.as_secs()
    } else {
        updates::DEFAULT_CHECK_INTERVAL_SECS
    };
    snapshot
        .last_checked_unix
        .is_none_or(|last| now.saturating_sub(last) >= delay)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ManualUpdateAction {
    Active(updates::PinnedRelease),
    ActiveWithoutTarget,
    Confirm(updates::PinnedRelease),
    NoUpdate,
    NoPublishedRelease,
    MissingVerifiedAsset,
}

#[derive(Clone)]
struct UpdateConfirmation {
    requester_id: u64,
    target: updates::PinnedRelease,
}

fn manual_update_action(snapshot: &UpdateSnapshot) -> ManualUpdateAction {
    if snapshot.status == UpdateStatus::Applying {
        return snapshot.pending.clone().map_or(
            ManualUpdateAction::ActiveWithoutTarget,
            ManualUpdateAction::Active,
        );
    }
    if matches!(
        snapshot.status,
        UpdateStatus::Selected | UpdateStatus::Preparing | UpdateStatus::Ready
    ) && snapshot
        .pending
        .as_ref()
        .is_some_and(|pending| snapshot.immediate_release_id == Some(pending.release_id))
    {
        return ManualUpdateAction::Active(snapshot.pending.clone().expect("checked pending"));
    }
    // Keep an actual failed, still-newer pinned update retryable. Startup
    // reconciliation silently removes superseded failures before this is reached.
    if snapshot.status == UpdateStatus::Failed {
        if let Some(pending) = snapshot.pending.as_ref().filter(|pending| {
            version_is_newer(&pending.version, updates::current_build_version()) != Some(false)
                && pending.commit != updates::current_build_commit()
        }) {
            let latest_is_newer = snapshot.latest_release.as_ref().is_some_and(|latest| {
                version_is_newer(&latest.version, &pending.version) == Some(true)
            });
            if !latest_is_newer {
                return ManualUpdateAction::Confirm(pending.clone());
            }
        }
    }
    let Some(latest) = snapshot.latest_release.as_ref() else {
        return if snapshot.no_published_release {
            ManualUpdateAction::NoPublishedRelease
        } else {
            ManualUpdateAction::NoUpdate
        };
    };
    if snapshot.last_applied_release_id == Some(latest.release_id)
        || latest.commit == updates::current_build_commit()
        || version_is_newer(&latest.version, updates::current_build_version()) == Some(false)
    {
        return ManualUpdateAction::NoUpdate;
    }
    let verified_asset = latest.asset.as_ref().is_some_and(|asset| {
        asset.digest.as_deref().is_some_and(|digest| {
            digest.strip_prefix("sha256:").is_some_and(|hash| {
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        })
    });
    if verified_asset {
        ManualUpdateAction::Confirm(updates::PinnedRelease {
            release_id: latest.release_id,
            version: latest.version.clone(),
            tag: latest.tag.clone(),
            commit: latest.commit.clone(),
            url: latest.url.clone(),
            asset: latest.asset.clone(),
        })
    } else {
        ManualUpdateAction::MissingVerifiedAsset
    }
}

fn update_start_text(language: Language) -> &'static str {
    language.text(
        "Starting the update now. All active work, including subagent work, and any voice call will be interrupted. Interrupted work will not resume automatically. Session history and settings are preserved.",
        "업데이트를 시작합니다. 서브에이전트 작업을 포함한 모든 진행 중 작업과 통화가 중단되며 자동 재개되지 않습니다. 세션 기록과 설정은 유지됩니다.",
        "现在开始更新。包括子代理在内的所有进行中工作和通话都会中断，中断的工作不会自动恢复。会话历史和设置会保留。",
        "更新を開始します。サブエージェントを含む進行中の作業と通話は中断され、中断した作業は自動再開されません。セッション履歴と設定は保持されます。",
    )
}

fn manual_update_response(
    action: &ManualUpdateAction,
    language: Language,
    confirmation_id: &str,
) -> (String, Vec<CreateActionRow>) {
    let text = match action {
        ManualUpdateAction::Active(target) => format!(
            "**Discodex {}**\n{}", target.version, update_start_text(language)),
        ManualUpdateAction::ActiveWithoutTarget => language.text(
            "An update is already in progress.", "업데이트가 이미 진행 중입니다.",
            "更新已在进行中。", "更新はすでに進行中です。").to_owned(),
        ManualUpdateAction::Confirm(target) => format!(
            "**Discodex {}**\n{}\n[{}](<{}>)", target.version,
            language.text(
                "Update now? All active work, including subagent work, and any voice call will be interrupted. Interrupted work will not resume automatically. Session history and settings are preserved.",
                "지금 업데이트할까요? 서브에이전트 작업을 포함한 모든 진행 중 작업과 통화가 중단되며 자동 재개되지 않습니다. 세션 기록과 설정은 유지됩니다.",
                "现在更新吗？包括子代理在内的所有进行中工作和通话都会中断，且不会自动恢复。会话历史和设置会保留。",
                "今すぐ更新しますか？サブエージェントを含む進行中の作業と通話は中断され、自動再開されません。セッション履歴と設定は保持されます。"),
            language.text("Release notes", "변경 내용", "版本说明", "リリースノート"),
            target.url),
        ManualUpdateAction::NoUpdate => format!(
            "{} (v{})", language.text("Already up to date.", "이미 최신 버전입니다.",
            "已是最新版本。", "すでに最新バージョンです。"), updates::current_build_version()),
        ManualUpdateAction::NoPublishedRelease => format!(
            "{} (v{})", language.text(
                "No stable release has been published yet. Current version:",
                "아직 공개된 안정 버전이 없습니다. 현재 버전:",
                "尚未发布稳定版本。当前版本：", "安定版はまだ公開されていません。現在のバージョン:"),
            updates::current_build_version()),
        ManualUpdateAction::MissingVerifiedAsset => language.text(
            "The latest release has no verified prebuilt executable for this platform. No update was started.",
            "최신 릴리스에 이 플랫폼용 검증된 사전 빌드 실행 파일이 없어 업데이트를 시작하지 않았습니다.",
            "最新版本没有适用于此平台且可验证的预构建可执行文件。未开始更新。",
            "最新リリースにはこのプラットフォーム向けの検証済みビルドがありません。更新は開始していません。").to_owned(),
    };
    let rows = if matches!(action, ManualUpdateAction::Confirm(_)) {
        vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("discodex:update-confirm:{confirmation_id}:install"))
                .label(language.text("Update", "업데이트", "更新", "更新"))
                .style(ButtonStyle::Primary),
            CreateButton::new(format!("discodex:update-confirm:{confirmation_id}:cancel"))
                .label(language.text("Cancel", "취소", "取消", "キャンセル"))
                .style(ButtonStyle::Secondary),
        ])]
    } else {
        Vec::new()
    };
    (text, rows)
}

fn manual_check_failed(language: Language) -> String {
    language
        .text(
            "Could not check for updates. Please try again later.",
            "업데이트를 확인하지 못했습니다. 잠시 후 다시 시도해 주세요.",
            "无法检查更新，请稍后重试。",
            "更新を確認できませんでした。しばらくしてから再試行してください。",
        )
        .to_owned()
}

pub(crate) async fn manual_update(
    state: &Arc<BridgeState>,
    requester_id: u64,
) -> Result<(String, Vec<CreateActionRow>)> {
    if let Some(error) = &state.updates.initialization_error {
        bail!("update state is unavailable: {error}");
    }
    let _manual = state.updates.manual_lock.lock().await;
    // An already confirmed, pinned update keeps its target while it runs.
    let action = manual_update_action(&state.updates.store.lock().await.snapshot());
    if matches!(
        action,
        ManualUpdateAction::Active(_) | ManualUpdateAction::ActiveWithoutTarget
    ) {
        return Ok(manual_update_response(&action, state.config.language, ""));
    }
    // A check never selects or retries a release. Only the confirmation button does.
    if let Err(error) = state.updates.check_now().await {
        tracing::warn!(%error, "manual Discodex release check failed");
        return Ok((manual_check_failed(state.config.language), Vec::new()));
    }
    let action = manual_update_action(&state.updates.store.lock().await.snapshot());
    let key = uuid::Uuid::new_v4().simple().to_string();
    let mut confirmations = state.updates.confirmations.lock().await;
    confirmations.retain(|_, previous| previous.requester_id != requester_id);
    if let ManualUpdateAction::Confirm(target) = &action {
        confirmations.insert(
            key.clone(),
            UpdateConfirmation {
                requester_id,
                target: target.clone(),
            },
        );
    }
    Ok(manual_update_response(&action, state.config.language, &key))
}

fn confirm_update(
    store: &mut UpdateStore,
    confirmation: &UpdateConfirmation,
    requester_id: u64,
    install: bool,
) -> Result<()> {
    if confirmation.requester_id != requester_id {
        bail!("this confirmation belongs to another operator");
    }
    if !install {
        return Ok(());
    }
    let target = &confirmation.target;
    match manual_update_action(&store.snapshot()) {
        ManualUpdateAction::Confirm(current) | ManualUpdateAction::Active(current)
            if current == *target => {}
        _ => bail!("release confirmation is stale"),
    }
    store.request_immediate(target.release_id)?;
    Ok(())
}

pub(crate) async fn handle_confirmation(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
) -> Result<()> {
    interaction
        .create_response(ctx, CreateInteractionResponse::Acknowledge)
        .await?;
    let parts: Vec<_> = interaction.data.custom_id.split(':').collect();
    if parts.len() != 4 || !matches!(parts[3], "install" | "cancel") {
        bail!("invalid update confirmation");
    }
    let language = state.config.language;
    let _manual = state.updates.manual_lock.lock().await;
    let mut confirmations = state.updates.confirmations.lock().await;
    let confirmation = confirmations.get(parts[2]).cloned();
    let response = if let Some(confirmation) = confirmation {
        let result = confirm_update(
            &mut *state.updates.store.lock().await,
            &confirmation,
            interaction.user.id.get(),
            parts[3] == "install",
        );
        match result {
            Ok(()) => {
                confirmations.remove(parts[2]);
                if parts[3] == "install" {
                    state.updates.wake.notify_one();
                    update_start_text(language)
                } else {
                    language.text(
                        "Update cancelled.",
                        "업데이트를 취소했습니다.",
                        "已取消更新。",
                        "更新をキャンセルしました。",
                    )
                }
            }
            Err(error) => {
                tracing::warn!(%error, "update confirmation failed");
                language.text(
                    "Could not start this update. Run /discodex update again.",
                    "업데이트를 시작하지 못했습니다. /discodex update로 다시 확인해 주세요.",
                    "无法开始此更新，请重新运行 /discodex update。",
                    "更新を開始できませんでした。/discodex update を再実行してください。",
                )
            }
        }
    } else {
        language.text(
            "This confirmation is no longer active. Run /discodex update again.",
            "지난 확인 화면입니다. /discodex update로 다시 확인해 주세요.",
            "此确认已失效，请重新运行 /discodex update。",
            "この確認画面は無効です。/discodex update を再実行してください。",
        )
    };
    interaction
        .edit_response(
            ctx,
            serenity::builder::EditInteractionResponse::new()
                .content(response)
                .components(Vec::new())
                .allowed_mentions(mentions()),
        )
        .await?;
    Ok(())
}

fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn mentions() -> CreateAllowedMentions {
    CreateAllowedMentions::new()
        .all_users(false)
        .all_roles(false)
        .everyone(false)
        .replied_user(false)
}

fn card_target(snapshot: &UpdateSnapshot) -> Option<(u64, String, String)> {
    if let Some(target) = snapshot.pending.as_ref() {
        return Some((
            target.release_id,
            target.version.clone(),
            target.url.clone(),
        ));
    }
    if let Some(release) = snapshot.latest_release.as_ref() {
        return Some((
            release.release_id,
            release.version.clone(),
            release.url.clone(),
        ));
    }
    snapshot.last_result.as_ref().map(|target| {
        (
            target.release_id,
            target.version.clone(),
            target.url.clone(),
        )
    })
}

fn status_for(snapshot: &UpdateSnapshot, release_id: u64) -> UpdateStatus {
    if snapshot
        .pending
        .as_ref()
        .is_some_and(|p| p.release_id == release_id)
        || snapshot
            .last_result
            .as_ref()
            .is_some_and(|r| r.release_id == release_id)
    {
        snapshot.status
    } else {
        UpdateStatus::Idle
    }
}

fn render(
    snapshot: &UpdateSnapshot,
    language: Language,
) -> Option<(u64, String, Vec<CreateActionRow>)> {
    let (id, version, url) = card_target(snapshot)?;
    let release_status = status_for(snapshot, id);
    let status = if snapshot.skipped_release_id == Some(id) {
        language.text(
            "Skipped this version.",
            "이번 버전을 건너뛰었습니다.",
            "已跳过此版本。",
            "このバージョンをスキップしました。",
        )
    } else {
        match release_status {
            UpdateStatus::Selected | UpdateStatus::Ready
                if snapshot.immediate_release_id == Some(id) => update_start_text(language),
            UpdateStatus::Selected | UpdateStatus::Ready => language.text("Scheduled. Installation will wait until work and calls are idle.", "예약했습니다. 작업과 통화가 끝나고 유휴 상태가 되면 설치합니다.", "已安排，将在工作和通话结束且空闲后安装。", "予約しました。作業と通話が終了し、待機状態になってからインストールします。"),
            UpdateStatus::Preparing => language.text("Preparing the selected version. You can keep working.", "선택한 버전을 준비하고 있습니다. 계속 작업할 수 있습니다.", "正在准备所选版本，可以继续工作。", "選択したバージョンを準備中です。作業は続けられます。"),
            UpdateStatus::Applying => language.text("Installing. Discodex will reconnect with your session and settings preserved.", "설치 중입니다. 세션과 설정을 유지한 채 다시 연결합니다.", "正在安装，将保留会话和设置并重新连接。", "インストール中です。セッションと設定を保持して再接続します。"),
            UpdateStatus::Failed => language.text("Update failed. It will not retry automatically. You can retry with Update.", "업데이트에 실패했습니다. 자동 재시도하지 않습니다. 업데이트 버튼으로 다시 시도할 수 있습니다.", "更新失败，不会自动重试。可点击更新重试。", "更新に失敗しました。自動では再試行しません。「更新」で再試行できます。"),
            UpdateStatus::Applied => language.text("Updated. Your session and settings were preserved.", "업데이트했습니다. 세션과 설정을 유지했습니다.", "已更新，会话和设置已保留。", "更新しました。セッションと設定は保持されています。"),
            UpdateStatus::Idle => language.text("Choose Update to start; your session and settings are preserved.", "업데이트를 누르면 시작합니다. 세션과 설정은 유지됩니다.", "选择更新即可开始；会话和设置将被保留。", "「更新」で開始します。セッションと設定は保持されます。"),
        }
    };
    let mut text = format!(
        "**Discodex {version}**\n{status}\n[{}](<{url}>)",
        language.text("Release notes", "변경 내용", "版本说明", "リリースノート")
    );
    if snapshot.muted {
        text.push_str(language.text(
            "\nUpdate notifications are off. You can turn them back on in Settings.",
            "\n업데이트 알림을 껐습니다. 설정에서 다시 켤 수 있습니다.",
            "\n更新通知已关闭，可在设置中重新开启。",
            "\n更新通知はオフです。設定で再度オンにできます。",
        ));
    }
    let complete =
        release_status == UpdateStatus::Applied && snapshot.last_applied_release_id == Some(id);
    let rows = if complete || snapshot.skipped_release_id == Some(id) || snapshot.muted {
        Vec::new()
    } else {
        let busy = matches!(
            release_status,
            UpdateStatus::Selected
                | UpdateStatus::Preparing
                | UpdateStatus::Ready
                | UpdateStatus::Applying
        );
        vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("discodex:update:{id}:install"))
                .label(language.text("Update", "업데이트", "更新", "更新"))
                .style(ButtonStyle::Primary)
                .disabled(busy),
            CreateButton::new(format!("discodex:update:{id}:skip"))
                .label(language.text(
                    "Skip this version",
                    "이번 버전은 스킵",
                    "跳过此版本",
                    "このバージョンをスキップ",
                ))
                .disabled(release_status == UpdateStatus::Applying),
            CreateButton::new(format!("discodex:update:{id}:mute"))
                .label(language.text(
                    "Never notify again",
                    "다시 알리지 않음",
                    "不再通知",
                    "今後通知しない",
                ))
                .disabled(release_status == UpdateStatus::Applying),
        ])]
    };
    Some((id, text, rows))
}

pub(crate) async fn handle_component(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &Arc<BridgeState>,
) -> Result<()> {
    if let Some(error) = &state.updates.initialization_error {
        bail!("update state is unavailable: {error}");
    }
    let parts = interaction.data.custom_id.split(':').collect::<Vec<_>>();
    if parts.len() != 4 {
        bail!("invalid update action");
    }
    let id: u64 = parts[2].parse().context("invalid release identity")?;
    // Authorisation is performed by commands::handle_component before dispatch.
    interaction
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    let language = state.config.language;
    let snapshot = state.updates.store.lock().await.snapshot();
    if snapshot.message_id != Some(interaction.message.id.get())
        || snapshot.notified_release_id != Some(id)
    {
        interaction
            .edit_response(
                ctx,
                serenity::builder::EditInteractionResponse::new()
                    .content(language.text(
                        "This update notice has been replaced. Use the latest notice.",
                        "새 업데이트 알림을 사용해 주세요.",
                        "此通知已被替换，请使用最新通知。",
                        "最新の更新通知を使用してください。",
                    ))
                    .allowed_mentions(mentions()),
            )
            .await?;
        return Ok(());
    }
    if snapshot.status == UpdateStatus::Applying {
        interaction
            .edit_response(
                ctx,
                serenity::builder::EditInteractionResponse::new()
                    .content(language.text(
                        "The update is already being installed.",
                        "업데이트를 이미 설치하고 있습니다.",
                        "正在安装更新。",
                        "更新をインストール中です。",
                    ))
                    .allowed_mentions(mentions()),
            )
            .await?;
        return Ok(());
    }
    match parts[3] {
        "install" => {
            // A public notice may be stale. Resolve latest before presenting the
            // same explicit interrupt warning used by /discodex update.
            let _manual = state.updates.manual_lock.lock().await;
            if let Err(error) = state.updates.check_now().await {
                tracing::warn!(%error, "notice update release check failed");
                interaction
                    .edit_response(
                        ctx,
                        serenity::builder::EditInteractionResponse::new()
                            .content(manual_check_failed(language))
                            .allowed_mentions(mentions()),
                    )
                    .await?;
                return Ok(());
            }
            let action = manual_update_action(&state.updates.store.lock().await.snapshot());
            let key = uuid::Uuid::new_v4().simple().to_string();
            if let ManualUpdateAction::Confirm(target) = &action {
                state.updates.confirmations.lock().await.insert(
                    key.clone(),
                    UpdateConfirmation {
                        requester_id: interaction.user.id.get(),
                        target: target.clone(),
                    },
                );
            }
            let (content, components) = manual_update_response(&action, language, &key);
            interaction
                .edit_response(
                    ctx,
                    serenity::builder::EditInteractionResponse::new()
                        .content(content)
                        .components(components)
                        .allowed_mentions(mentions()),
                )
                .await?;
        }
        "skip" | "mute" => {
            let message = {
                let mut store = state.updates.store.lock().await;
                if parts[3] == "skip" {
                    store.skip_release(id)?;
                    language.text(
                        "Skipped this version.",
                        "이번 버전을 건너뛰었습니다.",
                        "已跳过此版本。",
                        "このバージョンをスキップしました。",
                    )
                } else {
                    store.set_muted(true)?;
                    language.text(
                        "Update notifications are off. Turn them back on in Settings.",
                        "업데이트 알림을 껐습니다. 설정에서 다시 켤 수 있습니다.",
                        "更新通知已关闭，可在设置中重新开启。",
                        "更新通知をオフにしました。設定で再度オンにできます。",
                    )
                }
            };
            interaction
                .edit_response(
                    ctx,
                    serenity::builder::EditInteractionResponse::new()
                        .content(message)
                        .allowed_mentions(mentions()),
                )
                .await?;
            state.updates.wake.notify_one();
        }
        _ => bail!("unknown update action"),
    }
    Ok(())
}

fn carries_notice(components: &serde_json::Value, release_id: u64) -> bool {
    let prefix = format!("discodex:update:{release_id}:");
    components.as_array().into_iter().flatten().any(|row| {
        row.get("components")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .any(|button| {
                button
                    .get("custom_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| id.starts_with(&prefix))
            })
    })
}

async fn recover_notice(
    http: &Http,
    channel: ChannelId,
    bot_id: serenity::all::UserId,
    id: u64,
    since: u64,
) -> Result<Option<MessageId>> {
    let mut before = None;
    loop {
        let mut query = serenity::builder::GetMessages::new().limit(100);
        if let Some(cursor) = before {
            query = query.before(cursor);
        }
        let messages = channel.messages(http, query).await?;
        if messages.is_empty() {
            return Ok(None);
        }
        for message in &messages {
            if message.author.id == bot_id
                && carries_notice(&serde_json::to_value(&message.components)?, id)
            {
                return Ok(Some(message.id));
            }
        }
        let oldest = messages.last().expect("nonempty history page");
        if oldest.timestamp.unix_timestamp() < since.saturating_sub(2) as i64 {
            return Ok(None);
        }
        before = Some(oldest.id);
    }
}

async fn deliver_notice(
    state: &BridgeState,
    http: &Http,
    allow_new: bool,
    last_rendered: &mut Option<String>,
) -> Result<()> {
    let mut store = state.updates.store.lock().await;
    let snapshot = store.snapshot();
    let Some((id, content, rows)) = render(&snapshot, state.config.language) else {
        return Ok(());
    };
    let fingerprint = serde_json::to_string(&(id, &content, &rows))?;
    let existing = snapshot.notified_release_id == Some(id);
    let channel = ChannelId::new(state.config.home_channel_id);
    let mut message_id = if existing {
        snapshot.message_id.map(MessageId::new)
    } else {
        None
    };
    if existing && message_id.is_none() {
        if let (Some(since), Some(bot_id)) = (
            snapshot.notice_started_unix,
            *state.bot_user_id.lock().await,
        ) {
            message_id = recover_notice(http, channel, bot_id, id, since).await?;
            if let Some(found) = message_id {
                store.record_notice(id, found.get())?;
            }
        }
    }
    if let Some(message_id) = message_id {
        if last_rendered.as_ref() == Some(&fingerprint) {
            return Ok(());
        }
        channel
            .edit_message(
                http,
                message_id,
                EditMessage::new()
                    .content(content)
                    .components(rows)
                    .allowed_mentions(mentions()),
            )
            .await?;
    } else {
        if !allow_new
            || snapshot.muted
            || snapshot.skipped_release_id == Some(id)
            || snapshot.last_applied_release_id == Some(id)
        {
            return Ok(());
        }
        if !existing {
            store.begin_notice(id, epoch())?;
        }
        let message = channel
            .send_message(
                http,
                CreateMessage::new()
                    .content(content)
                    .components(rows)
                    .flags(MessageFlags::SUPPRESS_NOTIFICATIONS)
                    .allowed_mentions(mentions())
                    .nonce(serenity::all::Nonce::String(id.to_string()))
                    .enforce_nonce(true),
            )
            .await?;
        store.record_notice(id, message.id.get())?;
    }
    *last_rendered = Some(fingerprint);
    Ok(())
}

fn result_message(
    snapshot: &UpdateSnapshot,
    language: Language,
    current_commit: &str,
) -> Option<(String, String)> {
    let token = snapshot.result_notification_id.as_ref()?;
    if snapshot.result_notified_id.as_ref() == Some(token) {
        return None;
    }
    let result = snapshot.last_result.as_ref()?;
    let status = match snapshot.status {
        UpdateStatus::Applied if result.commit == current_commit => language.text(
            "Update complete. Discodex has restarted; your session and settings were preserved.",
            "업데이트 완료. Discodex가 재시작됐으며 세션과 설정을 유지했습니다.",
            "更新完成。Discodex 已重启，会话和设置已保留。",
            "更新完了。Discodex が再起動し、セッションと設定は保持されています。",
        ),
        UpdateStatus::Failed => language.text(
            "Update failed. Run /discodex update to retry.",
            "업데이트에 실패했습니다. /discodex update로 다시 시도할 수 있습니다.",
            "更新失败。可运行 /discodex update 重试。",
            "更新に失敗しました。/discodex update で再試行できます。",
        ),
        _ => return None,
    };
    let mut content = format!("**Discodex {}**\n{status}", result.version);
    if snapshot.status == UpdateStatus::Failed
        && let Some(error) = &snapshot.last_error
    {
        content.push_str("\n\n");
        content.push_str(error);
    }
    Some((token.clone(), content))
}

fn result_message_pages(token: &str, content: &str) -> Vec<CreateMessage> {
    split_discord_content(content)
        .into_iter()
        .enumerate()
        .map(|(page, content)| {
            let nonce = if page == 0 {
                token.chars().filter(|c| *c != '-').take(24).collect()
            } else {
                format!("{:x}", Sha256::digest(format!("{token}:{page}")))
                    .chars()
                    .take(24)
                    .collect()
            };
            CreateMessage::new()
                .content(content)
                .allowed_mentions(mentions())
                .nonce(serenity::all::Nonce::String(nonce))
                .enforce_nonce(true)
        })
        .collect()
}

async fn deliver_result_with<F, Fut>(
    snapshot: &UpdateSnapshot,
    language: Language,
    current_commit: &str,
    mut send: F,
) -> Result<Option<String>>
where
    F: FnMut(CreateMessage) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let Some((token, content)) = result_message(snapshot, language, current_commit) else {
        return Ok(None);
    };
    for message in result_message_pages(&token, &content) {
        send(message).await?;
    }
    Ok(Some(token))
}

async fn deliver_result(state: &BridgeState, http: &Http) -> Result<()> {
    let mut store = state.updates.store.lock().await;
    let channel = ChannelId::new(state.config.home_channel_id);
    let Some(token) = deliver_result_with(
        &store.snapshot(),
        state.config.language,
        updates::current_build_commit(),
        |message| async move {
            channel.send_message(http, message).await?;
            Ok(())
        },
    )
    .await?
    else {
        return Ok(());
    };
    store.record_result_notified(&token)?;
    Ok(())
}

fn immediate_requested(snapshot: &UpdateSnapshot) -> bool {
    snapshot
        .pending
        .as_ref()
        .is_some_and(|p| snapshot.immediate_release_id == Some(p.release_id))
}

async fn interrupt_owned_turns(state: &BridgeState) {
    // Turn-state handlers take current_turn before active_turns. Keep that order
    // while taking a stable snapshot, then release both locks before IPC.
    let current = state.current_turn.lock().await;
    let mut turns = state.active_turns.lock().await.clone();
    if let Some(turn_id) = current.as_ref() {
        turns
            .entry(state.native.thread_id())
            .or_insert_with(|| turn_id.clone());
    }
    drop(current);

    for (thread_id, turn_id, result) in
        request_turn_interruptions(&turns, INTERRUPT_TIMEOUT, |method, params| {
            state.native.request(method, params)
        })
        .await
    {
        match result {
            Ok(Ok(_)) => {
                tracing::info!(%thread_id, %turn_id, "requested active turn interruption for update")
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, %thread_id, %turn_id, "could not interrupt active turn for update")
            }
            Err(error) => {
                tracing::warn!(%error, %thread_id, %turn_id, "active turn interruption timed out for update")
            }
        }
    }
}

type TurnInterruption = (
    String,
    String,
    std::result::Result<Result<serde_json::Value>, tokio::time::error::Elapsed>,
);

/// Each request retains its native result or deadline error. Neither prevents
/// the caller from handing off the already-confirmed update afterwards.
async fn request_turn_interruptions<F, Fut>(
    turns: &HashMap<String, String>,
    timeout: Duration,
    request: F,
) -> Vec<TurnInterruption>
where
    F: Fn(&'static str, serde_json::Value) -> Fut,
    Fut: std::future::Future<Output = Result<serde_json::Value>>,
{
    let requests = turns.iter().map(|(thread_id, turn_id)| {
        let request = &request;
        async move {
            let result = tokio::time::timeout(
                timeout,
                request(
                    "turn/interrupt",
                    serde_json::json!({"threadId": thread_id, "turnId": turn_id}),
                ),
            )
            .await;
            (thread_id.clone(), turn_id.clone(), result)
        }
    });
    futures_util::future::join_all(requests).await
}

pub(crate) async fn run(
    state: Arc<BridgeState>,
    http: Arc<Http>,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) {
    if let Some(error) = &state.updates.initialization_error {
        tracing::warn!(%error, "update notifications unavailable; bridge continues normally");
        return;
    }
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut retry_after = Instant::now();
    let mut rendered = None;
    loop {
        tokio::select! { _ = &mut stop => break, _ = tick.tick() => {}, _ = state.updates.wake.notified() => {} }
        if state.bot_user_id.lock().await.is_none() {
            continue;
        }
        if state.updates.applying() && state.updates.updater_is_started() {
            // Before launch, applying closes ingress while the worker interrupts
            // its active session. Once launched, the helper owns disk state.
            let dir = state.updates.state_dir.clone();
            let recovery = tokio::task::spawn_blocking(move || -> Result<UpdateStore> {
                let mut store = UpdateStore::load(&dir)?;
                let snapshot = store.snapshot();
                if snapshot.status == UpdateStatus::Applying {
                    if let Some(attempt) = snapshot.apply_attempt_id {
                        if !updates::helper_running(&dir, &attempt)? {
                            store.fail_stopped_helper(
                                &attempt,
                                "Detached updater stopped before completing the update".into(),
                            )?;
                        }
                    }
                }
                UpdateStore::load(&dir)
            })
            .await;
            match recovery {
                Ok(Ok(store))
                    if matches!(
                        store.snapshot().status,
                        UpdateStatus::Failed | UpdateStatus::Applied
                    ) =>
                {
                    *state.updates.store.lock().await = store;
                    state.updates.release_apply();
                    state.voice.input_gate.ready.notify_one();
                }
                Ok(Ok(_)) => {}
                error => tracing::warn!(?error, "could not reconcile detached updater"),
            }
            continue;
        }
        let before = state.updates.store.lock().await.snapshot();
        let immediate = immediate_requested(&before);
        if !immediate {
            if let Err(error) = deliver_result(&state, &http).await {
                tracing::warn!(%error, "could not deliver update result");
            }
            let due = check_due(&before, epoch());
            if due && Instant::now() >= retry_after {
                if let Err(error) = state.updates.check_now().await {
                    tracing::warn!(%error, "could not check Discodex releases");
                    retry_after = Instant::now() + RETRY_DELAY;
                }
            }
            let is_idle = idle(&state).await;
            if let Err(error) = deliver_notice(&state, &http, is_idle, &mut rendered).await {
                tracing::warn!(%error, "could not deliver update notice");
            }
        }
        let snapshot = state.updates.store.lock().await.snapshot();
        if !immediate
            || !matches!(
                snapshot.status,
                UpdateStatus::Selected | UpdateStatus::Preparing | UpdateStatus::Ready
            )
        {
            continue;
        }
        let Some(target) = snapshot.pending else {
            continue;
        };
        if snapshot.prepared.is_none() {
            let id = target.release_id;
            if let Err(error) = state.updates.store.lock().await.mark_preparing(id) {
                tracing::warn!(%error, "could not record update preparation");
                continue;
            }
            let dir = state.updates.state_dir.clone();
            let result =
                tokio::task::spawn_blocking(move || updates::prepare_update(target, &dir)).await;
            let mut store = state.updates.store.lock().await;
            if store
                .snapshot()
                .pending
                .as_ref()
                .is_some_and(|pending| pending.release_id == id)
            {
                let saved = match result {
                    Ok(Ok(prepared)) => store.store_prepared(prepared),
                    result => store
                        .mark_apply_failed(id, format!("Update preparation failed: {result:?}")),
                };
                if let Err(error) = saved {
                    tracing::warn!(%error, "could not save prepared update");
                }
            }
            state.updates.wake.notify_one();
            continue;
        }
        // Final gate is serialized with ingress and output handlers. All later
        // ingress is durably queued until this short replacement finishes.
        // Only an explicit confirmation can enter the durable applying phase.
        // This closes ingress before active turns and voice are interrupted.
        if !state.updates.claim_apply(true) {
            continue;
        }
        let (prepared, attempt_id) = {
            let mut store = state.updates.store.lock().await;
            let snapshot = store.snapshot();
            match snapshot.prepared {
                Some(prepared) if snapshot.status == UpdateStatus::Ready => {
                    let attempt_id = match store.mark_applying(prepared.release_id) {
                        Ok(attempt_id) => attempt_id,
                        Err(error) => {
                            tracing::warn!(%error, "could not record update handoff");
                            state.updates.release_apply();
                            continue;
                        }
                    };
                    (prepared, attempt_id)
                }
                _ => {
                    state.updates.release_apply();
                    continue;
                }
            }
        };
        if immediate {
            if let Err(error) =
                tokio::time::timeout(HANDLER_DRAIN_TIMEOUT, state.updates.wait_for_handlers()).await
            {
                tracing::warn!(%error, "ingress handler drain timed out before update interruption");
            }
            interrupt_owned_turns(&state).await;
            match tokio::time::timeout(
                VOICE_LEAVE_TIMEOUT,
                state.voice.leave(GuildId::new(state.config.guild_id)),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "could not fully close voice before update restart")
                }
                Err(error) => {
                    tracing::warn!(%error, "voice shutdown timed out before update restart")
                }
            }
            if let Err(error) =
                tokio::time::timeout(HANDLER_DRAIN_TIMEOUT, state.updates.wait_for_handlers()).await
            {
                tracing::warn!(%error, "handler drain timed out before confirmed update restart");
            }
        }
        if immediate {
            if let Err(error) = tokio::time::timeout(
                INTERRUPT_TIMEOUT,
                deliver_notice(&state, &http, false, &mut rendered),
            )
            .await
            {
                tracing::warn!(%error, "update notice delivery timed out before restart");
            }
        } else {
            let _ = deliver_notice(&state, &http, false, &mut rendered).await;
        }
        let config = state.updates.config_path.clone();
        let id = prepared.release_id;
        let helper_attempt = attempt_id.clone();
        state.updates.updater_started();
        let launched = tokio::task::spawn_blocking(move || {
            updates::launch_apply(&prepared, &config, &helper_attempt)
        })
        .await;
        if !matches!(&launched, Ok(Ok(()))) {
            let mut store = state.updates.store.lock().await;
            if let Err(error) = store.cancel_unstarted_handoff(
                id,
                &attempt_id,
                format!("Update handoff failed: {launched:?}"),
            ) {
                tracing::warn!(%error, "could not cancel unstarted updater");
            }
            // launch_apply may already have cancelled its ticket on timeout.
            // A second cancellation then returns false, but admission must reopen.
            match UpdateStore::load(&state.updates.state_dir) {
                Ok(latest) => {
                    let snapshot = latest.snapshot();
                    let still_applying = snapshot.status == UpdateStatus::Applying
                        && snapshot.apply_attempt_id.as_deref() == Some(attempt_id.as_str());
                    *store = latest;
                    if !still_applying {
                        state.updates.release_apply();
                        state.voice.input_gate.ready.notify_one();
                    }
                }
                Err(error) => tracing::warn!(%error, "could not reload update handoff outcome"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn update_interrupts_main_and_children_and_returns_all_failures() {
        let turns = HashMap::from([
            ("main".to_owned(), "main-turn".to_owned()),
            ("child-refused".to_owned(), "refused-turn".to_owned()),
            ("child-waiting".to_owned(), "waiting-turn".to_owned()),
        ]);
        let before = turns.clone();
        let calls = SyncMutex::new(Vec::new());
        let results =
            request_turn_interruptions(&turns, Duration::from_millis(1), |method, params| {
                calls.lock().unwrap().push((method, params.clone()));
                async move {
                    match params["threadId"].as_str().unwrap() {
                        "main" => Ok(serde_json::json!({})),
                        "child-refused" => Err(anyhow::anyhow!("native interrupt refusal")),
                        "child-waiting" => std::future::pending().await,
                        other => panic!("unexpected interruption target {other}"),
                    }
                }
            })
            .await;
        let mut results: HashMap<_, _> = results
            .into_iter()
            .map(|(thread, turn, result)| (thread, (turn, result)))
            .collect();
        let (turn, result) = results.remove("main").unwrap();
        assert_eq!(turn, "main-turn");
        assert_eq!(result.unwrap().unwrap(), serde_json::json!({}));
        let (turn, result) = results.remove("child-refused").unwrap();
        assert_eq!(turn, "refused-turn");
        assert_eq!(
            result.unwrap().unwrap_err().to_string(),
            "native interrupt refusal"
        );
        let (turn, result) = results.remove("child-waiting").unwrap();
        assert_eq!(turn, "waiting-turn");
        assert!(result.is_err());
        assert!(results.is_empty());
        assert_eq!(turns, before);
        let mut actual = calls.lock().unwrap().clone();
        actual.sort_by_key(|(_, params)| params["threadId"].as_str().unwrap().to_owned());
        assert_eq!(
            actual,
            vec![
                (
                    "turn/interrupt",
                    serde_json::json!({"threadId":"child-refused", "turnId":"refused-turn"})
                ),
                (
                    "turn/interrupt",
                    serde_json::json!({"threadId":"child-waiting", "turnId":"waiting-turn"})
                ),
                (
                    "turn/interrupt",
                    serde_json::json!({"threadId":"main", "turnId":"main-turn"})
                ),
            ]
        );
        let idle = request_turn_interruptions(&HashMap::new(), Duration::ZERO, |method, params| {
            calls.lock().unwrap().push((method, params));
            std::future::ready(Ok(serde_json::json!({})))
        })
        .await;
        assert!(idle.is_empty());
        assert_eq!(calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn old_or_equal_selected_version_is_not_newer_than_installed() {
        assert_eq!(version_is_newer("v0.1.11", "0.1.12"), Some(false));
        assert_eq!(version_is_newer("0.1.12", "0.1.12"), Some(false));
        assert_eq!(version_is_newer("0.1.13", "0.1.12"), Some(true));
        assert_eq!(version_is_newer("unknown", "0.1.12"), None);
    }

    #[test]
    fn activity_requires_full_idle_window_and_never_applies_over_a_handler() {
        assert!(!idle_activity(1, IDLE_DELAY, false));
        assert!(!idle_activity(
            0,
            IDLE_DELAY - Duration::from_secs(1),
            false
        ));
        assert!(!idle_activity(0, IDLE_DELAY, true));
        assert!(idle_activity(0, IDLE_DELAY, false));
    }
    #[test]
    fn explicit_update_claims_transition_even_with_active_handlers() {
        assert!(ready_activity(0, Duration::ZERO, false, true));
        assert!(!ready_activity(0, Duration::ZERO, false, false));
        assert!(!ready_activity(1, Duration::ZERO, false, true));
        assert!(!ready_activity(0, Duration::ZERO, true, true));
        let dir = tempfile::tempdir().unwrap();
        let controller = Arc::new(
            UpdateController::new(dir.path().to_owned(), dir.path().join("config.json")).unwrap(),
        );
        let handler = controller.activity();
        assert!(controller.claim_apply(true));
        controller.release_apply();
        drop(handler);
        assert!(!controller.claim_apply(false));
        assert!(controller.claim_apply(true));
        assert!(controller.applying());
        assert!(!controller.claim_apply(true));
    }

    #[test]
    fn new_voice_activity_is_rejected_after_update_gate_closes() {
        let dir = tempfile::tempdir().unwrap();
        let controller = Arc::new(
            UpdateController::new(dir.path().to_owned(), dir.path().join("config.json")).unwrap(),
        );
        assert!(controller.try_activity().is_some());
        assert!(controller.claim_apply(true));
        assert!(controller.try_activity().is_none());
        controller.release_apply();
        assert!(controller.try_activity().is_some());
    }

    #[test]
    fn explicit_update_preserves_queued_input_without_waiting_for_submission() {
        assert!(!delivery_blocks_update(QueueDelivery::Queued, true));
        assert!(delivery_blocks_update(QueueDelivery::Queued, false));
        for delivery in [QueueDelivery::Submitting, QueueDelivery::Unknown] {
            assert!(delivery_blocks_update(delivery, true));
        }
        assert!(!delivery_blocks_update(QueueDelivery::Accepted, true));
    }

    #[test]
    fn confirmation_localizes_update_cancel_without_selecting_update() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UpdateStore::load(dir.path()).unwrap();
        store
            .apply_check(updates::CheckResult::Modified {
                checked_at_unix: 100,
                etag: None,
                release: Some(release_notice(
                    9,
                    Some(&format!("sha256:{}", "a".repeat(64))),
                )),
            })
            .unwrap();
        let before = store.snapshot();
        let action = manual_update_action(&before);
        for (language, update, cancel) in [
            (Language::English, "Update", "Cancel"),
            (Language::Korean, "업데이트", "취소"),
            (Language::Chinese, "更新", "取消"),
            (Language::Japanese, "更新", "キャンセル"),
        ] {
            let (text, rows) = manual_update_response(&action, language, "choice");
            assert!(text.lines().count() >= 3);
            let json = serde_json::to_value(rows).unwrap();
            assert_eq!(json[0]["components"].as_array().unwrap().len(), 2);
            assert_eq!(json[0]["components"][0]["label"], update);
            assert_eq!(json[0]["components"][1]["label"], cancel);
            assert_eq!(
                json[0]["components"][0]["custom_id"],
                "discodex:update-confirm:choice:install"
            );
        }
        assert_eq!(store.snapshot(), before);
        assert_eq!(UpdateStore::load(dir.path()).unwrap().snapshot(), before);
    }

    #[test]
    fn only_matching_confirmed_release_starts_and_cancel_leaves_state_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UpdateStore::load(dir.path()).unwrap();
        store
            .apply_check(updates::CheckResult::Modified {
                checked_at_unix: 100,
                etag: None,
                release: Some(release_notice(
                    9,
                    Some(&format!("sha256:{}", "a".repeat(64))),
                )),
            })
            .unwrap();
        let ManualUpdateAction::Confirm(target) = manual_update_action(&store.snapshot()) else {
            panic!("expected confirmation")
        };
        let choice = UpdateConfirmation {
            requester_id: 12,
            target,
        };
        let before = store.snapshot();
        confirm_update(&mut store, &choice, 12, false).unwrap();
        assert_eq!(store.snapshot(), before);
        assert!(confirm_update(&mut store, &choice, 13, true).is_err());
        assert_eq!(store.snapshot(), before);
        confirm_update(&mut store, &choice, 12, true).unwrap();
        let selected = store.snapshot();
        assert!(immediate_requested(&selected));
        assert_eq!(selected.status, UpdateStatus::Selected);
        assert_eq!(selected.pending, Some(choice.target.clone()));
        confirm_update(&mut store, &choice, 12, true).unwrap();
        assert_eq!(store.snapshot(), selected);
        store.skip_release(9).unwrap();
        store
            .apply_check(updates::CheckResult::Modified {
                checked_at_unix: 200,
                etag: None,
                release: Some(release_notice(10, None)),
            })
            .unwrap();
        let moved_on = store.snapshot();
        assert!(confirm_update(&mut store, &choice, 12, true).is_err());
        assert_eq!(store.snapshot(), moved_on);
    }

    #[test]
    fn newer_release_replaces_unconfirmed_selection_but_never_confirmed_work() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UpdateStore::load(dir.path()).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        store
            .apply_check(updates::CheckResult::Modified {
                checked_at_unix: 100,
                etag: None,
                release: Some(release_notice(9, Some(&digest))),
            })
            .unwrap();
        let old = store.select_update(9).unwrap();
        let old_choice = UpdateConfirmation {
            requester_id: 12,
            target: old.clone(),
        };
        store
            .apply_check(updates::CheckResult::Modified {
                checked_at_unix: 200,
                etag: None,
                release: Some(release_notice(10, Some(&digest))),
            })
            .unwrap();
        assert!(confirm_update(&mut store, &old_choice, 12, true).is_err());
        assert_eq!(store.snapshot().pending, Some(old));
        let ManualUpdateAction::Confirm(new) = manual_update_action(&store.snapshot()) else {
            panic!("expected new release")
        };
        assert_eq!(new.release_id, 10);
        confirm_update(
            &mut store,
            &UpdateConfirmation {
                requester_id: 12,
                target: new.clone(),
            },
            12,
            true,
        )
        .unwrap();
        store
            .apply_check(updates::CheckResult::Modified {
                checked_at_unix: 300,
                etag: None,
                release: Some(release_notice(11, Some(&digest))),
            })
            .unwrap();
        assert_eq!(
            manual_update_action(&store.snapshot()),
            ManualUpdateAction::Active(new)
        );
    }

    #[test]
    fn result_is_new_message_even_when_muted_without_notice_and_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UpdateStore::load(dir.path()).unwrap();
        store
            .apply_check(updates::CheckResult::Modified {
                checked_at_unix: 100,
                etag: None,
                release: Some(release_notice(9, None)),
            })
            .unwrap();
        store.set_muted(true).unwrap();
        store.request_immediate(9).unwrap();
        assert!(result_message(&store.snapshot(), Language::English, "commit-9").is_none());
        store.mark_applied(9).unwrap();
        let snapshot = UpdateStore::load(dir.path()).unwrap().snapshot();
        assert!(snapshot.message_id.is_none());
        assert!(snapshot.muted);
        assert!(result_message(&snapshot, Language::English, "old-commit").is_none());
        for language in [
            Language::English,
            Language::Korean,
            Language::Chinese,
            Language::Japanese,
        ] {
            let (_, text) = result_message(&snapshot, language, "commit-9").unwrap();
            assert_eq!(text.lines().count(), 2);
        }
        let (token, _) = result_message(&snapshot, Language::Korean, "commit-9").unwrap();
        store.record_result_notified(&token).unwrap();
        let restored = UpdateStore::load(dir.path()).unwrap().snapshot();
        assert!(result_message(&restored, Language::Korean, "commit-9").is_none());
    }

    #[test]
    fn failed_update_reports_once_and_retry_can_report_another_result() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UpdateStore::load(dir.path()).unwrap();
        store
            .apply_check(updates::CheckResult::Modified {
                checked_at_unix: 100,
                etag: None,
                release: Some(release_notice(9, None)),
            })
            .unwrap();
        store.request_immediate(9).unwrap();
        store
            .mark_apply_failed(9, "download failed".into())
            .unwrap();
        let (first, text) = result_message(&store.snapshot(), Language::English, "old").unwrap();
        assert!(text.contains("failed"));
        store.record_result_notified(&first).unwrap();
        assert!(result_message(&store.snapshot(), Language::English, "old").is_none());
        store.request_immediate(9).unwrap();
        store.mark_applied(9).unwrap();
        let (second, _) = result_message(&store.snapshot(), Language::English, "commit-9").unwrap();
        assert_ne!(first, second);
    }

    fn failed_result_snapshot(error: Option<String>) -> UpdateSnapshot {
        UpdateSnapshot {
            status: UpdateStatus::Failed,
            last_result: Some(pinned_release(9)),
            last_error: error,
            result_notification_id: Some("11111111-2222-4333-8444-555555555555".into()),
            ..UpdateSnapshot::default()
        }
    }

    #[test]
    fn failed_result_preserves_actual_rollback_outcome_in_every_language() {
        for error in [
            Some(
                "update failed; previous worker and service state were restored: candidate start failed",
            ),
            Some(
                "update failed (candidate start failed); rollback incomplete (profile runtime restore failed: read-only filesystem)",
            ),
            None,
        ] {
            let snapshot = failed_result_snapshot(error.map(str::to_owned));
            let before = snapshot.clone();
            for (language, status) in [
                (
                    Language::English,
                    "Update failed. Run /discodex update to retry.",
                ),
                (
                    Language::Korean,
                    "업데이트에 실패했습니다. /discodex update로 다시 시도할 수 있습니다.",
                ),
                (
                    Language::Chinese,
                    "更新失败。可运行 /discodex update 重试。",
                ),
                (
                    Language::Japanese,
                    "更新に失敗しました。/discodex update で再試行できます。",
                ),
            ] {
                let (token, content) = result_message(&snapshot, language, "old").unwrap();
                let detail = error
                    .map(|error| format!("\n\n{error}"))
                    .unwrap_or_default();
                assert_eq!(content, format!("**Discodex 9.0.0**\n{status}{detail}"));
                assert_eq!(Some(token), snapshot.result_notification_id);
                assert_eq!(snapshot, before);
            }
        }
    }

    #[tokio::test]
    async fn result_delivery_retries_stable_pages_and_completes_only_after_all_pages() {
        let snapshot = failed_result_snapshot(Some(format!(
            "update failed (candidate start failed); rollback incomplete:\n{}",
            "복원错误🙂\n".repeat(1_400),
        )));
        let before = snapshot.clone();
        let (token, content) = result_message(&snapshot, Language::English, "old").unwrap();
        let mut partial = Vec::new();
        let failure = deliver_result_with(&snapshot, Language::English, "old", |message| {
            partial.push(serde_json::to_value(message).unwrap());
            std::future::ready(if partial.len() == 2 {
                Err(anyhow::anyhow!("second page delivery failed"))
            } else {
                Ok(())
            })
        })
        .await
        .unwrap_err();
        assert_eq!(failure.to_string(), "second page delivery failed");
        assert_eq!(partial.len(), 2);
        assert_eq!(snapshot, before);
        assert_eq!(snapshot.result_notified_id, None);

        let mut delivered = Vec::new();
        let complete = deliver_result_with(&snapshot, Language::English, "old", |message| {
            delivered.push(serde_json::to_value(message).unwrap());
            std::future::ready(Ok(()))
        })
        .await
        .unwrap();
        assert_eq!(complete.as_deref(), Some(token.as_str()));
        assert!(delivered.len() > 2);
        assert_eq!(&delivered[..partial.len()], partial.as_slice());
        let chunks = delivered
            .iter()
            .map(|page| page["content"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 2_000));
        assert_eq!(chunks.concat(), content);
        let nonces = delivered
            .iter()
            .map(|page| {
                assert_eq!(page["enforce_nonce"], true);
                let nonce = page["nonce"].as_str().unwrap();
                assert!(nonce.len() <= 25);
                nonce
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(nonces.len(), delivered.len());
        assert_eq!(
            delivered[0]["nonce"],
            token
                .chars()
                .filter(|c| *c != '-')
                .take(24)
                .collect::<String>()
        );
        for page in result_message_pages("66666666-7777-4888-8999-000000000000", &content) {
            let page = serde_json::to_value(page).unwrap();
            assert!(!nonces.contains(page["nonce"].as_str().unwrap()));
        }
        assert_eq!(snapshot, before);

        let mut notified = snapshot;
        notified.result_notified_id = complete;
        assert_eq!(
            deliver_result_with(&notified, Language::English, "old", |_| {
                std::future::ready(Err(anyhow::anyhow!("already notified")))
            })
            .await
            .unwrap(),
            None
        );
    }

    #[test]
    fn failed_check_retries_after_fifteen_minutes_across_reload() {
        let mut snapshot = UpdateSnapshot {
            last_checked_unix: Some(100),
            ..UpdateSnapshot::default()
        };
        assert!(!check_due(&snapshot, 100 + 15 * 60));
        assert!(check_due(&snapshot, 100 + 6 * 60 * 60));
        snapshot.last_check_error = Some("offline".into());
        let restored = serde_json::from_value(serde_json::to_value(snapshot).unwrap()).unwrap();
        assert!(!check_due(&restored, 100 + 15 * 60 - 1));
        assert!(check_due(&restored, 100 + 15 * 60));
    }

    #[test]
    fn unresolved_input_blocks_update_but_accepted_input_does_not() {
        for delivery in [
            QueueDelivery::Queued,
            QueueDelivery::Submitting,
            QueueDelivery::Unknown,
        ] {
            assert!(has_pending_delivery(delivery));
        }
        assert!(!has_pending_delivery(QueueDelivery::Accepted));
        assert!(!has_pending_delivery(QueueDelivery::Rejected));
    }
    fn pinned_release(release_id: u64) -> updates::PinnedRelease {
        updates::PinnedRelease {
            release_id,
            version: format!("{}.0.0", release_id),
            tag: format!("v{}.0.0", release_id),
            commit: format!("commit-{release_id}"),
            url: "https://example.invalid/release".into(),
            asset: None,
        }
    }

    fn release_notice(release_id: u64, digest: Option<&str>) -> updates::ReleaseNotice {
        updates::ReleaseNotice {
            release_id,
            version: format!("{}.0.0", release_id),
            tag: format!("v{}.0.0", release_id),
            commit: format!("commit-{release_id}"),
            url: "https://example.invalid/release".into(),
            asset: Some(updates::ReleaseAsset {
                name: "discodex-test".into(),
                download_url: "https://example.invalid/download".into(),
                digest: digest.map(str::to_owned),
                size: 1,
            }),
            body: String::new(),
            published_at: None,
        }
    }

    #[test]
    fn manual_update_preserves_active_target_and_retries_failed_target() {
        let valid_digest = format!("sha256:{}", "a".repeat(64));
        for status in [
            UpdateStatus::Selected,
            UpdateStatus::Preparing,
            UpdateStatus::Ready,
            UpdateStatus::Applying,
        ] {
            let snapshot = UpdateSnapshot {
                status,
                immediate_release_id: Some(7),
                pending: Some(pinned_release(7)),
                latest_release: Some(release_notice(8, Some(&valid_digest))),
                ..UpdateSnapshot::default()
            };
            assert_eq!(
                manual_update_action(&snapshot),
                ManualUpdateAction::Active(pinned_release(7))
            );
        }
        let active_without_target = UpdateSnapshot {
            status: UpdateStatus::Applying,
            ..UpdateSnapshot::default()
        };
        assert_eq!(
            manual_update_action(&active_without_target),
            ManualUpdateAction::ActiveWithoutTarget
        );
        let failed = UpdateSnapshot {
            status: UpdateStatus::Failed,
            pending: Some(pinned_release(7)),
            latest_release: None,
            ..UpdateSnapshot::default()
        };
        assert_eq!(
            manual_update_action(&failed),
            ManualUpdateAction::Confirm(pinned_release(7))
        );
    }

    #[test]
    fn newer_latest_release_takes_precedence_over_failed_pinned_target() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let failed = UpdateSnapshot {
            status: UpdateStatus::Failed,
            pending: Some(pinned_release(7)),
            latest_release: Some(release_notice(8, Some(&digest))),
            ..UpdateSnapshot::default()
        };
        let action = manual_update_action(&failed);
        let ManualUpdateAction::Confirm(target) = &action else {
            panic!("expected latest release confirmation")
        };
        assert_eq!(target.release_id, 8);
        assert_eq!(target.version, "8.0.0");
        let (text, buttons) = manual_update_response(&action, Language::English, "latest");
        assert!(text.starts_with("**Discodex 8.0.0**"));
        assert!(text.contains("Update now?"));
        let components = serde_json::to_value(buttons).unwrap();
        assert_eq!(components[0]["components"].as_array().unwrap().len(), 2);

        let retry_same = UpdateSnapshot {
            status: UpdateStatus::Failed,
            pending: Some(pinned_release(7)),
            latest_release: Some(release_notice(7, Some(&digest))),
            ..UpdateSnapshot::default()
        };
        assert_eq!(
            manual_update_action(&retry_same),
            ManualUpdateAction::Confirm(pinned_release(7))
        );
    }

    #[test]
    fn no_published_release_state_selects_distinct_manual_response_action() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UpdateStore::load(dir.path()).unwrap();
        store
            .apply_check(updates::CheckResult::NoPublishedRelease {
                checked_at_unix: 100,
                etag: None,
            })
            .unwrap();
        let reloaded = UpdateStore::load(dir.path()).unwrap().snapshot();
        assert!(reloaded.no_published_release);
        assert_eq!(
            manual_update_action(&reloaded),
            ManualUpdateAction::NoPublishedRelease
        );
    }

    #[test]
    fn applied_release_is_not_requeued_after_not_modified_check() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = UpdateStore::load(dir.path()).unwrap();
        let notice = release_notice(11, Some(&format!("sha256:{}", "b".repeat(64))));
        store
            .apply_check(updates::CheckResult::Modified {
                checked_at_unix: 100,
                etag: Some("release-tag".into()),
                release: Some(notice),
            })
            .unwrap();
        store.select_update(11).unwrap();
        store.mark_applied(11).unwrap();
        store
            .apply_check(updates::CheckResult::NotModified {
                checked_at_unix: 200,
                etag: Some("release-tag".into()),
            })
            .unwrap();
        let reopened = UpdateStore::load(dir.path()).unwrap().snapshot();
        assert_eq!(reopened.last_applied_release_id, Some(11));
        assert_eq!(
            manual_update_action(&reopened),
            ManualUpdateAction::NoUpdate
        );
    }

    #[test]
    fn manual_update_offers_only_latest_release_with_verified_prebuilt_digest() {
        let valid_digest = format!("sha256:{}", "A".repeat(64));
        let snapshot = UpdateSnapshot {
            latest_release: Some(release_notice(9, Some(&valid_digest))),
            muted: true,
            skipped_release_id: Some(9),
            ..UpdateSnapshot::default()
        };
        assert_eq!(
            manual_update_action(&snapshot),
            ManualUpdateAction::Confirm(updates::PinnedRelease {
                release_id: 9,
                version: "9.0.0".into(),
                tag: "v9.0.0".into(),
                commit: "commit-9".into(),
                url: "https://example.invalid/release".into(),
                asset: snapshot.latest_release.as_ref().unwrap().asset.clone(),
            })
        );
        for digest in [None, Some("sha1:bad"), Some("sha256:bad")] {
            let snapshot = UpdateSnapshot {
                latest_release: Some(release_notice(9, digest)),
                ..UpdateSnapshot::default()
            };
            assert_eq!(
                manual_update_action(&snapshot),
                ManualUpdateAction::MissingVerifiedAsset
            );
        }
        assert_eq!(
            manual_update_action(&UpdateSnapshot::default()),
            ManualUpdateAction::NoUpdate
        );
    }

    #[test]
    fn update_card_has_three_localized_actions_and_no_mentions() {
        let mut snapshot = UpdateSnapshot::default();
        snapshot.latest_release = Some(updates::ReleaseNotice {
            release_id: 42,
            version: "v2.0.0".into(),
            tag: "v2.0.0".into(),
            commit: "abc".into(),
            url: "https://example.invalid/release".into(),
            asset: None,
            body: String::new(),
            published_at: None,
        });
        for (lang, expected) in [
            (Language::English, "Update"),
            (Language::Korean, "업데이트"),
            (Language::Chinese, "更新"),
            (Language::Japanese, "更新"),
        ] {
            let (_, content, rows) = render(&snapshot, lang).unwrap();
            assert!(content.contains("v2.0.0"));
            let json = serde_json::to_value(rows).unwrap();
            assert_eq!(json[0]["components"].as_array().unwrap().len(), 3);
            assert_eq!(json[0]["components"][0]["label"], expected);
            assert_eq!(
                json[0]["components"][0]["custom_id"],
                "discodex:update:42:install"
            );
        }
        snapshot.muted = true;
        assert!(render(&snapshot, Language::English).unwrap().2.is_empty());
    }
    #[test]
    fn later_release_does_not_inherit_prior_success_or_failure() {
        let previous = updates::PinnedRelease {
            release_id: 1,
            version: "1.0.0".into(),
            tag: "v1.0.0".into(),
            commit: "old".into(),
            url: "https://example.invalid/1".into(),
            asset: None,
        };
        let mut snapshot = UpdateSnapshot::default();
        snapshot.last_result = Some(previous.clone());
        snapshot.last_applied_release_id = Some(1);
        snapshot.notified_release_id = Some(1);
        snapshot.latest_release = Some(updates::ReleaseNotice {
            release_id: 2,
            version: "2.0.0".into(),
            tag: "v2.0.0".into(),
            commit: "new".into(),
            url: "https://example.invalid/2".into(),
            asset: None,
            body: String::new(),
            published_at: None,
        });
        for status in [UpdateStatus::Applied, UpdateStatus::Failed] {
            snapshot.status = status;
            let (id, _, rows) = render(&snapshot, Language::English).unwrap();
            assert_eq!(id, 2);
            assert_eq!(status_for(&snapshot, 2), UpdateStatus::Idle);
            assert_eq!(rows.len(), 1);
        }
        snapshot.pending = Some(previous);
        snapshot.status = UpdateStatus::Ready;
        assert_eq!(render(&snapshot, Language::English).unwrap().0, 1);
        assert_eq!(status_for(&snapshot, 1), UpdateStatus::Ready);
    }

    #[test]
    fn ambiguous_notice_recovery_matches_exact_release_component() {
        let components = serde_json::json!([{"type":1,"components":[{"type":2,"custom_id":"discodex:update:17:install"}]}]);
        assert!(carries_notice(&components, 17));
        assert!(!carries_notice(&components, 1));
        assert!(!carries_notice(&serde_json::json!([]), 17));
    }

    #[test]
    fn ignored_ingress_does_not_reset_idle_but_accepted_input_does() {
        let dir = tempfile::tempdir().unwrap();
        let controller = Arc::new(
            UpdateController::new(dir.path().to_owned(), dir.path().join("config.json")).unwrap(),
        );
        let sentinel = Instant::now() + IDLE_DELAY;
        controller.activity.lock().unwrap().last = sentinel;

        let ignored = controller.admission();
        assert_eq!(controller.activity.lock().unwrap().active, 1);
        drop(ignored);
        let after_ignored = controller.activity.lock().unwrap();
        assert_eq!(after_ignored.active, 0);
        assert_eq!(after_ignored.last, sentinel);
        drop(after_ignored);

        let before_accept = Instant::now();
        let mut accepted = controller.admission();
        assert_eq!(controller.activity.lock().unwrap().active, 1);
        accepted.mark_activity();
        let marked_at = controller.activity.lock().unwrap().last;
        let after_accept = Instant::now();
        assert!(marked_at >= before_accept && marked_at <= after_accept);
        drop(accepted);
        let after_drop_at = Instant::now();
        let after_drop = controller.activity.lock().unwrap();
        assert_eq!(after_drop.active, 0);
        assert!(after_drop.last >= before_accept && after_drop.last <= after_drop_at);
        assert_ne!(after_drop.last, sentinel);
    }

    #[tokio::test]
    async fn activity_guard_finishes_before_shutdown_drain() {
        let dir = tempfile::tempdir().unwrap();
        let controller = Arc::new(
            UpdateController::new(dir.path().to_owned(), dir.path().join("config.json")).unwrap(),
        );
        let activity = controller.activity();
        assert!(!controller.claim_apply(false));
        assert!(
            tokio::time::timeout(Duration::from_millis(5), controller.wait_for_handlers())
                .await
                .is_err()
        );
        drop(activity);
        controller.wait_for_handlers().await;
        assert_eq!(controller.activity.lock().unwrap().active, 0);
    }
}
