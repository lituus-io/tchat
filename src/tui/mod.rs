pub mod layout;
pub mod theme;
pub mod widgets;

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;

use crate::event::OutboundCommand;
use crate::store::StoreRead;
use crate::types::{PlatformId, SpaceId, Timestamp, TopicId};

/// Actions produced by key handling. The main loop processes these.
pub enum Action {
    None,
    Redraw,
    Quit,
    Send(OutboundCommand),
}

/// Keyboard input mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Insert,
}

/// Connection status per platform, displayed in the status bar.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connected,
    Connecting,
    Reconnecting(u32),
    Disconnected,
}

/// Full TUI state. Owned exclusively by the main thread.
pub struct TuiState {
    pub layout_mode: layout::LayoutMode,
    pub input_mode: InputMode,
    pub active_space: Option<SpaceId>,
    pub active_thread: Option<TopicId>,
    pub space_cursor: usize,
    pub message_scroll: u16,
    pub input: InputState,
    pub size: (u16, u16),
    pub connections: [ConnectionStatus; 2],
    /// Last error message to show in the status bar.
    pub last_error: Option<String>,
    /// Spaces for which we have already fetched history (avoid re-fetching).
    pub fetched_spaces: HashSet<SpaceId>,
    /// Show the detailed status panel (toggle with 's').
    pub show_status_panel: bool,
    /// Session info for the status panel.
    pub session_info: SessionInfo,
}

/// Metadata about the current session, displayed in the status panel.
pub struct SessionInfo {
    pub self_user_name: String,
    pub self_user_id: String,
    pub self_user_email: String,
    pub auth_mode: String,
    pub total_spaces: usize,
    pub total_dms: usize,
    pub total_rooms: usize,
    pub xsrf_present: bool,
    pub browser_channel: String,
    pub platform_version: String,
}

impl Default for SessionInfo {
    fn default() -> Self {
        Self {
            self_user_name: String::new(),
            self_user_id: String::new(),
            self_user_email: String::new(),
            auth_mode: "unknown".into(),
            total_spaces: 0,
            total_dms: 0,
            total_rooms: 0,
            xsrf_present: false,
            browser_channel: "disabled".into(),
            platform_version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

/// Input line state.
pub struct InputState {
    pub text: String,
    pub cursor: usize,
}

impl TuiState {
    pub fn new() -> Self {
        Self {
            layout_mode: layout::LayoutMode::Standard,
            input_mode: InputMode::Normal,
            active_space: None,
            active_thread: None,
            space_cursor: 0,
            message_scroll: 0,
            input: InputState {
                text: String::new(),
                cursor: 0,
            },
            size: (80, 24),
            connections: [ConnectionStatus::Disconnected; 2],
            last_error: None,
            fetched_spaces: HashSet::new(),
            show_status_panel: false,
            session_info: SessionInfo::default(),
        }
    }

    pub fn active_platform(&self) -> PlatformId {
        self.active_space
            .map(|s| s.platform)
            .unwrap_or(PlatformId::GoogleChat)
    }

    /// Periodic tick — expire typing indicators, blink cursor, etc.
    pub fn tick(&mut self) {
        // Future: clear typing indicators after timeout
    }
}

impl Default for TuiState {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────── Key handling ───────────────────

/// Handle a key event, returning the action for the main loop to process.
pub fn handle_key<S: StoreRead>(state: &mut TuiState, key: KeyEvent, store: &S) -> Action {
    match state.input_mode {
        InputMode::Normal => handle_normal_key(state, key, store),
        InputMode::Insert => handle_insert_key(state, key),
    }
}

fn handle_normal_key<S: StoreRead>(state: &mut TuiState, key: KeyEvent, store: &S) -> Action {
    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('i') => {
            state.input_mode = InputMode::Insert;
            Action::Redraw
        }
        KeyCode::Char('j') | KeyCode::Down => {
            state.message_scroll = state.message_scroll.saturating_add(1);
            Action::Redraw
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.message_scroll = state.message_scroll.saturating_sub(1);
            Action::Redraw
        }
        KeyCode::Char('J') => {
            state.space_cursor = state.space_cursor.saturating_add(1);
            Action::Redraw
        }
        KeyCode::Char('K') => {
            state.space_cursor = state.space_cursor.saturating_sub(1);
            Action::Redraw
        }
        KeyCode::Char('f') => {
            state.layout_mode = match state.layout_mode {
                layout::LayoutMode::Focused => layout::LayoutMode::Standard,
                _ => layout::LayoutMode::Focused,
            };
            Action::Redraw
        }
        KeyCode::Char('t') => {
            state.layout_mode = match state.layout_mode {
                layout::LayoutMode::ThreadView => layout::LayoutMode::Standard,
                _ => layout::LayoutMode::ThreadView,
            };
            Action::Redraw
        }
        KeyCode::Char('s') => {
            state.show_status_panel = !state.show_status_panel;
            Action::Redraw
        }
        KeyCode::Enter => {
            // Select the space under cursor and fetch history if needed
            if let Some(space) = store.spaces_sorted().nth(state.space_cursor) {
                let space_id = space.id;
                state.active_space = Some(space_id);
                state.message_scroll = 0;

                if !state.fetched_spaces.contains(&space_id) {
                    state.fetched_spaces.insert(space_id);
                    return Action::Send(OutboundCommand::FetchHistory {
                        space_id,
                        before: Timestamp::MAX,
                        count: 50,
                    });
                }
            }
            Action::Redraw
        }
        KeyCode::Char('g') => {
            state.message_scroll = 0;
            Action::Redraw
        }
        KeyCode::Char('G') => {
            state.message_scroll = u16::MAX; // Will clamp in render
            Action::Redraw
        }
        _ => Action::None,
    }
}

fn handle_insert_key(state: &mut TuiState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc => {
            state.input_mode = InputMode::Normal;
            Action::Redraw
        }
        KeyCode::Enter => {
            if state.input.text.is_empty() {
                return Action::None;
            }
            let text = std::mem::take(&mut state.input.text);
            state.input.cursor = 0;
            if let Some(space_id) = state.active_space {
                Action::Send(OutboundCommand::SendMessage {
                    space_id,
                    text,
                    thread_id: state.active_thread,
                })
            } else {
                Action::None
            }
        }
        KeyCode::Char(c) => {
            state.input.text.insert(state.input.cursor, c);
            state.input.cursor += c.len_utf8();
            Action::Redraw
        }
        KeyCode::Backspace => {
            if state.input.cursor > 0 {
                let prev = state.input.text[..state.input.cursor]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                state.input.text.drain(prev..state.input.cursor);
                state.input.cursor = prev;
            }
            Action::Redraw
        }
        KeyCode::Left => {
            if state.input.cursor > 0 {
                let prev = state.input.text[..state.input.cursor]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                state.input.cursor = prev;
            }
            Action::Redraw
        }
        KeyCode::Right => {
            if state.input.cursor < state.input.text.len() {
                let next = state.input.text[state.input.cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| state.input.cursor + i)
                    .unwrap_or(state.input.text.len());
                state.input.cursor = next;
            }
            Action::Redraw
        }
        _ => Action::None,
    }
}

// ─────────────────── Rendering ───────────────────

/// Render the full TUI frame. Borrows store immutably (zero-copy via GATs).
pub fn render<S: StoreRead>(frame: &mut Frame, store: &S, state: &TuiState) {
    let areas = layout::compute_areas(state.layout_mode, frame.area());

    // Space list sidebar
    if let Some(sidebar_area) = areas.space_list {
        widgets::space_list::render(frame, sidebar_area, store, state);
    }

    // Message content area
    widgets::message_list::render(frame, areas.messages, store, state);

    // Status bar (full width)
    widgets::status_bar::render(frame, areas.status, store, state);

    // Input box (full width)
    widgets::input_box::render(frame, areas.input, state);

    // Status panel overlay (toggled with 's')
    widgets::status_panel::render(frame, frame.area(), state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    /// Minimal StoreRead implementation for key-handling tests.
    struct EmptyStore;

    impl crate::store::StoreRead for EmptyStore {
        type MsgIter<'a> = std::iter::Empty<&'a crate::types::Message>;
        type SpaceIter<'a> = std::iter::Empty<&'a crate::types::Space>;

        fn messages_in_space<'a>(&'a self, _space: SpaceId) -> Self::MsgIter<'a> {
            std::iter::empty()
        }
        fn spaces_sorted<'a>(&'a self) -> Self::SpaceIter<'a> {
            std::iter::empty()
        }
        fn user(&self, _id: crate::types::UserId) -> Option<&crate::types::User> {
            None
        }
        fn space(&self, _id: SpaceId) -> Option<&crate::types::Space> {
            None
        }
        fn self_user(&self, _platform: PlatformId) -> Option<crate::types::UserId> {
            None
        }
    }

    fn store() -> EmptyStore {
        EmptyStore
    }

    #[test]
    fn normal_mode_q_produces_quit() {
        let mut state = TuiState::new();
        let action = handle_key(&mut state, key(KeyCode::Char('q')), &store());
        assert!(matches!(action, Action::Quit));
    }

    #[test]
    fn normal_mode_i_enters_insert() {
        let mut state = TuiState::new();
        handle_key(&mut state, key(KeyCode::Char('i')), &store());
        assert_eq!(state.input_mode, InputMode::Insert);
    }

    #[test]
    fn insert_mode_esc_exits_to_normal() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        handle_key(&mut state, key(KeyCode::Esc), &store());
        assert_eq!(state.input_mode, InputMode::Normal);
    }

    #[test]
    fn insert_mode_char_inserts_text() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        handle_key(&mut state, key(KeyCode::Char('h')), &store());
        handle_key(&mut state, key(KeyCode::Char('i')), &store());
        assert_eq!(state.input.text, "hi");
        assert_eq!(state.input.cursor, 2);
    }

    #[test]
    fn insert_mode_backspace_deletes() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        handle_key(&mut state, key(KeyCode::Char('a')), &store());
        handle_key(&mut state, key(KeyCode::Char('b')), &store());
        handle_key(&mut state, key(KeyCode::Backspace), &store());
        assert_eq!(state.input.text, "a");
        assert_eq!(state.input.cursor, 1);
    }

    #[test]
    fn insert_mode_enter_produces_send_command() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        let space = SpaceId {
            platform: PlatformId::GoogleChat,
            id: crate::types::InternedId::MIN,
        };
        state.active_space = Some(space);
        handle_key(&mut state, key(KeyCode::Char('h')), &store());
        handle_key(&mut state, key(KeyCode::Char('i')), &store());
        let action = handle_key(&mut state, key(KeyCode::Enter), &store());
        assert!(matches!(
            action,
            Action::Send(OutboundCommand::SendMessage { .. })
        ));
        assert!(state.input.text.is_empty());
    }

    #[test]
    fn normal_mode_j_scrolls_down() {
        let mut state = TuiState::new();
        assert_eq!(state.message_scroll, 0);
        handle_key(&mut state, key(KeyCode::Char('j')), &store());
        assert_eq!(state.message_scroll, 1);
    }

    #[test]
    fn normal_mode_capital_j_switches_space() {
        let mut state = TuiState::new();
        assert_eq!(state.space_cursor, 0);
        handle_key(&mut state, key(KeyCode::Char('J')), &store());
        assert_eq!(state.space_cursor, 1);
    }

    #[test]
    fn normal_mode_g_scrolls_to_top() {
        let mut state = TuiState::new();
        state.message_scroll = 50;
        handle_key(&mut state, key(KeyCode::Char('g')), &store());
        assert_eq!(state.message_scroll, 0);
    }

    #[test]
    fn normal_mode_f_toggles_focused() {
        let mut state = TuiState::new();
        assert_eq!(state.layout_mode, layout::LayoutMode::Standard);
        handle_key(&mut state, key(KeyCode::Char('f')), &store());
        assert_eq!(state.layout_mode, layout::LayoutMode::Focused);
        handle_key(&mut state, key(KeyCode::Char('f')), &store());
        assert_eq!(state.layout_mode, layout::LayoutMode::Standard);
    }

    #[test]
    fn normal_mode_t_toggles_thread_view() {
        let mut state = TuiState::new();
        handle_key(&mut state, key(KeyCode::Char('t')), &store());
        assert_eq!(state.layout_mode, layout::LayoutMode::ThreadView);
        handle_key(&mut state, key(KeyCode::Char('t')), &store());
        assert_eq!(state.layout_mode, layout::LayoutMode::Standard);
    }

    #[test]
    fn insert_mode_left_right_moves_cursor() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        handle_key(&mut state, key(KeyCode::Char('a')), &store());
        handle_key(&mut state, key(KeyCode::Char('b')), &store());
        handle_key(&mut state, key(KeyCode::Char('c')), &store());
        assert_eq!(state.input.cursor, 3);

        handle_key(&mut state, key(KeyCode::Left), &store());
        assert_eq!(state.input.cursor, 2);

        handle_key(&mut state, key(KeyCode::Left), &store());
        assert_eq!(state.input.cursor, 1);

        handle_key(&mut state, key(KeyCode::Right), &store());
        assert_eq!(state.input.cursor, 2);
    }

    #[test]
    fn insert_mode_empty_enter_does_nothing() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        state.active_space = Some(SpaceId {
            platform: PlatformId::GoogleChat,
            id: crate::types::InternedId::MIN,
        });
        let action = handle_key(&mut state, key(KeyCode::Enter), &store());
        assert!(matches!(action, Action::None));
    }

    // ─────── Additional key handling tests ───────

    #[test]
    fn normal_mode_capital_g_scrolls_to_bottom() {
        let mut state = TuiState::new();
        handle_key(&mut state, key(KeyCode::Char('G')), &store());
        assert_eq!(state.message_scroll, u16::MAX);
    }

    #[test]
    fn normal_mode_k_scrolls_up() {
        let mut state = TuiState::new();
        state.message_scroll = 5;
        handle_key(&mut state, key(KeyCode::Char('k')), &store());
        assert_eq!(state.message_scroll, 4);
    }

    #[test]
    fn normal_mode_k_saturates_at_zero() {
        let mut state = TuiState::new();
        assert_eq!(state.message_scroll, 0);
        handle_key(&mut state, key(KeyCode::Char('k')), &store());
        assert_eq!(state.message_scroll, 0);
    }

    #[test]
    fn normal_mode_capital_k_moves_space_cursor_up() {
        let mut state = TuiState::new();
        state.space_cursor = 3;
        handle_key(&mut state, key(KeyCode::Char('K')), &store());
        assert_eq!(state.space_cursor, 2);
    }

    #[test]
    fn normal_mode_capital_k_saturates_at_zero() {
        let mut state = TuiState::new();
        assert_eq!(state.space_cursor, 0);
        handle_key(&mut state, key(KeyCode::Char('K')), &store());
        assert_eq!(state.space_cursor, 0);
    }

    #[test]
    fn normal_mode_down_arrow_scrolls() {
        let mut state = TuiState::new();
        handle_key(&mut state, key(KeyCode::Down), &store());
        assert_eq!(state.message_scroll, 1);
    }

    #[test]
    fn normal_mode_up_arrow_scrolls() {
        let mut state = TuiState::new();
        state.message_scroll = 2;
        handle_key(&mut state, key(KeyCode::Up), &store());
        assert_eq!(state.message_scroll, 1);
    }

    #[test]
    fn normal_mode_unrecognized_key_is_none() {
        let mut state = TuiState::new();
        let action = handle_key(&mut state, key(KeyCode::Char('z')), &store());
        assert!(matches!(action, Action::None));
    }

    #[test]
    fn insert_mode_enter_without_active_space_is_none() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        state.active_space = None;
        handle_key(&mut state, key(KeyCode::Char('h')), &store());
        handle_key(&mut state, key(KeyCode::Char('i')), &store());
        let action = handle_key(&mut state, key(KeyCode::Enter), &store());
        assert!(matches!(action, Action::None));
        // Text should be consumed even without active space
    }

    #[test]
    fn insert_mode_cursor_left_at_start_stays() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        assert_eq!(state.input.cursor, 0);
        handle_key(&mut state, key(KeyCode::Left), &store());
        assert_eq!(state.input.cursor, 0);
    }

    #[test]
    fn insert_mode_cursor_right_at_end_stays() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        handle_key(&mut state, key(KeyCode::Char('a')), &store());
        assert_eq!(state.input.cursor, 1);
        handle_key(&mut state, key(KeyCode::Right), &store());
        assert_eq!(state.input.cursor, 1); // already at end
    }

    #[test]
    fn insert_mode_backspace_at_start_does_nothing() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        handle_key(&mut state, key(KeyCode::Backspace), &store());
        assert_eq!(state.input.cursor, 0);
        assert!(state.input.text.is_empty());
    }

    #[test]
    fn insert_mode_multibyte_char_handles_cursor() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        // Insert a multibyte character
        handle_key(&mut state, key(KeyCode::Char('é')), &store());
        assert_eq!(state.input.text, "é");
        assert_eq!(state.input.cursor, 2); // é is 2 bytes in UTF-8
        handle_key(&mut state, key(KeyCode::Backspace), &store());
        assert!(state.input.text.is_empty());
        assert_eq!(state.input.cursor, 0);
    }

    #[test]
    fn insert_mode_unrecognized_key_is_none() {
        let mut state = TuiState::new();
        state.input_mode = InputMode::Insert;
        let action = handle_key(&mut state, key(KeyCode::F(1)), &store());
        assert!(matches!(action, Action::None));
    }

    #[test]
    fn normal_mode_enter_with_empty_store_just_redraws() {
        let mut state = TuiState::new();
        let action = handle_key(&mut state, key(KeyCode::Enter), &store());
        assert!(matches!(action, Action::Redraw));
        assert!(state.active_space.is_none());
    }

    #[test]
    fn tui_state_default_values() {
        let state = TuiState::new();
        assert_eq!(state.input_mode, InputMode::Normal);
        assert!(state.active_space.is_none());
        assert!(state.active_thread.is_none());
        assert_eq!(state.space_cursor, 0);
        assert_eq!(state.message_scroll, 0);
        assert!(state.input.text.is_empty());
        assert_eq!(state.input.cursor, 0);
        assert_eq!(state.size, (80, 24));
        assert!(state.last_error.is_none());
        assert!(state.fetched_spaces.is_empty());
        assert_eq!(state.connections, [ConnectionStatus::Disconnected; 2]);
    }

    #[test]
    fn active_platform_defaults_to_googlechat() {
        let state = TuiState::new();
        assert_eq!(state.active_platform(), PlatformId::GoogleChat);
    }

    #[test]
    fn active_platform_follows_active_space() {
        let mut state = TuiState::new();
        state.active_space = Some(SpaceId {
            platform: PlatformId::Slack,
            id: crate::types::InternedId::MIN,
        });
        assert_eq!(state.active_platform(), PlatformId::Slack);
    }

    #[test]
    fn layout_mode_starts_standard() {
        let state = TuiState::new();
        assert_eq!(state.layout_mode, layout::LayoutMode::Standard);
    }

    #[test]
    fn tick_does_not_crash() {
        let mut state = TuiState::new();
        state.tick(); // Should be a no-op but not panic
    }
}
