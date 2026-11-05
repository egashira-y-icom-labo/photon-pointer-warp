//! UI 言語。OS の言語 ID は呼び出し側が渡す。

use serde::{Deserialize, Serialize};

/// `config.toml` の `language`。キーが無いときは `auto`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LanguageConfig {
    #[default]
    Auto,
    En,
    Ja,
}

/// 画面に出す言語。英語がフォールバック。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    En,
    Ja,
}

impl Language {
    /// `system_lang_id` は Win32 `GetUserDefaultUILanguage` の LANGID。
    /// 下位 10 ビットが `0x11`（日本語）のときだけ日本語にする。
    pub fn resolve(config: LanguageConfig, system_lang_id: u16) -> Self {
        match config {
            LanguageConfig::En => Language::En,
            LanguageConfig::Ja => Language::Ja,
            LanguageConfig::Auto => {
                if (system_lang_id & 0x03FF) == 0x0011 {
                    Language::Ja
                } else {
                    Language::En
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_language_ignores_the_system_id() {
        assert_eq!(Language::resolve(LanguageConfig::En, 0x0411), Language::En);
        assert_eq!(Language::resolve(LanguageConfig::Ja, 0x0409), Language::Ja);
    }

    #[test]
    fn auto_uses_japanese_only_for_the_primary_language() {
        assert_eq!(Language::resolve(LanguageConfig::Auto, 0x0411), Language::Ja);
        assert_eq!(Language::resolve(LanguageConfig::Auto, 0x0011), Language::Ja);
        assert_eq!(Language::resolve(LanguageConfig::Auto, 0x0409), Language::En);
        assert_eq!(Language::resolve(LanguageConfig::Auto, 0x0804), Language::En);
        assert_eq!(Language::resolve(LanguageConfig::Auto, 0), Language::En);
    }
}
