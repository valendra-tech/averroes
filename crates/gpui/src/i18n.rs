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
            .map(|(key, value)| (key, SharedString::new(value)))
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
