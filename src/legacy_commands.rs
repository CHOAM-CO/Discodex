use anyhow::{Context as _, Result};
use serenity::{
    all::{CommandType, GuildId},
    http::Http,
};

const RETIRED_COMMAND_NAMES: [&str; 5] = [
    "voice-profile",
    "agent-settings",
    "runtime-settings",
    "resume",
    "usage",
];

fn is_retired_command(name: &str) -> bool {
    RETIRED_COMMAND_NAMES.contains(&name)
}

/// Remove retired chat-input commands from this authenticated application's scopes.
///
/// The names are the exact legacy command contract observed in the app's guild
/// inventory. Unrelated commands, including `/discodex`, are preserved.
pub(crate) async fn remove_retired_commands(http: &Http, guild_id: GuildId) -> Result<usize> {
    let mut removed = 0;
    let guild_commands = guild_id
        .get_commands(http)
        .await
        .context("list guild application commands before legacy cleanup")?;
    for command in guild_commands {
        if command.kind == CommandType::ChatInput && is_retired_command(&command.name) {
            guild_id
                .delete_command(http, command.id)
                .await
                .with_context(|| format!("delete retired guild command /{}", command.name))?;
            removed += 1;
        }
    }

    let global_commands = http
        .get_global_commands()
        .await
        .context("list global application commands before legacy cleanup")?;
    for command in global_commands {
        if command.kind == CommandType::ChatInput && is_retired_command(&command.name) {
            serenity::all::Command::delete_global_command(http, command.id)
                .await
                .with_context(|| format!("delete retired global command /{}", command.name))?;
            removed += 1;
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::is_retired_command;

    #[test]
    fn only_confirmed_legacy_command_names_are_retired() {
        for name in [
            "voice-profile",
            "agent-settings",
            "runtime-settings",
            "resume",
            "usage",
        ] {
            assert!(is_retired_command(name));
        }
        for name in ["discodex", "codex", "help", "settings", "VOICE-PROFILE"] {
            assert!(!is_retired_command(name));
        }
    }
}
