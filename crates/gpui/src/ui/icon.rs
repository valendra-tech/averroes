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

pub fn spinner(size: f32) -> Svg {
    svg()
        .flex_none()
        .size(px(size))
        .path("providers/spinner.svg")
}

pub fn arrow_up(size: f32) -> Svg {
    svg()
        .flex_none()
        .size(px(size))
        .path("providers/arrow-up.svg")
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

pub fn provider_logo(provider: &str, size: f32) -> Svg {
    svg()
        .flex_none()
        .size(px(size))
        .path(provider_logo_path(provider))
        .text_color(rgb(0x20131a))
}

fn provider_logo_path(provider: &str) -> &'static str {
    match provider {
        "openai" => "providers/openai.svg",
        "anthropic" => "providers/anthropic.svg",
        "codex" => "providers/openai.svg",
        "copilot" | "github-copilot" => "providers/github-copilot.svg",
        "qdivzero" | "qdiv-zero" => "providers/qdivzero.svg",
        "deepseek" => "providers/deepseek.svg",
        "groq" => "providers/generic.svg",
        "ollama" | "ollama-cloud" | "ollama_cloud" => "providers/ollama.svg",
        _ => "providers/generic.svg",
    }
}
