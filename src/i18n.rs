use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum Language {
    #[default]
    #[serde(rename = "en", alias = "english")]
    English,
    #[serde(rename = "ko", alias = "korean")]
    Korean,
    #[serde(rename = "zh", alias = "zh-CN", alias = "chinese")]
    Chinese,
    #[serde(rename = "ja", alias = "japanese")]
    Japanese,
}
impl Language {
    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Korean => "ko",
            Self::Chinese => "zh",
            Self::Japanese => "ja",
        }
    }
    pub fn text<'a>(self, en: &'a str, ko: &'a str, zh: &'a str, ja: &'a str) -> &'a str {
        match self {
            Self::English => en,
            Self::Korean => ko,
            Self::Chinese => zh,
            Self::Japanese => ja,
        }
    }

    pub const fn message(self, key: MessageKey) -> &'static str {
        key.text(self)
    }
}

include!(concat!(env!("OUT_DIR"), "/message_resources.rs"));
impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl FromStr for Language {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "en" | "english" | "1" => Ok(Self::English),
            "ko" | "korean" | "2" => Ok(Self::Korean),
            "zh" | "zh-cn" | "chinese" | "3" => Ok(Self::Chinese),
            "ja" | "japanese" | "4" => Ok(Self::Japanese),
            _ => anyhow::bail!(
                "{}",
                Language::English.message(MessageKey::CommonLanguageUnsupported)
            ),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn language_codes_round_trip_and_select_requested_message() {
        for (language, code, expected) in [
            (Language::English, "en", "English"),
            (Language::Korean, "ko", "Korean"),
            (Language::Chinese, "zh", "Chinese"),
            (Language::Japanese, "ja", "Japanese"),
        ] {
            assert_eq!(code.parse::<Language>().unwrap(), language);
            assert_eq!(serde_json::to_value(language).unwrap(), code);
            assert_eq!(
                language.text("English", "Korean", "Chinese", "Japanese"),
                expected
            );
        }
        assert_eq!(Language::default(), Language::English);
        assert!("unknown".parse::<Language>().is_err());
    }

    #[test]
    fn message_key_selects_each_supported_locale() {
        assert_eq!(
            Language::English.message(MessageKey::CommonLanguageUnsupported),
            "Unsupported language; choose en, ko, zh, or ja."
        );
        assert_eq!(
            Language::Korean.message(MessageKey::CommonLanguageUnsupported),
            "지원하지 않는 언어입니다. en, ko, zh, ja 중에서 선택하세요."
        );
        assert_eq!(
            Language::Chinese.message(MessageKey::CommonLanguageUnsupported),
            "不支持此语言；请选择 en、ko、zh 或 ja。"
        );
        assert_eq!(
            Language::Japanese.message(MessageKey::CommonLanguageUnsupported),
            "サポートされていない言語です。en、ko、zh、ja から選択してください。"
        );
    }
}
