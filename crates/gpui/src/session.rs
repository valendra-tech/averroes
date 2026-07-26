use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct SessionTab {
    pub id: SessionId,
    pub title: String,
    pub dirty: bool,
    pub workspace_id: Option<String>,
}

pub struct SessionManager {
    tabs: Vec<SessionTab>,
    active: usize,
    workspace_id: Option<String>,
}

impl SessionManager {
    pub fn new(workspace_id: Option<String>) -> Self {
        Self {
            tabs: vec![new_tab(workspace_id.clone())],
            active: 0,
            workspace_id,
        }
    }

    pub fn tabs(&self) -> &[SessionTab] {
        &self.tabs
    }

    pub fn active(&self) -> &SessionTab {
        &self.tabs[self.active]
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn select(&mut self, id: &SessionId) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| &tab.id == id) else {
            return false;
        };

        self.active = index;
        true
    }

    pub fn new_session(&mut self) -> SessionId {
        let tab = new_tab(self.workspace_id.clone());
        let id = tab.id.clone();
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        id
    }

    pub fn close(&mut self, id: &SessionId) -> SessionId {
        self.try_close(id)
            .unwrap_or_else(|| self.active().id.clone())
    }

    pub fn try_close(&mut self, id: &SessionId) -> Option<SessionId> {
        let Some(index) = self.tabs.iter().position(|tab| &tab.id == id) else {
            return None;
        };

        if self.tabs.len() == 1 {
            let tab = new_tab(self.workspace_id.clone());
            let id = tab.id.clone();
            self.tabs[0] = tab;
            self.active = 0;
            return Some(id);
        }

        self.tabs.remove(index);
        if index < self.active {
            self.active -= 1;
        } else if index == self.active {
            self.active = index.saturating_sub(1);
        }

        Some(self.active().id.clone())
    }

    pub fn rename_active(&mut self, title: impl Into<String>) {
        let id = self.active().id.clone();
        self.rename(&id, title);
    }

    pub fn set_dirty(&mut self, id: &SessionId, dirty: bool) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.id == id) {
            tab.dirty = dirty;
        }
    }

    pub fn set_workspace_id(&mut self, id: &SessionId, workspace_id: String) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.id == id) {
            tab.workspace_id = Some(workspace_id);
        }
    }

    pub fn rename(&mut self, id: &SessionId, title: impl Into<String>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.id == id) {
            tab.title = title.into();
        }
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new(None)
    }
}

fn new_tab(workspace_id: Option<String>) -> SessionTab {
    SessionTab {
        id: SessionId(Uuid::new_v4().to_string()),
        title: "New session".into(),
        dirty: false,
        workspace_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_selects_new_tab() {
        let mut manager = SessionManager::new(None);
        let id = manager.new_session();
        assert_eq!(manager.tabs().len(), 2);
        assert_eq!(manager.active().id, id);
        assert_eq!(manager.active_index(), 1);
    }

    #[test]
    fn closing_active_tab_selects_nearest_remaining_tab() {
        let mut manager = SessionManager::new(None);
        let first = manager.active().id.clone();
        let second = manager.new_session();
        manager.new_session();
        manager.select(&second);
        manager.close(&second);
        assert_eq!(manager.active().id, first);
    }

    #[test]
    fn closing_last_tab_keeps_one_fresh_session() {
        let mut manager = SessionManager::new(None);
        let id = manager.active().id.clone();
        manager.close(&id);
        assert_eq!(manager.tabs().len(), 1);
        assert_eq!(manager.active().title, "New session");
    }

    #[test]
    fn selecting_unknown_tab_returns_false_and_preserves_active_tab() {
        let mut manager = SessionManager::new(None);
        let active = manager.active().id.clone();
        let unknown = SessionId("unknown".into());

        assert!(!manager.select(&unknown));
        assert_eq!(manager.active().id, active);
    }

    #[test]
    fn try_close_unknown_tab_returns_none_and_preserves_state() {
        let mut manager = SessionManager::new(None);
        let active = manager.active().id.clone();
        let unknown = SessionId("unknown".into());

        assert!(manager.try_close(&unknown).is_none());
        assert_eq!(manager.tabs().len(), 1);
        assert_eq!(manager.active().id, active);
    }

    #[test]
    fn set_dirty_updates_the_requested_tab() {
        let mut manager = SessionManager::new(None);
        let first = manager.active().id.clone();
        let second = manager.new_session();

        manager.set_dirty(&first, true);

        assert!(
            manager
                .tabs()
                .iter()
                .find(|tab| tab.id == first)
                .unwrap()
                .dirty
        );
        assert!(
            !manager
                .tabs()
                .iter()
                .find(|tab| tab.id == second)
                .unwrap()
                .dirty
        );
    }

    #[test]
    fn rename_updates_the_requested_tab() {
        let mut manager = SessionManager::new(None);
        let first = manager.active().id.clone();
        let second = manager.new_session();

        manager.rename(&first, "First session");

        assert_eq!(
            manager
                .tabs()
                .iter()
                .find(|tab| tab.id == first)
                .unwrap()
                .title,
            "First session"
        );
        assert_eq!(
            manager
                .tabs()
                .iter()
                .find(|tab| tab.id == second)
                .unwrap()
                .title,
            "New session"
        );
    }

    #[test]
    fn session_tab_element_ids_use_stable_session_prefixes() {
        let manager = SessionManager::new(None);
        let id = &manager.active().id;

        let ids = crate::ui::tabs::session_tab_ids(id);

        assert_eq!(ids.tab, format!("session-tab-{id}"));
        assert_eq!(ids.close, format!("session-tab-close-{id}"));
        assert_eq!(crate::ui::tabs::NEW_SESSION_ID, "session-tab-new");
    }
}
