use super::theme::UiTheme;
use gpui::*;
use std::borrow::Cow;

/// Makes provider reasoning readable without changing the stored value.
///
/// Some OpenAI-compatible gateways concatenate summary chunks with `****` and
/// omit the line break between them. That marker is transport noise rather
/// than useful Markdown. Keep the original reasoning in the conversation and
/// only repair it at the last moment before rendering.
pub fn normalize_reasoning_for_display(content: &str) -> Cow<'_, str> {
    if !content.contains('\r') && !content.contains("****") {
        return Cow::Borrowed(content);
    }

    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    Cow::Owned(
        normalized
            .split("****")
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

/// Renders a response while it is still arriving from a provider.
///
/// Keep the live path deliberately cheap. Once the provider closes the block,
/// the keyed Markdown TextView takes over and retains its parsed document
/// between frames.
pub fn render_streaming_markdown(theme: UiTheme, content: &str) -> Div {
    div()
        .w_full()
        .min_w(px(0.0))
        .text_sm()
        .text_color(theme.foreground)
        .children(content.split('\n').map(|line| {
            div()
                .w_full()
                .min_w(px(0.0))
                .whitespace_normal()
                .child(line.trim_end_matches('\r').to_string())
        }))
}

#[cfg(test)]
mod tests {
    use super::normalize_reasoning_for_display;
    use std::borrow::Cow;

    #[test]
    fn reasoning_display_separates_concatenated_provider_chunks() {
        assert_eq!(
            normalize_reasoning_for_display("First point****Second point\r\n\r\nThird point"),
            "First point\n\nSecond point\n\nThird point"
        );
    }

    #[test]
    fn reasoning_display_keeps_all_content() {
        let content = "A\n\nB\n\nC";
        let normalized = normalize_reasoning_for_display(content);
        assert_eq!(normalized, content);
        assert!(matches!(normalized, Cow::Borrowed(_)));
    }
}
