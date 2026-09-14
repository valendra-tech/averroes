use crate::ui::UiTheme;
use gpui::{
    div, px, AnyElement, InteractiveElement, IntoElement, ParentElement, Rgba, SharedString, Styled,
};
use gpui_component::scroll::ScrollableElement;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatchLineKind {
    Context,
    Added,
    Removed,
    Header,
}

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
    // `overflow_y_scrollbar` reuses the source element as the scrollable
    // content. Give the viewport a fixed, already-capped height; applying
    // `max_h` here would cap the content itself and leave no scroll extent.
    let height = tool_detail_viewport_height(&content, section, text_size);
    let content = div()
        .w_full()
        .min_w(px(0.0))
        .flex_none()
        .h_auto()
        .min_h_full()
        .child(content);

    #[cfg(test)]
    let debug_scroll_id = format!("{scroll_id}-content-end");

    #[cfg(test)]
    let content = content.child(
        div()
            .h(px(0.0))
            .flex_none()
            .debug_selector(move || debug_scroll_id.clone()),
    );

    div()
        .w_full()
        .min_w(px(0.0))
        .flex_none()
        .h(px(height))
        .font(crate::ui::UiTheme::mono_font())
        .text_size(px(text_size))
        .text_color(color)
        .whitespace_normal()
        .overflow_y_scrollbar()
        .id(scroll_id)
        .child(content)
        .into_any_element()
}

pub(crate) fn patch_content_for_display(input: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(input) {
        if let Some(patch) = value.get("patch").and_then(serde_json::Value::as_str) {
            if !patch.trim().is_empty() {
                return Some(patch.to_owned());
            }
        }
    }

    let input = input.trim();
    (input.starts_with("*** Begin Patch") || input.starts_with("diff ")).then(|| input.to_owned())
}

pub(crate) fn classify_patch_line(line: &str) -> PatchLineKind {
    if line.starts_with("@@")
        || line.starts_with("***")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("diff ")
        || line.starts_with("index ")
    {
        PatchLineKind::Header
    } else if line.starts_with('+') {
        PatchLineKind::Added
    } else if line.starts_with('-') {
        PatchLineKind::Removed
    } else {
        PatchLineKind::Context
    }
}

pub(crate) fn render_patch_diff(
    id_prefix: impl Into<String>,
    patch: &str,
    theme: UiTheme,
    text_size: f32,
) -> AnyElement {
    let id_prefix = id_prefix.into();
    let height = tool_detail_viewport_height(patch, ToolDetailSection::Arguments, text_size);
    let rows = patch.lines().enumerate().map(|(index, line)| {
        let (background, color) = match classify_patch_line(line) {
            PatchLineKind::Added => (theme.success_soft, theme.success_text),
            PatchLineKind::Removed => (theme.destructive_soft, theme.destructive_text),
            PatchLineKind::Header => (theme.surface_hover, theme.faint),
            PatchLineKind::Context => (theme.surface, theme.muted),
        };
        div()
            .id(SharedString::from(format!("{id_prefix}-line-{index}")))
            .w_full()
            .min_w(px(0.0))
            .px(px(crate::ui::tokens::SPACE_6))
            .flex_shrink_0()
            .bg(background)
            .text_color(color)
            .whitespace_normal()
            .child(if line.is_empty() {
                " ".to_owned()
            } else {
                line.to_owned()
            })
            .into_any_element()
    });

    div()
        .w_full()
        .min_w(px(0.0))
        .flex_none()
        .h(px(height))
        .font(UiTheme::mono_font())
        .text_size(px(text_size))
        .overflow_y_scrollbar()
        .id(format!("{id_prefix}-diff"))
        .children(rows)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{
        classify_patch_line, patch_content_for_display, tool_detail_max_height,
        tool_detail_viewport_height, PatchLineKind, ToolDetailSection,
    };

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

    #[gpui::test]
    fn long_tool_output_scrolls_inside_its_viewport(cx: &mut gpui::TestAppContext) {
        use gpui::{
            point, px, Context, IntoElement, ParentElement, Render, ScrollDelta, ScrollWheelEvent,
            Styled, VisualTestContext, Window,
        };

        struct TestView;

        impl Render for TestView {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                gpui::div()
                    .w(px(240.0))
                    .h(px(240.0))
                    .child(super::render_tool_detail(
                        "tool-output",
                        "line\n".repeat(100),
                        ToolDetailSection::Result,
                        gpui::Rgba::default(),
                        11.0,
                    ))
            }
        }

        fn draw(cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| {
                _ = window.draw(cx);
            });
        }

        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|_, _| TestView);
        draw(cx);

        let initial_y = cx
            .debug_bounds("tool-output-output-content-end")
            .unwrap()
            .origin
            .y;
        cx.simulate_event(ScrollWheelEvent {
            position: point(px(10.0), px(10.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-100.0))),
            ..Default::default()
        });
        draw(cx);

        let after_y = cx
            .debug_bounds("tool-output-output-content-end")
            .unwrap()
            .origin
            .y;
        assert!(
            after_y < initial_y,
            "tool output did not move after scrolling: initial={initial_y:?}, after={after_y:?}"
        );
    }

    #[test]
    fn extracts_patch_text_from_tool_arguments() {
        assert_eq!(
            patch_content_for_display(r#"{"patch":"*** Begin Patch\n+added\n*** End Patch"}"#),
            Some("*** Begin Patch\n+added\n*** End Patch".into())
        );
    }

    #[test]
    fn classifies_patch_lines_for_github_style_colors() {
        assert_eq!(classify_patch_line("+added"), PatchLineKind::Added);
        assert_eq!(classify_patch_line("-removed"), PatchLineKind::Removed);
        assert_eq!(classify_patch_line("@@ -1 +1 @@"), PatchLineKind::Header);
        assert_eq!(
            classify_patch_line("*** Update File: src/main.rs"),
            PatchLineKind::Header
        );
        assert_eq!(classify_patch_line(" unchanged"), PatchLineKind::Context);
    }
}
