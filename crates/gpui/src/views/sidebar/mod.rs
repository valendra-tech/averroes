pub mod file_tree;

use crate::theme::Theme;
use gpui::*;

pub struct Sidebar {
    collapsed: bool,
    theme: Theme,
    sessions: Vec<String>,
    selected_index: Option<usize>,
}

impl Sidebar {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            collapsed: false,
            theme: Theme::default(),
            sessions: vec!["Session 1".into()],
            selected_index: Some(0),
        }
    }

    pub fn toggle(&mut self) {
        self.collapsed = !self.collapsed;
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    fn select_session(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected_index = Some(index);
        cx.notify();
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .children(self.sessions.iter().enumerate().map(|(i, s)| {
                        let is_selected = self.selected_index == Some(i);
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_color(theme.fg)
                            .text_sm()
                            .cursor_pointer()
                            .bg(if is_selected { theme.border } else { theme.surface })
                            .hover(|style| {
                                if !is_selected {
                                    style.bg(theme.border)
                                } else {
                                    style
                                }
                            })
                            .id(ElementId::NamedInteger("session".into(), i as u64))
                            .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                                this.select_session(i, cx);
                            }))
                            .child(s.clone())
                    })),
            )
    }
}
