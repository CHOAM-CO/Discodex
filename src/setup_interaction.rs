//! Installer input and progress shared by terminal and desktop adapters.

use std::{future::Future, path::PathBuf, pin::Pin};

use anyhow::Result;
use tokio::sync::watch;

use crate::{agy_setup, i18n::Language, installation_check::Report};

pub type SetupFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub struct SetupPrompt {
    pub language: Language,
    pub text: String,
    pub default: String,
    pub secret: bool,
}

pub enum SetupProgress {
    Message(String),
    Diagnostic(String),
    CodexLogin { login_id: String, auth_url: String },
    CodexAccountReady,
    RuntimePrepared,
    ServiceStarted,
    InstallationReport(Option<Report>),
}

/// Adapters own presentation and input only. The installer retains ownership of
/// credentials, native connections, validation, service registration and readiness.
pub trait SetupInteractionPort: Send + Sync {
    fn on_progress(&self, progress: SetupProgress) -> Result<()>;

    fn request_input(&self, prompt: SetupPrompt) -> SetupFuture<'_, String>;

    fn request_installation_plan(
        &self,
        language: Language,
    ) -> SetupFuture<'_, crate::install::InstallationList>;

    fn retry_installation_check(&self, language: Language) -> SetupFuture<'_, bool>;

    /// The adapter opens the OS guidance and waits for the user's acknowledgement;
    /// acknowledgement never claims that macOS granted Full Disk Access.
    fn open_mac_fda_guidance(
        &self,
        executable: PathBuf,
        language: Language,
    ) -> SetupFuture<'_, ()>;

    /// Reuse agy_setup::prepare with its existing progress/input/cancel channels.
    /// The CLI adapter supplies terminal navigation; a desktop adapter supplies UI.
    fn prepare_antigravity(
        &self,
        config: agy_setup::SetupConfig,
        language: Language,
        cancel_rx: watch::Receiver<bool>,
    ) -> SetupFuture<'_, agy_setup::SetupReady>;
}

#[derive(Debug)]
pub struct SetupCancelled;

impl std::fmt::Display for SetupCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("setup operation cancelled")
    }
}

impl std::error::Error for SetupCancelled {}

pub(crate) fn check_cancelled(cancel_rx: &watch::Receiver<bool>) -> Result<()> {
    if *cancel_rx.borrow() || cancel_rx.has_changed().is_err() {
        return Err(SetupCancelled.into());
    }
    Ok(())
}

pub(crate) async fn cancelled(cancel_rx: &mut watch::Receiver<bool>) {
    loop {
        if *cancel_rx.borrow_and_update() || cancel_rx.changed().await.is_err() {
            return;
        }
    }
}

pub(crate) async fn input_or_cancel<T>(
    future: SetupFuture<'_, T>,
    mut cancel_rx: watch::Receiver<bool>,
) -> Result<T> {
    tokio::select! {
        biased;
        _ = cancelled(&mut cancel_rx) => Err(SetupCancelled.into()),
        result = future => result,
    }
}
