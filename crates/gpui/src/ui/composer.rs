use super::text_input::TextSelection;
use super::theme::UiTheme;
use gpui::{div, px, Div, Styled};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerMode {
    Build,
    Plan,
}

impl ComposerMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Build => "Build",
            Self::Plan => "Plan",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortLevel {
    Max,
    Balanced,
}

impl EffortLevel {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Max => "Max",
            Self::Balanced => "Balanced",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerState {
    pub text: String,
    pub focused: bool,
    pub mode: ComposerMode,
    pub effort: EffortLevel,
    pub processing: bool,
    pub selection: TextSelection,
}

impl Default for ComposerState {
    fn default() -> Self {
        Self {
            text: String::new(),
            focused: false,
            mode: ComposerMode::Build,
            effort: EffortLevel::Max,
            processing: false,
            selection: TextSelection::default(),
        }
    }
}

impl ComposerState {
    pub fn replace_text(&mut self, range: Option<Range<usize>>, text: &str) {
        let range = range.or_else(|| Some(self.selection.range.clone()));
        let range = range.unwrap_or(self.text.len()..self.text.len());
        self.text.replace_range(range.clone(), text);
        self.selection.set_cursor(range.start + text.len());
        self.selection.unmark();
    }

    pub fn replace_text_utf16(&mut self, range: Option<Range<usize>>, text: &str) {
        self.selection.replace_text(&mut self.text, range, text);
    }

    pub fn replace_marked_text(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected_range: Option<Range<usize>>,
    ) {
        self.selection
            .replace_marked_text(&mut self.text, range, text, selected_range);
    }

    pub fn can_submit(&self) -> bool {
        !self.processing && !self.text.trim().is_empty()
    }

    pub fn take_submission(&mut self) -> Option<String> {
        if !self.can_submit() {
            return None;
        }

        self.selection = TextSelection::default();
        Some(std::mem::take(&mut self.text))
    }

    pub fn move_to(&mut self, offset: usize) {
        self.selection.set_cursor(offset);
    }

    pub fn select_to(&mut self, offset: usize) {
        if self.selection.reversed {
            self.selection.range.start = offset;
        } else {
            self.selection.range.end = offset;
        }
        if self.selection.range.end < self.selection.range.start {
            self.selection.reversed = !self.selection.reversed;
            self.selection.range = self.selection.range.end..self.selection.range.start;
        }
    }

    pub fn select_for_click(&mut self, offset: usize, click_count: usize, shift: bool) {
        if click_count >= 2 {
            self.selection.select_all(&self.text);
        } else if shift {
            self.select_to(offset);
        } else {
            self.move_to(offset);
        }
    }
}

pub fn composer_surface(theme: UiTheme, focused: bool, processing: bool) -> Div {
    let border = if focused {
        theme.brand_magenta
    } else {
        theme.border
    };

    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .bg(theme.card)
        .text_color(theme.foreground)
        .border_1()
        .border_color(border)
        .rounded(px(10.0))
        .shadow_sm()
        .opacity(if processing { 0.86 } else { 1.0 })
        .font(UiTheme::ui_font())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_only_text_cannot_be_submitted() {
        let mut state = ComposerState::default();
        state.text = "   \n\t".into();

        assert!(!state.can_submit());
        assert_eq!(state.take_submission(), None);
        assert_eq!(state.text, "   \n\t");
    }

    #[test]
    fn processing_disables_submit() {
        let mut state = ComposerState::default();
        state.text = "inspect this".into();
        state.processing = true;

        assert!(!state.can_submit());
        assert_eq!(state.take_submission(), None);
        assert_eq!(state.text, "inspect this");
    }

    #[test]
    fn submission_takes_text_and_preserves_mode() {
        let mut state = ComposerState::default();
        state.text = "inspect this".into();

        assert_eq!(state.take_submission().as_deref(), Some("inspect this"));
        assert!(state.text.is_empty());
        assert_eq!(state.mode, ComposerMode::Build);
        assert_eq!(state.effort, EffortLevel::Max);
    }

    #[test]
    fn native_input_replacement_updates_text() {
        let mut state = ComposerState::default();

        state.replace_text(None, "hello");

        assert_eq!(state.text, "hello");
    }

    #[test]
    fn submission_resets_selection_for_the_next_message() {
        let mut state = ComposerState::default();
        state.text = "hello".into();
        state.selection.set_cursor(state.text.len());

        assert_eq!(state.take_submission().as_deref(), Some("hello"));
        assert_eq!(state.selection, TextSelection::default());
    }

    #[test]
    fn dropdown_labels_match_composer_choices() {
        assert_eq!(ComposerMode::Build.label(), "Build");
        assert_eq!(ComposerMode::Plan.label(), "Plan");
        assert_eq!(EffortLevel::Max.label(), "Max");
        assert_eq!(EffortLevel::Balanced.label(), "Balanced");
    }

    #[test]
    fn double_click_selects_all_composer_text() {
        let mut state = ComposerState::default();
        state.text = "select this entire message".into();

        state.select_for_click(8, 2, false);

        assert_eq!(state.selection.range, 0..state.text.len());
    }
}
