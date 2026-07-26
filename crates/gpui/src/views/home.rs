use crate::ui::UiTheme;
use crate::ui::{button, ButtonVariant};
use averroes_core::workspace::WorkspaceConfig;
use gpui::*;
use gpui::prelude::FluentBuilder;

#[derive(Debug, Clone)]
pub enum HomeEvent {
    OpenSession(String),
    NewSession,
    SelectWorkspace(String),
    AddWorkspace,
}

pub struct HomeView {
    theme: UiTheme,
    workspaces: Vec<WorkspaceConfig>,
    active_workspace_id: Option<String>,
    sessions: Vec<SessionEntry>,
}

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub id: String,
    pub title: String,
    pub message_count: usize,
    pub modified: String,
}

impl EventEmitter<HomeEvent> for HomeView {}

impl HomeView {
    pub fn new(
        _cx: &mut Context<Self>,
        workspaces: Vec<WorkspaceConfig>,
        active_workspace_id: Option<String>,
        sessions: Vec<SessionEntry>,
    ) -> Self {
        Self { theme: UiTheme::light(), workspaces, active_workspace_id, sessions }
    }

    pub fn update_state(
        &mut self,
        workspaces: Vec<WorkspaceConfig>,
        active_workspace_id: Option<String>,
        sessions: Vec<SessionEntry>,
    ) {
        self.workspaces = workspaces;
        self.active_workspace_id = active_workspace_id;
        self.sessions = sessions;
    }
}

impl Render for HomeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let has_workspace = self.active_workspace_id.is_some();

        let mut rail_entries: Vec<gpui::AnyElement> = Vec::new();
        for ws in &self.workspaces {
            let id = ws.id.clone();
            let is_active = self.active_workspace_id.as_deref() == Some(&id);
            let name = ws.name.clone();
            let root = ws.root.clone();
            rail_entries.push(
                div()
                    .id(ElementId::Name(format!("home-ws-{}", id).into()))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(36.0))
                    .px(px(12.0))
                    .bg(if is_active { theme.accent } else { rgba(0x00000000) })
                    .border_l_1()
                    .border_color(if is_active { theme.primary } else { rgba(0x00000000) })
                    .text_color(theme.foreground)
                    .cursor_pointer()
                    .hover(|style| if !is_active { style.bg(theme.accent) } else { style })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        cx.emit(HomeEvent::SelectWorkspace(id.clone()));
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .w_full()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .overflow_x_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .font(UiTheme::mono_font())
                                    .overflow_x_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(root.display().to_string()),
                            ),
                    )
                    .into_any_element(),
            );
        }

        let mut session_cards: Vec<gpui::AnyElement> = Vec::new();
        for session in &self.sessions {
            let id = session.id.clone();
            let title = session.title.clone();
            let count = session.message_count;
            session_cards.push(
                div()
                    .id(ElementId::Name(format!("home-session-{}", id).into()))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .p(px(12.0))
                    .bg(theme.card)
                    .rounded(px(UiTheme::RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        cx.emit(HomeEvent::OpenSession(id.clone()));
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{} msgs", count)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(session.modified.clone()),
                    )
                    .into_any_element(),
            );
        }

        let rail = div()
            .id(ElementId::Name("home-rail".into()))
            .flex()
            .flex_col()
            .w(px(220.0))
            .h_full()
            .bg(theme.background)
            .border_r_1()
            .border_color(theme.border)
            .overflow_y_scroll()
            .child(
                div()
                    .pt(px(12.0))
                    .pb(px(8.0))
                    .px(px(12.0))
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("Workspaces"),
            )
            .children(rail_entries)
            .child(
                div()
                    .id(ElementId::Name("home-add-ws".into()))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(6.0))
                    .w_full()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent).text_color(theme.foreground))
                    .rounded(px(UiTheme::RADIUS))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        cx.emit(HomeEvent::AddWorkspace);
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(20.0))
                            .h(px(20.0))
                            .rounded(px(3.0))
                            .text_color(theme.primary)
                            .text_lg()
                            .font_weight(FontWeight::LIGHT)
                            .child("+"),
                    )
                    .child("Add workspace"),
            );

        let rail = if self.workspaces.is_empty() {
            rail.child(
                div()
                    .px(px(12.0))
                    .py(px(8.0))
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("No workspaces yet"),
            )
        } else {
            rail
        };

        let new_session_panel = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .font(UiTheme::display_font())
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .child("New session"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Start a conversation in the current workspace."),
            )
            .child(
                button(theme, ButtonVariant::Primary, "Start")
                    .id(ElementId::Name("home-new-session".into()))
                    .w(px(120.0))
                    .when(!has_workspace, |el| {
                        el.text_color(theme.muted_foreground)
                            .cursor(CursorStyle::Arrow)
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        if has_workspace {
                            cx.emit(HomeEvent::NewSession);
                        }
                    })),
            );

        let mut sessions_panel = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .font(UiTheme::display_font())
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .child("Recent sessions"),
            );

        if session_cards.is_empty() {
            sessions_panel = sessions_panel.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("No sessions in this workspace yet."),
            );
        } else {
            for card in session_cards {
                sessions_panel = sessions_panel.child(card);
            }
        }

        let main_area = div()
            .id(ElementId::Name("home-main-area".into()))
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .px(px(48.0))
            .py(px(32.0))
            .gap(px(32.0))
            .overflow_y_scroll()
            .child(new_session_panel)
            .child(sessions_panel);

        div()
            .id(ElementId::Name("home-view".into()))
            .flex()
            .flex_row()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font(UiTheme::ui_font())
            .child(rail)
            .child(main_area)
    }
}

impl HomeView {
    fn active_workspace(&self) -> Option<&WorkspaceConfig> {
        self.active_workspace_id.as_ref()
            .and_then(|id| self.workspaces.iter().find(|w| &w.id == id))
    }
}
