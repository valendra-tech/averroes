use gpui::{App, Global, SharedString};
use std::collections::HashMap;

/// Languages shipped with the desktop application.
///
/// The catalog is embedded at compile time so changing the language never
/// performs filesystem or network I/O during a render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    English,
    Spanish,
}

impl Locale {
    pub fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Spanish => "Español",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::English => Self::Spanish,
            Self::Spanish => Self::English,
        }
    }

    /// Maps a system locale tag such as `es-ES` to a shipped catalog.
    /// Unsupported languages intentionally fall back to English.
    pub fn from_tag(tag: &str) -> Self {
        let tag = tag.trim().to_ascii_lowercase();
        let language = tag.split(['-', '_']).next().unwrap_or_default();
        if language == "es" {
            Self::Spanish
        } else {
            Self::English
        }
    }

    pub fn system_default() -> Self {
        sys_locale::get_locale()
            .map(|tag| Self::from_tag(&tag))
            .unwrap_or(Self::English)
    }
}

#[derive(Clone)]
pub struct Localization {
    locale: Locale,
    messages: HashMap<String, SharedString>,
}

impl Global for Localization {}

impl Localization {
    pub fn new(locale: Locale) -> Self {
        let source = match locale {
            Locale::English => include_str!("../locales/en.json"),
            Locale::Spanish => include_str!("../locales/es.json"),
        };
        let messages = serde_json::from_str::<HashMap<String, String>>(source)
            .expect("embedded localization catalog must be valid JSON")
            .into_iter()
            .map(|(key, value)| {
                (
                    key,
                    SharedString::new(value.replace("{app_name}", crate::brand::APP_NAME)),
                )
            })
            .collect();
        Self { locale, messages }
    }

    pub fn locale(&self) -> Locale {
        self.locale
    }

    pub fn text(&self, key: &str) -> SharedString {
        self.messages
            .get(key)
            .cloned()
            .unwrap_or_else(|| SharedString::new(key))
    }

    pub fn format(&self, key: &str, values: &[(&str, String)]) -> String {
        let mut text = self.text(key).to_string();
        for (name, value) in values {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }
}

pub fn text(cx: &App, key: &str) -> SharedString {
    cx.global::<Localization>().text(key)
}

pub fn format(cx: &App, key: &str, values: &[(&str, String)]) -> String {
    cx.global::<Localization>().format(key, values)
}

#[cfg(test)]
mod tests {
    use super::{Locale, Localization};

    #[test]
    fn product_identity_is_resolved_in_both_embedded_catalogs() {
        for locale in [Locale::English, Locale::Spanish] {
            let localization = Localization::new(locale);
            assert_eq!(
                localization.text("sidebar.brand").as_ref(),
                "QDivZero Harness"
            );
            assert!(localization
                .text("app.needs_attention")
                .contains("QDivZero Harness"));
            assert!(localization
                .messages
                .values()
                .all(|text| !text.contains("{app_name}") && !text.contains("Averroes")));
        }
    }

    #[test]
    fn maps_spanish_locale_tags_to_the_spanish_catalog() {
        assert_eq!(Locale::from_tag("es-ES"), Locale::Spanish);
        assert_eq!(Locale::from_tag("es_MX"), Locale::Spanish);
        assert_eq!(Locale::from_tag("ES"), Locale::Spanish);
    }

    #[test]
    fn falls_back_to_english_for_unsupported_locales() {
        assert_eq!(Locale::from_tag("en-US"), Locale::English);
        assert_eq!(Locale::from_tag("fr-FR"), Locale::English);
        assert_eq!(Locale::from_tag(""), Locale::English);
    }
}
