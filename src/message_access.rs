use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MessagePolicy {
    Everyone,
    RegisteredServer,
    RegisteredChannels,
    #[default]
    Paired,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct MessageAccess {
    pub policy: MessagePolicy,
    pub paired_user_ids: Vec<u64>,
    pub whitelist: Vec<u64>,
    pub blacklist: Vec<u64>,
    pub channel_ids: Vec<u64>,
}

impl MessageAccess {
    /// Use explicit configured policy when present; otherwise migrate legacy operators
    /// and the configured home/transcript channels into the initial policy state.
    pub fn from_config(config: &Config) -> Self {
        if let Some(access) = &config.message_access {
            return access.clone();
        }
        let mut channel_ids = vec![config.home_channel_id, config.transcript_channel_id];
        channel_ids.dedup();
        Self {
            policy: MessagePolicy::Paired,
            paired_user_ids: config.operator_ids.clone(),
            channel_ids,
            ..Self::default()
        }
    }

    /// Load durable message-access settings, falling back to legacy config migration
    /// only when no state file exists.
    pub fn load(path: &Path, config: &Config) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse message access state {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::from_config(config))
            }
            Err(error) => {
                Err(error).with_context(|| format!("read message access state {}", path.display()))
            }
        }
    }

    /// Atomically replace the durable state file, preserving the complete lists.
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("message access path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create message access directory {}", parent.display()))?;
        let bytes = serde_json::to_vec_pretty(self)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).with_context(|| {
            format!(
                "create temporary message access state in {}",
                parent.display()
            )
        })?;
        temp.write_all(&bytes).with_context(|| {
            format!(
                "write temporary message access state in {}",
                parent.display()
            )
        })?;
        temp.persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("replace message access state {}", path.display()))?;
        Ok(())
    }

    /// Apply precedence: self and blacklist always deny, whitelist always allows,
    /// then the selected source policy is evaluated.
    pub fn allows(
        &self,
        author_id: u64,
        is_self: bool,
        guild_id: Option<u64>,
        channel_id: u64,
        parent_channel_id: Option<u64>,
        registered_guild_id: u64,
    ) -> bool {
        if is_self || self.blacklist.contains(&author_id) {
            return false;
        }
        if self.whitelist.contains(&author_id) {
            return true;
        }
        match self.policy {
            MessagePolicy::Everyone => true,
            MessagePolicy::RegisteredServer => guild_id == Some(registered_guild_id),
            MessagePolicy::RegisteredChannels => {
                self.channel_ids.contains(&channel_id)
                    || parent_channel_id.is_some_and(|id| self.channel_ids.contains(&id))
            }
            MessagePolicy::Paired => self.paired_user_ids.contains(&author_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::InputMode;
    use std::path::PathBuf;

    fn config(access: Option<MessageAccess>) -> Config {
        Config {
            language: crate::i18n::Language::English,
            profile: "test-profile".into(),
            name: "Test worker".into(),
            token_env: "DISCODEX_TEST_TOKEN".into(),
            credentials_file: None,
            guild_id: 11,
            home_channel_id: 21,
            transcript_channel_id: 22,
            operator_ids: vec![31, 32],
            voice_channel_ids: vec![],
            codex_path: PathBuf::from("codex"),
            worker_path: None,
            codex_home: None,
            project_dir: PathBuf::from("."),
            state_dir: PathBuf::from("state"),
            model: None,
            effort: None,
            service_tier: None,
            memory_enabled: None,
            permissions: None,
            voice: None,
            input_mode: InputMode::Steering,
            message_access: access,
        }
    }

    #[test]
    fn legacy_config_migrates_operators_and_unique_registered_channels() {
        let migrated = MessageAccess::from_config(&config(None));
        assert_eq!(migrated.policy, MessagePolicy::Paired);
        assert_eq!(migrated.paired_user_ids, vec![31, 32]);
        assert_eq!(migrated.channel_ids, vec![21, 22]);

        let duplicate = Config {
            transcript_channel_id: 21,
            ..config(None)
        };
        assert_eq!(MessageAccess::from_config(&duplicate).channel_ids, vec![21]);
    }

    #[test]
    fn explicit_configured_message_access_is_used_as_is() {
        let configured = MessageAccess {
            policy: MessagePolicy::Everyone,
            paired_user_ids: vec![41],
            whitelist: vec![42],
            blacklist: vec![43],
            channel_ids: vec![51, 52],
        };
        assert_eq!(
            MessageAccess::from_config(&config(Some(configured.clone()))),
            configured
        );
    }

    #[test]
    fn policies_respect_self_blacklist_whitelist_and_source_scope() {
        let mut access = MessageAccess {
            policy: MessagePolicy::Everyone,
            whitelist: vec![2, 3],
            blacklist: vec![3],
            ..MessageAccess::default()
        };
        let check = |access: &MessageAccess, author, is_self, guild, channel, parent| {
            access.allows(author, is_self, guild, channel, parent, 10)
        };
        assert!(check(&access, 1, false, None, 90, None));
        assert!(!check(&access, 1, true, None, 90, None));
        assert!(check(&access, 2, false, None, 90, None));
        assert!(!check(&access, 3, false, None, 90, None));

        access.policy = MessagePolicy::RegisteredServer;
        assert!(check(&access, 1, false, Some(10), 90, None));
        assert!(!check(&access, 1, false, Some(11), 90, None));
        assert!(!check(&access, 1, false, None, 90, None));

        access.policy = MessagePolicy::RegisteredChannels;
        access.channel_ids = vec![20];
        assert!(check(&access, 1, false, Some(10), 20, None));
        assert!(check(&access, 1, false, Some(10), 21, Some(20)));
        assert!(!check(&access, 1, false, Some(10), 21, None));
        assert!(!check(&access, 1, false, None, 21, None));

        access.policy = MessagePolicy::Paired;
        access.paired_user_ids = vec![4];
        assert!(check(&access, 4, false, None, 90, None));
        assert!(!check(&access, 5, false, Some(10), 90, None));
    }

    #[test]
    fn state_round_trip_and_missing_file_migration_preserve_all_lists() {
        let dir = tempfile::tempdir().expect("temp state dir");
        let path = dir.path().join("message-access.json");
        let migrated = MessageAccess::load(&path, &config(None)).expect("legacy fallback");
        assert_eq!(migrated.policy, MessagePolicy::Paired);

        let state = MessageAccess {
            policy: MessagePolicy::RegisteredChannels,
            paired_user_ids: vec![1, 2, 3],
            whitelist: vec![4, 5],
            blacklist: vec![6, 7],
            channel_ids: vec![8, 9, 10],
        };
        state.save(&path).expect("save state");
        assert_eq!(MessageAccess::load(&path, &config(None)).unwrap(), state);
    }
}
