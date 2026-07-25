#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub bg: String,
    pub fg: String,
    pub accent: String,
    pub error: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "dark".into(),
            bg: "#1e1e2e".into(),
            fg: "#cdd6f4".into(),
            accent: "#89b4fa".into(),
            error: "#f38ba8".into(),
        }
    }
}

impl Theme {
    pub fn light() -> Self {
        Self {
            name: "light".into(),
            bg: "#eff1f5".into(),
            fg: "#4c4f69".into(),
            accent: "#1e66f5".into(),
            error: "#d20f39".into(),
        }
    }
}
