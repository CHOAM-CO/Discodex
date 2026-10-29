use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InputMode {
    #[default]
    Steering,
    NextTurn,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub language: crate::i18n::Language,
    pub profile: String,
    pub name: String,
    pub token_env: String,
    #[serde(default)]
    pub credentials_file: Option<PathBuf>,
    pub guild_id: u64,
    pub home_channel_id: u64,
    pub transcript_channel_id: u64,
    #[serde(default)]
    pub operator_ids: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_access: Option<crate::message_access::MessageAccess>,
    #[serde(default)]
    pub voice_channel_ids: Vec<u64>,
    pub codex_path: PathBuf,
    #[serde(default)]
    pub worker_path: Option<PathBuf>,
    #[serde(default)]
    pub codex_home: Option<PathBuf>,
    pub project_dir: PathBuf,
    pub state_dir: PathBuf,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Use the literal "off" to clear a thread service tier via native null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_enabled: Option<bool>,
    #[serde(default)]
    pub permissions: Option<String>,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub input_mode: InputMode,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text =
            fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
        let mut config: Self = serde_json::from_str(&text)
            .with_context(|| format!("parse JSON config {}", path.display()))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        resolve_from(base, &mut config.codex_path);
        if let Some(worker_path) = &mut config.worker_path {
            resolve_from(base, worker_path);
        }
        resolve_from(base, &mut config.project_dir);
        resolve_from(base, &mut config.state_dir);
        if let Some(home) = &mut config.codex_home {
            resolve_from(base, home);
        }
        if let Some(credentials) = &mut config.credentials_file {
            resolve_from(base, credentials);
        }
        Ok(config)
    }

    /// Reads the Discord application token without exposing it through config serialization.
    pub fn token(&self) -> Result<String> {
        if let Ok(value) = env::var(&self.token_env) {
            if !value.trim().is_empty() {
                return Ok(value.trim().to_owned());
            }
        }
        if let Some(path) = &self.credentials_file {
            let value = fs::read_to_string(path)
                .with_context(|| format!("read credential file {}", path.display()))?;
            let value = value.trim();
            if !value.is_empty() {
                return Ok(value.to_owned());
            }
        }
        bail!(
            "Discord token is unavailable; set {} or configure credentialsFile",
            self.token_env
        )
    }
}

fn resolve_from(base: &Path, path: &mut PathBuf) {
    if path.is_relative() {
        *path = base.join(&*path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_resolves_owned_paths_relative_to_config_and_defaults_to_steering() {
        let dir = tempfile::tempdir().expect("temporary config directory");
        let path = dir.path().join("profile.json");
        fs::write(
            &path,
            r#"{
                "profile":"test",
                "name":"Test",
                "tokenEnv":"DISCODEX_TEST_TOKEN",
                "guildId":1,
                "homeChannelId":2,
                "transcriptChannelId":3,
                "codexPath":"bin/codex",
                "projectDir":"project",
                "stateDir":"state"
            }"#,
        )
        .expect("write config");

        let config = Config::load(&path).expect("load config");

        assert_eq!(config.input_mode, InputMode::Steering);
        assert_eq!(config.language, crate::i18n::Language::English);
        assert_eq!(config.codex_path, dir.path().join("bin/codex"));
        assert_eq!(config.project_dir, dir.path().join("project"));
        assert_eq!(config.state_dir, dir.path().join("state"));
    }
}
