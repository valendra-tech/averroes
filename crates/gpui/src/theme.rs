use gpui::rgb;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: gpui::Rgba,
    pub surface: gpui::Rgba,
    pub border: gpui::Rgba,
    pub fg: gpui::Rgba,
    pub muted: gpui::Rgba,
    pub accent: gpui::Rgba,
    pub success: gpui::Rgba,
    pub error: gpui::Rgba,
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
        Self {
            bg: rgb(0xeff1f5),
            surface: rgb(0xe6e9ef),
            border: rgb(0xccd0da),
            fg: rgb(0x4c4f69),
            muted: rgb(0x9ca0b0),
            accent: rgb(0x1e66f5),
            success: rgb(0x40a02b),
            error: rgb(0xd20f39),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}
