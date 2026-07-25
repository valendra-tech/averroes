pub mod file_tree;

use crate::theme::Theme;
use gpui::*;

pub struct Sidebar {
    collapsed: bool,
    theme: Theme,
    sessions: Vec<String>,
}

impl Sidebar {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            collapsed: false,
            theme: Theme::default(),
            sessions: vec!["Session 1".into()],
        }
    }

    pub fn toggle(&mut self) {
        self.collapsed = !self.collapsed;
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;

        if self.collapsed {
            return div().w_0();
        }

        div()
            .flex()
            .flex_col()
            .w(px(220.0))
            .h_full()
            .bg(theme.surface)
            .border_r_1()
            .border_color(theme.border)
            .p_4()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.fg)
                    .child("Sessions"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(self.sessions.iter().map(|s| {
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_color(theme.fg)
                            .text_sm()
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.border))
                            .child(s.clone())
                    })),
            )
    }
}
