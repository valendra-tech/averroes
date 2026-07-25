use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct KeyBinding {
    pub key: String,
    pub modifiers: Vec<String>,
    pub action: String,
}

pub struct ShortcutMap {
    bindings: HashMap<String, Vec<KeyBinding>>,
}

impl ShortcutMap {
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }

    pub fn defaults() -> Self {
        let mut map = Self::new();
        map.bind(
            "global",
            KeyBinding {
                key: "q".into(),
                modifiers: vec!["cmd".into()],
                action: "quit".into(),
            },
        );
        map.bind(
            "global",
            KeyBinding {
                key: "enter".into(),
                modifiers: vec!["cmd".into()],
                action: "send_message".into(),
            },
        );
        map
    }

    pub fn bind(&mut self, context: &str, binding: KeyBinding) {
        self.bindings
            .entry(context.into())
            .or_default()
            .push(binding);
    }
}
