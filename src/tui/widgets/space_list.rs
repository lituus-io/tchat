//! Sidebar widget showing spaces grouped by platform.
//!
//! ```text
//!   Spaces
//!
//!   Google Chat
//!   ● team-eng                [3]
//!     general
//!     Bob Smith
//!
//!   Slack
//!     ops-alerts              [1]
//! ```

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::store::StoreRead;
use crate::tui::theme;
use crate::types::PlatformId;

use super::super::TuiState;

pub fn render<S: StoreRead>(frame: &mut Frame, area: Rect, store: &S, state: &TuiState) {
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_type(BorderType::Plain)
        .border_style(theme::border())
        .title(Span::styled(" spaces ", theme::section_header()));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut current_platform: Option<PlatformId> = None;

    for (space_index, space) in store.spaces_sorted().enumerate() {
        // Platform header on change
        if current_platform != Some(space.platform) {
            if current_platform.is_some() {
                lines.push(Line::raw(""));
            }
            let (label, color) = match space.platform {
                PlatformId::GoogleChat => ("Google Chat", theme::PLATFORM_GCHAT),
                PlatformId::Slack => ("Slack", theme::PLATFORM_SLACK),
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(label, ratatui::style::Style::default().fg(color)),
            ]));
            current_platform = Some(space.platform);
        }

        let is_active = state.active_space == Some(space.id);
        let is_cursor = space_index == state.space_cursor;

        // Indicator
        let indicator = if is_active { "●" } else { " " };

        // Space name — truncate to fit
        let max_name_width = inner.width.saturating_sub(8) as usize;
        let name = if space.name.len() > max_name_width {
            format!("{}…", &space.name[..max_name_width.saturating_sub(1)])
        } else {
            space.name.clone()
        };

        // Build the line
        let name_style = if is_active || is_cursor {
            theme::space_selected()
        } else {
            theme::text()
        };

        let indicator_style = if is_active {
            theme::active()
        } else {
            theme::dim()
        };

        let mut spans = vec![
            Span::styled(format!("  {indicator} "), indicator_style),
            Span::styled(name, name_style),
        ];

        // Unread count
        if space.unread_count > 0 {
            // Right-align the count
            let count_str = format!(" [{}]", space.unread_count);
            spans.push(Span::styled(count_str, theme::unread()));
        }

        // Typing indicator
        if !space.typing_users.is_empty() {
            spans.push(Span::styled(" …", theme::muted()));
        }

        let line_style = if is_cursor && !is_active {
            ratatui::style::Style::default().bg(theme::HIGHLIGHT_BG)
        } else {
            ratatui::style::Style::default()
        };

        lines.push(Line::from(spans).style(line_style));
    }

    if lines.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("  No spaces yet", theme::muted()));
        lines.push(Line::styled("  Waiting for sync…", theme::dim()));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}
