//! Status panel — simple table showing session details.
//!
//! Toggled with `s` in normal mode. Renders over the message area.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::tui::theme;

use super::super::{ConnectionStatus, TuiState};

pub fn render(frame: &mut Frame, area: Rect, state: &TuiState) {
    if !state.show_status_panel {
        return;
    }

    frame.render_widget(Clear, area);

    let info = &state.session_info;

    let (dot, status_text, status_style) = match state.connections[0] {
        ConnectionStatus::Connected => ("●", "Connected", theme::connection(true)),
        ConnectionStatus::Connecting => ("◌", "Connecting...", theme::muted()),
        ConnectionStatus::Reconnecting(_) => ("◌", "Reconnecting", theme::connection(false)),
        ConnectionStatus::Disconnected => ("○", "Disconnected", theme::connection(false)),
    };

    let rows: Vec<(&str, String, ratatui::style::Style)> = vec![
        ("User", info.self_user_name.clone(), theme::bold()),
        ("Email", info.self_user_email.clone(), theme::text()),
        ("", String::new(), theme::text()),
        ("Auth", info.auth_mode.clone(), theme::text()),
        ("Status", format!("{dot} {status_text}"), status_style),
        (
            "XSRF",
            if info.xsrf_present {
                "present".into()
            } else {
                "missing".into()
            },
            if info.xsrf_present {
                theme::connection(true)
            } else {
                theme::connection(false)
            },
        ),
        ("Events", info.browser_channel.clone(), theme::text()),
        ("", String::new(), theme::text()),
        (
            "Spaces",
            format!(
                "{} ({} rooms, {} DMs)",
                info.total_spaces, info.total_rooms, info.total_dms
            ),
            theme::text(),
        ),
        ("Version", info.platform_version.clone(), theme::text()),
    ];

    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::raw(""));

    for (label, value, style) in &rows {
        if label.is_empty() {
            lines.push(Line::raw(""));
        } else {
            lines.push(Line::from(vec![
                Span::styled(format!("  {label:<10}"), theme::dim()),
                Span::styled(value.as_str(), *style),
            ]));
        }
    }

    if let Some(ref error) = state.last_error {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("  Error     ", theme::dim()),
            Span::styled(
                error.as_str(),
                ratatui::style::Style::default().fg(theme::STATUS_ERR),
            ),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled("  s to close", theme::dim()));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}
