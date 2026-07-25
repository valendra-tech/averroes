use ratatui::{layout::Rect, widgets::{Block, Borders, Paragraph}, Frame};

pub fn draw_prompt(f: &mut Frame, area: Rect) {
    let block = Block::default().title("Prompt").borders(Borders::ALL);
    let paragraph = Paragraph::new("> ").block(block);
    f.render_widget(paragraph, area);
}
