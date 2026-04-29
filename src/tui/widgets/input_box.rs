//! Input box — clean single-line input with prompt character.
//!
//! ```text
//!  ❯ type your message here_
//! ```
//!
//! In normal mode, shows a hint. In insert mode, shows the cursor.
//! Future: could support /commands, @mentions, and mode switching.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::tui::theme;

use super::super::{InputMode, TuiState};

pub fn render(frame: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_type(BorderType::Plain)
        .border_style(theme::border());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    match state.input_mode {
        InputMode::Normal => render_normal_hint(frame, inner),
        InputMode::Insert => render_insert_input(frame, inner, state),
    }
}

fn render_normal_hint(frame: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled(" ❯ ", theme::dim()),
        Span::styled("press i to compose, q to quit", theme::dim()),
    ]);
    let paragraph = Paragraph::new(vec![line]);
    frame.render_widget(paragraph, area);
}

fn render_insert_input(frame: &mut Frame, area: Rect, state: &TuiState) {
    let text = &state.input.text;
    let cursor = state.input.cursor;

    let line = if text.is_empty() {
        Line::from(vec![
            Span::styled(" ❯ ", theme::prompt()),
            Span::styled("type a message…", theme::dim()),
        ])
    } else {
        let before = &text[..cursor];
        let after = &text[cursor..];
        Line::from(vec![
            Span::styled(" ❯ ", theme::prompt()),
            Span::styled(before.to_owned(), theme::text()),
            Span::styled(after.to_owned(), theme::text()),
        ])
    };

    let paragraph = Paragraph::new(vec![line]);
    frame.render_widget(paragraph, area);

    // Position the terminal cursor for visual feedback
    let cursor_x = area.x + 3 + cursor as u16; // " ❯ " = 3 chars
    let cursor_y = area.y;
    if cursor_x < area.x + area.width {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}
