use gpui::*;

use crate::runtime::AgentFactory;
use crate::session::{SessionId, SessionManager};
use crate::shortcuts::{CloseSession, FocusInput, NewSession, Quit, SendMessage, ToggleSettings};
use crate::ui::tabs::{SessionTabs, SessionTabsEvent};
use crate::ui::UiTheme;
use crate::views::chat::{ChatView, ChatViewEvent};
use crate::views::home::{HomeEvent, HomeView, SessionEntry};
use crate::views::settings::{SettingsEvent, SettingsView};
use averroes_core::session::SessionStore;
use averroes_core::workspace::WorkspaceStore;
use std::collections::HashMap;
use std::sync::Arc;

pub struct AverroesApp {
    session_tabs: Entity<SessionTabs>,
    session_views: HashMap<SessionId, Entity<ChatView>>,
    session_subscriptions: HashMap<SessionId, Subscription>,
    settings: Entity<SettingsView>,
    home: Entity<HomeView>,
    sessions: SessionManager,
    factory: Arc<AgentFactory>,
    active_view: ActiveView,
    theme: UiTheme,
    workspace_store: WorkspaceStore,
    session_store: SessionStore,
    _session_tabs_subscription: Subscription,
    _settings_subscription: Subscription,
    _home_subscription: Subscription,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveView {
    Home,
    Chat,
    Settings,
}

impl AverroesApp {
    pub fn new(cx: &mut Context<Self>, factory: Arc<AgentFactory>) -> Self {
        let workspace_store = WorkspaceStore::new();
        let session_store = SessionStore::with_dir(workspace_store.sessions_dir());
        let sessions = Self::restore_sessions(&workspace_store);
        let active_id = sessions.active().id.clone();
        let theme = UiTheme::light();

        let session_tabs =
            cx.new(|cx| SessionTabs::new(cx, sessions.tabs().to_vec(), active_id.clone()));
        let settings = cx.new(|cx| SettingsView::new(cx, factory.config.clone()));
        let home = cx.new(|cx| {
            HomeView::new(
                cx,
                workspace_store.workspaces().to_vec(),
                workspace_store.active_workspace().map(|w| w.id.clone()),
                list_sessions(&session_store),
            )
        });

        let agent = factory.new_agent(&active_id);
        let chat_factory = Arc::clone(&factory);
        let workspaces = workspace_store.workspaces().to_vec();
        let chat = cx.new(|cx| {
            ChatView::new(
                cx,
                active_id.clone(),
                Some(agent),
                chat_factory,
                workspace_store.workspace_root(),
                workspace_store.active_workspace().map(|w| w.id.clone()),
                workspaces.clone(),
            )
        });
        let mut session_views = HashMap::new();
        session_views.insert(active_id.clone(), chat.clone());
        let chat_subscription = cx.subscribe(&chat, |this, _chat, event: &ChatViewEvent, cx| {
            this.handle_chat_event(event, cx)
        });
        let mut session_subscriptions = HashMap::new();
        session_subscriptions.insert(active_id, chat_subscription);
        let session_tabs_subscription = cx.subscribe(
            &session_tabs,
            |this, _tabs, event: &SessionTabsEvent, cx| match event {
                SessionTabsEvent::Select(id) => this.select_session(id.clone(), cx),
                SessionTabsEvent::Close(id) => this.close_session(id.clone(), cx),
                SessionTabsEvent::New => this.create_session(cx),
                SessionTabsEvent::Home => this.show_home(cx),
            },
        );
        let settings_subscription =
            cx.subscribe(&settings, |this, _settings, event: &SettingsEvent, cx| {
                this.handle_settings_event(event, cx);
            });
        let home_subscription = cx.subscribe(&home, |this, _home, event: &HomeEvent, cx| {
            this.handle_home_event(event, cx);
        });

        Self {
            session_tabs,
            session_views,
            session_subscriptions,
            settings,
            home,
            sessions,
            factory,
            active_view: ActiveView::Chat,
            theme,
            workspace_store,
            session_store,
            _session_tabs_subscription: session_tabs_subscription,
            _settings_subscription: settings_subscription,
            _home_subscription: home_subscription,
        }
    }

    fn restore_sessions(store: &WorkspaceStore) -> SessionManager {
        let workspace_id = store.active_workspace().map(|w| w.id.clone());
        let open_tabs = store.load_open_tabs();
        if open_tabs.is_empty() {
            return SessionManager::new(workspace_id);
        }
        let session_store = SessionStore::with_dir(store.sessions_dir());
        let mut manager = SessionManager::new_empty(workspace_id.clone());
        for id in &open_tabs {
            let title = session_store
                .load(id)
                .ok()
                .and_then(|msgs| {
                    msgs.first().map(|m| {
                        let t = m.text().chars().take(40).collect::<String>();
                        if t.is_empty() { "New session".into() } else { t }
                    })
                })
                .unwrap_or_else(|| "New session".into());
            manager.add_tab(SessionId(id.clone()), title);
        }
        if manager.tabs().is_empty() {
            return SessionManager::new(workspace_id);
        }
        if let Some(first_id) = manager.tabs().first().map(|t| t.id.clone()) {
            manager.select(&first_id);
        }
        manager
    }

    fn persist_tabs(&self) {
        let ids: Vec<String> = self.sessions.tabs().iter().map(|t| t.id.0.clone()).collect();
        let _ = self.workspace_store.save_open_tabs(&ids);
    }

    fn refresh_home(&mut self, cx: &mut Context<Self>) {
        let entries = list_sessions(&self.session_store);
        let workspaces = self.workspace_store.workspaces().to_vec();
        let active = self.workspace_store.active_workspace().map(|w| w.id.clone());
        self.home.update(cx, |home, _cx| {
            home.update_state(workspaces, active, entries);
        });
        cx.notify();
    }

    fn show_home(&mut self, cx: &mut Context<Self>) {
        self.refresh_home(cx);
        self.active_view = ActiveView::Home;
        cx.notify();
    }

    fn handle_home_event(&mut self, event: &HomeEvent, cx: &mut Context<Self>) {
        match event {
            HomeEvent::OpenSession(id) => {
                let sid = SessionId(id.clone());
                if !self.sessions.tabs().iter().any(|t| &t.id == &sid) {
                    let title = self
                        .session_store
                        .load(id)
                        .ok()
                        .and_then(|msgs| {
                            msgs.first().map(|m| {
                                let t = m.text().chars().take(40).collect::<String>();
                                if t.is_empty() { "New session".into() } else { t }
                            })
                        })
                        .unwrap_or_else(|| "New session".into());
                    self.sessions.add_tab(sid.clone(), title);
                }
                if !self.session_views.contains_key(&sid) {
                    self.add_session_view(sid.clone(), cx);
                }
                self.sessions.select(&sid);
                self.active_view = ActiveView::Chat;
                self.sync_navigation(cx);
                self.persist_tabs();
                cx.notify();
            }
            HomeEvent::NewSession => {
                self.create_session(cx);
            }
            HomeEvent::SelectWorkspace(id) => {
                self.workspace_store.set_active(id);
                self.session_store = SessionStore::with_dir(self.workspace_store.sessions_dir());
                self.session_views.clear();
                self.session_subscriptions.clear();
                self.sessions = Self::restore_sessions(&self.workspace_store);
                self.sync_navigation(cx);
                self.active_view = ActiveView::Home;
                self.refresh_home(cx);
            }
            HomeEvent::AddWorkspace => {
                let options = gpui::PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some("Select workspace directory".into()),
                };
                let rx = cx.prompt_for_paths(options);
                cx.spawn(async move |this, cx| {
                    let result = rx.await;
                    if let Ok(Ok(Some(paths))) = result {
                        if let Some(path) = paths.into_iter().next() {
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "workspace".into());
                            _ = this.update(cx, |app, cx| {
                                app.workspace_store.add_workspace(name, path);
                                app.session_store =
                                    SessionStore::with_dir(app.workspace_store.sessions_dir());
                                app.refresh_home(cx);
                            });
                        }
                    }
                }).detach();
            }
        }
    }

    fn add_session_view(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let agent = self.factory.new_agent(&id);
        let factory = Arc::clone(&self.factory);
        let session_id = id.clone();
        let root = self.workspace_store.workspace_root();
        let workspace_id = self.workspace_store.active_workspace().map(|w| w.id.clone());
        let workspaces = self.workspace_store.workspaces().to_vec();
        let chat = cx.new(|cx| {
            ChatView::new(cx, session_id, Some(agent), factory, root, workspace_id, workspaces)
        });
        let subscription = cx.subscribe(&chat, |this, _chat, event: &ChatViewEvent, cx| {
            this.handle_chat_event(event, cx)
        });
        self.session_views.insert(id.clone(), chat.clone());
        self.session_subscriptions.insert(id, subscription);
        chat.update(cx, |chat, cx| chat.request_composer_focus(cx));
    }

    fn sync_navigation(&mut self, cx: &mut Context<Self>) {
        let tabs = self.sessions.tabs().to_vec();
        let active_id = self.sessions.active().id.clone();
        self.session_tabs.update(cx, |tabs_view, _cx| {
            tabs_view.set_sessions(tabs.clone(), active_id.clone());
        });
    }

    fn select_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if !self.sessions.select(&id) {
            return;
        }
        self.active_view = ActiveView::Chat;
        self.focus_active_session(cx);
        self.sync_navigation(cx);
        cx.notify();
    }

    fn create_session(&mut self, cx: &mut Context<Self>) {
        let id = self.sessions.new_session();
        self.add_session_view(id, cx);
        self.active_view = ActiveView::Chat;
        self.sync_navigation(cx);
        self.persist_tabs();
        cx.notify();
    }

    fn close_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(active_id) = self.sessions.try_close(&id) else {
            return;
        };
        self.session_views.remove(&id);
        self.session_subscriptions.remove(&id);
        if !self.session_views.contains_key(&active_id) {
            self.add_session_view(active_id, cx);
        } else {
            self.focus_active_session(cx);
        }
        self.active_view = ActiveView::Chat;
        self.sync_navigation(cx);
        self.persist_tabs();
        cx.notify();
    }

    fn focus_active_session(&mut self, cx: &mut Context<Self>) {
        let active_id = self.sessions.active().id.clone();
        if let Some(chat) = self.session_views.get(&active_id) {
            chat.update(cx, |chat, cx| chat.request_composer_focus(cx));
        }
    }

    fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.active_view = match self.active_view {
            ActiveView::Chat | ActiveView::Home => ActiveView::Settings,
            ActiveView::Settings => ActiveView::Chat,
        };
        cx.notify();
    }

    fn handle_new_session(&mut self, _: &NewSession, _: &mut Window, cx: &mut Context<Self>) {
        self.create_session(cx);
    }

    fn handle_close_session(&mut self, _: &CloseSession, _: &mut Window, cx: &mut Context<Self>) {
        let id = self.sessions.active().id.clone();
        self.close_session(id, cx);
    }

    fn handle_focus_input(&mut self, _: &FocusInput, _: &mut Window, cx: &mut Context<Self>) {
        self.focus_active_session(cx);
    }

    fn handle_send_message(&mut self, _: &SendMessage, _: &mut Window, cx: &mut Context<Self>) {
        let id = self.sessions.active().id.clone();
        if let Some(chat) = self.session_views.get(&id) {
            chat.update(cx, |chat, cx| chat.submit_composer(cx));
        }
    }

    fn handle_toggle_settings(&mut self, _: &ToggleSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_settings(cx);
    }

    fn handle_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        self.persist_tabs();
        for chat in self.session_views.values() {
            chat.update(cx, |chat, _cx| chat.save_messages());
        }
        cx.quit();
    }

    fn handle_chat_event(&mut self, event: &ChatViewEvent, cx: &mut Context<Self>) {
        match event {
            ChatViewEvent::Submitted { session_id, text } => {
                self.sessions.set_dirty(session_id, true);
                if let Some(chat) = self.session_views.get(session_id) {
                    if let Some(wid) = chat.read(cx).workspace_id.clone() {
                        self.sessions.set_workspace_id(session_id, wid);
                    }
                }
                if self.sessions.tabs().iter().find(|tab| &tab.id == session_id)
                    .is_some_and(|tab| tab.title == "New session")
                {
                    let provider = self.factory.provider.clone();
                    let runtime = self.factory.runtime.clone();
                    let session_id = session_id.clone();
                    let text = text.clone();
                    let fallback_text = text.clone();
                    cx.spawn(async move |this, cx| {
                        let title = runtime.spawn(async move {
                            averroes_core::session::generate_session_title(provider.as_ref(), &text).await
                        }).await.unwrap_or(Err("join error".to_string()))
                        .unwrap_or_else(|_| session_title(&fallback_text));
                        _ = this.update(cx, |app, cx| {
                            app.sessions.rename(&session_id, title);
                            app.sync_navigation(cx);
                            cx.notify();
                        });
                    }).detach();
                }
            }
            ChatViewEvent::WorkspaceChanged { workspace_id, .. } => {
                self.workspace_store.set_active(workspace_id);
                self.session_store = SessionStore::with_dir(self.workspace_store.sessions_dir());
                self.session_views.clear();
                self.session_subscriptions.clear();
                self.sessions = Self::restore_sessions(&self.workspace_store);
                self.active_view = ActiveView::Chat;
                let active_id = self.sessions.active().id.clone();
                self.add_session_view(active_id, cx);
                self.refresh_home(cx);
                self.sync_navigation(cx);
                cx.notify();
            }
        }
    }

    fn handle_settings_event(&mut self, event: &SettingsEvent, cx: &mut Context<Self>) {
        match event {
            SettingsEvent::Saved => match AgentFactory::reload() {
                Ok(factory) => {
                    let factory = Arc::new(factory);
                    let session_ids = self.session_views.keys().cloned().collect::<Vec<_>>();
                    for session_id in session_ids {
                        if let Some(chat) = self.session_views.get(&session_id) {
                            let provider = factory.provider.clone();
                            let model = factory.provider.default_model().to_string();
                            let chat_factory = Arc::clone(&factory);
                            chat.update(cx, |chat, _cx| {
                                chat.reconfigure_agent(provider, model, chat_factory);
                            });
                        }
                    }
                    self.factory = factory;
                    self.settings.update(cx, |settings, _cx| settings.clear_error());
                }
                Err(error) => {
                    self.settings.update(cx, |settings, _cx| settings.set_error(error.to_string()));
                }
            },
        }
        cx.notify();
    }
}

impl Render for AverroesApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let active_id = self.sessions.active().id.clone();
        let active_title = self.sessions.active().title.clone();
        let active_content = match self.active_view {
            ActiveView::Home => self.home.clone().into_any_element(),
            ActiveView::Chat => self.session_views.get(&active_id).cloned()
                .map(|chat| chat.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            ActiveView::Settings => self.settings.clone().into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font(UiTheme::ui_font())
            .on_action(cx.listener(Self::handle_new_session))
            .on_action(cx.listener(Self::handle_close_session))
            .on_action(cx.listener(Self::handle_focus_input))
            .on_action(cx.listener(Self::handle_send_message))
            .on_action(cx.listener(Self::handle_toggle_settings))
            .on_action(cx.listener(Self::handle_quit))
            .child(self.session_tabs.clone())
            .children({
                let mut children: Vec<gpui::Div> = Vec::new();
                if self.active_view != ActiveView::Home {
                    children.push(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .h(px(32.0))
                            .px(px(16.0))
                            .bg(theme.card)
                            .border_b_1()
                            .border_color(theme.border)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .font(UiTheme::mono_font())
                            .child(active_title),
                    );
                }
                children.push(
                    div()
                        .flex()
                        .flex_row()
                        .flex_1()
                        .min_w(px(0.0))
                        .min_h(px(0.0))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w(px(0.0))
                                .min_h(px(0.0))
                                .child(active_content),
                        ),
                );
                children
            })
    }
}

fn list_sessions(store: &SessionStore) -> Vec<SessionEntry> {
    let mut entries = Vec::new();
    if let Ok(ids) = store.list_sessions() {
        for id in ids {
            if let Ok(msgs) = store.load(&id) {
                let title = msgs.first().map(|m| {
                    let t = m.text().chars().take(48).collect::<String>();
                    if t.is_empty() { "New session".into() } else { t }
                }).unwrap_or_else(|| "New session".into());
                entries.push(SessionEntry { id, title, message_count: msgs.len(), modified: String::new() });
            }
        }
    }
    entries
}

fn session_title(text: &str) -> String {
    let trimmed = text.trim();
    let mut title = trimmed.chars().take(32).collect::<String>();
    if trimmed.chars().count() > 32 { title.push_str("..."); }
    if title.is_empty() { "New session".into() } else { title }
}
