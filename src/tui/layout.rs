use ratatui::layout::{Constraint, Layout, Rect};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LayoutMode {
    /// Sidebar + messages. Clean two-column layout.
    Standard,
    /// Full-width message view, no sidebar.
    Focused,
    /// Two conversations side by side (future).
    Split,
    /// Messages + thread panel on right.
    ThreadView,
}

/// Computed screen areas for each widget.
pub struct LayoutAreas {
    pub space_list: Option<Rect>,
    pub messages: Rect,
    pub members: Option<Rect>,
    pub thread: Option<Rect>,
    pub status: Rect,
    pub input: Rect,
}

/// Compute widget areas for the given layout mode and terminal size.
pub fn compute_areas(mode: LayoutMode, area: Rect) -> LayoutAreas {
    // Status bar and input always span full width at the bottom
    let rows = Layout::vertical([
        Constraint::Min(4),    // content area
        Constraint::Length(1), // status bar
        Constraint::Length(2), // input
    ])
    .split(area);

    let content = rows[0];
    let status = rows[1];
    let input = rows[2];

    match mode {
        LayoutMode::Standard => standard_layout(content, status, input),
        LayoutMode::Focused => focused_layout(content, status, input),
        LayoutMode::ThreadView => thread_layout(content, status, input),
        LayoutMode::Split => standard_layout(content, status, input),
    }
}

fn standard_layout(content: Rect, status: Rect, input: Rect) -> LayoutAreas {
    // Sidebar width adapts: 24 chars for narrow, hide for very narrow
    let sidebar_width = if content.width >= 60 { 24 } else { 0 };

    if sidebar_width == 0 {
        return LayoutAreas {
            space_list: None,
            messages: content,
            members: None,
            thread: None,
            status,
            input,
        };
    }

    let columns =
        Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(30)]).split(content);

    LayoutAreas {
        space_list: Some(columns[0]),
        messages: columns[1],
        members: None, // Clean aesthetic — no members panel by default
        thread: None,
        status,
        input,
    }
}

fn focused_layout(content: Rect, status: Rect, input: Rect) -> LayoutAreas {
    LayoutAreas {
        space_list: None,
        messages: content,
        members: None,
        thread: None,
        status,
        input,
    }
}

fn thread_layout(content: Rect, status: Rect, input: Rect) -> LayoutAreas {
    let sidebar_width = if content.width >= 80 { 24 } else { 0 };

    let columns = if sidebar_width > 0 {
        Layout::horizontal([
            Constraint::Length(sidebar_width),
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(content)
    } else {
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(content)
    };

    if sidebar_width > 0 {
        LayoutAreas {
            space_list: Some(columns[0]),
            messages: columns[1],
            members: None,
            thread: Some(columns[2]),
            status,
            input,
        }
    } else {
        LayoutAreas {
            space_list: None,
            messages: columns[0],
            members: None,
            thread: Some(columns[1]),
            status,
            input,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    #[test]
    fn standard_layout_allocates_sidebar_and_messages() {
        let areas = compute_areas(LayoutMode::Standard, rect(120, 40));
        assert!(areas.space_list.is_some());
        assert_eq!(areas.space_list.unwrap().width, 24);
        // Status and input span full width
        assert_eq!(areas.status.width, 120);
        assert_eq!(areas.input.width, 120);
    }

    #[test]
    fn focused_layout_has_no_sidebar() {
        let areas = compute_areas(LayoutMode::Focused, rect(120, 40));
        assert!(areas.space_list.is_none());
        assert!(areas.members.is_none());
        assert!(areas.thread.is_none());
        assert_eq!(areas.messages.width, 120);
    }

    #[test]
    fn thread_layout_splits_message_area() {
        let areas = compute_areas(LayoutMode::ThreadView, rect(120, 40));
        assert!(areas.space_list.is_some());
        assert!(areas.thread.is_some());
        assert!(areas.members.is_none());
    }

    #[test]
    fn small_terminal_hides_sidebar() {
        let areas = compute_areas(LayoutMode::Standard, rect(50, 20));
        assert!(areas.space_list.is_none());
    }

    #[test]
    fn status_and_input_always_full_width() {
        for mode in [
            LayoutMode::Standard,
            LayoutMode::Focused,
            LayoutMode::ThreadView,
        ] {
            let areas = compute_areas(mode, rect(100, 30));
            assert_eq!(areas.status.width, 100, "status width for {mode:?}");
            assert_eq!(areas.input.width, 100, "input width for {mode:?}");
        }
    }

    #[test]
    fn standard_layout_sidebar_width_is_24() {
        let areas = compute_areas(LayoutMode::Standard, rect(80, 24));
        let sidebar = areas.space_list.unwrap();
        assert_eq!(sidebar.width, 24);
    }

    #[test]
    fn standard_layout_messages_fill_remaining() {
        let areas = compute_areas(LayoutMode::Standard, rect(80, 24));
        let sidebar = areas.space_list.unwrap();
        assert_eq!(sidebar.width + areas.messages.width, 80);
    }

    #[test]
    fn thread_layout_hides_sidebar_on_narrow() {
        let areas = compute_areas(LayoutMode::ThreadView, rect(70, 24));
        assert!(areas.space_list.is_none());
        assert!(areas.thread.is_some());
    }

    #[test]
    fn thread_layout_shows_sidebar_on_wide() {
        let areas = compute_areas(LayoutMode::ThreadView, rect(120, 40));
        assert!(areas.space_list.is_some());
        assert!(areas.thread.is_some());
        assert_eq!(areas.space_list.unwrap().width, 24);
    }

    #[test]
    fn split_mode_falls_back_to_standard() {
        let areas_split = compute_areas(LayoutMode::Split, rect(100, 30));
        let areas_std = compute_areas(LayoutMode::Standard, rect(100, 30));
        assert_eq!(areas_split.messages.width, areas_std.messages.width);
        assert_eq!(
            areas_split.space_list.map(|r| r.width),
            areas_std.space_list.map(|r| r.width)
        );
    }

    #[test]
    fn status_bar_is_one_row() {
        let areas = compute_areas(LayoutMode::Standard, rect(80, 24));
        assert_eq!(areas.status.height, 1);
    }

    #[test]
    fn input_area_is_two_rows() {
        let areas = compute_areas(LayoutMode::Standard, rect(80, 24));
        assert_eq!(areas.input.height, 2);
    }

    #[test]
    fn minimum_content_height() {
        // Even with tiny terminal, content area should have at least min height
        let areas = compute_areas(LayoutMode::Standard, rect(80, 7));
        assert!(areas.messages.height >= 4);
    }

    #[test]
    fn members_always_none_for_now() {
        for mode in [
            LayoutMode::Standard,
            LayoutMode::Focused,
            LayoutMode::ThreadView,
            LayoutMode::Split,
        ] {
            let areas = compute_areas(mode, rect(120, 40));
            assert!(
                areas.members.is_none(),
                "members should be None for {mode:?}"
            );
        }
    }

    #[test]
    fn thread_panel_absent_in_standard_and_focused() {
        let std = compute_areas(LayoutMode::Standard, rect(100, 30));
        let foc = compute_areas(LayoutMode::Focused, rect(100, 30));
        assert!(std.thread.is_none());
        assert!(foc.thread.is_none());
    }

    #[test]
    fn exact_boundary_60_shows_sidebar() {
        let areas = compute_areas(LayoutMode::Standard, rect(60, 20));
        assert!(areas.space_list.is_some());
    }

    #[test]
    fn just_below_boundary_59_hides_sidebar() {
        let areas = compute_areas(LayoutMode::Standard, rect(59, 20));
        assert!(areas.space_list.is_none());
    }
}
