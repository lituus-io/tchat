use crossbeam::channel::Sender;
use crossterm::event::{self, Event};

/// Terminal event wrapper sent from the input thread to the main loop.
pub enum TerminalEvent {
    Key(crossterm::event::KeyEvent),
    Resize(u16, u16),
}

/// Terminal input loop. Runs on a dedicated thread.
///
/// Reads terminal events via crossterm (blocking) and sends them through
/// the channel to the main loop. Exits when the channel receiver is dropped.
pub fn input_loop(tx: Sender<TerminalEvent>) {
    loop {
        match event::read() {
            Ok(Event::Key(key)) => {
                if tx.send(TerminalEvent::Key(key)).is_err() {
                    break; // Main thread dropped the receiver
                }
            }
            Ok(Event::Resize(w, h)) => {
                if tx.send(TerminalEvent::Resize(w, h)).is_err() {
                    break;
                }
            }
            Ok(_) => {} // Mouse events, paste events — ignore for now
            Err(_) => break,
        }
    }
}
