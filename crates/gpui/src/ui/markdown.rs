use super::animation::{fade_in, STREAM_LINE_FADE_DURATION};
use super::theme::UiTheme;
use gpui::*;
use std::borrow::Cow;

/// Makes provider reasoning readable without changing the stored value.
///
/// Some OpenAI-compatible gateways concatenate summary chunks with `****` or
/// split a `**` wrapper across lines. Those markers are transport noise rather
/// than useful Markdown. Keep the original reasoning in the conversation and
/// only repair it at the last moment before rendering.
pub fn normalize_reasoning_for_display(content: &str) -> Cow<'_, str> {
    if !content.contains('\r')
        && !content.contains("****")
        && !content.lines().any(has_unpaired_strong_delimiter)
    {
        return Cow::Borrowed(content);
    }

    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let normalized = if normalized.contains("****") {
        normalized
            .split("****")
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        normalized
    };

    if normalized.lines().any(has_unpaired_strong_delimiter) {
        Cow::Owned(strip_unpaired_strong_delimiters(&normalized))
    } else {
        Cow::Owned(normalized)
    }
}

fn has_unpaired_strong_delimiter(line: &str) -> bool {
    line.match_indices("**").count() % 2 == 1
}

fn strip_unpaired_strong_delimiters(content: &str) -> String {
    let mut normalized = String::with_capacity(content.len());
    for line in content.split_inclusive('\n') {
        let (line, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |line| (line, "\n"));
        let line = if has_unpaired_strong_delimiter(line) {
            let line = line.strip_prefix("**").unwrap_or(line);
            line.strip_suffix("**").unwrap_or(line)
        } else {
            line
        };
        normalized.push_str(line);
        normalized.push_str(newline);
    }
    normalized
}

/// Renders a response while it is still arriving from a provider.
///
/// Keep the live path deliberately cheap. Once the provider closes the block,
/// the keyed Markdown TextView takes over and retains its parsed document
/// between frames.
fn last_non_empty_line_index(content: &str) -> Option<usize> {
    content
        .split('\n')
        .enumerate()
        .fold(None, |last_index, (index, line)| {
            (!line.trim_end_matches('\r').trim().is_empty())
                .then_some(index)
                .or(last_index)
        })
}

pub fn render_streaming_markdown(theme: UiTheme, content: &str, animation_id: &str) -> Div {
    let last_line_index = last_non_empty_line_index(content);
    div()
        .w_full()
        .min_w(px(0.0))
        .text_sm()
        .text_color(theme.foreground)
        .children(content.split('\n').enumerate().map(|(line_index, line)| {
            let line_element = div()
                .w_full()
                .min_w(px(0.0))
                .whitespace_normal()
                .child(line.trim_end_matches('\r').to_string());
            if Some(line_index) == last_line_index {
                fade_in(
                    line_element,
                    format!("{animation_id}-line-{line_index}"),
                    STREAM_LINE_FADE_DURATION,
                )
                .into_any_element()
            } else {
                line_element.into_any_element()
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::{last_non_empty_line_index, normalize_reasoning_for_display};
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

    #[test]
    fn reasoning_display_strips_unpaired_strong_delimiters_per_line() {
        assert_eq!(
            normalize_reasoning_for_display("**Preparing the review\nLoading a skill**"),
            "Preparing the review\nLoading a skill"
        );
    }

    #[test]
    fn reasoning_display_keeps_well_formed_strong_markdown() {
        let content = "**Checkpoint complete**";
        assert_eq!(normalize_reasoning_for_display(content), content);
    }

    #[test]
    fn streaming_animation_targets_only_the_last_non_empty_line() {
        assert_eq!(last_non_empty_line_index("one\ntwo"), Some(1));
        assert_eq!(last_non_empty_line_index("one\ntwo\n"), Some(1));
        assert_eq!(last_non_empty_line_index("\n\n"), None);
    }
}
