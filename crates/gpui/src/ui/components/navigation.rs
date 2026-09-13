use crate::ui::tokens;
use crate::ui::UiTheme;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, px, AnyElement, Div, ElementId, InteractiveElement, IntoElement, ParentElement, Rgba,
    SharedString, Stateful, Styled,
};
use gpui_component::{Icon, IconName};

enum RowIcon {
    Named(IconName),
    Path(&'static str),
}

pub struct SidebarRow {
    id: ElementId,
    label: SharedString,
    icon: Option<RowIcon>,
    icon_color: Option<Rgba>,
    leading: Option<AnyElement>,
    accessory: Option<AnyElement>,
    indent: f32,
    selected: bool,
    processing: bool,
    unread: bool,
}

impl SidebarRow {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            icon_color: None,
            leading: None,
            accessory: None,
            indent: tokens::SPACE_12,
            selected: false,
            processing: false,
            unread: false,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(RowIcon::Named(icon));
        self
    }

    pub fn icon_path(mut self, path: &'static str) -> Self {
        self.icon = Some(RowIcon::Path(path));
        self
    }

    pub fn icon_color(mut self, color: Rgba) -> Self {
        self.icon_color = Some(color);
        self
    }

    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    pub fn accessory(mut self, element: impl IntoElement) -> Self {
        self.accessory = Some(element.into_any_element());
        self
    }

    pub fn indent(mut self, indent: f32) -> Self {
        self.indent = indent;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn processing(mut self, processing: bool) -> Self {
        self.processing = processing;
        self
    }

    pub fn unread(mut self, unread: bool) -> Self {
        self.unread = unread;
        self
    }

    pub fn render(self, theme: UiTheme) -> Stateful<Div> {
        let icon_color = self.icon_color.unwrap_or(theme.muted);
        div()
            .id(self.id)
            .flex_none()
            .w_full()
            .h(px(tokens::ROW_HEIGHT))
            .pl(px(self.indent))
            .pr(px(tokens::SPACE_12))
            .flex()
            .items_center()
            .gap(px(tokens::SPACE_12))
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .cursor_pointer()
            .text_size(px(tokens::TEXT_BODY))
            .when(self.selected, |row| {
                row.bg(theme.selection).font_weight(tokens::WEIGHT_MEDIUM)
            })
            .hover(|style| style.bg(theme.surface_hover))
            .children(self.leading)
            .children(self.icon.map(|icon| {
                match icon {
                    RowIcon::Named(name) => Icon::new(name)
                        .size(px(tokens::ICON_SIZE))
                        .text_color(icon_color)
                        .into_any_element(),
                    RowIcon::Path(path) => Icon::default()
                        .path(path)
                        .size(px(tokens::ICON_SIZE))
                        .text_color(icon_color)
                        .into_any_element(),
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(self.label),
            )
            .when(self.unread, |row| {
                row.child(
                    div()
                        .flex_none()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(theme.accent),
                )
            })
            .when(self.processing, |row| {
                row.child(
                    Icon::new(IconName::Loader)
                        .size(px(tokens::ICON_SMALL))
                        .text_color(theme.accent_text),
                )
            })
            .children(self.accessory)
    }
}

pub fn section_header(theme: UiTheme, label: impl Into<SharedString>, top: f32) -> AnyElement {
    div()
        .px(px(tokens::SPACE_12))
        .pt(px(top))
        .pb(px(tokens::SPACE_12))
        .text_size(px(tokens::TEXT_CAPTION))
        .font_weight(tokens::WEIGHT_SEMIBOLD)
        .text_color(theme.muted)
        .child(label.into())
        .into_any_element()
}

pub fn toolbar_surface(theme: UiTheme) -> Div {
    div()
        .flex_none()
        .h(px(tokens::TOOLBAR_HEIGHT))
        .px(px(tokens::SPACE_16))
        .flex()
        .items_center()
        .gap(px(tokens::SPACE_8))
        .border_b_1()
        .border_color(theme.hairline)
}

pub fn segmented_tab(
    theme: UiTheme,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id.into())
        .h(px(tokens::CONTROL_LARGE))
        .px(px(tokens::SPACE_12))
        .flex()
        .items_center()
        .rounded(px(tokens::RADIUS_CONTROL))
        .cursor_pointer()
        .text_size(px(tokens::TEXT_BODY))
        .text_color(if selected {
            theme.foreground
        } else {
            theme.muted
        })
        .when(selected, |tab| {
            tab.bg(theme.surface).font_weight(tokens::WEIGHT_MEDIUM)
        })
        .hover(|style| style.bg(theme.surface_hover))
        .child(label.into())
}
