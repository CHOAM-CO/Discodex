//! Instructions for the conversational voice model, separate from Codex text turns.
//! Verified against official Codex rust-v0.162.0-alpha.17.2:
//! core/src/realtime_prompt.rs -> RealtimeSessionConfig.instructions -> session.instructions.

pub(crate) fn for_agent(name: &str, language: crate::i18n::Language) -> String {
    let identity = crate::instructions::agent_identity_instructions(name, language);
    let body = language.message(crate::i18n::MessageKey::NativeRealtimeVoicePrompt);
    format!("{identity}\n\n{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;

    #[test]
    fn realtime_prompt_uses_selected_language_and_shared_agent_identity() {
        for (language, localized_heading) in [
            (Language::English, "## Discord voice conversation"),
            (Language::Korean, "## Discord 음성 대화"),
            (Language::Chinese, "## Discord 语音对话"),
            (Language::Japanese, "## Discordの音声会話"),
        ] {
            let prompt = for_agent("Dynamic Agent", language);
            assert!(prompt.contains(localized_heading));
            assert!(prompt.contains("\"name\":\"Dynamic Agent\""));
            assert!(prompt.contains("[USER]"));
            assert!(prompt.contains("voice_leave"));
        }
    }
}
