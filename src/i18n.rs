//! The interface's language.
//!
//! Text lives in `locales/app.yml`, keyed by where it's used. Malgel
//! follows the system's language unless the user picks one in Settings,
//! and falls back to English for anything not translated.

use crate::settings::Settings;

/// The languages Malgel's interface is translated into: locale code and
/// the language's own name for it.
pub const LANGUAGES: [(&str, &str); 5] = [
    ("en", "English"),
    ("fr", "Français"),
    ("es", "Español"),
    ("zh-CN", "简体中文"),
    ("ja", "日本語"),
];

/// Switch the interface (GPUI Kit's own text too) to the language
/// `settings` asks for.
pub fn apply(settings: &Settings) {
    let language = settings
        .language
        .as_deref()
        .and_then(supported)
        .unwrap_or_else(system_language);
    rust_i18n::set_locale(language);
}

/// The supported language closest to the system's, or English.
pub fn system_language() -> &'static str {
    sys_locale::get_locale()
        .as_deref()
        .and_then(supported)
        .unwrap_or("en")
}

/// The supported language for a locale like `fr-CA`, `zh_Hans_CN` or `ja`.
pub fn supported(locale: &str) -> Option<&'static str> {
    let primary = locale
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some(match primary.as_str() {
        "en" => "en",
        "fr" => "fr",
        "es" => "es",
        // Simplified Chinese is the only Chinese translation so far.
        "zh" => "zh-CN",
        "ja" => "ja",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_system_locales() {
        assert_eq!(supported("fr-CA"), Some("fr"));
        assert_eq!(supported("es_MX.UTF-8"), Some("es"));
        assert_eq!(supported("zh-Hans-CN"), Some("zh-CN"));
        assert_eq!(supported("ja_JP"), Some("ja"));
        assert_eq!(supported("EN-us"), Some("en"));
        assert_eq!(supported("de-DE"), None);
        assert_eq!(supported(""), None);
    }

    #[test]
    fn every_key_is_translated() {
        let yaml = include_str!("../locales/app.yml");
        let mut missing = Vec::new();
        let mut key = String::new();
        let mut seen = Vec::new();
        for line in yaml.lines() {
            if line.starts_with("_version") || line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            if !line.starts_with(' ') {
                if !key.is_empty() {
                    for (code, _) in LANGUAGES {
                        if !seen.contains(&code.to_string()) {
                            missing.push(format!("{key} ({code})"));
                        }
                    }
                }
                key = line.trim_end_matches(':').to_string();
                seen.clear();
            } else if let Some((code, _)) = line.trim().split_once(':') {
                seen.push(code.to_string());
            }
        }
        for (code, _) in LANGUAGES {
            if !key.is_empty() && !seen.contains(&code.to_string()) {
                missing.push(format!("{key} ({code})"));
            }
        }
        assert!(missing.is_empty(), "untranslated: {missing:?}");
    }
}
