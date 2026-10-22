use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model_catalog::ModelSelection;

// All state writers share this short read/modify/write critical section.
static NATIVE_STATE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgyThreadState {
    pub conversation_id: Option<String>,
    pub attempted_tail_order: Option<u64>,
    pub current_run: Option<AgyRunAttempt>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgyContextReference {
    pub item_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub order: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgyContextRange {
    pub first: AgyContextReference,
    pub last: AgyContextReference,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgyRunAttempt {
    pub binding: crate::agy_mcp::NativeRunBinding,
    pub context_range: Option<AgyContextRange>,
    pub outcome: String,
    pub terminal_status: Option<String>,
    pub exit_status: Option<String>,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeState {
    pub thread_id: Option<String>,
    #[serde(default)]
    pub previous_thread_ids: Vec<String>,
    #[serde(default)]
    pub pending_new_thread: bool,
    #[serde(default)]
    pub(crate) model_selection: Option<ModelSelection>,
    #[serde(default)]
    pub agy_threads: BTreeMap<String, AgyThreadState>,
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
        let _guard = NATIVE_STATE_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("native state lock poisoned"))?;
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> Result<NativeState> {
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
        let _guard = NATIVE_STATE_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("native state lock poisoned"))?;
        let mut state = self.load_unlocked()?;
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
        let _guard = NATIVE_STATE_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("native state lock poisoned"))?;
        let mut state = self.load_unlocked()?;
        state.model_selection = Some(selection.clone());
        self.save(&state)
    }

    pub(crate) fn save_thread_and_model_selection(
        &self,
        thread_id: &str,
        selection: &ModelSelection,
    ) -> Result<()> {
        let _guard = NATIVE_STATE_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("native state lock poisoned"))?;
        let mut state = self.load_unlocked()?;
        if let Some(previous) = state.thread_id.as_ref() {
            if previous != thread_id && !state.previous_thread_ids.contains(previous) {
                state.previous_thread_ids.push(previous.clone());
            }
        }
        state.thread_id = Some(thread_id.to_owned());
        state.pending_new_thread = false;
        state.model_selection = Some(selection.clone());
        self.save(&state)
    }

    pub(crate) fn save_pending_model_selection(
        &self,
        previous_thread_id: &str,
        selection: &ModelSelection,
    ) -> Result<()> {
        let _guard = NATIVE_STATE_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("native state lock poisoned"))?;
        let mut state = self.load_unlocked()?;
        if !state
            .previous_thread_ids
            .iter()
            .any(|id| id == previous_thread_id)
        {
            state
                .previous_thread_ids
                .push(previous_thread_id.to_owned());
        }
        state.model_selection = Some(selection.clone());
        state.pending_new_thread = true;
        self.save(&state)
    }

    pub fn mark_pending_new_thread(&self) -> Result<()> {
        let _guard = NATIVE_STATE_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("native state lock poisoned"))?;
        let mut state = self.load_unlocked()?;
        state.pending_new_thread = true;
        self.save(&state)
    }

    pub(crate) fn agy_thread(&self, thread_id: &str) -> Result<AgyThreadState> {
        Ok(self
            .load()?
            .agy_threads
            .get(thread_id)
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) fn update_agy_thread<T>(
        &self,
        thread_id: &str,
        update: impl FnOnce(&mut AgyThreadState) -> Result<T>,
    ) -> Result<T> {
        let _guard = NATIVE_STATE_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("native state lock poisoned"))?;
        let mut state = self.load_unlocked()?;
        let result = update(state.agy_threads.entry(thread_id.to_owned()).or_default())?;
        self.save(&state)?;
        Ok(result)
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
        match fs::metadata(&self.path) {
            Ok(metadata) => temp.as_file().set_permissions(metadata.permissions())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect native state {}", self.path.display()));
            }
        }
        temp.as_file().sync_all()?;
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

    #[test]
    fn model_fork_switch_persists_thread_and_selection_as_one_state_change() {
        let dir = tempfile::tempdir().expect("temporary state directory");
        let store = StateStore::new(dir.path());
        store.save_thread_id("source-thread").unwrap();
        let selection = ModelSelection {
            executor: crate::model_catalog::ModelExecutor::Codex,
            provider: "example-provider".to_owned(),
            model: "example-model".to_owned(),
        };

        store
            .save_thread_and_model_selection("forked-thread", &selection)
            .unwrap();
        let state = store.load().unwrap();
        assert_eq!(state.thread_id.as_deref(), Some("forked-thread"));
        assert_eq!(state.previous_thread_ids, vec!["source-thread"]);
        assert_eq!(state.model_selection, Some(selection));
        assert!(!state.pending_new_thread);
    }
}
