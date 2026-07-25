use gpui::{rgb, Rgba};

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
}

impl Default for UiTheme {
    fn default() -> Self {
        Self::light()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: Rgba,
    pub surface: Rgba,
    pub border: Rgba,
    pub fg: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
    pub success: Rgba,
    pub error: Rgba,
}

impl Theme {
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

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}
