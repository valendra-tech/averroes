use gpui::{
    fill, hsla, point, px, relative, size, App, Bounds, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, GlobalElementId, IntoElement, LayoutId, PaintQuad,
    Pixels, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, blue,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub trait TextField {
    fn text(&self) -> &str;
    fn placeholder(&self) -> &str;
    fn selection_range(&self) -> Range<usize>;
    fn selection_reversed(&self) -> bool;
    fn marked_range(&self) -> Option<Range<usize>>;
    fn cursor_offset(&self) -> usize;
    fn set_last_layout(&mut self, line: ShapedLine, bounds: Bounds<Pixels>);
}

pub struct TextFieldElement<V: EntityInputHandler + TextField> {
    view: Entity<V>,
    focus_handle: FocusHandle,
}

pub fn text_field_element<V: EntityInputHandler + TextField>(
    view: Entity<V>,
    focus_handle: FocusHandle,
) -> TextFieldElement<V> {
    TextFieldElement { view, focus_handle }
}

pub struct TextFieldPrepaint {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl<V: EntityInputHandler + TextField> IntoElement for TextFieldElement<V> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<V: EntityInputHandler + TextField> Element for TextFieldElement<V> {
    type RequestLayoutState = ();
    type PrepaintState = TextFieldPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.view.read(cx);
        let content = input.text().to_string();
        let selected_range = input.selection_range();
        let cursor = input.cursor_offset().min(content.len());
        let marked_range = input.marked_range();
        let style = window.text_style();

        let (display_text, text_color) = if content.is_empty() {
            (
                input.placeholder().to_string(),
                hsla(0., 0., 0., 0.2),
            )
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        let runs = if let Some(ref marked) = marked_range {
            let marked = marked.start.min(display_text.len())
                ..marked.end.min(display_text.len());
            vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end.saturating_sub(marked.start),
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len().saturating_sub(marked.end),
                    ..run.clone()
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let display_text: SharedString = display_text.into();
        let line =
            window
                .text_system()
                .shape_line(display_text, font_size, &runs, None);

        let cursor_pos = line.x_for_index(cursor);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    blue(),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    hsla(0.7, 0.6, 0.7, 0.2),
                )),
                None,
            )
        };

        TextFieldPrepaint {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }

        let line = prepaint.line.take().unwrap();
        line.paint(bounds.origin, window.line_height(), window, cx)
            .unwrap();

        if self.focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }

        self.view.update(cx, |view, _cx| {
            view.set_last_layout(line, bounds);
        });
    }
}

pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

pub fn utf16_range_to_byte_range(text: &str, range: &Range<usize>) -> Range<usize> {
    utf16_offset_to_byte_index(text, range.start)..utf16_offset_to_byte_index(text, range.end)
}

pub fn render_text_with_cursor(text: &str, selection: &TextSelection) -> String {
    if text.is_empty() {
        return "|".to_string();
    }
    let cursor = selection.cursor_offset();
    let cursor = cursor.min(text.len());
    let mut result = String::with_capacity(text.len() + 1);
    result.push_str(&text[..cursor]);
    result.push('|');
    result.push_str(&text[cursor..]);
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSelection {
    pub range: Range<usize>,
    pub reversed: bool,
    pub marked_range: Option<Range<usize>>,
}

impl Default for TextSelection {
    fn default() -> Self {
        Self {
            range: 0..0,
            reversed: false,
            marked_range: None,
        }
    }
}

impl TextSelection {
    pub fn cursor_offset(&self) -> usize {
        if self.reversed {
            self.range.start
        } else {
            self.range.end
        }
    }

    pub fn set_cursor(&mut self, offset: usize) {
        self.range = offset..offset;
        self.reversed = false;
    }

    pub fn selected_text_range(&self, text: &str) -> UTF16Selection {
        UTF16Selection {
            range: byte_range_to_utf16_range(text, &self.range),
            reversed: self.reversed,
        }
    }

    pub fn marked_text_range(&self, text: &str) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| byte_range_to_utf16_range(text, range))
    }

    pub fn replace_text(
        &mut self,
        text: &mut String,
        replacement_range_utf16: Option<Range<usize>>,
        replacement: &str,
    ) {
        let range = replacement_range_utf16
            .as_ref()
            .map(|range| utf16_range_to_byte_range(text, range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.range.clone());
        text.replace_range(range.clone(), replacement);
        self.set_cursor(range.start + replacement.len());
        self.marked_range = None;
    }

    pub fn replace_marked_text(
        &mut self,
        text: &mut String,
        replacement_range_utf16: Option<Range<usize>>,
        replacement: &str,
        selected_range_utf16: Option<Range<usize>>,
    ) {
        let range = replacement_range_utf16
            .as_ref()
            .map(|range| utf16_range_to_byte_range(text, range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.range.clone());
        text.replace_range(range.clone(), replacement);
        self.marked_range = if replacement.is_empty() {
            None
        } else {
            Some(range.start..range.start + replacement.len())
        };
        self.range = selected_range_utf16
            .as_ref()
            .map(|selected| {
                let selected = utf16_range_to_byte_range(replacement, selected);
                range.start + selected.start..range.start + selected.end
            })
            .unwrap_or_else(|| range.start + replacement.len()..range.start + replacement.len());
        self.reversed = false;
    }

    pub fn unmark(&mut self) {
        self.marked_range = None;
    }

    pub fn select_all(&mut self, text: &str) {
        self.range = 0..text.len();
        self.reversed = false;
    }

    pub fn move_left(&mut self, text: &str, extend: bool) {
        let cursor = self.cursor_offset();
        if !extend && !self.range.is_empty() {
            self.set_cursor(self.range.start);
        } else {
            self.select_to(previous_grapheme_boundary(text, cursor), extend);
        }
    }

    pub fn move_right(&mut self, text: &str, extend: bool) {
        let cursor = self.cursor_offset();
        if !extend && !self.range.is_empty() {
            self.set_cursor(self.range.end);
        } else {
            self.select_to(next_grapheme_boundary(text, cursor), extend);
        }
    }

    pub fn move_home(&mut self, extend: bool) {
        self.select_to(0, extend);
    }

    pub fn move_end(&mut self, text: &str, extend: bool) {
        self.select_to(text.len(), extend);
    }

    pub fn backspace(&mut self, text: &mut String) {
        if self.range.is_empty() {
            let cursor = self.cursor_offset();
            self.range = previous_grapheme_boundary(text, cursor)..cursor;
        }
        self.replace_text(text, None, "");
    }

    pub fn delete(&mut self, text: &mut String) {
        if self.range.is_empty() {
            let cursor = self.cursor_offset();
            self.range = cursor..next_grapheme_boundary(text, cursor);
        }
        self.replace_text(text, None, "");
    }

    fn select_to(&mut self, target: usize, extend: bool) {
        if !extend {
            self.set_cursor(target);
            return;
        }
        let anchor = if self.reversed {
            self.range.end
        } else {
            self.range.start
        };
        self.range = anchor.min(target)..anchor.max(target);
        self.reversed = target < anchor;
    }
}

fn byte_range_to_utf16_range(text: &str, range: &Range<usize>) -> Range<usize> {
    byte_offset_to_utf16_offset(text, range.start)..byte_offset_to_utf16_offset(text, range.end)
}

fn byte_offset_to_utf16_offset(text: &str, offset: usize) -> usize {
    text.get(..offset).unwrap_or(text).encode_utf16().count()
}

fn previous_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .take_while(|(index, _)| *index < offset)
        .last()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .find(|(index, _)| *index >= offset)
        .map(|(index, grapheme)| index + grapheme.len())
        .unwrap_or(text.len())
}

fn utf16_offset_to_byte_index(text: &str, offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }

    let mut utf16_offset = 0;
    for (byte_index, character) in text.char_indices() {
        if utf16_offset >= offset {
            return byte_index;
        }
        utf16_offset += character.len_utf16();
        if utf16_offset >= offset {
            return byte_index + character.len_utf8();
        }
    }

    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_ranges_convert_without_splitting_unicode() {
        let text = "a🙂b";
        let range = utf16_range_to_byte_range(text, &(1..3));

        assert_eq!(&text[range], "🙂");
        assert_eq!(utf16_len(text), 4);
    }

    #[test]
    fn selected_text_is_replaced_at_the_cursor() {
        let mut text = "hello".to_string();
        let mut selection = TextSelection {
            range: 1..4,
            reversed: false,
            marked_range: None,
        };

        selection.replace_text(&mut text, None, "i");

        assert_eq!(text, "hio");
        assert_eq!(selection.range, 2..2);
    }

    #[test]
    fn backspace_removes_one_grapheme() {
        let mut text = "a🙂".to_string();
        let mut selection = TextSelection {
            range: text.len()..text.len(),
            ..TextSelection::default()
        };

        selection.backspace(&mut text);

        assert_eq!(text, "a");
        assert_eq!(selection.range, 1..1);
    }

    #[test]
    fn shift_selection_replaces_selected_text() {
        let mut text = "hello".to_string();
        let mut selection = TextSelection::default();

        selection.move_end(&text, false);
        selection.move_left(&text, true);
        selection.replace_text(&mut text, None, "!");

        assert_eq!(text, "hell!");
        assert_eq!(selection.range, 5..5);
    }
}
