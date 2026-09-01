use averroes_core::connection::ConnectionKind;
use gpui::{px, svg, Styled, Svg};

/// Bundled provider marks used by the connection UI.
///
/// These are local assets so the settings screen remains deterministic and
/// never fetches branding at runtime. Compatible APIs intentionally use a
/// neutral icon because they do not represent a provider brand.
pub fn provider_logo(kind: ConnectionKind, size: f32) -> Svg {
    let asset = match kind {
        ConnectionKind::Codex | ConnectionKind::OpenAi => "providers/openai.svg",
        ConnectionKind::Anthropic => "providers/anthropic.svg",
        ConnectionKind::Copilot => "providers/github-copilot.svg",
        // QDivZero does not publish a bundled brand asset in this app; use
        // the neutral mark rather than fabricating a provider logo.
        ConnectionKind::QDivZero => "providers/qdivzero.svg",
        ConnectionKind::DeepSeek => "providers/deepseek.svg",
        // Groq has no bundled licensed brand asset yet; keep the neutral
        // provider mark instead of inventing or modifying its logo.
        ConnectionKind::Groq => "providers/generic.svg",
        ConnectionKind::Ollama | ConnectionKind::OllamaCloud => "providers/ollama.svg",
        ConnectionKind::Compatible => "providers/generic.svg",
    };

    svg()
        .flex_none()
        .size(px(size))
        .path(asset)
        .text_color(gpui::rgb(0x111827))
}
