pub mod attachments;
pub mod commands;
pub mod config;
pub mod discord;
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
pub mod state;
pub mod voice;
mod voice_input;
pub mod voice_tools;

#[cfg(any(target_os = "macos", test))]
pub mod mac_signing;

pub(crate) mod realtime_prompt;
