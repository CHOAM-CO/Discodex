pub mod agy;
pub mod agy_mcp;
pub mod agy_setup;
pub mod attachments;
mod bundled_skill;
pub mod commands;
pub mod config;
pub(crate) mod diagnostics;
pub mod discord;
mod discord_agy_setup;
pub mod discord_tools;
mod discord_updates;
pub mod i18n;
pub mod install;
pub mod installation_check;
mod instructions;
mod legacy_commands;
pub mod message_access;
pub mod native;
pub mod reply_files;
pub(crate) mod settings_operation;
pub mod state;
pub mod voice;
mod voice_input;
pub mod voice_tools;

#[cfg(any(target_os = "macos", test))]
pub mod mac_signing;

pub(crate) mod realtime_prompt;

pub mod harness;
pub(crate) mod task_plan;
pub(crate) mod task_tools;
pub(crate) mod worktree_tools;

pub(crate) mod async_questions;
pub(crate) mod discord_plugins;
pub mod logging;
pub(crate) mod native_features;

pub(crate) mod table_messages;
pub(crate) mod table_render;
pub(crate) mod table_source;

pub(crate) mod agy_model_catalog;
pub(crate) mod model_catalog;
