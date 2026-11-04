use crate::ui::{material::MaterialMode, tokens, UiTheme};
use gpui::{div, point, px, BoxShadow, Div, Styled};

pub fn floating_surface(theme: UiTheme, mode: MaterialMode, radius: f32) -> Div {
    div()
        .relative()
        .rounded(px(radius))
        .bg(theme.chrome_fill(mode))
        .border_1()
        .border_color(theme.hairline)
        .shadow(vec![BoxShadow {
            color: theme.chrome_shadow.into(),
            offset: point(px(0.0), px(2.0)),
            blur_radius: px(8.0),
            spread_radius: px(0.0),
            inset: false,
        }])
}

pub fn panel_surface(theme: UiTheme, radius: f32) -> Div {
    div()
        .relative()
        .rounded(px(radius))
        .border_1()
        .border_color(theme.rim)
        .shadow(vec![BoxShadow {
            color: theme.chrome_shadow.into(),
            offset: point(px(0.0), px(5.0)),
            blur_radius: px(18.0),
            spread_radius: px(0.0),
            inset: false,
        }])
}

pub fn workspace_surface(theme: UiTheme) -> Div {
    div()
        .relative()
        .rounded(px(tokens::RADIUS_WORKSPACE))
        .border_1()
        .border_color(theme.rim)
        .flex()
        .flex_col()
        .min_w(px(0.0))
        .min_h(px(0.0))
        .bg(theme.background)
        // GPUI clips descendants with a rectangular mask. Keep its rounded
        // background visible below opaque route/status content at the corners.
        .pb(px(tokens::RADIUS_WORKSPACE))
        .overflow_hidden()
}
