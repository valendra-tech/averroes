#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab {
    Files,
    Context,
    Changes,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InspectorState {
    pub active: Option<InspectorTab>,
}

impl InspectorState {
    pub fn select(&mut self, tab: InspectorTab) {
        self.active = Some(tab);
    }
    pub fn toggle(&mut self, tab: InspectorTab) {
        self.active = if self.active == Some(tab) {
            None
        } else {
            Some(tab)
        };
    }
    pub fn close(&mut self) {
        self.active = None;
    }
    pub fn files_open(&self) -> bool {
        self.active == Some(InspectorTab::Files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_tabs_keeps_one_inspector_open_until_explicitly_closed() {
        let mut state = InspectorState::default();
        state.select(InspectorTab::Files);
        state.select(InspectorTab::Context);
        assert_eq!(state.active, Some(InspectorTab::Context));
        state.select(InspectorTab::Changes);
        assert_eq!(state.active, Some(InspectorTab::Changes));
        assert!(!state.files_open());
        state.toggle(InspectorTab::Files);
        assert!(state.files_open());
        state.toggle(InspectorTab::Files);
        assert_eq!(state.active, None);
    }
}
