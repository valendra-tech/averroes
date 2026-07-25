use gpui::rgb;
use gpui::{Font, FontFallbacks, Rgba};

#[derive(Debug, Clone, Copy)]
pub struct UiTheme {
    pub background: Rgba,
    pub foreground: Rgba,
    pub card: Rgba,
    pub primary: Rgba,
    pub brand_orange: Rgba,
    pub brand_coral: Rgba,
    pub brand_magenta: Rgba,
    pub muted_foreground: Rgba,
    pub border: Rgba,
    pub accent: Rgba,
    pub destructive: Rgba,
}

impl UiTheme {
    pub const RADIUS: f32 = 6.0;

    pub fn light() -> Self {
        Self {
            background: rgb(0xfff9f4),
            foreground: rgb(0x20131a),
            card: rgb(0xffffff),
            primary: rgb(0xb83a27),
            brand_orange: rgb(0xf15a2a),
            brand_coral: rgb(0xe94b2f),
            brand_magenta: rgb(0xd94b83),
            muted_foreground: rgb(0x725f5b),
            border: rgb(0xead8ce),
            accent: rgb(0xffe4d5),
            destructive: rgb(0xb42318),
        }
    }

    pub const UI_FONT: &'static str = "Inter";
    pub const DISPLAY_FONT: &'static str = "Space Grotesk";
    pub const MONO_FONT: &'static str = "IBM Plex Mono";

    fn font_with_fallbacks(family: &'static str, fallbacks: &[&'static str]) -> Font {
        Font {
            family: family.into(),
            features: Default::default(),
            fallbacks: Some(FontFallbacks::from_fonts(
                fallbacks
                    .iter()
                    .map(|fallback| (*fallback).to_string())
                    .collect(),
            )),
            weight: Default::default(),
            style: Default::default(),
        }
    }

    pub fn ui_font() -> Font {
        Self::font_with_fallbacks(
            Self::UI_FONT,
            &[
                "-apple-system",
                "BlinkMacSystemFont",
                "Segoe UI",
                "sans-serif",
            ],
        )
    }

    pub fn display_font() -> Font {
        Self::font_with_fallbacks(
            Self::DISPLAY_FONT,
            &["Inter", "-apple-system", "sans-serif"],
        )
    }

    pub fn mono_font() -> Font {
        Self::font_with_fallbacks(
            Self::MONO_FONT,
            &["SFMono-Regular", "Menlo", "Monaco", "monospace"],
        )
    }
}

impl Default for UiTheme {
    fn default() -> Self {
        Self::light()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LegacyTheme {
    pub bg: Rgba,
    pub surface: Rgba,
    pub border: Rgba,
    pub fg: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
    pub success: Rgba,
    pub error: Rgba,
}

pub type Theme = LegacyTheme;

impl LegacyTheme {
    pub fn dark() -> Self {
        Self {
            bg: rgb(0x1e1e2e),
            surface: rgb(0x313244),
            border: rgb(0x45475a),
            fg: rgb(0xcdd6f4),
            muted: rgb(0x6c7086),
            accent: rgb(0x89b4fa),
            success: rgb(0xa6e3a1),
            error: rgb(0xf38ba8),
        }
    }

    pub fn light() -> Self {
        let theme = UiTheme::light();
        Self {
            bg: theme.background,
            surface: theme.card,
            border: theme.border,
            fg: theme.foreground,
            muted: theme.muted_foreground,
            accent: theme.primary,
            success: theme.brand_coral,
            error: theme.destructive,
        }
    }
}

impl Default for LegacyTheme {
    fn default() -> Self {
        Self::light()
    }
}

#[cfg(test)]
mod tests {
    use super::{Theme, UiTheme};
    use gpui::Font;

    fn assert_font(font: Font, family: &str, fallbacks: &[&str]) {
        assert_eq!(font.family.as_str(), family);
        let actual = font.fallbacks.expect("font fallbacks");
        let expected = fallbacks
            .iter()
            .map(|fallback| fallback.to_string())
            .collect::<Vec<_>>();
        assert_eq!(actual.fallback_list(), expected.as_slice());
    }

    #[test]
    fn font_helpers_define_platform_fallbacks() {
        assert_font(
            UiTheme::ui_font(),
            UiTheme::UI_FONT,
            &[
                "-apple-system",
                "BlinkMacSystemFont",
                "Segoe UI",
                "sans-serif",
            ],
        );
        assert_font(
            UiTheme::display_font(),
            UiTheme::DISPLAY_FONT,
            &["Inter", "-apple-system", "sans-serif"],
        );
        assert_font(
            UiTheme::mono_font(),
            UiTheme::MONO_FONT,
            &["SFMono-Regular", "Menlo", "Monaco", "monospace"],
        );
    }

    #[test]
    fn legacy_theme_defaults_to_light_tokens() {
        let legacy = Theme::default();
        let light = UiTheme::light();

        assert_eq!(legacy.bg, light.background);
        assert_eq!(legacy.surface, light.card);
        assert_eq!(legacy.border, light.border);
        assert_eq!(legacy.fg, light.foreground);
        assert_eq!(legacy.muted, light.muted_foreground);
        assert_eq!(legacy.accent, light.primary);
        assert_eq!(legacy.success, light.brand_coral);
        assert_eq!(legacy.error, light.destructive);
    }
}
