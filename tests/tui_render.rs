use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::Terminal;

use tchat::event::InboundEvent;
use tchat::store::Store;
use tchat::tui::widgets::{input_box, message_list, space_list, status_bar};
use tchat::tui::{ConnectionStatus, InputMode, TuiState};
use tchat::types::*;

// ─────────────────── Helpers ───────────────────

fn buffer_to_string(buf: &Buffer) -> String {
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf.cell((x, y)).unwrap().symbol());
        }
        s.push('\n');
    }
    s
}

fn make_store() -> Store {
    Store::new()
}

fn add_space(store: &mut Store, name: &str) -> SpaceId {
    let id = store.interner.intern(name);
    let space_id = SpaceId {
        platform: PlatformId::GoogleChat,
        id,
    };
    let space = Space {
        id: space_id,
        name: name.to_owned(),
        kind: SpaceKind::Room,
        platform: PlatformId::GoogleChat,
        unread_count: 0,
        last_activity: Timestamp::ZERO,
        sort_timestamp: Timestamp::ZERO,
        typing_users: Vec::new(),
    };
    store.ingest(InboundEvent::SpaceUpdated { space });
    space_id
}

fn add_user(store: &mut Store, name: &str) -> UserId {
    let id = store.interner.intern(name);
    let user_id = UserId {
        platform: PlatformId::GoogleChat,
        id,
    };
    store.ingest(InboundEvent::WorldSync {
        platform: PlatformId::GoogleChat,
        spaces: vec![],
        self_user: User {
            id: user_id,
            display_name: name.to_owned(),
            email: None,
            avatar_url: None,
            presence: PresenceStatus::Active,
            is_bot: false,
        },
    });
    user_id
}

fn add_message(store: &mut Store, space: SpaceId, sender: UserId, text: &str, ts: u64) {
    let msg_id = MessageId(store.interner.intern(&format!("msg_{ts}")));
    let message = Message {
        id: msg_id,
        space_id: space,
        sender,
        timestamp: Timestamp(ts),
        edit_timestamp: None,
        text: text.to_owned(),
        annotations: Vec::new(),
        reactions: Vec::new(),
        thread_id: None,
        message_type: MessageType::User,
        platform: PlatformId::GoogleChat,
    };
    store.ingest(InboundEvent::MessagePosted {
        message,
        space_id_raw: String::new(),
        topic_id_raw: None,
        message_id_raw: String::new(),
    });
}

// ─────────────────── space_list tests ───────────────────

#[test]
fn space_list_empty_store_shows_no_spaces() {
    let store = make_store();
    let state = TuiState::new();
    let area = Rect::new(0, 0, 30, 10);

    let backend = TestBackend::new(30, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            space_list::render(frame, area, &store, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains("No spaces yet"),
        "Expected 'No spaces yet' in:\n{text}"
    );
}

#[test]
fn space_list_shows_both_space_names() {
    let mut store = make_store();
    add_space(&mut store, "team-eng");
    add_space(&mut store, "general");
    let state = TuiState::new();
    let area = Rect::new(0, 0, 30, 10);

    let backend = TestBackend::new(30, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            space_list::render(frame, area, &store, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(text.contains("team-eng"), "Expected 'team-eng' in:\n{text}");
    assert!(text.contains("general"), "Expected 'general' in:\n{text}");
}

#[test]
fn space_list_active_space_shows_bullet() {
    let mut store = make_store();
    let space_id = add_space(&mut store, "active-room");
    let mut state = TuiState::new();
    state.active_space = Some(space_id);
    let area = Rect::new(0, 0, 30, 10);

    let backend = TestBackend::new(30, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            space_list::render(frame, area, &store, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains('●'),
        "Expected bullet indicator for active space in:\n{text}"
    );
}

#[test]
fn space_list_unread_count_shows_bracket_number() {
    let mut store = make_store();
    let space_id = add_space(&mut store, "alerts");
    store.ingest(InboundEvent::ReadStateUpdated {
        space_id,
        last_read: Timestamp::ZERO,
        unread_count: 7,
    });
    let state = TuiState::new();
    let area = Rect::new(0, 0, 30, 10);

    let backend = TestBackend::new(30, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            space_list::render(frame, area, &store, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains("[7]"),
        "Expected '[7]' unread count in:\n{text}"
    );
}

// ─────────────────── message_list tests ───────────────────

#[test]
fn message_list_no_active_space_shows_help() {
    let store = make_store();
    let state = TuiState::new();
    let area = Rect::new(0, 0, 50, 15);

    let backend = TestBackend::new(50, 15);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            message_list::render(frame, area, &store, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains("tchat"),
        "Expected 'tchat' in help text:\n{text}"
    );
    assert!(
        text.contains("j/k"),
        "Expected 'j/k scroll' hint in help text:\n{text}"
    );
}

#[test]
fn message_list_active_space_no_messages_shows_empty() {
    let mut store = make_store();
    let space_id = add_space(&mut store, "empty-room");
    let mut state = TuiState::new();
    state.active_space = Some(space_id);
    let area = Rect::new(0, 0, 50, 15);

    let backend = TestBackend::new(50, 15);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            message_list::render(frame, area, &store, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains("No messages yet"),
        "Expected 'No messages yet' in:\n{text}"
    );
}

#[test]
fn message_list_shows_sender_and_text() {
    let mut store = make_store();
    let space_id = add_space(&mut store, "chat-room");
    let sender = add_user(&mut store, "Alice");
    add_message(&mut store, space_id, sender, "Hello world", 3_600_000_000);

    let mut state = TuiState::new();
    state.active_space = Some(space_id);
    let area = Rect::new(0, 0, 60, 15);

    let backend = TestBackend::new(60, 15);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            message_list::render(frame, area, &store, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains("Alice"),
        "Expected sender 'Alice' in:\n{text}"
    );
    assert!(
        text.contains("Hello world"),
        "Expected message text 'Hello world' in:\n{text}"
    );
}

// ─────────────────── input_box tests ───────────────────

#[test]
fn input_box_normal_mode_shows_hint() {
    let state = TuiState::new();
    let area = Rect::new(0, 0, 50, 3);

    let backend = TestBackend::new(50, 3);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            input_box::render(frame, area, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains("press i to compose"),
        "Expected 'press i to compose' hint in:\n{text}"
    );
}

#[test]
fn input_box_insert_mode_with_text_shows_content() {
    let mut state = TuiState::new();
    state.input_mode = InputMode::Insert;
    state.input.text = "Hello team".to_owned();
    state.input.cursor = 10;
    let area = Rect::new(0, 0, 50, 3);

    let backend = TestBackend::new(50, 3);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            input_box::render(frame, area, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains("Hello team"),
        "Expected input text 'Hello team' in:\n{text}"
    );
}

// ─────────────────── status_bar tests ───────────────────

#[test]
fn status_bar_connected_shows_connected() {
    let store = make_store();
    let mut state = TuiState::new();
    state.connections[0] = ConnectionStatus::Connected;
    let area = Rect::new(0, 0, 60, 1);

    let backend = TestBackend::new(60, 1);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            status_bar::render(frame, area, &store, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains("connected"),
        "Expected 'connected' in status bar:\n{text}"
    );
}

#[test]
fn status_bar_disconnected_shows_disconnected() {
    let store = make_store();
    let mut state = TuiState::new();
    state.connections[0] = ConnectionStatus::Disconnected;
    let area = Rect::new(0, 0, 60, 1);

    let backend = TestBackend::new(60, 1);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            status_bar::render(frame, area, &store, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains("disconnected"),
        "Expected 'disconnected' in status bar:\n{text}"
    );
}

#[test]
fn status_bar_active_space_shows_name() {
    let mut store = make_store();
    let space_id = add_space(&mut store, "team-eng");
    let mut state = TuiState::new();
    state.connections[0] = ConnectionStatus::Connected;
    state.active_space = Some(space_id);
    let area = Rect::new(0, 0, 60, 1);

    let backend = TestBackend::new(60, 1);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            status_bar::render(frame, area, &store, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains("team-eng"),
        "Expected space name 'team-eng' in status bar:\n{text}"
    );
}

#[test]
fn status_bar_insert_mode_shows_insert() {
    let store = make_store();
    let mut state = TuiState::new();
    state.connections[0] = ConnectionStatus::Connected;
    state.input_mode = InputMode::Insert;
    let area = Rect::new(0, 0, 60, 1);

    let backend = TestBackend::new(60, 1);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            status_bar::render(frame, area, &store, &state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buf);
    assert!(
        text.contains("INSERT"),
        "Expected 'INSERT' mode badge in status bar:\n{text}"
    );
}
