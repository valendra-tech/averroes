use gpui::{div, px, AnyElement, IntoElement, ParentElement, Rgba, SharedString, Styled};
use gpui_component::scroll::ScrollableElement;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolDetailSection {
    Arguments,
    Result,
}

impl ToolDetailSection {
    pub(crate) fn scroll_suffix(self) -> &'static str {
        match self {
            Self::Arguments => "input",
            Self::Result => "output",
        }
    }
}

pub(crate) fn tool_detail_max_height(section: ToolDetailSection) -> f32 {
    match section {
        ToolDetailSection::Arguments => 180.0,
        ToolDetailSection::Result => 220.0,
    }
}

pub(crate) fn tool_detail_viewport_height(
    content: &str,
    section: ToolDetailSection,
    text_size: f32,
) -> f32 {
    let line_height = (text_size * 1.45).max(14.0);
    let line_count = content.lines().count().max(1) as f32;
    (line_count * line_height + 4.0)
        .min(tool_detail_max_height(section))
        .max(18.0)
}

pub(crate) fn render_tool_detail(
    id_prefix: impl Into<String>,
    content: impl Into<SharedString>,
    section: ToolDetailSection,
    color: Rgba,
    text_size: f32,
) -> AnyElement {
    let scroll_id = format!("{}-{}", id_prefix.into(), section.scroll_suffix());
    let content: SharedString = content.into();
    // `overflow_y_scrollbar` wraps the element in a full-size scroll area.
    // A child in a flex column has no intrinsic height once that wrapper is
    // applied, so using only `max_h` makes the viewport collapse to zero and
    // leaves the Arguments/Result labels with an apparently empty body. Give
    // the viewport an intrinsic height for short payloads and let the
    // scrollbar take over once the payload reaches the limit.
    let height = tool_detail_viewport_height(&content, section, text_size);
    div()
        .w_full()
        .min_w(px(0.0))
        .flex_none()
        .h(px(height))
        .max_h(px(tool_detail_max_height(section)))
        .font(crate::ui::UiTheme::mono_font())
        .text_size(px(text_size))
        .text_color(color)
        .whitespace_normal()
        .overflow_y_scrollbar()
        .id(scroll_id)
        .child(content)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{tool_detail_max_height, tool_detail_viewport_height, ToolDetailSection};

    #[test]
    fn result_viewport_is_larger_but_both_sections_are_bounded() {
        let arguments = tool_detail_max_height(ToolDetailSection::Arguments);
        let result = tool_detail_max_height(ToolDetailSection::Result);

        assert!(arguments > 0.0);
        assert!(result > 0.0);
        assert!(result > arguments);
    }

    #[test]
    fn arguments_and_result_use_distinct_scroll_suffixes() {
        assert_ne!(
            ToolDetailSection::Arguments.scroll_suffix(),
            ToolDetailSection::Result.scroll_suffix()
        );
    }

    #[test]
    fn viewport_keeps_short_payloads_visible_and_caps_long_payloads() {
        let short = tool_detail_viewport_height("{}", ToolDetailSection::Arguments, 11.0);
        let long =
            tool_detail_viewport_height(&"line\n".repeat(500), ToolDetailSection::Result, 11.0);

        assert!(short >= 18.0);
        assert!(long <= tool_detail_max_height(ToolDetailSection::Result));
    }
}
