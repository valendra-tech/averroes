use gpui::{px, rgb, App, Font, FontFallbacks, Rgba, Window};

#[derive(Debug, Clone, Copy)]
pub struct UiTheme {
    pub background: Rgba,
    pub rail: Rgba,
    pub chrome_opaque: Rgba,
    pub canvas: Rgba,
    pub chrome_tint: Rgba,
    pub rim: Rgba,
    pub chrome_shadow: Rgba,
    pub paper_canvas: Rgba,
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
        let accent = rgb(0x006DDD);
        let success = rgb(0x30D158);
        let warning = rgb(0xFF9F0A);
        let destructive = rgb(0xFF453A);
        Self {
            background: rgb(0x1D1E23),
            rail: rgb(0x262831).opacity(0.76),
            chrome_opaque: rgb(0x282A31),
            canvas: rgb(0x0C0F16).opacity(0.25),
            chrome_tint: rgb(0x262831).opacity(0.76),
            rim: rgb(0xFFFFFF).opacity(0.14),
            chrome_shadow: rgb(0x000000).opacity(0.20),
            paper_canvas: rgb(0x292B32),
            surface: rgb(0x25272E),
            surface_subtle: rgb(0x282A31),
            surface_hover: rgb(0x353840),
            foreground: rgb(0xEEEFF3),
            muted: rgb(0xB0B3BF),
            faint: rgb(0xA9ADB8),
            border: rgb(0x3A3A3C),
            hairline: rgb(0xFFFFFF).opacity(0.08),
            accent,
            accent_hover: rgb(0x0070E0),
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
        let accent = rgb(0x0067D9);
        let success = rgb(0x34C759);
        let warning = rgb(0xFF9500);
        let destructive = rgb(0xFF3B30);
        Self {
            background: rgb(0xFDFDFD),
            rail: rgb(0xFBFBFD).opacity(0.70),
            chrome_opaque: rgb(0xF3F3F5),
            canvas: rgb(0xFFFFFF).opacity(0.17),
            chrome_tint: rgb(0xFBFBFD).opacity(0.70),
            rim: rgb(0xFFFFFF).opacity(0.82),
            chrome_shadow: rgb(0x141E32).opacity(0.07),
            paper_canvas: rgb(0xE9EAEE),
            surface: rgb(0xFFFFFF),
            surface_subtle: rgb(0xF3F3F5),
            surface_hover: rgb(0xE8E9ED),
            foreground: rgb(0x202127),
            muted: rgb(0x5D616C),
            faint: rgb(0x5D616C),
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

    pub fn canvas_fill(self, mode: super::material::MaterialMode) -> Rgba {
        if mode == super::material::MaterialMode::Opaque {
            self.chrome_opaque
        } else {
            self.canvas
        }
    }

    pub fn chrome_fill(self, mode: super::material::MaterialMode) -> Rgba {
        if mode == super::material::MaterialMode::Opaque {
            self.chrome_opaque
        } else {
            self.chrome_tint
        }
    }

    pub fn navigation_fill(self, mode: super::material::MaterialMode) -> Rgba {
        if mode == super::material::MaterialMode::Opaque {
            self.chrome_opaque
        } else {
            // Automatic delegates the panel material to AppKit.
            gpui::rgba(0)
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
        // Pane chrome and ordinary form controls stay restrained.
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
            ("faint/rail", theme.faint, theme.rail),
            ("muted/rail", theme.muted, theme.rail),
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
            ("faint/rail", theme.faint, theme.rail),
            ("muted/rail", theme.muted, theme.rail),
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
    fn opaque_sidebar_fallback_preserves_readable_text() {
        use crate::ui::material::MaterialMode;
        for theme in [UiTheme::light(), UiTheme::dark()] {
            let rendered = theme.navigation_fill(MaterialMode::Opaque);
            for (label, color) in [
                ("foreground", theme.foreground),
                ("muted", theme.muted),
                ("faint", theme.faint),
            ] {
                assert_text_contrast(&theme, label, color, rendered);
            }
        }
    }

    #[test]
    fn gpui_appearance_maps_to_the_expected_component_mode() {
        assert!(!gpui_component::ThemeMode::from(gpui::WindowAppearance::Light).is_dark());
        assert!(gpui_component::ThemeMode::from(gpui::WindowAppearance::Dark).is_dark());
    }

    #[test]
    fn primary_action_labels_are_readable_in_both_appearances() {
        for theme in [UiTheme::light(), UiTheme::dark()] {
            assert_text_contrast(&theme, "primary label", rgb(0xFFFFFF), theme.accent);
            assert_text_contrast(
                &theme,
                "hovered primary label",
                rgb(0xFFFFFF),
                theme.accent_hover,
            );
        }
    }
}
