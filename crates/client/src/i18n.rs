use std::collections::HashMap;
use std::sync::OnceLock;
use web_sys::window;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Language {
    #[default]
    En,
    Zh,
    Hi,
    Es,
    Fr,
    Ar,
    Bn,
    Pt,
    Ru,
    De,
}

impl Language {
    pub const ALL: [Language; 10] = [
        Language::En,
        Language::Zh,
        Language::Hi,
        Language::Es,
        Language::Fr,
        Language::Ar,
        Language::Bn,
        Language::Pt,
        Language::Ru,
        Language::De,
    ];

    pub fn code(&self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Zh => "zh",
            Language::Hi => "hi",
            Language::Es => "es",
            Language::Fr => "fr",
            Language::Ar => "ar",
            Language::Bn => "bn",
            Language::Pt => "pt",
            Language::Ru => "ru",
            Language::De => "de",
        }
    }

    pub fn native_name(&self) -> &'static str {
        match self {
            Language::En => "English",
            Language::Zh => "简体中文",
            Language::Hi => "हिन्दी",
            Language::Es => "Español",
            Language::Fr => "Français",
            Language::Ar => "العربية",
            Language::Bn => "বাংলা",
            Language::Pt => "Português",
            Language::Ru => "Русский",
            Language::De => "Deutsch",
        }
    }

    pub fn is_rtl(&self) -> bool {
        matches!(self, Language::Ar)
    }

    pub fn from_code(code: &str) -> Option<Language> {
        let clean = code.trim().to_lowercase();
        let prefix = clean.split(['-', '_']).next().unwrap_or(&clean);
        match prefix {
            "en" => Some(Language::En),
            "zh" => Some(Language::Zh),
            "hi" => Some(Language::Hi),
            "es" => Some(Language::Es),
            "fr" => Some(Language::Fr),
            "ar" => Some(Language::Ar),
            "bn" => Some(Language::Bn),
            "pt" => Some(Language::Pt),
            "ru" => Some(Language::Ru),
            "de" => Some(Language::De),
            _ => None,
        }
    }
}

static TRANSLATIONS_RAW: &str = include_str!("../translations.json");
static TRANSLATIONS: OnceLock<HashMap<String, HashMap<String, String>>> = OnceLock::new();

fn get_translations() -> &'static HashMap<String, HashMap<String, String>> {
    TRANSLATIONS.get_or_init(|| {
        serde_json::from_str(TRANSLATIONS_RAW)
            .expect("translations.json must be valid JSON matching language map")
    })
}

/// Lookup a localized string for a given language.
/// Automatically falls back to English if the key is missing in the requested language.
pub fn t(lang: Language, key: &'static str) -> &'static str {
    let map = get_translations();
    if let Some(val) = map.get(lang.code()).and_then(|m| m.get(key)) {
        return val.as_str();
    }
    if let Some(val) = map.get("en").and_then(|m| m.get(key)) {
        return val.as_str();
    }
    key
}

/// Helper for single-placeholder interpolation, e.g. replacing "{type}" with a value.
pub fn t_replace_1(lang: Language, key: &'static str, placeholder: &str, val: &str) -> String {
    let template = t(lang, key);
    template.replace(placeholder, val)
}

/// Detect browser locale using `window.navigator.languages` and `window.navigator.language`.
pub fn detect_browser_language() -> Language {
    if let Some(win) = window() {
        let nav = win.navigator();
        let languages = nav.languages();
        for i in 0..languages.length() {
            if let Some(lang_str) = languages.get(i).as_string() {
                if let Some(lang) = Language::from_code(&lang_str) {
                    return lang;
                }
            }
        }
        if let Some(lang_str) = nav.language() {
            if let Some(lang) = Language::from_code(&lang_str) {
                return lang;
            }
        }
    }
    Language::En
}

/// Dynamically update `dir` ("rtl" or "ltr") and `lang` attributes on the document root element.
pub fn update_document_direction(lang: Language) {
    if let Some(win) = window() {
        if let Some(doc) = win.document() {
            if let Some(root) = doc.document_element() {
                let dir = if lang.is_rtl() { "rtl" } else { "ltr" };
                let _ = root.set_attribute("dir", dir);
                let _ = root.set_attribute("lang", lang.code());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_10_languages_loaded_with_full_keys() {
        let map = get_translations();
        assert_eq!(map.len(), 10, "Must contain exactly 10 languages");

        let en_keys = map.get("en").expect("en must exist");
        assert!(en_keys.len() >= 70, "en must have all keys");

        for lang in Language::ALL {
            let lang_map = map.get(lang.code()).unwrap_or_else(|| panic!("Missing {}", lang.code()));
            for (key, _) in en_keys {
                assert!(
                    lang_map.contains_key(key),
                    "Language {} missing key '{}'",
                    lang.code(),
                    key
                );
            }
        }
    }

    #[test]
    fn test_language_code_parsing() {
        assert_eq!(Language::from_code("en"), Some(Language::En));
        assert_eq!(Language::from_code("en-US"), Some(Language::En));
        assert_eq!(Language::from_code("zh_CN"), Some(Language::Zh));
        assert_eq!(Language::from_code("es-ES"), Some(Language::Es));
        assert_eq!(Language::from_code("pt-BR"), Some(Language::Pt));
        assert_eq!(Language::from_code("ar-EG"), Some(Language::Ar));
        assert_eq!(Language::from_code("de-DE"), Some(Language::De));
        assert_eq!(Language::from_code("unknown_xyz"), None);
    }

    #[test]
    fn test_rtl_detection() {
        assert!(Language::Ar.is_rtl());
        assert!(!Language::En.is_rtl());
        assert!(!Language::Es.is_rtl());
        assert!(!Language::Zh.is_rtl());
    }

    #[test]
    fn test_interpolation() {
        assert_eq!(t_replace_1(Language::En, "sys_joined", "{name}", "Ana"), "Ana joined");
        for lang in Language::ALL {
            let joined = t_replace_1(lang, "sys_joined", "{name}", "Ana");
            assert!(joined.contains("Ana") && !joined.contains("{name}"), "{}", lang.code());
        }
    }

    #[test]
    fn test_fallback_behavior() {
        assert_eq!(t(Language::En, "scan_qr"), "📱 Scan QR");
        assert_eq!(t(Language::Es, "scan_qr"), "📱 Escanear QR");
        // Unknown key falls back to key
        assert_eq!(t(Language::En, "unknown_key_xyz"), "unknown_key_xyz");
    }
}
