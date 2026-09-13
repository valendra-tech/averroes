use gpui::{px, rgb, App, Font, FontFallbacks, Rgba, Window};

#[derive(Debug, Clone, Copy)]
pub struct UiTheme {
    pub background: Rgba,
    pub rail: Rgba,
    pub surface: Rgba,
    pub surface_subtle: Rgba,
    pub surface_hover: Rgba,
    pub foreground: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    pub border: Rgba,
    pub accent: Rgba,
    pub accent_hover: Rgba,
    pub accent_text: Rgba,
    pub accent_soft: Rgba,
    pub selection: Rgba,
    pub hairline: Rgba,
    pub focus_ring: Rgba,
    pub success: Rgba,
    pub success_text: Rgba,
    pub success_soft: Rgba,
    pub warning: Rgba,
    pub warning_text: Rgba,
    pub warning_soft: Rgba,
    pub destructive: Rgba,
    pub destructive_text: Rgba,
    pub destructive_soft: Rgba,
}

impl UiTheme {
    pub const RADIUS: f32 = crate::ui::tokens::RADIUS_CARD;

    pub fn dark() -> Self {
        let accent = rgb(0x0A84FF);
        let success = rgb(0x30D158);
        let warning = rgb(0xFF9F0A);
        let destructive = rgb(0xFF453A);
        Self {
            background: rgb(0x1E1E1E),
            rail: rgb(0x2A2A2C).opacity(0.72),
            surface: rgb(0x2C2C2E),
            surface_subtle: rgb(0x232325),
            surface_hover: rgb(0x3A3A3C),
            foreground: rgb(0xFFFFFF),
            muted: rgb(0xA1A1A6),
            faint: rgb(0x98989D),
            border: rgb(0x3A3A3C),
            hairline: rgb(0xFFFFFF).opacity(0.08),
            accent,
            accent_hover: rgb(0x409CFF),
            accent_text: rgb(0x409CFF),
            accent_soft: accent.opacity(0.22),
            selection: accent.opacity(0.28),
            focus_ring: rgb(0x0A84FF),
            success,
            success_text: success,
            success_soft: success.opacity(0.18),
            warning,
            warning_text: warning,
            warning_soft: warning.opacity(0.18),
            destructive,
            destructive_text: rgb(0xFF6961),
            destructive_soft: destructive.opacity(0.18),
        }
    }

    pub fn light() -> Self {
        let accent = rgb(0x007AFF);
        let success = rgb(0x34C759);
        let warning = rgb(0xFF9500);
        let destructive = rgb(0xFF3B30);
        Self {
            background: rgb(0xFFFFFF),
            rail: rgb(0xECECEC).opacity(0.72),
            surface: rgb(0xFFFFFF),
            surface_subtle: rgb(0xF5F5F7),
            surface_hover: rgb(0xE5E5EA),
            foreground: rgb(0x1D1D1F),
            muted: rgb(0x5A5A5F),
            faint: rgb(0x69696E),
            border: rgb(0xD1D1D6),
            hairline: rgb(0x000000).opacity(0.08),
            accent,
            accent_hover: rgb(0x0066D6),
            accent_text: rgb(0x0040DD),
            accent_soft: accent.opacity(0.14),
            selection: accent.opacity(0.18),
            focus_ring: rgb(0x007AFF),
            success,
            success_text: rgb(0x1F7A35),
            success_soft: success.opacity(0.14),
            warning,
            warning_text: rgb(0xC93400),
            warning_soft: warning.opacity(0.14),
            destructive,
            destructive_text: rgb(0xD70015),
            destructive_soft: destructive.opacity(0.12),
        }
    }

    pub fn current(cx: &App) -> Self {
        if Self::is_dark(cx) {
            Self::dark()
        } else {
            Self::light()
        }
    }

    pub fn is_dark(cx: &App) -> bool {
        gpui_component::Theme::global(cx).is_dark()
    }

    pub fn install_component_theme(cx: &mut App) {
        use gpui_component::ThemeMode;

        let mode = ThemeMode::from(cx.window_appearance());
        Self::apply_component_theme(mode, cx);
    }

    pub fn sync_component_theme(window: &mut Window, cx: &mut App) {
        use gpui_component::ThemeMode;

        let mode = ThemeMode::from(window.appearance());
        Self::apply_component_theme(mode, cx);
        window.refresh();
    }

    fn apply_component_theme(mode: gpui_component::ThemeMode, cx: &mut App) {
        use gpui_component::Theme;

        Theme::change(mode, None, cx);
        let palette = if mode.is_dark() {
            Self::dark()
        } else {
            Self::light()
        };
        let theme = Theme::global_mut(cx);
        theme.mode = mode;
        theme.font_family = Self::UI_FONT.into();
        theme.mono_font_family = Self::MONO_FONT.into();
        theme.font_size = px(crate::ui::tokens::TEXT_BODY);
        theme.mono_font_size = px(crate::ui::tokens::TEXT_SMALL);
        theme.radius = px(crate::ui::tokens::RADIUS_CONTROL);
        theme.radius_lg = px(crate::ui::tokens::RADIUS_SHEET);
        // This is deliberately a flat workspace. Elevation is provided by
        // color and borders, not a blanket layer of drop shadows.
        theme.shadow = false;
        theme.focus_ring = true;

        theme.background = palette.background.into();
        theme.foreground = palette.foreground.into();
        theme.border = palette.border.into();
        theme.input = palette.border.into();
        theme.muted = palette.surface_hover.into();
        theme.muted_foreground = palette.muted.into();
        theme.accent = palette.surface_hover.into();
        theme.accent_foreground = palette.foreground.into();
        theme.secondary = palette.surface_subtle.into();
        theme.secondary_hover = palette.surface_hover.into();
        theme.secondary_active = palette.surface_hover.into();
        theme.secondary_foreground = palette.foreground.into();
        theme.primary = palette.accent.into();
        theme.primary_hover = palette.accent_hover.into();
        theme.primary_active = palette.accent.into();
        theme.primary_foreground = rgb(0xFFFFFF).into();
        theme.button_primary = palette.accent.into();
        theme.button_primary_hover = palette.accent_hover.into();
        theme.button_primary_active = palette.accent.into();
        theme.button_primary_foreground = rgb(0xFFFFFF).into();
        theme.button = palette.surface_hover.into();
        theme.button_hover = palette.border.into();
        theme.button_active = palette.surface_subtle.into();
        theme.button_foreground = palette.foreground.into();
        theme.button_secondary = palette.surface_hover.into();
        theme.button_secondary_hover = palette.border.into();
        theme.button_secondary_active = palette.surface_subtle.into();
        theme.button_secondary_foreground = palette.foreground.into();
        theme.popover = palette.surface.into();
        theme.popover_foreground = palette.foreground.into();
        theme.colors.list = palette.surface.into();
        theme.list_hover = palette.surface_hover.into();
        theme.list_active = palette.accent_soft.into();
        theme.list_active_border = palette.accent.into();
        theme.ring = palette.focus_ring.into();
        theme.selection = palette.selection.into();
        theme.sidebar = palette.rail.into();
        theme.sidebar_foreground = palette.foreground.into();
        theme.sidebar_border = palette.border.into();
        theme.sidebar_accent = palette.surface_hover.into();
        theme.sidebar_accent_foreground = palette.foreground.into();
        theme.sidebar_primary = palette.accent.into();
        theme.sidebar_primary_foreground = rgb(0xFFFFFF).into();
        theme.title_bar = palette.rail.into();
        theme.title_bar_border = palette.border.into();
        theme.status_bar = palette.rail.into();
        theme.status_bar_border = palette.border.into();
        theme.success = palette.success.into();
        theme.danger = palette.destructive.into();
        theme.warning = palette.warning.into();
        Theme::sync_base(cx);
    }

    // Native macOS families keep the app crisp at small UI sizes and avoid
    // depending on bundled web fonts being present on a user's machine.
    pub const UI_FONT: &'static str = ".SystemUIFont";
    pub const DISPLAY_FONT: &'static str = ".SystemUIFont";
    pub const MONO_FONT: &'static str = ".SF NS Mono";

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
            &["Helvetica Neue", "Segoe UI", "Arial", "sans-serif"],
        )
    }

    pub fn display_font() -> Font {
        Self::font_with_fallbacks(
            Self::DISPLAY_FONT,
            &["Helvetica Neue", "Segoe UI", "Arial", "sans-serif"],
        )
    }

    pub fn mono_font() -> Font {
        Self::font_with_fallbacks(Self::MONO_FONT, &["Menlo", "Monaco", "monospace"])
    }
}

impl Default for UiTheme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative_luminance(color: Rgba) -> f32 {
        fn channel(value: f32) -> f32 {
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
    }

    fn contrast_ratio(a: Rgba, b: Rgba) -> f32 {
        let la = relative_luminance(a);
        let lb = relative_luminance(b);
        let (lighter, darker) = if la >= lb { (la, lb) } else { (lb, la) };
        (lighter + 0.05) / (darker + 0.05)
    }

    fn assert_text_contrast(_theme: &UiTheme, label: &str, fg: Rgba, bg: Rgba) {
        let ratio = contrast_ratio(fg, bg);
        assert!(
            ratio >= 4.5,
            "{label} contrast is {ratio:.2}:1, expected at least 4.5:1"
        );
    }

    #[test]
    fn dark_palette_text_meets_wcag_aa() {
        let theme = UiTheme::dark();
        for (label, fg, bg) in [
            ("foreground/background", theme.foreground, theme.background),
            ("foreground/surface", theme.foreground, theme.surface),
            ("muted/background", theme.muted, theme.background),
            ("muted/surface", theme.muted, theme.surface),
            ("faint/background", theme.faint, theme.background),
            ("faint/surface", theme.faint, theme.surface),
            ("accent_text/surface", theme.accent_text, theme.surface),
            ("success_text/surface", theme.success_text, theme.surface),
            ("warning_text/surface", theme.warning_text, theme.surface),
            (
                "destructive_text/surface",
                theme.destructive_text,
                theme.surface,
            ),
        ] {
            assert_text_contrast(&theme, &format!("dark {label}"), fg, bg);
        }
    }

    #[test]
    fn light_palette_text_meets_wcag_aa() {
        let theme = UiTheme::light();
        for (label, fg, bg) in [
            ("foreground/background", theme.foreground, theme.background),
            ("foreground/surface", theme.foreground, theme.surface),
            ("muted/background", theme.muted, theme.background),
            ("muted/surface", theme.muted, theme.surface),
            ("faint/background", theme.faint, theme.background),
            ("faint/surface", theme.faint, theme.surface),
            ("accent_text/surface", theme.accent_text, theme.surface),
            ("success_text/surface", theme.success_text, theme.surface),
            ("warning_text/surface", theme.warning_text, theme.surface),
            (
                "destructive_text/surface",
                theme.destructive_text,
                theme.surface,
            ),
        ] {
            assert_text_contrast(&theme, &format!("light {label}"), fg, bg);
        }
    }

    #[test]
    fn light_and_dark_palettes_have_readable_contrast_directions() {
        let light = UiTheme::light();
        let dark = UiTheme::dark();
        assert!(light.background.r > light.foreground.r);
        assert!(dark.background.r < dark.foreground.r);
        assert_ne!(light.rail, dark.rail);
    }

    #[test]
    fn gpui_appearance_maps_to_the_expected_component_mode() {
        assert!(!gpui_component::ThemeMode::from(gpui::WindowAppearance::Light).is_dark());
        assert!(gpui_component::ThemeMode::from(gpui::WindowAppearance::Dark).is_dark());
    }
}
