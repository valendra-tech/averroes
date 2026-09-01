use gpui::*;
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

#[derive(Clone)]
pub struct SelectionState {
    range: Range<usize>,
    dragging: bool,
    anchor: usize,
}

impl SelectionState {
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }
}

pub struct SelectableTextElement {
    content: SharedString,
    selection: Rc<RefCell<SelectionState>>,
    paint_text: bool,
}

pub struct SelectableTextPrepaint {
    lines: Vec<ShapedLine>,
    line_height: Pixels,
    bounds: Bounds<Pixels>,
    layout: Option<SelectableLayout>,
}

#[derive(Clone)]
struct SelectableLayout {
    lines: Vec<ShapedLine>,
    line_height: Pixels,
    bounds: Bounds<Pixels>,
}

impl SelectableTextElement {
    pub fn new(content: impl Into<SharedString>) -> Self {
        Self {
            content: content.into(),
            selection: Rc::new(RefCell::new(SelectionState {
                range: 0..0,
                dragging: false,
                anchor: 0,
            })),
            paint_text: true,
        }
    }
}

impl IntoElement for SelectableTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SelectableTextElement {
    type RequestLayoutState = ();
    type PrepaintState = SelectableTextPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let text_color = style.color;
        let content = self.content.clone();

        let layout_id = window.request_measured_layout(Style::default(), {
            move |_known_dimensions, _available_space, window, cx| {
                let line_count = content.split('\n').count();
                let height = line_height * (line_count as f32);
                let mut max_width = px(0.0);
                for line_text in content.split('\n') {
                    let run = TextRun {
                        len: line_text.len(),
                        font: style.font(),
                        color: text_color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let line: SharedString = line_text.to_string().into();
                    let shaped = window.text_system().shape_line(line, font_size, &[run], None);
                    max_width = max_width.max(shaped.width());
                }
                size(max_width, height)
            }
        });

        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let text_color = style.color;

        let mut shaped_lines: Vec<ShapedLine> = Vec::new();

        let line_texts: Vec<&str> = self.content.split('\n').collect();
        for line_text in line_texts {
            let run = TextRun {
                len: line_text.len(),
                font: style.font(),
                color: text_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line: SharedString = line_text.to_string().into();
            let shaped = window.text_system().shape_line(line, font_size, &[run], None);
            shaped_lines.push(shaped);
        }

        let layout = SelectableLayout {
            lines: shaped_lines.clone(),
            line_height,
            bounds,
        };

        SelectableTextPrepaint {
            lines: shaped_lines,
            line_height,
            bounds,
            layout: Some(layout),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let bounds = prepaint.bounds;
        let content = self.content.clone();
        let selection = self.selection.clone();
        let layout = prepaint.layout.clone();
        let line_height = prepaint.line_height;
        let paint_text = self.paint_text;

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        {
            let sel = selection.borrow();
            let mut y = bounds.origin.y;
            let mut char_offset = 0usize;
            for line in &prepaint.lines {
                let line_len = line.len();
                if !sel.range.is_empty() {
                    let sel_start = char_offset.max(sel.range.start).saturating_sub(char_offset).min(line_len);
                    let sel_end = (char_offset + line_len).min(sel.range.end).saturating_sub(char_offset).min(line_len);
                    if sel_start < sel_end {
                        let x1 = bounds.origin.x + line.x_for_index(sel_start);
                        let x2 = bounds.origin.x + line.x_for_index(sel_end);
                        window.paint_quad(fill(
                            Bounds::from_corners(point(x1, y), point(x2, y + line_height)),
                            hsla(0.7, 0.6, 0.7, 0.2),
                        ));
                    }
                }
                char_offset += line_len;
                if char_offset < self.content.len() {
                    char_offset += 1;
                }
                y += line_height;
            }
        }

        if paint_text {
            let mut y = bounds.origin.y;
            for line in &prepaint.lines {
                line.paint(point(bounds.origin.x, y), line_height, TextAlign::Left, None, window, cx).unwrap();
                y += line_height;
            }
        }

        let sel = selection.clone();
        window.on_mouse_event({
            let layout = layout.clone();
            let hitbox = hitbox.clone();
            move |event: &MouseDownEvent, phase, window, _cx| {
                if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                    return;
                }
                if let Some(layout) = &layout {
                    let idx = char_index_at_point(&layout, event.position);
                    let len = total_len(&layout);
                    let idx = idx.min(len);
                    let mut s = sel.borrow_mut();
                    s.range = idx..idx;
                    s.anchor = idx;
                    s.dragging = true;
                }
            }
        });

        let sel = selection.clone();
        window.on_mouse_event({
            let layout = layout.clone();
            move |event: &MouseMoveEvent, phase, window, _cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let mut s = sel.borrow_mut();
                if !s.dragging {
                    return;
                }
                if let Some(layout) = &layout {
                    let idx = char_index_at_point(&layout, event.position);
                    let len = total_len(&layout);
                    let idx = idx.min(len);
                    s.range = s.anchor.min(idx)..s.anchor.max(idx);
                }
                window.refresh();
            }
        });

        let sel = selection.clone();
        window.on_mouse_event({
            let hitbox = hitbox.clone();
            move |_event: &MouseUpEvent, phase, window, _cx| {
                if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                    return;
                }
                let mut s = sel.borrow_mut();
                s.dragging = false;
                window.refresh();
            }
        });

        let sel = selection.clone();
        let content_for_key = content.clone();
        window.on_key_event({
            move |event: &KeyDownEvent, phase, _window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                if event.keystroke.modifiers.platform && event.keystroke.key == "c" {
                    let s = sel.borrow();
                    if !s.range.is_empty() {
                        let text: String = content_for_key[s.range.clone()].to_string();
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }
            }
        });
    }
}

fn total_len(layout: &SelectableLayout) -> usize {
    layout.lines.iter().map(|l| l.len()).sum::<usize>()
        + layout.lines.len().saturating_sub(1)
}

fn char_index_at_point(layout: &SelectableLayout, point: Point<Pixels>) -> usize {
    let mut char_offset = 0usize;
    let rel_y = point.y - layout.bounds.origin.y;

    if rel_y < px(0.0) {
        return 0;
    }

    let line_idx_f = f32::from(rel_y) / f32::from(layout.line_height);
    if line_idx_f < 0.0 {
        return 0;
    }
    let line_idx = line_idx_f.floor() as usize;

    for (i, line) in layout.lines.iter().enumerate() {
        if i == line_idx {
            let rel_x = (point.x - layout.bounds.origin.x).max(px(0.0));
            return char_offset + line.index_for_x(rel_x).unwrap_or(line.len());
        }
        char_offset += line.len();
        if i + 1 < layout.lines.len() {
            char_offset += 1;
        }
    }

    total_len(layout)
}
