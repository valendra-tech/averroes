use ratatui::{layout::Rect, widgets::{Block, Borders, Paragraph}, Frame};

pub fn draw_chat(f: &mut Frame, area: Rect) {
    let block = Block::default().title("Chat").borders(Borders::ALL);
    let paragraph = Paragraph::new("Averroes AI — ready.\n").block(block);
    f.render_widget(paragraph, area);
}
