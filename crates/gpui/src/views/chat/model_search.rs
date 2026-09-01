use crate::ui::{utf16_range_to_byte_range, TextField, TextSelection};
use gpui::prelude::*;
use gpui::*;
use std::ops::Range;

pub struct ModelSearchState {
    text: String,
    selection: TextSelection,
    focus_handle: FocusHandle,
    last_layout: Option<ShapedLine>,
}

impl ModelSearchState {
    pub fn new(focus_handle: FocusHandle) -> Self {
        Self {
            text: String::new(),
            selection: TextSelection::default(),
            focus_handle,
            last_layout: None,
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.selection = TextSelection::default();
    }

    pub fn value(&self) -> &str {
        &self.text
    }

    pub fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let modifiers = event.keystroke.modifiers;
        let plain =
            !modifiers.control && !modifiers.alt && !modifiers.platform && !modifiers.function;

        match event.keystroke.key.as_str() {
            "backspace" if plain => {
                self.selection.backspace(&mut self.text);
                cx.notify();
                true
            }
            "delete" if plain => {
                self.selection.delete(&mut self.text);
                cx.notify();
                true
            }
            "left" if plain => {
                self.selection.move_left(&self.text, modifiers.shift);
                cx.notify();
                true
            }
            "right" if plain => {
                self.selection.move_right(&self.text, modifiers.shift);
                cx.notify();
                true
            }
            "home" if plain => {
                self.selection.move_home(modifiers.shift);
                cx.notify();
                true
            }
            "end" if plain => {
                self.selection.move_end(&self.text, modifiers.shift);
                cx.notify();
                true
            }
            "a" if (modifiers.control || modifiers.platform)
                && !modifiers.alt
                && !modifiers.function =>
            {
                self.selection.select_all(&self.text);
                cx.notify();
                true
            }
            "c" if (modifiers.control || modifiers.platform)
                && !modifiers.alt
                && !modifiers.function =>
            {
                if !self.selection.range.is_empty() {
                    let text = self.text[self.selection.range.clone()].to_string();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                true
            }
            "x" if (modifiers.control || modifiers.platform)
                && !modifiers.alt
                && !modifiers.function =>
            {
                if !self.selection.range.is_empty() {
                    let text = self.text[self.selection.range.clone()].to_string();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.selection.replace_text(&mut self.text, None, "");
                    cx.notify();
                }
                true
            }
            "v" if (modifiers.control || modifiers.platform)
                && !modifiers.alt
                && !modifiers.function =>
            {
                if let Some(text) = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text().map(|text| text.replace('\n', " ")))
                {
                    self.selection.replace_text(&mut self.text, None, &text);
                    cx.notify();
                }
                true
            }
            _ => false,
        }
    }
}

impl EntityInputHandler for ModelSearchState {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = utf16_range_to_byte_range(&self.text, &range_utf16);
        adjusted_range.replace(range_utf16);
        self.text.get(range).map(str::to_string)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(self.selection.selected_text_range(&self.text))
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.selection.marked_text_range(&self.text)
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.selection.unmark();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selection
            .replace_text(&mut self.text, range_utf16, text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selection
            .replace_marked_text(&mut self.text, range_utf16, text, new_selected_range);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl TextField for ModelSearchState {
    fn text(&self) -> &str {
        &self.text
    }

    fn placeholder(&self) -> &str {
        "Buscar modelos"
    }

    fn selection_range(&self) -> Range<usize> {
        self.selection.range.clone()
    }

    fn selection_reversed(&self) -> bool {
        self.selection.reversed
    }

    fn marked_range(&self) -> Option<Range<usize>> {
        self.selection.marked_range.clone()
    }

    fn cursor_offset(&self) -> usize {
        self.selection.cursor_offset()
    }

    fn set_last_layout(&mut self, _lines: Vec<ShapedLine>, _line_height: Pixels, _bounds: Bounds<Pixels>) {
        self.last_layout = None;
    }
}
