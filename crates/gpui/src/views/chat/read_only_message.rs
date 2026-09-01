use crate::ui::{utf16_range_to_byte_range, TextField, TextSelection};
use gpui::prelude::*;
use gpui::*;
use std::ops::Range;

pub struct ReadOnlyMessage {
    content: String,
    selection: TextSelection,
    focus_handle: FocusHandle,
}

impl ReadOnlyMessage {
    pub fn new(focus_handle: FocusHandle) -> Self {
        Self { content: String::new(), selection: TextSelection::default(), focus_handle }
    }

    pub fn set_content(&mut self, content: String) {
        self.content = content;
        self.selection = TextSelection::default();
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for ReadOnlyMessage {
    fn text_for_range(&mut self, range_utf16: Range<usize>, adjusted_range: &mut Option<Range<usize>>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<String> {
        let range = utf16_range_to_byte_range(&self.content, &range_utf16);
        adjusted_range.replace(range_utf16);
        self.content.get(range).map(str::to_string)
    }

    fn selected_text_range(&mut self, _ignore_disabled_input: bool, _window: &mut Window, _cx: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(self.selection.selected_text_range(&self.content))
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.selection.marked_text_range(&self.content)
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) { self.selection.unmark(); }

    fn replace_text_in_range(&mut self, _range_utf16: Option<Range<usize>>, _text: &str, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_and_mark_text_in_range(&mut self, _range_utf16: Option<Range<usize>>, _text: &str, _new_selected_range: Option<Range<usize>>, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn bounds_for_range(&mut self, _range_utf16: Range<usize>, _element_bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Bounds<Pixels>> { None }

    fn character_index_for_point(&mut self, _point: Point<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> { None }
}

impl TextField for ReadOnlyMessage {
    fn text(&self) -> &str { &self.content }
    fn placeholder(&self) -> &str { "" }
    fn selection_range(&self) -> Range<usize> { self.selection.range.clone() }
    fn selection_reversed(&self) -> bool { self.selection.reversed }
    fn marked_range(&self) -> Option<Range<usize>> { self.selection.marked_range.clone() }
    fn cursor_offset(&self) -> usize { self.selection.cursor_offset() }
    fn set_last_layout(&mut self, _lines: Vec<ShapedLine>, _line_height: Pixels, _bounds: Bounds<Pixels>) {}
}
