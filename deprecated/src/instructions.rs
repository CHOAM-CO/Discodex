/// Worker identity replaces the model's default coding-agent base prompt.
/// Runtime tool definitions and permissions still come from the app-server.
pub(crate) fn agent_base_instructions(name: &str, language: crate::i18n::Language) -> String {
    format!(
        "{}\n\n{}\n\n{}",
        agent_identity_instructions(name, language),
        voice_join_policy(language),
        voice_leave_policy(language)
    )
}

pub(crate) fn voice_join_policy(language: crate::i18n::Language) -> &'static str {
    language.message(crate::i18n::MessageKey::NativeVoiceJoinPolicy)
}

/// Shared identity; the voice model delegates actions instead of executing MCP tools.
pub(crate) fn agent_identity_instructions(name: &str, language: crate::i18n::Language) -> String {
    let profile = serde_json::json!({"name": name});
    let body = language.message(crate::i18n::MessageKey::NativeAgentIdentityInstructions);
    let style = language.message(crate::i18n::MessageKey::NativeExplanationStyle);
    format!(
        "{}\n\n{style}",
        body.replace("{profile}", &profile.to_string())
    )
}

/// Policy for ending only the active voice call when the user asks to leave.
pub(crate) fn voice_leave_policy(language: crate::i18n::Language) -> &'static str {
    language.message(crate::i18n::MessageKey::NativeVoiceLeavePolicy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;

    #[test]
    fn identity_and_leave_policy_follow_selected_language_and_dynamic_name() {
        for (language, localized_intro) in [
            (Language::English, "Your agent profile is"),
            (Language::Korean, "당신의 에이전트 프로필은"),
            (Language::Chinese, "你的智能体资料是"),
            (Language::Japanese, "あなたのエージェントプロフィールは"),
        ] {
            let prompt = agent_base_instructions("Dynamic Agent", language);
            assert!(prompt.contains(localized_intro));
            assert!(prompt.contains("\"name\":\"Dynamic Agent\""));
            assert!(prompt.contains("voice_leave"));
        }
    }
}
