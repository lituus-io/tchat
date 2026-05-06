//! Status bar — single line at the bottom showing connection state,
//! input mode, active space, and unread count.
//!
//! ```text
//!  ● connected  NORMAL  #team-eng  3 unread
//! ```
//!
//! Compact single-line status. Future-ready for mode indicators like
//! CHAT / RESEARCH / AGENT.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::store::StoreRead;
use crate::tui::theme;

use super::super::{ConnectionStatus, InputMode, TuiState};

pub fn render<S: StoreRead>(frame: &mut Frame, area: Rect, store: &S, state: &TuiState) {
    let mut spans: Vec<Span<'_>> = Vec::new();

    spans.push(Span::raw(" "));

    // Connection indicator
    let (dot, conn_text, conn_style) = match state.connections[0] {
        ConnectionStatus::Connected => ("●", "connected", theme::connection(true)),
        ConnectionStatus::Connecting => ("◌", "connecting", theme::muted()),
        ConnectionStatus::Reconnecting(n) => {
            spans.push(Span::styled("◌ ", theme::connection(false)));
            spans.push(Span::styled(
                format!("reconnecting ({n})"),
                theme::connection(false),
            ));
            spans.push(Span::styled("  ", theme::dim()));
            // Skip the normal dot/text push below
            ("", "", theme::dim())
        }
        ConnectionStatus::Disconnected => ("○", "disconnected", theme::connection(false)),
    };

    if !dot.is_empty() {
        spans.push(Span::styled(format!("{dot} "), conn_style));
        spans.push(Span::styled(conn_text, conn_style));
        spans.push(Span::styled("  ", theme::dim()));
    }

    // Input mode badge
    let mode_str = match state.input_mode {
        InputMode::Normal => "NORMAL",
        InputMode::Insert => "INSERT",
    };
    spans.push(Span::styled(
        format!(" {mode_str} "),
        theme::mode_badge(mode_str),
    ));
    spans.push(Span::styled("  ", theme::dim()));

    // Active space name
    if let Some(space_id) = state.active_space {
        let name = store
            .space(space_id)
            .map(|s| s.name.as_str())
            .unwrap_or("—");
        spans.push(Span::styled(name.to_owned(), theme::text()));
        spans.push(Span::styled("  ", theme::dim()));
    }

    // Total unread count across all spaces
    let total_unread: u32 = store.spaces_sorted().map(|s| s.unread_count).sum();
    if total_unread > 0 {
        spans.push(Span::styled(
            format!("{total_unread} unread"),
            theme::unread(),
        ));
    }

    // Slack connection (if active)
    if matches!(state.connections[1], ConnectionStatus::Connected) {
        spans.push(Span::styled("  ", theme::dim()));
        spans.push(Span::styled(
            "● slack",
            ratatui::style::Style::default().fg(theme::PLATFORM_SLACK),
        ));
    }

    // Error display
    if let Some(ref error) = state.last_error {
        spans.push(Span::styled("  ", theme::dim()));
        spans.push(Span::styled(
            format!("err: {error}"),
            ratatui::style::Style::default().fg(theme::STATUS_ERR),
        ));
    }

    let line = Line::from(spans).style(theme::status_bar());
    let paragraph = Paragraph::new(vec![line]);
    frame.render_widget(paragraph, area);
}
