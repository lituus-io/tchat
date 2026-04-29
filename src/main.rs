use std::time::Duration;

use crossbeam::channel::{self, Receiver, Sender};

use tchat::config::{Config, PlatformKind};
use tchat::error::AppError;
use tchat::event::{InboundEvent, OutboundCommand};
use tchat::platform;
use tchat::store::Store;
use tchat::terminal::{self, TerminalEvent};
use tchat::tui::{self, Action, ConnectionStatus, TuiState};
use tchat::types::PlatformId;

fn main() -> Result<(), AppError> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .init();

    // First non-flag argv slot decides the mode: known subcommand →
    // agent CLI handler; otherwise → existing TUI startup.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if tchat::agent::cli::dispatch(&argv)? {
        return Ok(());
    }

    let config = Config::load()?;
    let config = tchat::setup::ensure_configured(config);

    // Authentication strategy:
    // 1. Try to extract cookies from Chrome's database (no Chrome process needed)
    // 2. Fall back to launching Chrome for interactive auth
    enum AuthMode {
        Direct(platform::googlechat::direct::DirectSession),
        Chrome(platform::googlechat::auth::Tokens),
    }

    let mut auth_mode = None;
    for platform_config in &config.platforms {
        match platform_config.kind {
            PlatformKind::GoogleChat => {
                eprintln!("  Connecting to Google Chat...");

                // Strategy:
                // 1. Try loading saved cookies (from a previous Chrome auth)
                // 2. Try extracting from Chrome's DB (if user is logged in)
                // 3. Fall back to launching Chrome for interactive auth
                //    (saves cookies for next time)

                // Try direct HTTP first: load cookies from tchat's Chrome profile DB
                // or from saved JSON. Validate by fetching XSRF.
                let direct_ok = 'direct: {
                    // Try: saved JSON cookies (from previous CDP extraction)
                    // Then: tchat's persistent Chrome profile DB
                    // Then: user's regular Chrome DB
                    let cookies = platform::googlechat::cookies::load_saved_cookies()
                        .or_else(|_| platform::googlechat::cookies::extract_chrome_cookies());
                    if let Ok(cookies) = cookies {
                        let n = cookies.cookies.len();
                        eprintln!("  Found {n} cookies, validating...");
                        let mut session = platform::googlechat::direct::DirectSession::new(cookies);
                        match session.fetch_xsrf_token() {
                            Ok(()) => {
                                eprintln!("  \x1b[32m✓\x1b[0m Direct mode — no Chrome needed");
                                auth_mode = Some(AuthMode::Direct(session));
                                break 'direct true;
                            }
                            Err(e) => {
                                eprintln!("  \x1b[33m!\x1b[0m Cookies invalid: {e}");
                            }
                        }
                    }
                    false
                };
                if direct_ok {
                    // Already set auth_mode above
                } else {
                    eprintln!("  Launching Chrome for authentication...");
                    match platform::googlechat::auth::authenticate(
                        platform_config.account.as_deref(),
                    ) {
                        Ok(tokens) => {
                            eprintln!("  \x1b[32m✓\x1b[0m Google Chat authenticated");

                            // Extract cookies from Chrome and switch to direct HTTP.
                            // This lets us close Chrome immediately (it's unstable for
                            // long sessions) and use fast direct HTTP for the IO loop.
                            if let Ok(tab) = tokens.get_tab() {
                                match platform::googlechat::cookies::extract_from_chrome_session(
                                    &tab,
                                ) {
                                    Ok(cookies) => {
                                        // Save for future runs
                                        let _ =
                                            platform::googlechat::cookies::save_cookies(&cookies);

                                        // Validate cookies work
                                        let mut session =
                                            platform::googlechat::direct::DirectSession::new(
                                                cookies,
                                            );
                                        if session.fetch_xsrf_token().is_ok() {
                                            eprintln!("  \x1b[32m✓\x1b[0m Switched to direct HTTP (Chrome closing)");
                                            // Drop tokens → Chrome process exits
                                            drop(tokens);
                                            auth_mode = Some(AuthMode::Direct(session));
                                        } else {
                                            eprintln!("  \x1b[33m!\x1b[0m Direct mode failed, using Chrome");
                                            auth_mode = Some(AuthMode::Chrome(tokens));
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "  \x1b[33m!\x1b[0m Cookie extraction failed: {e}"
                                        );
                                        auth_mode = Some(AuthMode::Chrome(tokens));
                                    }
                                }
                            } else {
                                auth_mode = Some(AuthMode::Chrome(tokens));
                            }
                        }
                        Err(e) => {
                            eprintln!("  \x1b[31m✗\x1b[0m Authentication failed: {e}");
                            eprintln!("    Run tchat again to retry.");
                            std::process::exit(1);
                        }
                    }
                }
            }
            PlatformKind::Slack => {
                eprintln!("  \x1b[33m!\x1b[0m Slack not yet implemented");
            }
        }
    }
    eprintln!();

    let (inbound_tx, inbound_rx) = channel::unbounded::<InboundEvent>();
    let (terminal_tx, terminal_rx) = channel::unbounded::<TerminalEvent>();
    let mut outbound_channels: Vec<(PlatformId, Sender<OutboundCommand>)> = Vec::new();

    // Use regular threads (not scoped) so we can force-exit on quit.
    // Scoped threads block until ALL join, but the terminal input thread
    // blocks on crossterm::event::read() and can't be interrupted.

    let _terminal_handle = std::thread::spawn(move || terminal::input_loop(terminal_tx));

    if let Some(mode) = auth_mode {
        let (cmd_tx, cmd_rx) = channel::unbounded::<OutboundCommand>();
        let inbound = inbound_tx.clone();
        outbound_channels.push((PlatformId::GoogleChat, cmd_tx));

        match mode {
            AuthMode::Direct(session) => {
                std::thread::spawn(move || {
                    platform::googlechat::io_loop_direct(session, inbound, cmd_rx);
                });
            }
            AuthMode::Chrome(tokens) => {
                std::thread::spawn(move || {
                    platform::googlechat::io_loop_with_tokens(tokens, inbound, cmd_rx);
                });
            }
        }
    }

    drop(inbound_tx);

    let result = main_loop(&config, inbound_rx, terminal_rx, &outbound_channels);

    // Force-kill any remaining Chrome processes on exit
    let _ = std::process::Command::new("pkill")
        .args(["-f", "chrome.*tchat"])
        .output();

    // Exit the process cleanly — don't wait for blocked threads
    // (terminal input thread is stuck in crossterm::event::read)
    if result.is_ok() {
        std::process::exit(0);
    }
    result
}

fn main_loop(
    _config: &Config,
    inbound_rx: Receiver<InboundEvent>,
    terminal_rx: Receiver<TerminalEvent>,
    outbound: &[(PlatformId, Sender<OutboundCommand>)],
) -> Result<(), AppError> {
    let mut store = Store::new();
    let mut tui_state = TuiState::new();

    // Set auth mode info for the status panel
    // (determined during auth in main())
    // We detect this by checking if Chrome is mentioned in stderr output
    // For now, default to "Chrome (persistent profile)"
    tui_state.session_info.auth_mode = "Chrome (persistent profile)".into();
    tui_state.session_info.xsrf_present = true;
    let mut terminal = ratatui::init();

    let result = run_event_loop(
        &mut store,
        &mut tui_state,
        &mut terminal,
        &inbound_rx,
        &terminal_rx,
        outbound,
    );

    ratatui::restore();
    result
}

fn run_event_loop(
    store: &mut Store,
    tui_state: &mut TuiState,
    terminal: &mut ratatui::DefaultTerminal,
    inbound_rx: &Receiver<InboundEvent>,
    terminal_rx: &Receiver<TerminalEvent>,
    outbound: &[(PlatformId, Sender<OutboundCommand>)],
) -> Result<(), AppError> {
    loop {
        terminal.draw(|frame| {
            tui::render(frame, store, tui_state);
        })?;

        crossbeam::channel::select! {
            recv(inbound_rx) -> event => {
                match event {
                    Ok(ev) => handle_inbound(store, tui_state, ev),
                    Err(_) => {} // Platform threads exited — keep TUI running
                }
            }
            recv(terminal_rx) -> input => {
                match input {
                    Ok(TerminalEvent::Key(key)) => {
                        let action = tui::handle_key(tui_state, key, store);
                        match action {
                            Action::Quit => {
                                for (_, tx) in outbound {
                                    let _ = tx.send(OutboundCommand::Disconnect);
                                }
                                break;
                            }
                            Action::Send(cmd) => {
                                let target = tui_state.active_platform();
                                platform::dispatch_command(outbound, target, cmd);
                            }
                            Action::Redraw | Action::None => {}
                        }
                    }
                    Ok(TerminalEvent::Resize(w, h)) => {
                        tui_state.size = (w, h);
                    }
                    Err(_) => break,
                }
            }
            default(Duration::from_millis(100)) => {
                tui_state.tick();
            }
        }
    }

    Ok(())
}

fn handle_inbound(store: &mut Store, tui_state: &mut TuiState, event: InboundEvent) {
    match &event {
        InboundEvent::Connected { platform } => {
            set_connection(tui_state, *platform, ConnectionStatus::Connected);
        }
        InboundEvent::Disconnected { platform, reason } => {
            set_connection(tui_state, *platform, ConnectionStatus::Disconnected);
            tui_state.last_error = Some(format!("{reason:?}"));
        }
        InboundEvent::Reconnecting {
            platform, attempt, ..
        } => {
            set_connection(
                tui_state,
                *platform,
                ConnectionStatus::Reconnecting(*attempt),
            );
        }
        _ => {}
    }

    if let InboundEvent::WorldSync {
        ref spaces,
        ref self_user,
        ..
    } = event
    {
        if tui_state.active_space.is_none() {
            if let Some(first) = spaces.first() {
                tui_state.active_space = Some(first.id);
            }
        }
        // Update session info for the status panel
        tui_state.session_info.self_user_name = self_user.display_name.clone();
        tui_state.session_info.self_user_email = self_user.email.clone().unwrap_or_default();
        tui_state.session_info.total_spaces = spaces.len();
        tui_state.session_info.total_rooms = spaces
            .iter()
            .filter(|s| {
                matches!(
                    s.kind,
                    tchat::types::SpaceKind::Room | tchat::types::SpaceKind::ThreadedRoom
                )
            })
            .count();
        tui_state.session_info.total_dms = spaces
            .iter()
            .filter(|s| {
                matches!(
                    s.kind,
                    tchat::types::SpaceKind::DirectMessage | tchat::types::SpaceKind::GroupDm
                )
            })
            .count();
    }

    store.ingest(event);
}

fn set_connection(state: &mut TuiState, platform: PlatformId, status: ConnectionStatus) {
    let idx = match platform {
        PlatformId::GoogleChat => 0,
        PlatformId::Slack => 1,
    };
    state.connections[idx] = status;
}
