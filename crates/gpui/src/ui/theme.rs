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
    pub accent_soft: Rgba,
    pub focus_ring: Rgba,
    pub success: Rgba,
    pub success_soft: Rgba,
    pub warning: Rgba,
    pub destructive: Rgba,
    pub destructive_soft: Rgba,
}

impl UiTheme {
    pub const RADIUS: f32 = 10.0;

    pub fn dark() -> Self {
        Self {
            background: rgb(0x171716),
            rail: rgb(0x1c1c1b),
            surface: rgb(0x222220),
            surface_subtle: rgb(0x262624),
            surface_hover: rgb(0x2e2e2c),
            foreground: rgb(0xf3f3f0),
            muted: rgb(0xb7b7b1),
            faint: rgb(0x85857e),
            border: rgb(0x343432),
            accent: rgb(0xe8e8e5),
            accent_hover: rgb(0xffffff),
            accent_soft: rgb(0x3b415e),
            focus_ring: rgb(0xaeb7e8),
            success: rgb(0x8fc69c),
            success_soft: rgb(0x25362c),
            warning: rgb(0xd8a45f),
            destructive: rgb(0xe17a72),
            destructive_soft: rgb(0x3d2526),
        }
    }

    pub fn light() -> Self {
        Self {
            background: rgb(0xfdfdfc),
            rail: rgb(0xf7f7f5),
            surface: rgb(0xffffff),
            surface_subtle: rgb(0xf5f5f2),
            surface_hover: rgb(0xecece8),
            foreground: rgb(0x20201e),
            muted: rgb(0x6f6f69),
            faint: rgb(0x989891),
            border: rgb(0xe5e5e0),
            accent: rgb(0x171717),
            accent_hover: rgb(0x000000),
            accent_soft: rgb(0xe5eef3),
            focus_ring: rgb(0x7b83a8),
            success: rgb(0x367a48),
            success_soft: rgb(0xe6f3e9),
            warning: rgb(0xa46616),
            destructive: rgb(0xb8423b),
            destructive_soft: rgb(0xf9e7e5),
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
        theme.font_size = px(14.0);
        theme.mono_font_size = px(13.0);
        theme.radius = px(Self::RADIUS);
        theme.radius_lg = px(14.0);
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
        theme.primary_foreground = palette.background.into();
        theme.button_primary = palette.accent.into();
        theme.button_primary_hover = palette.accent_hover.into();
        theme.button_primary_active = palette.accent.into();
        theme.button_primary_foreground = palette.background.into();
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
        // gpui-component paints the selection quad after the text layout. Keep
        // it translucent so selecting a message never covers the glyphs.
        // The light palette needs a little more opacity to remain visible;
        // the dark palette already has enough contrast at a lower alpha.
        let selection_opacity = if mode.is_dark() { 0.62 } else { 0.72 };
        theme.selection = palette.accent_soft.opacity(selection_opacity).into();
        theme.sidebar = palette.rail.into();
        theme.sidebar_foreground = palette.foreground.into();
        theme.sidebar_border = palette.border.into();
        theme.sidebar_accent = palette.surface_hover.into();
        theme.sidebar_accent_foreground = palette.foreground.into();
        theme.sidebar_primary = palette.accent.into();
        theme.sidebar_primary_foreground = palette.background.into();
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
    pub const MONO_FONT: &'static str = "Menlo";

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
        Self::font_with_fallbacks(Self::MONO_FONT, &["Monaco", "SFMono-Regular", "monospace"])
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
