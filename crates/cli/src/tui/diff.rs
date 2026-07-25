use ratatui::{layout::Rect, widgets::{Block, Borders, Paragraph}, Frame};

pub fn draw_diff(f: &mut Frame, area: Rect) {
    let block = Block::default().title("Diff").borders(Borders::ALL);
    let paragraph = Paragraph::new("No changes yet.\n").block(block);
    f.render_widget(paragraph, area);
}
