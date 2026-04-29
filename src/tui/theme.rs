//! Visual theme: minimalist terminal aesthetic.
//!
//! Design principles:
//! - Minimal chrome, subtle borders (DarkGray)
//! - Monochrome base with strategic accent colors
//! - Clean typography with clear visual hierarchy
//! - Agent-harness ready: mode indicators, status patterns

use ratatui::style::{Color, Modifier, Style};

// ─────────────────── Base palette ───────────────────

pub const FG: Color = Color::White;
pub const FG_DIM: Color = Color::DarkGray;
pub const FG_MUTED: Color = Color::Gray;
pub const BORDER: Color = Color::DarkGray;
pub const ACCENT: Color = Color::Cyan;
pub const HIGHLIGHT_BG: Color = Color::DarkGray;

// ─────────────────── Status colors ───────────────────

pub const STATUS_OK: Color = Color::Green;
pub const STATUS_WARN: Color = Color::Yellow;
pub const STATUS_ERR: Color = Color::Red;

// ─────────────────── Platform colors ───────────────────

pub const PLATFORM_GCHAT: Color = Color::Cyan;
pub const PLATFORM_SLACK: Color = Color::Magenta;

// ─────────────────── Semantic styles ───────────────────

/// Main content text.
pub fn text() -> Style {
    Style::default().fg(FG)
}

/// Dimmed secondary text (timestamps, metadata).
pub fn dim() -> Style {
    Style::default().fg(FG_DIM)
}

/// Muted text (placeholders, hints).
pub fn muted() -> Style {
    Style::default().fg(FG_MUTED)
}

/// Bold emphasis (sender names, headers).
pub fn bold() -> Style {
    Style::default().fg(FG).add_modifier(Modifier::BOLD)
}

/// Active/selected item.
pub fn active() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Unread indicator.
pub fn unread() -> Style {
    Style::default()
        .fg(STATUS_WARN)
        .add_modifier(Modifier::BOLD)
}

/// Status bar background.
pub fn status_bar() -> Style {
    Style::default().fg(FG_MUTED).bg(Color::Black)
}

/// Mode badge (NORMAL, INSERT, etc).
pub fn mode_badge(mode: &str) -> Style {
    match mode {
        "INSERT" => Style::default()
            .fg(Color::Black)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD),
        "NORMAL" => Style::default().fg(FG_DIM).add_modifier(Modifier::BOLD),
        _ => Style::default().fg(FG_MUTED),
    }
}

/// Connection status indicator.
pub fn connection(connected: bool) -> Style {
    if connected {
        Style::default().fg(STATUS_OK)
    } else {
        Style::default().fg(STATUS_ERR)
    }
}

/// Input prompt character style.
pub fn prompt() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Subtle border style.
pub fn border() -> Style {
    Style::default().fg(BORDER)
}

/// Selected space in sidebar.
pub fn space_selected() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Reaction count.
pub fn reaction() -> Style {
    Style::default().fg(FG_MUTED)
}

/// Section header in sidebar.
pub fn section_header() -> Style {
    Style::default().fg(FG_DIM).add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    #[test]
    fn text_style() {
        let s = text();
        assert_eq!(s.fg, Some(Color::White));
    }

    #[test]
    fn dim_style() {
        let s = dim();
        assert_eq!(s.fg, Some(Color::DarkGray));
    }

    #[test]
    fn bold_style() {
        let s = bold();
        assert_eq!(s.fg, Some(Color::White));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn active_style() {
        let s = active();
        assert_eq!(s.fg, Some(Color::Cyan));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn unread_style() {
        let s = unread();
        assert_eq!(s.fg, Some(Color::Yellow));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn mode_badge_insert() {
        let s = mode_badge("INSERT");
        assert_eq!(s.fg, Some(Color::Black));
        assert_eq!(s.bg, Some(Color::Cyan));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn mode_badge_normal() {
        let s = mode_badge("NORMAL");
        assert_eq!(s.fg, Some(Color::DarkGray));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn connection_styles() {
        let s = connection(true);
        assert_eq!(s.fg, Some(Color::Green));

        let s = connection(false);
        assert_eq!(s.fg, Some(Color::Red));
    }

    #[test]
    fn prompt_style() {
        let s = prompt();
        assert_eq!(s.fg, Some(Color::Cyan));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn border_style() {
        let s = border();
        assert_eq!(s.fg, Some(Color::DarkGray));
    }
}
