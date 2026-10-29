//! Application-level ownership for asynchronous native thread-setting changes.

use std::future::Future;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use tokio::sync::watch;

use crate::{
    model_catalog::{ModelExecutor, ModelSelection},
    native::{Native, ThreadSettingRevision, ThreadSettingUpdate, ThreadSettingUpdateOutcome},
};

#[derive(Clone)]
pub(crate) struct SettingsOperations {
    active: Arc<Mutex<HashMap<OperationSlot, RegisteredOperation>>>,
    language: crate::i18n::Language,
}

impl SettingsOperations {
    pub(crate) fn new(language: crate::i18n::Language) -> Self {
        Self {
            active: Arc::default(),
            language,
        }
    }
}

impl Default for SettingsOperations {
    fn default() -> Self {
        Self::new(crate::i18n::Language::English)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct OperationKey {
    generation: u64,
    thread_id: String,
    field: &'static str,
    params: String,
    expected: String,
    expected_model_scope: Option<String>,
    model_selection: Option<String>,
    current_turn_params: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct OperationSlot {
    thread_id: String,
    field: &'static str,
}

struct RegisteredOperation {
    key: OperationKey,
    sender: watch::Sender<Option<Arc<SettingOutcome>>>,
}

pub(crate) struct SettingRequest {
    pub generation: u64,
    pub thread_id: String,
    pub field: &'static str,
    pub params: Value,
    pub expected: Value,
    pub expected_model_scope: Option<Option<String>>,
    pub model_selection: Option<ModelSelection>,
    pub current_turn_params: Option<Value>,
}

pub(crate) enum ApplicationOutcome {
    Applied(Value),
    Superseded,
    StaleTarget,
    Failed(anyhow::Error),
}

pub(crate) enum PersistenceOutcome {
    NotNeeded,
    Saved,
    Failed(anyhow::Error),
}

pub(crate) enum CurrentTurnOutcome {
    NotRequested,
    Applied(Value),
    Failed(anyhow::Error),
    Unconfirmed(Value),
}

pub(crate) struct SettingOutcome {
    pub application: ApplicationOutcome,
    pub persistence: PersistenceOutcome,
    pub current_turn: CurrentTurnOutcome,
    pub target_current: bool,
    pub superseded: bool,
}

#[derive(Debug)]
enum ConfirmationResult<R> {
    Applied { snapshot: Value, revision: R },
    Superseded,
}

impl SettingsOperations {
    pub(crate) async fn apply_setting(
        &self,
        native: &Native,
        request: SettingRequest,
    ) -> Result<Arc<SettingOutcome>> {
        self.apply_setting_with_dispatch(native, request, None)
            .await
    }

    pub(crate) async fn apply_setting_with_dispatch(
        &self,
        native: &Native,
        request: SettingRequest,
        dispatched: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<Arc<SettingOutcome>> {
        let key = operation_key(&request);
        let native = native.clone();
        let language = self.language;
        self.run_shared(key, async move {
            execute_once(&native, request, language, dispatched).await
        })
        .await
    }

    async fn run_shared<F>(&self, key: OperationKey, operation: F) -> Result<Arc<SettingOutcome>>
    where
        F: Future<Output = SettingOutcome> + Send + 'static,
    {
        let (mut receiver, owner) = {
            let mut active = self.active.lock().map_err(|_| {
                anyhow!(
                    "{}",
                    self.language
                        .message(crate::i18n::MessageKey::SettingsOperationRegistryLockPoisoned)
                )
            })?;
            let slot = operation_slot(&key);
            // Only the current request for a thread+field can be joined. A
            // distinct request replaces its slot, so A→B→A starts fresh A.
            if let Some(registered) = active.get(&slot).filter(|registered| registered.key == key) {
                (registered.sender.subscribe(), None)
            } else {
                let (sender, receiver) = watch::channel(None);
                active.insert(
                    slot.clone(),
                    RegisteredOperation {
                        key: key.clone(),
                        sender: sender.clone(),
                    },
                );
                (receiver, Some((sender, slot)))
            }
        };

        let language = self.language;
        if let Some((sender, slot)) = owner {
            let active = self.active.clone();
            tokio::spawn(async move {
                let mut completion = OperationCompletionGuard {
                    active,
                    slot,
                    sender: sender.clone(),
                    language,
                };
                let outcome = Arc::new(operation.await);
                completion.complete(outcome);
            });
        }

        loop {
            if let Some(outcome) = receiver.borrow().clone() {
                return Ok(outcome);
            }
            receiver.changed().await.context(
                self.language
                    .message(crate::i18n::MessageKey::SettingsOperationResultMissing),
            )?;
        }
    }
}

struct OperationCompletionGuard {
    active: Arc<Mutex<HashMap<OperationSlot, RegisteredOperation>>>,
    slot: OperationSlot,
    sender: watch::Sender<Option<Arc<SettingOutcome>>>,
    language: crate::i18n::Language,
}

impl OperationCompletionGuard {
    fn complete(&mut self, outcome: Arc<SettingOutcome>) {
        self.remove_current_slot();
        self.sender.send_replace(Some(outcome));
    }

    fn remove_current_slot(&self) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active
            .get(&self.slot)
            .is_some_and(|registered| registered.sender.same_channel(&self.sender))
        {
            active.remove(&self.slot);
        }
    }
}

impl Drop for OperationCompletionGuard {
    fn drop(&mut self) {
        let fallback = self.sender.borrow().is_none().then(|| {
            Arc::new(SettingOutcome {
                application: ApplicationOutcome::Failed(anyhow!(
                    "{}",
                    self.language
                        .message(crate::i18n::MessageKey::SettingsOperationWorkerExited)
                )),
                persistence: PersistenceOutcome::NotNeeded,
                current_turn: CurrentTurnOutcome::NotRequested,
                target_current: false,
                superseded: false,
            })
        });
        self.remove_current_slot();
        if let Some(outcome) = fallback {
            self.sender.send_replace(Some(outcome));
        }
    }
}

fn operation_key(request: &SettingRequest) -> OperationKey {
    OperationKey {
        generation: request.generation,
        thread_id: request.thread_id.clone(),
        field: request.field,
        params: request.params.to_string(),
        expected: request.expected.to_string(),
        expected_model_scope: request
            .expected_model_scope
            .as_ref()
            .map(|scope| scope.as_deref().unwrap_or("<none>").to_owned()),
        model_selection: request.model_selection.as_ref().map(ModelSelection::value),
        current_turn_params: request.current_turn_params.as_ref().map(Value::to_string),
    }
}

fn operation_slot(key: &OperationKey) -> OperationSlot {
    OperationSlot {
        thread_id: key.thread_id.clone(),
        field: key.field,
    }
}

async fn execute_once(
    native: &Native,
    request: SettingRequest,
    language: crate::i18n::Language,
    dispatched: Option<tokio::sync::oneshot::Sender<()>>,
) -> SettingOutcome {
    if !target_is_current(native, &request) {
        return stale_target();
    }
    if let Some(expected_scope) = request.expected_model_scope.as_ref() {
        let snapshot = match native.refresh_thread_state().await {
            Ok(snapshot) => snapshot,
            Err(error) => return failed(error, CurrentTurnOutcome::NotRequested),
        };
        if !target_is_current(native, &request) || model_scope(&snapshot) != *expected_scope {
            return stale_target();
        }
    }

    let update = match native
        .request_thread_setting_update(
            &request.thread_id,
            request.params.clone(),
            request.field,
            request.expected.clone(),
        )
        .await
    {
        Ok(update) => update,
        Err(error) => return failed(error, CurrentTurnOutcome::NotRequested),
    };

    let current_turn = match request.current_turn_params.clone() {
        Some(params) => current_turn_result(native.request("turn/settings/update", params).await),
        None => CurrentTurnOutcome::NotRequested,
    };
    if let Some(dispatched) = dispatched {
        let _ = dispatched.send(());
    }

    let confirmation = async move {
        match update {
            ThreadSettingUpdate::Confirmed { snapshot, revision } => {
                Ok(ConfirmationResult::Applied { snapshot, revision })
            }
            ThreadSettingUpdate::Pending(mut ticket) => {
                let revision = ticket.revision();
                match ticket.wait().await? {
                    ThreadSettingUpdateOutcome::Applied(snapshot) => {
                        Ok(ConfirmationResult::Applied { snapshot, revision })
                    }
                    ThreadSettingUpdateOutcome::Superseded => Ok(ConfirmationResult::Superseded),
                }
            }
        }
    };
    let persist = request.model_selection.clone().map(|selection| {
        let native = native.clone();
        let generation = request.generation;
        let thread_id = request.thread_id.clone();
        move |revision| async move {
            native
                .save_model_selection_if_setting_current(
                    &revision, generation, &thread_id, &selection,
                )
                .await
        }
    });
    resolve_confirmation(
        confirmation,
        current_turn,
        || target_is_current(native, &request),
        |revision| native.thread_setting_revision_is_current(revision),
        persist,
        language,
    )
    .await
}

async fn resolve_confirmation<C, T, R, I, P, PF>(
    confirmation: C,
    current_turn: CurrentTurnOutcome,
    mut target_is_current: T,
    mut revision_is_current: I,
    mut persist: Option<P>,
    language: crate::i18n::Language,
) -> SettingOutcome
where
    C: Future<Output = Result<ConfirmationResult<R>>>,
    T: FnMut() -> bool,
    R: Clone,
    I: FnMut(&R) -> bool,
    P: FnOnce(R) -> PF,
    PF: Future<Output = Result<bool>>,
{
    match confirmation.await {
        Err(error) => SettingOutcome {
            application: ApplicationOutcome::Failed(error),
            persistence: PersistenceOutcome::NotNeeded,
            current_turn,
            target_current: target_is_current(),
            superseded: false,
        },
        Ok(ConfirmationResult::Superseded) => SettingOutcome {
            application: ApplicationOutcome::Superseded,
            persistence: PersistenceOutcome::NotNeeded,
            current_turn,
            target_current: target_is_current(),
            superseded: true,
        },
        Ok(ConfirmationResult::Applied { snapshot, revision }) => {
            let mut target_current = target_is_current();
            let mut superseded = !revision_is_current(&revision);
            let persistence = if !superseded && target_current {
                match persist.take() {
                    Some(persist) => match persist(revision.clone()).await.context(
                        language
                            .message(crate::i18n::MessageKey::SettingsOperationPersistenceFailed),
                    ) {
                        Ok(true) => PersistenceOutcome::Saved,
                        Ok(false) => {
                            superseded |= !revision_is_current(&revision);
                            target_current = target_is_current();
                            PersistenceOutcome::NotNeeded
                        }
                        Err(error) => PersistenceOutcome::Failed(error),
                    },
                    None => PersistenceOutcome::NotNeeded,
                }
            } else {
                PersistenceOutcome::NotNeeded
            };
            superseded |= !revision_is_current(&revision);
            SettingOutcome {
                application: ApplicationOutcome::Applied(snapshot),
                persistence,
                current_turn,
                target_current,
                superseded,
            }
        }
    }
}

fn stale_target() -> SettingOutcome {
    SettingOutcome {
        application: ApplicationOutcome::StaleTarget,
        persistence: PersistenceOutcome::NotNeeded,
        current_turn: CurrentTurnOutcome::NotRequested,
        target_current: false,
        superseded: false,
    }
}

fn failed(error: anyhow::Error, current_turn: CurrentTurnOutcome) -> SettingOutcome {
    SettingOutcome {
        application: ApplicationOutcome::Failed(error),
        persistence: PersistenceOutcome::NotNeeded,
        current_turn,
        target_current: false,
        superseded: false,
    }
}

fn current_turn_result(result: Result<Value>) -> CurrentTurnOutcome {
    match result {
        Ok(response) if response.get("status").and_then(Value::as_str) == Some("applied") => {
            CurrentTurnOutcome::Applied(response)
        }
        Ok(response) => CurrentTurnOutcome::Unconfirmed(response),
        Err(error) => CurrentTurnOutcome::Failed(error),
    }
}

fn target_is_current(native: &Native, request: &SettingRequest) -> bool {
    native.current_generation() == request.generation && native.thread_id() == request.thread_id
}

pub(crate) fn model_scope(thread: &Value) -> Option<String> {
    let model = thread.get("model")?.as_str()?;
    let executor = thread
        .get("modelExecutor")
        .cloned()
        .and_then(|value| serde_json::from_value::<ModelExecutor>(value).ok())
        .unwrap_or_default();
    let provider = if executor == ModelExecutor::Antigravity {
        Some("antigravity")
    } else {
        thread.get("modelProvider").and_then(Value::as_str)
    };
    Some(match provider {
        Some(provider) => ModelSelection {
            executor,
            provider: provider.to_owned(),
            model: model.to_owned(),
        }
        .value(),
        None => model.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn identical_pending_request_executes_once_and_shares_terminal_outcome() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let operations = SettingsOperations::default();
        let request = SettingRequest {
            generation: 4,
            thread_id: "thread-a".to_owned(),
            field: "effort",
            params: serde_json::json!({"threadId":"thread-a", "effort":"high"}),
            expected: Value::String("high".to_owned()),
            expected_model_scope: None,
            model_selection: None,
            current_turn_params: None,
        };
        let key = operation_key(&request);
        let effects = Arc::new(AtomicUsize::new(0));
        let current_turn_effects = Arc::new(AtomicUsize::new(0));
        let persistence_effects = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let first_operations = operations.clone();
        let first_effects = effects.clone();
        let first_turn_effects = current_turn_effects.clone();
        let first_persistence_effects = persistence_effects.clone();
        let first_release = release.clone();
        let first = tokio::spawn(async move {
            first_operations
                .run_shared(key.clone(), async move {
                    first_effects.fetch_add(1, Ordering::SeqCst);
                    first_release.notified().await;
                    first_turn_effects.fetch_add(1, Ordering::SeqCst);
                    first_persistence_effects.fetch_add(1, Ordering::SeqCst);
                    SettingOutcome {
                        application: ApplicationOutcome::Applied(
                            serde_json::json!({"effort":"high"}),
                        ),
                        persistence: PersistenceOutcome::Saved,
                        current_turn: CurrentTurnOutcome::Applied(
                            serde_json::json!({"status":"applied"}),
                        ),
                        target_current: true,
                        superseded: false,
                    }
                })
                .await
                .unwrap()
        });
        tokio::task::yield_now().await;
        let second_operations = operations.clone();
        let second_effects = effects.clone();
        let second_key = operation_key(&request);
        let second = tokio::spawn(async move {
            second_operations
                .run_shared(second_key, async move {
                    second_effects.fetch_add(1, Ordering::SeqCst);
                    applied_outcome()
                })
                .await
                .unwrap()
        });
        tokio::task::yield_now().await;
        release.notify_one();
        let first = first.await.unwrap();
        let second = second.await.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(current_turn_effects.load(Ordering::SeqCst), 1);
        assert_eq!(persistence_effects.load(Ordering::SeqCst), 1);
        assert!(
            matches!(&first.application, ApplicationOutcome::Applied(snapshot) if snapshot["effort"] == "high")
        );
    }

    #[tokio::test]
    async fn model_effort_and_service_tier_do_not_share_an_operation() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let operations = SettingsOperations::default();
        let model = SettingRequest {
            generation: 4,
            thread_id: "thread-a".to_owned(),
            field: "model",
            params: serde_json::json!({"threadId":"thread-a", "model":"m1"}),
            expected: Value::String("m1".to_owned()),
            expected_model_scope: None,
            model_selection: None,
            current_turn_params: None,
        };
        let model_key = operation_key(&model);
        let effort = SettingRequest {
            field: "effort",
            params: serde_json::json!({"threadId":"thread-a", "effort":"high"}),
            expected: Value::String("high".to_owned()),
            ..model
        };
        let service_tier = request_for("serviceTier", "priority");
        let model_effects = Arc::new(AtomicUsize::new(0));
        let effort_effects = Arc::new(AtomicUsize::new(0));
        let tier_effects = Arc::new(AtomicUsize::new(0));
        let (effort_started_tx, effort_started_rx) = tokio::sync::oneshot::channel();
        let (tier_started_tx, tier_started_rx) = tokio::sync::oneshot::channel();
        let (release_effort_tx, release_effort_rx) = tokio::sync::oneshot::channel();
        let first_ops = operations.clone();
        let first_effect = model_effects.clone();
        let first = tokio::spawn(async move {
            first_ops
                .run_shared(model_key, async move {
                    first_effect.fetch_add(1, Ordering::SeqCst);
                    SettingOutcome {
                        application: ApplicationOutcome::Applied(serde_json::json!({"model":"m1"})),
                        persistence: PersistenceOutcome::Saved,
                        current_turn: CurrentTurnOutcome::Applied(
                            serde_json::json!({"field":"model"}),
                        ),
                        target_current: true,
                        superseded: false,
                    }
                })
                .await
                .unwrap()
        });
        let second_ops = operations.clone();
        let second_effect = effort_effects.clone();
        let second = tokio::spawn(async move {
            second_ops
                .run_shared(operation_key(&effort), async move {
                    second_effect.fetch_add(1, Ordering::SeqCst);
                    let _ = effort_started_tx.send(());
                    let _ = release_effort_rx.await;
                    SettingOutcome {
                        application: ApplicationOutcome::Applied(
                            serde_json::json!({"effort":"high"}),
                        ),
                        persistence: PersistenceOutcome::NotNeeded,
                        current_turn: CurrentTurnOutcome::Applied(
                            serde_json::json!({"field":"effort"}),
                        ),
                        target_current: true,
                        superseded: false,
                    }
                })
                .await
                .unwrap()
        });
        let third_ops = operations.clone();
        let third_effect = tier_effects.clone();
        let third = tokio::spawn(async move {
            third_ops
                .run_shared(operation_key(&service_tier), async move {
                    third_effect.fetch_add(1, Ordering::SeqCst);
                    let _ = tier_started_tx.send(());
                    SettingOutcome {
                        application: ApplicationOutcome::Applied(
                            serde_json::json!({"serviceTier":"priority"}),
                        ),
                        persistence: PersistenceOutcome::NotNeeded,
                        current_turn: CurrentTurnOutcome::Applied(
                            serde_json::json!({"field":"serviceTier"}),
                        ),
                        target_current: true,
                        superseded: false,
                    }
                })
                .await
                .unwrap()
        });
        effort_started_rx.await.unwrap();
        tier_started_rx.await.unwrap();

        let model = first.await.unwrap();
        let tier = third.await.unwrap();
        assert!(matches!(
            &model.application,
            ApplicationOutcome::Applied(value) if value["model"] == "m1"
        ));
        assert!(matches!(
            &tier.application,
            ApplicationOutcome::Applied(value) if value["serviceTier"] == "priority"
        ));
        assert!(matches!(
            &tier.current_turn,
            CurrentTurnOutcome::Applied(value) if value["field"] == "serviceTier"
        ));
        assert_eq!(model_effects.load(Ordering::SeqCst), 1);
        assert_eq!(tier_effects.load(Ordering::SeqCst), 1);
        assert_eq!(effort_effects.load(Ordering::SeqCst), 1);

        release_effort_tx.send(()).unwrap();
        let effort = second.await.unwrap();
        assert!(matches!(
            &effort.application,
            ApplicationOutcome::Applied(value) if value["effort"] == "high"
        ));
        assert!(matches!(
            &effort.current_turn,
            CurrentTurnOutcome::Applied(value) if value["field"] == "effort"
        ));
        assert_eq!(effort_effects.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dropped_waiter_does_not_cancel_worker_or_block_later_observer() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let operations = SettingsOperations::default();
        let request = request_for("effort", "high");
        let key = operation_key(&request);
        let effects = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let owner_ops = operations.clone();
        let owner_effects = effects.clone();
        let owner_release = release.clone();
        let owner = tokio::spawn(async move {
            owner_ops
                .run_shared(key, async move {
                    owner_effects.fetch_add(1, Ordering::SeqCst);
                    let _ = started_tx.send(());
                    owner_release.notified().await;
                    applied_outcome()
                })
                .await
        });
        started_rx.await.unwrap();
        owner.abort();

        let observer_ops = operations.clone();
        let observer_effects = effects.clone();
        let observer_key = operation_key(&request);
        let observer = tokio::spawn(async move {
            observer_ops
                .run_shared(observer_key, async move {
                    observer_effects.fetch_add(1, Ordering::SeqCst);
                    applied_outcome()
                })
                .await
                .unwrap()
        });
        tokio::task::yield_now().await;
        release.notify_one();
        let outcome = observer.await.unwrap();
        assert!(matches!(
            &outcome.application,
            ApplicationOutcome::Applied(_)
        ));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_b_a_same_field_runs_the_latest_a_after_b_replaces_a() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let operations = SettingsOperations::default();
        let a = request_for("effort", "high");
        let b = request_for("effort", "low");
        let release_a = Arc::new(tokio::sync::Notify::new());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let a_effects = Arc::new(AtomicUsize::new(0));
        let first_ops = operations.clone();
        let first_release = release_a.clone();
        let first_count = a_effects.clone();
        let first = tokio::spawn(async move {
            first_ops
                .run_shared(operation_key(&a), async move {
                    first_count.fetch_add(1, Ordering::SeqCst);
                    let _ = started_tx.send(());
                    first_release.notified().await;
                    applied_outcome()
                })
                .await
                .unwrap()
        });
        started_rx.await.unwrap();

        let b_ops = operations.clone();
        let b_task = tokio::spawn(async move {
            b_ops
                .run_shared(operation_key(&b), async { applied_outcome() })
                .await
                .unwrap()
        });
        let _ = b_task.await.unwrap();

        let latest_a_ops = operations.clone();
        let latest_a_count = a_effects.clone();
        let latest_a = tokio::spawn(async move {
            latest_a_ops
                .run_shared(operation_key(&request_for("effort", "high")), async move {
                    latest_a_count.fetch_add(1, Ordering::SeqCst);
                    applied_outcome()
                })
                .await
                .unwrap()
        });
        tokio::task::yield_now().await;
        release_a.notify_one();
        let _ = first.await.unwrap();
        let _ = latest_a.await.unwrap();
        assert_eq!(a_effects.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failed_request_can_be_retried_with_the_same_input() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let operations = SettingsOperations::default();
        let key = operation_key(&request_for("effort", "high"));
        let attempts = Arc::new(AtomicUsize::new(0));
        let first_attempts = attempts.clone();
        let first = operations
            .run_shared(key.clone(), async move {
                first_attempts.fetch_add(1, Ordering::SeqCst);
                SettingOutcome {
                    application: ApplicationOutcome::Failed(anyhow!("native response rejected")),
                    persistence: PersistenceOutcome::NotNeeded,
                    current_turn: CurrentTurnOutcome::NotRequested,
                    target_current: true,
                    superseded: false,
                }
            })
            .await
            .unwrap();
        assert!(matches!(
            &first.application,
            ApplicationOutcome::Failed(error) if error.to_string() == "native response rejected"
        ));

        let retry_attempts = attempts.clone();
        let retry = operations
            .run_shared(key, async move {
                retry_attempts.fetch_add(1, Ordering::SeqCst);
                applied_outcome()
            })
            .await
            .unwrap();
        assert!(matches!(&retry.application, ApplicationOutcome::Applied(_)));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn completed_request_is_reconfirmed_on_a_later_same_input() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let operations = SettingsOperations::default();
        let key = operation_key(&request_for("effort", "high"));
        let confirmations = Arc::new(AtomicUsize::new(0));
        let first_count = confirmations.clone();
        let first = operations
            .run_shared(key.clone(), async move {
                first_count.fetch_add(1, Ordering::SeqCst);
                applied_outcome()
            })
            .await
            .unwrap();
        let second_count = confirmations.clone();
        let second = operations
            .run_shared(key, async move {
                second_count.fetch_add(1, Ordering::SeqCst);
                applied_outcome()
            })
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(confirmations.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn current_turn_outcome_preserves_applied_unconfirmed_and_error_inputs() {
        let applied = current_turn_result(Ok(serde_json::json!({"status":"applied"})));
        assert!(
            matches!(applied, CurrentTurnOutcome::Applied(value) if value["status"] == "applied")
        );

        let unconfirmed =
            current_turn_result(Ok(serde_json::json!({"status":"targetUnavailable"})));
        assert!(
            matches!(unconfirmed, CurrentTurnOutcome::Unconfirmed(value) if value["status"] == "targetUnavailable")
        );

        let failed = current_turn_result(Err(anyhow!(std::io::Error::other("native code 42"))));
        assert!(
            matches!(failed, CurrentTurnOutcome::Failed(error) if error.chain().any(|cause| cause.to_string() == "native code 42"))
        );
    }

    #[tokio::test]
    async fn pending_confirmation_does_not_persist_until_applied() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let save_calls = Arc::new(AtomicUsize::new(0));
        let save_counter = save_calls.clone();
        let (confirmation_tx, confirmation_rx) = tokio::sync::oneshot::channel();
        let operation = tokio::spawn(async move {
            resolve_confirmation(
                async move {
                    confirmation_rx.await.context(
                        crate::i18n::Language::English
                            .message(crate::i18n::MessageKey::SettingsOperationResultMissing),
                    )?
                },
                CurrentTurnOutcome::NotRequested,
                || true,
                |_| true,
                Some(|_| async move {
                    save_counter.fetch_add(1, Ordering::SeqCst);
                    Ok(true)
                }),
                crate::i18n::Language::English,
            )
            .await
        });
        tokio::task::yield_now().await;
        assert_eq!(save_calls.load(Ordering::SeqCst), 0);
        confirmation_tx
            .send(Ok(ConfirmationResult::Applied {
                snapshot: serde_json::json!({"model":"m1"}),
                revision: 1_u8,
            }))
            .unwrap();
        let outcome = operation.await.unwrap();
        assert!(matches!(
            &outcome.application,
            ApplicationOutcome::Applied(snapshot) if snapshot["model"] == "m1"
        ));
        assert!(matches!(outcome.persistence, PersistenceOutcome::Saved));
        assert_eq!(save_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn applied_snapshot_survives_persistence_and_current_turn_errors() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let save_calls = Arc::new(AtomicUsize::new(0));
        let save_counter = save_calls.clone();
        let current_turn =
            CurrentTurnOutcome::Failed(anyhow!(std::io::Error::other("turn code 17")));
        let outcome = resolve_confirmation(
            async {
                Ok(ConfirmationResult::Applied {
                    snapshot: serde_json::json!({"model":"m2"}),
                    revision: 2_u8,
                })
            },
            current_turn,
            || true,
            |_| true,
            Some(|_| async move {
                save_counter.fetch_add(1, Ordering::SeqCst);
                Err(anyhow!(std::io::Error::other("state write code 23")))
            }),
            crate::i18n::Language::English,
        )
        .await;

        assert!(matches!(
            &outcome.application,
            ApplicationOutcome::Applied(snapshot) if snapshot["model"] == "m2"
        ));
        assert!(matches!(
            &outcome.persistence,
            PersistenceOutcome::Failed(error)
                if error.chain().any(|cause| cause.to_string() == "state write code 23")
        ));
        assert!(matches!(
            &outcome.current_turn,
            CurrentTurnOutcome::Failed(error)
                if error.chain().any(|cause| cause.to_string() == "turn code 17")
        ));
        assert_eq!(save_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stale_target_after_applied_confirmation_does_not_persist() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let save_calls = Arc::new(AtomicUsize::new(0));
        let save_counter = save_calls.clone();
        let outcome = resolve_confirmation(
            async {
                Ok(ConfirmationResult::Applied {
                    snapshot: serde_json::json!({"model":"m3"}),
                    revision: 3_u8,
                })
            },
            CurrentTurnOutcome::NotRequested,
            || false,
            |_| true,
            Some(|_| async move {
                save_counter.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            }),
            crate::i18n::Language::English,
        )
        .await;
        assert!(matches!(
            &outcome.application,
            ApplicationOutcome::Applied(snapshot) if snapshot["model"] == "m3"
        ));
        assert!(!outcome.target_current);
        assert!(matches!(outcome.persistence, PersistenceOutcome::NotNeeded));
        assert_eq!(save_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn superseded_late_applied_snapshot_is_preserved_without_persisting() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let save_calls = Arc::new(AtomicUsize::new(0));
        let save_counter = save_calls.clone();
        let outcome = resolve_confirmation(
            async {
                Ok(ConfirmationResult::Applied {
                    snapshot: serde_json::json!({"model":"m4"}),
                    revision: 4_u8,
                })
            },
            CurrentTurnOutcome::NotRequested,
            || true,
            |_| false,
            Some(|_| async move {
                save_counter.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            }),
            crate::i18n::Language::English,
        )
        .await;
        assert!(matches!(
            &outcome.application,
            ApplicationOutcome::Applied(snapshot) if snapshot["model"] == "m4"
        ));
        assert!(outcome.superseded);
        assert!(outcome.target_current);
        assert_eq!(save_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn already_applied_readback_uses_the_normal_selection_persistence_path() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let save_calls = Arc::new(AtomicUsize::new(0));
        let save_counter = save_calls.clone();
        let outcome = resolve_confirmation(
            async {
                Ok(ConfirmationResult::Applied {
                    snapshot: serde_json::json!({"model":"m5"}),
                    revision: 5_u8,
                })
            },
            CurrentTurnOutcome::NotRequested,
            || true,
            |_| true,
            Some(|_| async move {
                save_counter.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            }),
            crate::i18n::Language::English,
        )
        .await;
        assert!(matches!(&outcome.persistence, PersistenceOutcome::Saved));
        assert_eq!(save_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn supersession_during_conditional_persist_prevents_late_save() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let current = Arc::new(AtomicBool::new(true));
        let write_calls = Arc::new(AtomicUsize::new(0));
        let current_during_save = current.clone();
        let writes_during_save = write_calls.clone();
        let current_for_check = current.clone();
        let outcome = resolve_confirmation(
            async {
                Ok(ConfirmationResult::Applied {
                    snapshot: serde_json::json!({"model":"A"}),
                    revision: 6_u8,
                })
            },
            CurrentTurnOutcome::NotRequested,
            || true,
            move |_| current_for_check.load(Ordering::Acquire),
            Some(move |_| async move {
                // B supersedes A after the resolver's initial check. The
                // conditional Native save observes this and does not write.
                current_during_save.store(false, Ordering::Release);
                let still_current = current_during_save.load(Ordering::Acquire);
                if still_current {
                    writes_during_save.fetch_add(1, Ordering::SeqCst);
                }
                Ok(still_current)
            }),
            crate::i18n::Language::English,
        )
        .await;

        assert!(matches!(
            &outcome.application,
            ApplicationOutcome::Applied(snapshot) if snapshot["model"] == "A"
        ));
        assert!(outcome.superseded);
        assert!(outcome.target_current);
        assert!(matches!(outcome.persistence, PersistenceOutcome::NotNeeded));
        assert_eq!(write_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn operation_owner_panic_wakes_observers_with_a_terminal_error() {
        let operations = SettingsOperations::default();
        let request = request_for("effort", "high");
        let key = operation_key(&request);
        let owner_ops = operations.clone();
        let owner = tokio::spawn(async move {
            owner_ops
                .run_shared(
                    key,
                    async move { panic!("operation failed before completion") },
                )
                .await
        });
        let outcome = owner.await.unwrap().unwrap();
        assert!(matches!(
            &outcome.application,
            ApplicationOutcome::Failed(error)
                if error.to_string().contains("worker exited")
        ));
    }

    fn request_for(field: &'static str, value: &str) -> SettingRequest {
        let mut params = serde_json::json!({"threadId":"thread-a"});
        params[field] = Value::String(value.to_owned());
        SettingRequest {
            generation: 4,
            thread_id: "thread-a".to_owned(),
            field,
            params,
            expected: Value::String(value.to_owned()),
            expected_model_scope: None,
            model_selection: None,
            current_turn_params: None,
        }
    }

    fn applied_outcome() -> SettingOutcome {
        SettingOutcome {
            application: ApplicationOutcome::Applied(serde_json::json!({"effort":"high"})),
            persistence: PersistenceOutcome::NotNeeded,
            current_turn: CurrentTurnOutcome::NotRequested,
            target_current: true,
            superseded: false,
        }
    }

    #[test]
    fn model_scope_keeps_executor_provider_and_model_as_one_selection() {
        let codex =
            serde_json::json!({"model":"m1", "modelProvider":"openai", "modelExecutor":"codex"});
        let agy = serde_json::json!({"model":"m1", "modelProvider":"other", "modelExecutor":"antigravity"});

        assert_eq!(model_scope(&codex), Some(r#"["openai","m1"]"#.to_owned()));
        assert_eq!(
            model_scope(&agy),
            Some(r#"["antigravity","antigravity","m1"]"#.to_owned())
        );
    }
}
