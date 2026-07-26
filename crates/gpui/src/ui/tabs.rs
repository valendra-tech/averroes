use crate::session::{SessionId, SessionTab};
use crate::ui::theme::UiTheme;
use gpui::*;

pub const NEW_SESSION_ID: &str = "session-tab-new";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTabIds {
    pub tab: String,
    pub close: String,
}

pub fn session_tab_ids(id: &SessionId) -> SessionTabIds {
    let id = id.to_string();
    SessionTabIds {
        tab: format!("session-tab-{id}"),
        close: format!("session-tab-close-{id}"),
    }
}

#[derive(Debug, Clone)]
pub enum SessionTabsEvent {
    Select(SessionId),
    Close(SessionId),
    New,
    Home,
}

pub struct SessionTabs {
    tabs: Vec<SessionTab>,
    active_id: SessionId,
    theme: UiTheme,
}

impl EventEmitter<SessionTabsEvent> for SessionTabs {}

impl SessionTabs {
    pub fn new(_cx: &mut Context<Self>, tabs: Vec<SessionTab>, active_id: SessionId) -> Self {
        Self {
            tabs,
            active_id,
            theme: UiTheme::light(),
        }
    }

    pub fn set_sessions(&mut self, tabs: Vec<SessionTab>, active_id: SessionId) {
        self.tabs = tabs;
        self.active_id = active_id;
    }

    fn select_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        cx.emit(SessionTabsEvent::Select(id));
    }

    fn close_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        cx.emit(SessionTabsEvent::Close(id));
    }

    fn create_session(&mut self, cx: &mut Context<Self>) {
        cx.emit(SessionTabsEvent::New);
    }
}

impl Render for SessionTabs {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let active_id = self.active_id.clone();

        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(44.0))
            .pl(px(78.0))
            .pr(px(12.0))
            .gap_2()
            .bg(theme.card)
            .border_b_1()
            .border_color(theme.border)
            .font(UiTheme::ui_font())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(28.0))
                    .h(px(28.0))
                    .rounded(px(UiTheme::RADIUS))
                    .text_color(theme.primary)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent))
                    .id(ElementId::Name("tab-home".into()))
                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        cx.emit(SessionTabsEvent::Home);
                    }))
                    .child("\u{2302}"),
            )
            .children(self.tabs.iter().map(|tab| {
                let ids = session_tab_ids(&tab.id);
                let tab_id = ids.tab;
                let close_id = ids.close;
                let session_id = tab.id.clone();
                let close_session_id = tab.id.clone();
                let title = tab.title.clone();
                let is_active = tab.id == active_id;
                let marker_color = if is_active {
                    theme.brand_orange
                } else {
                    theme.brand_magenta
                };

                let marker = if tab.dirty {
                    div()
                        .w(px(6.0))
                        .h(px(6.0))
                        .rounded(px(3.0))
                        .bg(marker_color)
                } else {
                    div().w(px(6.0)).h(px(6.0))
                };

                let group = format!("tab-group-{}", tab.id);

                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(UiTheme::RADIUS))
                    .border_1()
                    .border_color(if is_active {
                        theme.accent
                    } else {
                        rgba(0x00000000)
                    })
                    .bg(if is_active { theme.accent } else { theme.card })
                    .text_color(if is_active {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .cursor_pointer()
                    .group(group.clone())
                    .hover(move |style| {
                        if is_active {
                            style
                        } else {
                            style.border_color(theme.border)
                        }
                    })
                    .id(ElementId::Name(tab_id.into()))
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        this.select_session(session_id.clone(), cx);
                    }))
                    .child(marker)
                    .child(title)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(18.0))
                            .h(px(18.0))
                            .rounded(px(3.0))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .opacity(0.0)
                            .group_hover(group, |style| style.opacity(1.0))
                            .hover(|style| style.bg(theme.accent))
                            .id(ElementId::Name(close_id.into()))
                            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                                cx.stop_propagation();
                                this.close_session(close_session_id.clone(), cx);
                            }))
                            .child("x"),
                    )
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(28.0))
                    .h(px(28.0))
                    .rounded(px(UiTheme::RADIUS))
                    .text_color(theme.primary)
                    .text_lg()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent))
                    .id(ElementId::Name(NEW_SESSION_ID.into()))
                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.create_session(cx);
                    }))
                    .child("+"),
            )
    }
}
