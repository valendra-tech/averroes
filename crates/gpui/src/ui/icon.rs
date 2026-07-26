use gpui::*;

pub fn plus_icon(size: f32) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(size))
        .h(px(size))
        .child("+")
}

pub fn chevron_down_icon(size: f32) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(size))
        .h(px(size))
        .child("\u{25BE}")
}
