//! Message content area — renders chat messages with sender, timestamp,
//! text, and reactions. Clean layout with clear visual hierarchy.
//!
//! ```text
//!   Alice · 10:42
//!   Hey, has anyone reviewed the PR?
//!
//!   Bob · 10:44
//!   Looks good to me
//!   👍 2
//! ```
//!
//! Design note: this pane is structured as a generic content renderer.
//! In the future, it could display agent reasoning, tool outputs, or
//! research results alongside chat messages.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::store::StoreRead;
use crate::tui::theme;
use crate::types::Timestamp;

use super::super::TuiState;

pub fn render<S: StoreRead>(frame: &mut Frame, area: Rect, store: &S, state: &TuiState) {
    let space_id = match state.active_space {
        Some(id) => id,
        None => {
            render_empty(frame, area);
            return;
        }
    };

    // Channel header
    let space_name = store
        .space(space_id)
        .map(|s| s.name.as_str())
        .unwrap_or("unknown");

    let header_title = format!(" {space_name} ");
    let block = Block::default()
        .borders(Borders::NONE)
        .title(Span::styled(header_title, theme::bold()));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line<'_>> = Vec::new();
    let self_user = store.self_user(space_id.platform);
    let mut msg_count = 0u32;

    for msg in store.messages_in_space(space_id) {
        if msg_count > 0 {
            lines.push(Line::raw("")); // Spacing between messages
        }

        // Sender name and timestamp
        let sender_name = store
            .user(msg.sender)
            .map(|u| u.display_name.as_str())
            .unwrap_or("unknown");

        let is_self = self_user == Some(msg.sender);
        let name_style = if is_self {
            theme::active()
        } else {
            theme::bold()
        };

        let ts_display = format_relative_time(msg.timestamp);
        let edited = if msg.edit_timestamp.is_some() {
            " (edited)"
        } else {
            ""
        };

        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(sender_name.to_owned(), name_style),
            Span::styled(format!(" · {ts_display}{edited}"), theme::dim()),
        ]));

        // Message text — wrap long lines
        for text_line in msg.text.lines() {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(text_line.to_owned(), theme::text()),
            ]));
        }

        // Reactions
        if !msg.reactions.is_empty() {
            let mut reaction_spans = vec![Span::raw("  ")];
            for reaction in &msg.reactions {
                let emoji_str = match &reaction.emoji {
                    crate::types::Emoji::Unicode(s) => s.as_str(),
                    crate::types::Emoji::Custom { shortcode, .. } => shortcode.as_str(),
                };
                let style = if reaction.includes_self {
                    theme::active()
                } else {
                    theme::reaction()
                };
                reaction_spans.push(Span::styled(
                    format!("{emoji_str} {} ", reaction.count),
                    style,
                ));
            }
            lines.push(Line::from(reaction_spans));
        }

        msg_count += 1;
    }

    // Typing indicator at bottom
    let space = store.space(space_id);
    if let Some(space) = space {
        if !space.typing_users.is_empty() {
            lines.push(Line::raw(""));
            let typers: Vec<&str> = space
                .typing_users
                .iter()
                .filter_map(|uid| store.user(*uid).map(|u| u.display_name.as_str()))
                .collect();
            let typing_text = match typers.len() {
                0 => "Someone is typing…".to_owned(),
                1 => format!("{} is typing…", typers[0]),
                2 => format!("{} and {} are typing…", typers[0], typers[1]),
                n => format!("{} and {} others are typing…", typers[0], n - 1),
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(typing_text, theme::muted()),
            ]));
        }
    }

    if lines.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("  No messages yet", theme::muted()));
    }

    // Apply scroll offset — show most recent messages at bottom
    let visible_height = inner.height as usize;
    let total_lines = lines.len();
    let scroll = if total_lines > visible_height {
        let max_scroll = total_lines.saturating_sub(visible_height);
        max_scroll.saturating_sub(state.message_scroll as usize)
    } else {
        0
    };

    let paragraph = Paragraph::new(lines)
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

fn render_empty(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::raw(""),
        Line::raw(""),
        Line::styled("  tchat", theme::bold()),
        Line::raw(""),
        Line::styled("  Terminal chat client", theme::muted()),
        Line::styled("  Select a space to begin", theme::dim()),
        Line::raw(""),
        Line::styled("  j/k  scroll    J/K  switch space", theme::dim()),
        Line::styled("  i    compose   q    quit", theme::dim()),
        Line::styled("  f    focus     t    threads", theme::dim()),
    ];
    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

/// Format a timestamp relative to now for compact display.
fn format_relative_time(ts: Timestamp) -> String {
    // Simple HH:MM for now. A production version would use
    // "just now", "5m ago", "yesterday", etc.
    let secs = ts.as_secs();
    if secs == 0 {
        return "—".to_owned();
    }
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    format!("{hours:02}:{minutes:02}")
}
