use gpui::*;

const PLUS_SVG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/plus.svg");
const CHEVRON_DOWN_SVG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/chevron-down.svg");

pub fn plus_icon(size: f32) -> impl IntoElement {
    svg()
        .path(PLUS_SVG.to_string())
        .w(px(size))
        .h(px(size))
}

pub fn chevron_down_icon(size: f32) -> impl IntoElement {
    svg()
        .path(CHEVRON_DOWN_SVG.to_string())
        .w(px(size))
        .h(px(size))
}
