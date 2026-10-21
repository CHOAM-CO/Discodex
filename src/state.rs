use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model_catalog::ModelSelection;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeState {
    pub thread_id: Option<String>,
    #[serde(default)]
    pub previous_thread_ids: Vec<String>,
    #[serde(default)]
    pub pending_new_thread: bool,
    #[serde(default)]
    pub(crate) model_selection: Option<ModelSelection>,
}

#[derive(Clone, Debug)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(state_dir: impl AsRef<Path>) -> Self {
        Self {
            path: state_dir.as_ref().join("native.json"),
        }
    }

    pub fn load(&self) -> Result<NativeState> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse native state {}", self.path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(NativeState::default()),
            Err(err) => {
                Err(err).with_context(|| format!("read native state {}", self.path.display()))
            }
        }
    }

    pub fn save_thread_id(&self, thread_id: &str) -> Result<()> {
        let mut state = self.load()?;
        if let Some(previous) = state.thread_id.as_ref() {
            if previous != thread_id && !state.previous_thread_ids.contains(previous) {
                state.previous_thread_ids.push(previous.clone());
            }
        }
        state.thread_id = Some(thread_id.to_owned());
        state.pending_new_thread = false;
        self.save(&state)
    }

    pub(crate) fn save_model_selection(&self, selection: &ModelSelection) -> Result<()> {
        let mut state = self.load()?;
        state.model_selection = Some(selection.clone());
        self.save(&state)
    }

    pub fn mark_pending_new_thread(&self) -> Result<()> {
        let mut state = self.load()?;
        state.pending_new_thread = true;
        self.save(&state)
    }

    fn save(&self, state: &NativeState) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("native state path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create state directory {}", parent.display()))?;
        let bytes = serde_json::to_vec_pretty(state)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)
            .with_context(|| format!("create temporary native state in {}", parent.display()))?;
        temp.write_all(&bytes)
            .with_context(|| format!("write temporary native state in {}", parent.display()))?;
        temp.persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| format!("replace native state {}", self.path.display()))?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_id_survives_a_state_store_reopen() {
        let dir = tempfile::tempdir().expect("temporary state directory");
        let store = StateStore::new(dir.path());
        store
            .save_thread_id("thread-actual")
            .expect("persist thread id");

        let reopened = StateStore::new(dir.path());
        assert_eq!(
            reopened.load().unwrap().thread_id.as_deref(),
            Some("thread-actual")
        );
    }

    #[test]
    fn explicit_new_keeps_the_durable_thread_until_native_input_materializes() {
        let dir = tempfile::tempdir().expect("temporary state directory");
        let store = StateStore::new(dir.path());
        store
            .save_thread_id("thread-old")
            .expect("persist previous durable thread");

        store
            .mark_pending_new_thread()
            .expect("record explicit new-thread selection");
        let pending = StateStore::new(dir.path())
            .load()
            .expect("reload pending selection");
        assert_eq!(pending.thread_id.as_deref(), Some("thread-old"));
        assert!(pending.pending_new_thread);

        store
            .save_thread_id("thread-new")
            .expect("persist materialized new conversation");
        let materialized = StateStore::new(dir.path())
            .load()
            .expect("reload materialized conversation");
        assert_eq!(materialized.thread_id.as_deref(), Some("thread-new"));
        assert_eq!(
            materialized.previous_thread_ids,
            vec!["thread-old".to_owned()]
        );
        assert!(!materialized.pending_new_thread);
    }
    #[test]
    fn provider_model_selection_round_trips_and_legacy_state_defaults_to_none() {
        let dir = tempfile::tempdir().expect("temporary state directory");
        let store = StateStore::new(dir.path());
        std::fs::write(store.path(), r#"{"thread_id":"legacy-thread"}"#)
            .expect("write legacy state without selection");
        assert_eq!(store.load().unwrap().model_selection, None);

        let selection = ModelSelection {
            executor: crate::model_catalog::ModelExecutor::Codex,
            provider: "local-provider".to_owned(),
            model: "local-model".to_owned(),
        };
        store
            .save_model_selection(&selection)
            .expect("save selection");
        assert_eq!(
            StateStore::new(dir.path()).load().unwrap().model_selection,
            Some(selection)
        );
    }
}
