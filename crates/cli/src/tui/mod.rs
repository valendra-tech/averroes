pub mod chat;
pub mod diff;
pub mod prompt;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

pub struct TuiApp {
    pub exit: bool,
}

impl TuiApp {
    pub fn new() -> Self { Self { exit: false } }

    pub fn draw(&self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(f.area());

        chat::draw_chat(f, chunks[0]);
        prompt::draw_prompt(f, chunks[1]);
    }
}
