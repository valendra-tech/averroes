pub struct FileTree {
    pub root: String,
    pub entries: Vec<String>,
}

impl FileTree {
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            entries: Vec::new(),
        }
    }
}
