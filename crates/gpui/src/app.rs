pub struct AverroesApp {
    pub sessions: Vec<String>,
    pub current_session: Option<String>,
}

impl AverroesApp {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            current_session: None,
        }
    }
}
