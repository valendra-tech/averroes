use gpui::{
    fill, hsla, point, px, relative, size, App, Bounds, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, GlobalElementId, IntoElement, LayoutId, PaintQuad,
    Pixels, ShapedLine, SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle,
    Window, blue,
};
use std::ops::Range;
use std::cell::Cell;

thread_local! {
    static CURSOR_ON: Cell<bool> = Cell::new(true);
    static LAST_BLINK: Cell<u64> = Cell::new(0);
}

fn is_cursor_visible() -> bool {
    CURSOR_ON.with(|on| {
        LAST_BLINK.with(|last| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let prev = last.get();
            if now.saturating_sub(prev) > 530 {
                let visible = !on.get();
                on.set(visible);
                last.set(now);
                visible
            } else {
                on.get()
            }
        })
    })
}

pub trait TextField {
    fn text(&self) -> &str;
    fn placeholder(&self) -> &str;
    fn selection_range(&self) -> Range<usize>;
    fn selection_reversed(&self) -> bool;
    fn marked_range(&self) -> Option<Range<usize>>;
    fn cursor_offset(&self) -> usize;
    fn set_last_layout(&mut self, lines: Vec<ShapedLine>, line_height: Pixels, bounds: Bounds<Pixels>);
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
    lines: Vec<ShapedLine>,
    line_height: Pixels,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
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
        style.size.height = relative(1.).into();
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
            (input.placeholder().to_string(), hsla(0., 0., 0., 0.2))
        } else {
            (content, style.color)
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let mut shaped_lines: Vec<ShapedLine> = Vec::new();
        let mut cursor_quad = None;
        let mut selection_quads = Vec::new();
        let mut y = bounds.top();
        let mut char_offset = 0usize;

        // Build colored runs
        let build_runs = |text: &str, base_offset: usize| -> Vec<TextRun> {
            let mark_start = marked_range.as_ref().map(|r| r.start).unwrap_or(0);
            let mark_end = marked_range.as_ref().map(|r| r.end).unwrap_or(0);
            let run = TextRun {
                len: text.len(),
                font: style.font(),
                color: text_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            if mark_end > base_offset && mark_start < base_offset + text.len() {
                let pre = mark_start.saturating_sub(base_offset).min(text.len());
                let marked = (mark_end.saturating_sub(base_offset)).min(text.len()).saturating_sub(pre);
                let post = text.len().saturating_sub(pre + marked);
                let mut runs = Vec::new();
                if pre > 0 { runs.push(TextRun { len: pre, ..run.clone() }); }
                if marked > 0 { runs.push(TextRun { len: marked, underline: Some(UnderlineStyle { color: Some(run.color), thickness: px(1.0), wavy: false }), ..run.clone() }); }
                if post > 0 { runs.push(TextRun { len: post, ..run.clone() }); }
                runs.into_iter().filter(|r| r.len > 0).collect()
            } else {
                vec![run]
            }
        };

        let line_texts: Vec<&str> = display_text.split('\n').collect();
        let line_count = line_texts.len();
        for (line_idx, line_text) in line_texts.into_iter().enumerate() {
            let runs = build_runs(line_text, char_offset);
            let line: SharedString = line_text.to_string().into();
            let shaped = window.text_system().shape_line(line, font_size, &runs, None);
            let line_len = shaped.len();

            if cursor_quad.is_none() && is_cursor_visible() && cursor >= char_offset && cursor <= char_offset + line_len {
                let local_idx = cursor.saturating_sub(char_offset).min(line_len);
                let x = bounds.left() + shaped.x_for_index(local_idx);
                let ch = line_height * 0.65;
                let cy = y + (line_height - ch) / 2.0;
                cursor_quad = Some(fill(Bounds::new(point(x, cy), size(px(1.), ch)), blue()));
            }

            if !selected_range.is_empty() {
                let sel_start = char_offset.max(selected_range.start).saturating_sub(char_offset).min(line_len);
                let sel_end = (char_offset + line_len).min(selected_range.end).saturating_sub(char_offset).min(line_len);
                if sel_start < sel_end {
                    let x1 = bounds.left() + shaped.x_for_index(sel_start);
                    let x2 = bounds.left() + shaped.x_for_index(sel_end);
                    selection_quads.push(fill(
                        Bounds::from_corners(point(x1, y), point(x2, y + line_height)),
                        hsla(0.7, 0.6, 0.7, 0.2),
                    ));
                }
            }

            shaped_lines.push(shaped);
            char_offset += line_len;
            if line_idx + 1 < line_count {
                char_offset += 1; // \n byte
            }
            y += line_height;
        }

        TextFieldPrepaint {
            lines: shaped_lines,
            line_height,
            cursor: cursor_quad,
            selection: selection_quads,
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

        let selections = std::mem::take(&mut prepaint.selection);
        for sel in selections {
            window.paint_quad(sel);
        }

        let mut y = bounds.origin.y;
        for line in &prepaint.lines {
            line.paint(point(bounds.origin.x, y), prepaint.line_height, TextAlign::Left, None, window, cx).unwrap();
            y += prepaint.line_height;
        }

        if self.focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }

        self.view.update(cx, |view, _cx| {
            view.set_last_layout(prepaint.lines.clone(), prepaint.line_height, bounds);
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

fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() { return text.len(); }
    if text.is_char_boundary(index) { return index; }
    let mut last = 0;
    for (i, _) in text.char_indices() {
        if i >= index { return last; }
        last = i;
    }
    last
}

fn ceil_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() { return text.len(); }
    if text.is_char_boundary(index) { return index; }
    for (i, _) in text.char_indices() {
        if i > index { return i; }
    }
    text.len()
}

fn utf16_offset_to_byte_index(text: &str, offset: usize) -> usize {
    text.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .nth(offset)
        .unwrap_or(text.len())
}

fn byte_range_to_utf16_range(text: &str, range: &Range<usize>) -> Range<usize> {
    let start_utf16 = text[..range.start].chars().map(|c| c.len_utf16()).sum();
    let end_utf16 = text[range.start..range.end].chars().map(|c| c.len_utf16()).sum::<usize>() + start_utf16;
    start_utf16..end_utf16
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSelection {
    pub range: Range<usize>,
    pub reversed: bool,
    pub marked_range: Option<Range<usize>>,
}

impl Default for TextSelection {
    fn default() -> Self {
        Self { range: 0..0, reversed: false, marked_range: None }
    }
}

impl TextSelection {
    pub fn cursor_offset(&self) -> usize {
        if self.reversed { self.range.start } else { self.range.end }
    }

    pub fn set_cursor(&mut self, offset: usize) {
        self.range = offset..offset;
        self.reversed = false;
    }

    pub fn selected_text_range(&self, text: &str) -> UTF16Selection {
        let start = floor_char_boundary(text, self.range.start);
        let end = floor_char_boundary(text, self.range.end);
        let safe_range = start..end;
        UTF16Selection { range: byte_range_to_utf16_range(text, &safe_range), reversed: self.reversed }
    }

    pub fn marked_text_range(&self, text: &str) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| {
            let start = floor_char_boundary(text, range.start);
            let end = floor_char_boundary(text, range.end);
            byte_range_to_utf16_range(text, &(start..end))
        })
    }

    pub fn replace_text(&mut self, text: &mut String, range_utf16: Option<Range<usize>>, new_text: &str) {
        let range = range_utf16
            .map(|r| utf16_range_to_byte_range(text, &r))
            .unwrap_or_else(|| self.range.clone());
        let start = floor_char_boundary(text, range.start);
        let end = ceil_char_boundary(text, range.end);
        text.replace_range(start..end, new_text);
        let new_cursor = start + new_text.len();
        self.range = new_cursor..new_cursor;
        self.reversed = false;
        self.marked_range = None;
    }

    pub fn replace_marked_text(
        &mut self, text: &mut String, range_utf16: Option<Range<usize>>, new_text: &str, selected_range: Option<Range<usize>>,
    ) {
        self.replace_text(text, range_utf16, new_text);
        if let Some(sel) = selected_range {
            self.marked_range = Some(sel.start..sel.end);
        }
    }

    pub fn unmark(&mut self) { self.marked_range = None; }

    pub fn backspace(&mut self, text: &mut String) {
        if !self.range.is_empty() {
            self.range.start = floor_char_boundary(text, self.range.start);
            self.range.end = ceil_char_boundary(text, self.range.end);
            text.replace_range(self.range.clone(), "");
            self.range = self.range.start..self.range.start;
        } else if self.range.start > 0 {
            let prev = floor_char_boundary(text, self.range.start.saturating_sub(1));
            text.replace_range(prev..self.range.start, "");
            self.range = prev..prev;
        }
        self.reversed = false;
    }

    pub fn delete(&mut self, text: &mut String) {
        if !self.range.is_empty() {
            self.range.start = floor_char_boundary(text, self.range.start);
            self.range.end = ceil_char_boundary(text, self.range.end);
            text.replace_range(self.range.clone(), "");
            self.range = self.range.start..self.range.start;
        } else if self.range.end < text.len() {
            let next = ceil_char_boundary(text, self.range.end + 1).min(text.len());
            text.replace_range(self.range.end..next, "");
        }
        self.reversed = false;
    }

    pub fn move_left(&mut self, text: &str, extend: bool) {
        let pos = if self.reversed { self.range.start } else { self.range.end };
        let pos = floor_char_boundary(text, pos);
        let new_pos = if pos > 0 { floor_char_boundary(text, pos.saturating_sub(1)) } else { 0 };
        if extend {
            if self.reversed { self.range.start = new_pos; } else { self.range.end = new_pos; }
            if self.range.end < self.range.start { self.range = self.range.end..self.range.start; self.reversed = true; }
        } else {
            self.range = new_pos..new_pos;
            self.reversed = false;
        }
    }

    pub fn move_right(&mut self, text: &str, extend: bool) {
        let pos = if self.reversed { self.range.start } else { self.range.end };
        let pos = floor_char_boundary(text, pos);
        let new_pos = if pos < text.len() { ceil_char_boundary(text, pos + 1).min(text.len()) } else { text.len() };
        if extend {
            if self.reversed { self.range.start = new_pos; } else { self.range.end = new_pos; }
            if self.range.end < self.range.start { self.range = self.range.end..self.range.start; self.reversed = true; }
        } else {
            self.range = new_pos..new_pos;
            self.reversed = false;
        }
    }

    pub fn move_home(&mut self, extend: bool) {
        if extend { if self.reversed { self.range.start = 0; } else { self.range.end = 0; } }
        else { self.range = 0..0; self.reversed = false; }
    }

    pub fn move_end(&mut self, text: &str, extend: bool) {
        if extend { if self.reversed { self.range.start = text.len(); } else { self.range.end = text.len(); } }
        else { self.range = text.len()..text.len(); self.reversed = false; }
    }

    pub fn select_all(&mut self, text: &str) {
        self.range = 0..text.len();
        self.reversed = false;
    }
}
