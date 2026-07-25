pub struct DiffView {
    pub original: String,
    pub modified: String,
}

impl DiffView {
    pub fn new() -> Self {
        Self {
            original: String::new(),
            modified: String::new(),
        }
    }
}
