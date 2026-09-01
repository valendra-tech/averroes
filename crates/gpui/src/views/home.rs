use crate::ui::UiTheme;
use crate::ui::{button, plus_icon, status_badge, ButtonVariant};
use averroes_core::workspace::WorkspaceConfig;
use gpui::prelude::FluentBuilder;
use gpui::*;

#[derive(Debug, Clone)]
pub enum HomeEvent {
    OpenSession(String),
    DeleteSession(String),
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
        Self {
            theme: UiTheme::light(),
            workspaces,
            active_workspace_id,
            sessions,
        }
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
        let workspace = self.active_workspace().cloned();
        let workspace_name = workspace
            .as_ref()
            .map(|workspace| workspace.name.clone())
            .unwrap_or_else(|| "No workspace selected".to_string());
        let workspace_root = workspace
            .as_ref()
            .map(|workspace| workspace.root.display().to_string())
            .unwrap_or_else(|| "Add a folder to start building".to_string());
        let session_count = self.sessions.len();

        let mut rail_entries: Vec<gpui::AnyElement> = Vec::new();
        for ws in &self.workspaces {
            let id = ws.id.clone();
            let is_active = self.active_workspace_id.as_deref() == Some(&id);
            let name = ws.name.clone();
            let root = ws.root.clone();
            let initial = name
                .chars()
                .next()
                .map(|character| character.to_uppercase().collect::<String>())
                .unwrap_or_else(|| "W".to_string());

            rail_entries.push(
                div()
                    .id(ElementId::Name(format!("home-ws-{}", id).into()))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .h(px(48.0))
                    .mx(px(10.0))
                    .px(px(10.0))
                    .bg(if is_active { theme.card } else { rgba(0x00000000) })
                    .border_l_1()
                    .border_color(if is_active {
                        theme.primary
                    } else {
                        rgba(0x00000000)
                    })
                    .rounded(px(UiTheme::RADIUS))
                    .text_color(theme.foreground)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.surface_hover))
                    .on_click(cx.listener(move |_this, _event, _window, cx| {
                        cx.emit(HomeEvent::SelectWorkspace(id.clone()));
                    }))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(26.0))
                            .h(px(26.0))
                            .rounded(px(7.0))
                            .bg(if is_active {
                                theme.primary
                            } else {
                                theme.accent
                            })
                            .text_color(if is_active {
                                theme.card
                            } else {
                                theme.primary
                            })
                            .font(UiTheme::display_font())
                            .font_weight(FontWeight::BOLD)
                            .child(initial),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .overflow_x_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(name),
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
            let del_id = id.clone();
            let group_name = format!("session-{}", id);
            let modified = if session.modified.is_empty() {
                "Recent".to_string()
            } else {
                session.modified.clone()
            };

            session_cards.push(
                div()
                    .id(ElementId::Name(format!("home-session-{}", id).into()))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .p(px(14.0))
                    .bg(theme.card)
                    .rounded(px(UiTheme::RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .group(group_name.clone())
                    .hover(|style| {
                        style
                            .bg(theme.surface_hover)
                            .border_color(theme.brand_orange)
                    })
                    .on_click(cx.listener(move |_this, _event, _window, cx| {
                        cx.emit(HomeEvent::OpenSession(id.clone()));
                    }))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(34.0))
                            .h(px(34.0))
                            .rounded(px(9.0))
                            .bg(theme.accent)
                            .text_color(theme.primary)
                            .font(UiTheme::display_font())
                            .font_weight(FontWeight::BOLD)
                            .text_lg()
                            .child("✦"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .overflow_x_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .font(UiTheme::mono_font())
                                    .child(format!("{} messages", count)),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(modified),
                            )
                            .child(
                                div()
                                    .id(ElementId::Name(format!("home-del-{}", del_id).into()))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(20.0))
                                    .h(px(20.0))
                                    .rounded(px(4.0))
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .opacity(0.0)
                                    .group_hover(group_name, |style| style.opacity(1.0))
                                    .hover(|style| {
                                        style
                                            .bg(theme.accent)
                                            .text_color(theme.destructive)
                                    })
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |_this, _event, _window, cx| {
                                        cx.stop_propagation();
                                        cx.emit(HomeEvent::DeleteSession(del_id.clone()));
                                    }))
                                    .child("×"),
                            ),
                    )
                    .into_any_element(),
            );
        }

        let rail = div()
            .id(ElementId::Name("home-rail".into()))
            .flex()
            .flex_col()
            .w(px(248.0))
            .h_full()
            .bg(theme.surface_subtle)
            .border_r_1()
            .border_color(theme.border)
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .pt(px(18.0))
                    .pb(px(14.0))
                    .px(px(20.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .font(UiTheme::mono_font())
                            .font_weight(FontWeight::BOLD)
                            .child("WORKSPACES"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.foreground)
                            .child("Choose your build space"),
                    ),
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
                    .py(px(9.0))
                    .mx(px(10.0))
                    .w_full()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.card)
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.surface_hover).text_color(theme.foreground))
                    .rounded(px(UiTheme::RADIUS))
                    .on_click(cx.listener(|_this, _event, _window, cx| {
                        cx.emit(HomeEvent::AddWorkspace);
                    }))
                    .child(plus_icon(16.0))
                    .child("Add workspace"),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(12.0))
                    .child(div()),
            )
            .child(
                div()
                    .px(px(20.0))
                    .pb(px(18.0))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .font(UiTheme::mono_font())
                    .child("⌘N  new session"),
            );

        let new_session_panel = div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .p(px(24.0))
            .bg(theme.accent)
            .border_1()
            .border_color(theme.brand_orange)
            .rounded(px(12.0))
            .shadow_sm()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font(UiTheme::mono_font())
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.primary)
                            .child("NEW BUILD"),
                    )
                    .child(status_badge(
                        theme,
                        if has_workspace {
                            "Ready"
                        } else {
                            "Add workspace"
                        },
                    )),
            )
            .child(
                div()
                    .font(UiTheme::display_font())
                    .text_2xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .child("Start with a clear prompt."),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!("Build, plan, and ship inside {}.", workspace_name)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        button(theme, ButtonVariant::Primary, "Start a session")
                            .id(ElementId::Name("home-new-session".into()))
                            .when(!has_workspace, |element| {
                                element
                                    .bg(theme.border)
                                    .border_color(theme.border)
                                    .text_color(theme.muted_foreground)
                                    .cursor(CursorStyle::Arrow)
                            })
                            .on_click(cx.listener(move |_this, _event, _window, cx| {
                                if has_workspace {
                                    cx.emit(HomeEvent::NewSession);
                                }
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .font(UiTheme::mono_font())
                            .child("⌘N"),
                    ),
            );

        let mut sessions_panel = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .font(UiTheme::display_font())
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child("Recent sessions"),
                    )
                    .child(status_badge(theme, format!("{} total", session_count))),
            );

        if session_cards.is_empty() {
            sessions_panel = sessions_panel.child(
                div()
                    .p(px(18.0))
                    .bg(theme.card)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(UiTheme::RADIUS))
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("No sessions in this workspace yet. Start a new build above."),
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
            .max_w(px(920.0))
            .px(px(48.0))
            .py(px(38.0))
            .gap(px(28.0))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_xs()
                            .font(UiTheme::mono_font())
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.brand_coral)
                            .child("OVERVIEW"),
                    )
                    .child(
                        div()
                            .font(UiTheme::display_font())
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .child(format!("Welcome to {}", workspace_name)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .font(UiTheme::mono_font())
                            .child(workspace_root),
                    ),
            )
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
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w(px(0.0))
                    .justify_center()
                    .bg(theme.background)
                    .child(main_area),
            )
    }
}

impl HomeView {
    fn active_workspace(&self) -> Option<&WorkspaceConfig> {
        self.active_workspace_id
            .as_ref()
            .and_then(|id| self.workspaces.iter().find(|workspace| &workspace.id == id))
    }
}
