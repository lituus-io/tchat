pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

pub mod api;
pub mod auth;
pub mod channel;
pub mod chunk;
pub mod convert;
pub mod cookies;
pub mod direct;
pub mod pblite;
pub mod search;
pub mod session;

use crate::event::{InboundEvent, OutboundCommand};
use crate::types::PlatformId;
use crossbeam::channel::{Receiver, Sender};

/// IO loop using direct HTTP with extracted cookies (no Chrome process).
///
/// Call this instead of `io_loop_with_tokens` when cookies are available.
pub fn io_loop_direct(
    mut session: direct::DirectSession,
    inbound_tx: Sender<InboundEvent>,
    outbound_rx: Receiver<OutboundCommand>,
) {
    // Fetch XSRF token
    if let Err(e) = session.fetch_xsrf_token() {
        tracing::warn!("XSRF fetch failed: {e}");
        let _ = inbound_tx.send(InboundEvent::Disconnected {
            platform: PlatformId::GoogleChat,
            reason: crate::event::DisconnectReason::AuthFailed(e.to_string()),
        });
        return;
    }
    tracing::warn!("XSRF token obtained");

    // Fetch spaces entirely via proto API — no DOM extraction needed.
    let (spaces, self_user) = fetch_spaces_via_proto(&mut session);
    let _ = inbound_tx.send(InboundEvent::WorldSync {
        platform: PlatformId::GoogleChat,
        spaces,
        self_user,
    });

    let _ = inbound_tx.send(InboundEvent::Connected {
        platform: PlatformId::GoogleChat,
    });
    tracing::warn!("Google Chat connected (direct mode)");

    // Spawn a direct-mode BrowserChannel thread for real-time events.
    // It runs against a clone of the cookies so the command session here
    // remains independent.
    {
        let bc_cookies = session.cookies.clone();
        let bc_xsrf = session.xsrf_token.clone();
        let bc_tx = inbound_tx.clone();
        std::thread::spawn(move || {
            let mut bc_session = direct::DirectSession::new(bc_cookies);
            bc_session.xsrf_token = bc_xsrf;
            channel::long_poll_loop_direct(bc_session, bc_tx);
            tracing::warn!("Direct-mode BrowserChannel thread exited");
        });
    }

    // Command loop — same as Chrome mode but via DirectSession
    for cmd in outbound_rx {
        match cmd {
            OutboundCommand::Disconnect => break,
            other => {
                let cmd_space_id = match &other {
                    OutboundCommand::FetchHistory { space_id, .. } => Some(*space_id),
                    _ => None,
                };

                let interner = &session.interner;
                if let Some(req) = convert::command_to_api_request(other, interner) {
                    dispatch_api_request_direct(&mut session, req, cmd_space_id, &inbound_tx);
                }
            }
        }
    }

    let _ = inbound_tx.send(InboundEvent::Disconnected {
        platform: PlatformId::GoogleChat,
        reason: crate::event::DisconnectReason::Shutdown,
    });
}

/// Dispatch a single API request using the direct HTTP session.
fn dispatch_api_request_direct(
    session: &mut direct::DirectSession,
    req: api::ApiRequest,
    cmd_space_id: Option<crate::types::SpaceId>,
    inbound_tx: &Sender<InboundEvent>,
) {
    // Helper macro for calling protobuf APIs
    macro_rules! call_direct {
        ($session:expr, $endpoint:expr, $req:expr, $resp_type:ty) => {{
            let body = prost::Message::encode_to_vec($req);
            $session
                .call_api($endpoint, &body)
                .map_err(|e| crate::error::ApiError::Http(e.to_string()))
                .and_then(|bytes| {
                    <$resp_type as prost::Message>::decode(bytes::Bytes::from(bytes))
                        .map_err(crate::error::ApiError::ProtoDecode)
                })
        }};
    }

    match req {
        api::ApiRequest::SendMessage(ref r) => {
            match call_direct!(session, "create_message", r, proto::CreateMessageResponse) {
                Ok(_) => tracing::debug!("Message sent"),
                Err(e) => tracing::warn!("Send failed: {e}"),
            }
        }
        api::ApiRequest::ListTopics(ref r) => {
            let space_id = cmd_space_id.unwrap_or(crate::types::SpaceId {
                platform: PlatformId::GoogleChat,
                id: crate::types::InternedId::MIN,
            });
            match call_direct!(session, "list_topics", r, proto::ListTopicsResponse) {
                Ok(resp) => {
                    let event = convert::list_topics_response_to_event_with_interner(
                        resp,
                        space_id,
                        &mut session.interner,
                    );
                    let _ = inbound_tx.send(event);
                }
                Err(e) => tracing::warn!("ListTopics failed: {e}"),
            }
        }
        api::ApiRequest::EditMessage(ref r) => {
            match call_direct!(session, "edit_message", r, proto::EditMessageResponse) {
                Ok(_) => tracing::debug!("Message edited"),
                Err(e) => tracing::warn!("Edit failed: {e}"),
            }
        }
        api::ApiRequest::DeleteMessage(ref r) => {
            match call_direct!(session, "delete_message", r, proto::DeleteMessageResponse) {
                Ok(_) => tracing::debug!("Message deleted"),
                Err(e) => tracing::warn!("Delete failed: {e}"),
            }
        }
        api::ApiRequest::SetTyping(ref r) => {
            let _ = call_direct!(
                session,
                "set_typing_state",
                r,
                proto::SetTypingStateResponse
            );
        }
        api::ApiRequest::MarkRead(ref r) => {
            let _ = call_direct!(
                session,
                "mark_group_readstate",
                r,
                proto::MarkGroupReadstateResponse
            );
        }
        api::ApiRequest::UpdateReaction(ref r) => {
            match call_direct!(session, "update_reaction", r, proto::UpdateReactionResponse) {
                Ok(_) => tracing::debug!("Reaction updated"),
                Err(e) => tracing::warn!("Reaction failed: {e}"),
            }
        }
        api::ApiRequest::GetGroup(ref r) => {
            let _ = call_direct!(session, "get_group", r, proto::GetGroupResponse);
        }
        api::ApiRequest::GetMembers(ref r) => {
            match call_direct!(session, "get_members", r, proto::GetMembersResponse) {
                Ok(resp) => {
                    // Use the session wrapper which has interner
                    let fake_session_interner = &mut session.interner;
                    let mut users = Vec::new();
                    for member in resp.members {
                        if let Some(u) = member.user {
                            if let Some(uid) = &u.user_id {
                                if let Some(id_str) = &uid.id {
                                    let interned = fake_session_interner.intern(id_str);
                                    users.push(crate::types::User {
                                        id: crate::types::UserId {
                                            platform: PlatformId::GoogleChat,
                                            id: interned,
                                        },
                                        display_name: u.name.unwrap_or_default(),
                                        email: u.email,
                                        avatar_url: u.avatar_url,
                                        presence: crate::types::PresenceStatus::Unknown,
                                        is_bot: u.bot_info.is_some(),
                                    });
                                }
                            }
                        }
                    }
                    let _ = inbound_tx.send(InboundEvent::UsersResolved { users });
                }
                Err(e) => tracing::warn!("GetMembers failed: {e}"),
            }
        }
        _ => {
            tracing::debug!("Direct mode: unhandled API request variant");
        }
    }
}

/// IO loop with pre-authenticated tokens (Chrome already launched).
///
/// Called from main.rs after `auth::authenticate()` completes before TUI.
/// The Tokens contain a live Chrome tab for proxying API calls.
pub fn io_loop_with_tokens(
    tokens: auth::Tokens,
    inbound_tx: Sender<InboundEvent>,
    outbound_rx: Receiver<OutboundCommand>,
) {
    let mut session = session::Session::new(tokens);

    // Ensure we have a tab on chat.google.com. The auth flow may have
    // bailed early (SID cookie present from previous session) before the
    // SAML SSO redirect chain completed, leaving all tabs on the IdP.
    {
        let browser = session.tokens.browser.as_ref().unwrap();
        // Use the last tab (the auth tab created by browser.new_tab())
        let tab = {
            let tabs = browser.get_tabs().lock().unwrap();
            tabs.last().cloned().unwrap()
        };

        let url = tab.get_url();
        eprintln!("  Auth tab URL: {url}");

        if !url.contains("chat.google.com") {
            eprintln!("  Navigating to chat.google.com (SSO redirect in progress)...");
            let _ = tab.navigate_to("https://chat.google.com/u/0/");
        }

        // Poll until the tab URL reaches chat.google.com
        // (the SSO redirect chain: chat.google.com → IdP → chat.google.com)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let current = tab.get_url();
            if current.contains("chat.google.com") {
                eprintln!("  Tab on chat.google.com");
                break;
            }
            if std::time::Instant::now() > deadline {
                eprintln!(
                    "  \x1b[33m!\x1b[0m Timed out waiting for chat.google.com (on: {})",
                    &current[..current.len().min(80)]
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        // Wait for SPA to fully initialize and XSRF token to be available
        std::thread::sleep(std::time::Duration::from_secs(3));

        // Extract XSRF token if we don't have one
        if session.xsrf_token.is_none() {
            let xsrf_result = tab.evaluate(
                "(() => { try { return (window.WIZ_global_data && window.WIZ_global_data.SMqcke) || ''; } catch(e) { return ''; } })()",
                false,
            ).ok().and_then(|v| {
                let s = v.value?.as_str()?.to_owned();
                if s.is_empty() { None } else { Some(s) }
            });
            if let Some(xsrf) = xsrf_result {
                eprintln!("  XSRF token extracted ({} chars)", xsrf.len());
                session.xsrf_token = Some(xsrf.clone());
                session.tokens.xsrf_token = Some(xsrf);
            } else {
                eprintln!("  \x1b[33m!\x1b[0m XSRF token not found on page");
            }
        }
    }
    let (spaces, self_user) = fetch_spaces_via_chrome_session(&mut session);
    let _ = inbound_tx.send(InboundEvent::WorldSync {
        platform: PlatformId::GoogleChat,
        spaces,
        self_user,
    });

    let _ = inbound_tx.send(InboundEvent::Connected {
        platform: PlatformId::GoogleChat,
    });

    tracing::warn!("Google Chat connected");

    // Open clean API tab for write commands (XHR, no SPA interference).
    let _ = session.ensure_clean_api_tab();

    // Start BrowserChannel for real-time events on a dedicated tab.
    // Uses a supervisor thread that restarts the long-poll when it dies.
    {
        let bc_session_tokens_browser = session.tokens.browser.as_ref().map(|_| ());
        if bc_session_tokens_browser.is_some() {
            let bc_tx = inbound_tx.clone();
            // The supervisor needs access to session.tokens to create new contexts.
            // We pass the browser via the session reference pattern used by setup_browserchannel.
            // For the supervisor, we just try setup once then spawn the poll loop.
            match setup_browserchannel(&session) {
                Ok(ctx) => {
                    std::thread::spawn(move || {
                        channel::long_poll_loop_threaded(ctx, bc_tx);
                        // If we get here, BC died. Log it.
                        tracing::warn!("BrowserChannel thread exited");
                    });
                    eprintln!("  \x1b[32m✓\x1b[0m BrowserChannel streaming started");
                }
                Err(e) => {
                    eprintln!("  \x1b[33m!\x1b[0m BrowserChannel setup failed: {e}");
                }
            }
        }
    }

    // Command loop
    for cmd in outbound_rx {
        match cmd {
            OutboundCommand::Disconnect => break,
            other => {
                // Capture the space_id before moving `other` into command_to_api_request
                let cmd_space_id = match &other {
                    OutboundCommand::FetchHistory { space_id, .. } => Some(*space_id),
                    _ => None,
                };

                let interner = &session.interner;
                if let Some(req) = convert::command_to_api_request(other, interner) {
                    match req {
                        api::ApiRequest::SendMessage(r) => {
                            // Debug: show what we're sending
                            let space_str = r
                                .parent_id
                                .as_ref()
                                .and_then(|p| p.topic_id.as_ref())
                                .and_then(|t| t.group_id.as_ref())
                                .and_then(|g| g.space_id.as_ref())
                                .and_then(|s| s.space_id.as_deref())
                                .unwrap_or("?");
                            let text_preview = r.text_body.as_deref().unwrap_or("?");
                            eprintln!(
                                "SEND to={space_str} text=\"{}\"",
                                &text_preview[..text_preview.len().min(40)]
                            );

                            match api::call_proto::<_, proto::CreateMessageResponse>(
                                &mut session,
                                "create_message",
                                &r,
                            ) {
                                Ok(_) => eprintln!("  ✓ sent"),
                                Err(e) => eprintln!("  ✗ {e}"),
                            }
                        }
                        api::ApiRequest::CatchUpGroup(r) => {
                            let space_id = cmd_space_id.unwrap_or(crate::types::SpaceId {
                                platform: PlatformId::GoogleChat,
                                id: crate::types::InternedId::MIN,
                            });
                            match api::call_proto::<_, proto::CatchUpResponse>(
                                &mut session,
                                "catch_up_group",
                                &r,
                            ) {
                                Ok(resp) => {
                                    let n = resp.events.len();
                                    let event = convert::history_response_to_event(
                                        resp,
                                        space_id,
                                        &mut session,
                                    );
                                    tracing::warn!("CatchUpGroup: {n} events");
                                    let _ = inbound_tx.send(event);
                                }
                                Err(e) => tracing::warn!("CatchUpGroup failed: {e}"),
                            }
                        }
                        api::ApiRequest::ListTopics(r) => {
                            let space_id = cmd_space_id.unwrap_or(crate::types::SpaceId {
                                platform: PlatformId::GoogleChat,
                                id: crate::types::InternedId::MIN,
                            });
                            match api::call_proto::<_, proto::ListTopicsResponse>(
                                &mut session,
                                "list_topics",
                                &r,
                            ) {
                                Ok(resp) => {
                                    let n = resp.topics.len();
                                    let event = convert::list_topics_response_to_event(
                                        resp,
                                        space_id,
                                        &mut session,
                                    );
                                    tracing::warn!("ListTopics: {n} topics");
                                    let _ = inbound_tx.send(event);
                                }
                                Err(e) => tracing::warn!("ListTopics failed: {e}"),
                            }
                        }
                        api::ApiRequest::EditMessage(r) => {
                            match api::call_proto::<_, proto::EditMessageResponse>(
                                &mut session,
                                "edit_message",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("Message edited"),
                                Err(e) => tracing::warn!("Edit failed: {e}"),
                            }
                        }
                        api::ApiRequest::DeleteMessage(r) => {
                            match api::call_proto::<_, proto::DeleteMessageResponse>(
                                &mut session,
                                "delete_message",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("Message deleted"),
                                Err(e) => tracing::warn!("Delete failed: {e}"),
                            }
                        }
                        api::ApiRequest::SetTyping(r) => {
                            match api::call_proto::<_, proto::SetTypingStateResponse>(
                                &mut session,
                                "set_typing_state",
                                &r,
                            ) {
                                Ok(_) => {}
                                Err(e) => tracing::debug!("SetTyping failed: {e}"),
                            }
                        }
                        api::ApiRequest::MarkRead(r) => {
                            match api::call_proto::<_, proto::MarkGroupReadstateResponse>(
                                &mut session,
                                "mark_group_readstate",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("Marked read"),
                                Err(e) => tracing::debug!("MarkRead failed: {e}"),
                            }
                        }
                        api::ApiRequest::UpdateReaction(r) => {
                            match api::call_proto::<_, proto::UpdateReactionResponse>(
                                &mut session,
                                "update_reaction",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("Reaction updated"),
                                Err(e) => tracing::warn!("Reaction failed: {e}"),
                            }
                        }
                        api::ApiRequest::GetGroup(r) => {
                            match api::call_proto::<_, proto::GetGroupResponse>(
                                &mut session,
                                "get_group",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("GetGroup ok"),
                                Err(e) => tracing::debug!("GetGroup failed: {e}"),
                            }
                        }
                        api::ApiRequest::GetMembers(r) => {
                            match api::call_proto::<_, proto::GetMembersResponse>(
                                &mut session,
                                "get_members",
                                &r,
                            ) {
                                Ok(resp) => {
                                    let event =
                                        convert::members_response_to_event(resp, &mut session);
                                    let _ = inbound_tx.send(event);
                                }
                                Err(e) => tracing::warn!("GetMembers failed: {e}"),
                            }
                        }
                        api::ApiRequest::GetUserPresence(r) => {
                            match api::call_proto::<_, proto::GetUserPresenceResponse>(
                                &mut session,
                                "get_user_presence",
                                &r,
                            ) {
                                Ok(resp) => {
                                    let event =
                                        convert::presence_response_to_event(resp, &mut session);
                                    let _ = inbound_tx.send(event);
                                }
                                Err(e) => tracing::warn!("GetUserPresence failed: {e}"),
                            }
                        }
                        api::ApiRequest::GetSelfUserStatus(r) => {
                            match api::call_proto::<_, proto::GetSelfUserStatusResponse>(
                                &mut session,
                                "get_self_user_status",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("RefreshSelf ok"),
                                Err(e) => tracing::warn!("RefreshSelf failed: {e}"),
                            }
                        }
                        api::ApiRequest::CreateGroup(r) => {
                            match api::call_proto::<_, proto::CreateGroupResponse>(
                                &mut session,
                                "create_group",
                                &r,
                            ) {
                                Ok(resp) => tracing::warn!(
                                    "CreateGroup OK: {:?}",
                                    resp.group.and_then(|g| g.name)
                                ),
                                Err(e) => tracing::warn!("CreateGroup failed: {e}"),
                            }
                        }
                        api::ApiRequest::CreateDm(r) => {
                            match api::call_proto::<_, proto::CreateDmResponse>(
                                &mut session,
                                "create_dm",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("CreateDm OK"),
                                Err(e) => tracing::warn!("CreateDm failed: {e}"),
                            }
                        }
                        api::ApiRequest::CreateMembership(r) => {
                            match api::call_proto::<_, proto::CreateMembershipResponse>(
                                &mut session,
                                "create_membership",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("CreateMembership OK"),
                                Err(e) => tracing::warn!("CreateMembership failed: {e}"),
                            }
                        }
                        api::ApiRequest::RemoveMemberships(r) => {
                            match api::call_proto::<_, proto::RemoveMembershipsResponse>(
                                &mut session,
                                "remove_memberships",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("RemoveMemberships OK"),
                                Err(e) => tracing::warn!("RemoveMemberships failed: {e}"),
                            }
                        }
                        api::ApiRequest::UpdateGroup(r) => {
                            match api::call_proto::<_, proto::UpdateGroupResponse>(
                                &mut session,
                                "update_group",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("UpdateGroup OK"),
                                Err(e) => tracing::warn!("UpdateGroup failed: {e}"),
                            }
                        }
                        api::ApiRequest::HideGroup(r) => {
                            match api::call_proto::<_, proto::HideGroupResponse>(
                                &mut session,
                                "hide_group",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("HideGroup OK"),
                                Err(e) => tracing::warn!("HideGroup failed: {e}"),
                            }
                        }
                        api::ApiRequest::ListMembers(r) => {
                            match api::call_proto::<_, proto::ListMembersResponse>(
                                &mut session,
                                "list_members",
                                &r,
                            ) {
                                Ok(resp) => {
                                    let n = resp.members.len();
                                    tracing::debug!("ListMembers: {n} members");
                                }
                                Err(e) => tracing::warn!("ListMembers failed: {e}"),
                            }
                        }
                        api::ApiRequest::CreateTopic(r) => {
                            match api::call_proto::<_, proto::CreateTopicResponse>(
                                &mut session,
                                "create_topic",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("CreateTopic OK"),
                                Err(e) => tracing::warn!("CreateTopic failed: {e}"),
                            }
                        }
                        api::ApiRequest::SetDndDuration(r) => {
                            match api::call_proto::<_, proto::SetDndDurationResponse>(
                                &mut session,
                                "set_dnd_duration",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("SetDnd OK"),
                                Err(e) => tracing::warn!("SetDnd failed: {e}"),
                            }
                        }
                        api::ApiRequest::SetCustomStatus(r) => {
                            match api::call_proto::<_, proto::SetCustomStatusResponse>(
                                &mut session,
                                "set_custom_status",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("SetCustomStatus OK"),
                                Err(e) => tracing::warn!("SetCustomStatus failed: {e}"),
                            }
                        }
                        api::ApiRequest::BlockEntity(r) => {
                            match api::call_proto::<_, proto::BlockEntityResponse>(
                                &mut session,
                                "block_entity",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("BlockEntity OK"),
                                Err(e) => tracing::warn!("BlockEntity failed: {e}"),
                            }
                        }
                        api::ApiRequest::SetPresenceShared(r) => {
                            match api::call_proto::<_, proto::SetPresenceSharedResponse>(
                                &mut session,
                                "set_presence_shared",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("SetPresenceShared OK"),
                                Err(e) => tracing::warn!("SetPresenceShared failed: {e}"),
                            }
                        }
                        api::ApiRequest::CreateCustomEmoji(r) => {
                            match api::call_proto::<_, proto::CreateCustomEmojiResponse>(
                                &mut session,
                                "create_custom_emoji",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("CreateCustomEmoji OK"),
                                Err(e) => tracing::warn!("CreateCustomEmoji failed: {e}"),
                            }
                        }
                        api::ApiRequest::AutocompleteSlashCommands(r) => {
                            match api::call_proto::<_, proto::AutocompleteSlashCommandsResponse>(
                                &mut session,
                                "autocomplete_slash_commands",
                                &r,
                            ) {
                                Ok(resp) => {
                                    let n = resp.bots_in_group.len() + resp.bots_not_in_group.len();
                                    tracing::debug!("AutocompleteSlashCommands: {n} bots");
                                }
                                Err(e) => tracing::warn!("AutocompleteSlashCommands failed: {e}"),
                            }
                        }
                        api::ApiRequest::CreateVideoCall(r) => {
                            match api::call_proto::<_, proto::CreateVideoCallResponse>(
                                &mut session,
                                "create_video_call",
                                &r,
                            ) {
                                Ok(_) => tracing::debug!("CreateVideoCall OK"),
                                Err(e) => tracing::warn!("CreateVideoCall failed: {e}"),
                            }
                        }
                        api::ApiRequest::PaginatedWorld(_) | api::ApiRequest::ListMessages(_) => {
                            tracing::debug!("Command handled elsewhere");
                        }
                    }
                }
            }
        }
    }

    let _ = inbound_tx.send(InboundEvent::Disconnected {
        platform: PlatformId::GoogleChat,
        reason: crate::event::DisconnectReason::Shutdown,
    });
}

/// The Google Chat SPA renders spaces in the sidebar. We use JavaScript
/// to read the DOM elements and extract space names and IDs.
fn extract_spaces_from_page(
    session: &mut session::Session,
) -> Result<InboundEvent, crate::error::AuthError> {
    let tab = session.tokens.get_tab()?;

    // Wait for the chat SPA to fully render
    std::thread::sleep(std::time::Duration::from_secs(5));

    // Extract space data from the sidebar DOM
    // The Chat web app renders spaces as list items with data attributes
    let js = r#"
    (() => {
        try {
            const spaces = [];

            // Method 1: Look for space elements in the sidebar
            // Google Chat uses aria-label and data-group-id attributes
            const items = document.querySelectorAll('[data-group-id], [data-topic-id]');
            for (const item of items) {
                const id = item.getAttribute('data-group-id') || item.getAttribute('data-topic-id') || '';
                const nameEl = item.querySelector('[data-name]') || item;
                const name = nameEl.getAttribute('data-name') ||
                             nameEl.getAttribute('aria-label') ||
                             nameEl.textContent.trim().substring(0, 50);
                if (id || name) {
                    spaces.push({id: id, name: name});
                }
            }

            // Method 2: If no data attributes, try aria-labels on list items
            if (spaces.length === 0) {
                const listItems = document.querySelectorAll('[role="listitem"], [role="option"], [role="treeitem"]');
                for (const li of listItems) {
                    const label = li.getAttribute('aria-label') || '';
                    const text = li.textContent.trim().substring(0, 80);
                    if (label && label.length > 1) {
                        spaces.push({id: '', name: label});
                    } else if (text && text.length > 1 && text.length < 80) {
                        spaces.push({id: '', name: text});
                    }
                }
            }

            // Method 3: Fallback — get any visible room/DM names
            if (spaces.length === 0) {
                const allElements = document.querySelectorAll('span, div');
                const seen = new Set();
                for (const el of allElements) {
                    if (el.children.length > 0) continue;
                    const text = el.textContent.trim();
                    if (text.length >= 2 && text.length <= 50 && !seen.has(text)) {
                        const rect = el.getBoundingClientRect();
                        // Only elements visible in the sidebar area (left 300px)
                        if (rect.left < 300 && rect.top > 50 && rect.width > 0) {
                            seen.add(text);
                            spaces.push({id: '', name: text});
                        }
                    }
                }
            }

            return JSON.stringify({count: spaces.length, spaces: spaces.slice(0, 100)});
        } catch(e) {
            return JSON.stringify({error: e.message});
        }
    })()
    "#;

    let result = tab
        .evaluate(js, false)
        .map_err(|e| crate::error::AuthError::SessionFetch(e.to_string()))?;

    let text = result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_owned()))
        .unwrap_or_else(|| "{}".to_owned());

    tracing::warn!("DOM extraction result: {}", &text[..text.len().min(500)]);

    let parsed: serde_json::Value =
        serde_json::from_str(&text).unwrap_or(serde_json::json!({"error": "parse failed"}));

    if let Some(err) = parsed.get("error") {
        return Err(crate::error::AuthError::SessionFetch(format!(
            "JS error: {err}"
        )));
    }

    let mut spaces = Vec::new();
    if let Some(space_arr) = parsed.get("spaces").and_then(|v| v.as_array()) {
        for s in space_arr {
            let raw_name = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let id_str = s.get("id").and_then(|v| v.as_str()).unwrap_or("");

            if raw_name.is_empty() {
                continue;
            }

            // Clean the display name — the SPA prefixes names with "Unread "
            // and often includes suffixes like " Pinned conversations".
            let name = clean_space_name(raw_name);

            // Keep the "dm/" prefix so make_group_id can detect DMs later.
            // Strip "space/" and "spaces/" to bare IDs (API format).
            let (interned_id, is_dm) = if id_str.starts_with("dm/") {
                // Keep "dm/" prefix so make_group_id knows it's a DM
                let bare = id_str.strip_prefix("dm/").unwrap();
                (format!("dm/{bare}"), true)
            } else if let Some(rest) = id_str.strip_prefix("space/") {
                (rest.to_owned(), false)
            } else if let Some(rest) = id_str.strip_prefix("spaces/") {
                (rest.to_owned(), false)
            } else if !id_str.is_empty() {
                (id_str.to_owned(), false)
            } else {
                (name.clone(), false)
            };

            let id = session.interner.intern(&interned_id);
            spaces.push(crate::types::Space {
                id: crate::types::SpaceId {
                    platform: PlatformId::GoogleChat,
                    id,
                },
                name,
                kind: if is_dm {
                    crate::types::SpaceKind::DirectMessage
                } else {
                    crate::types::SpaceKind::Room
                },
                platform: PlatformId::GoogleChat,
                unread_count: 0,
                last_activity: crate::types::Timestamp::ZERO,
                sort_timestamp: crate::types::Timestamp::ZERO,
                typing_users: Vec::new(),
            });
        }
    }

    tracing::warn!("Extracted {} spaces from page DOM", spaces.len());

    Ok(InboundEvent::WorldSync {
        platform: PlatformId::GoogleChat,
        spaces,
        self_user: crate::types::User {
            id: crate::types::UserId {
                platform: PlatformId::GoogleChat,
                id: session.interner.intern("self"),
            },
            display_name: "Me".to_owned(),
            email: None,
            avatar_url: None,
            presence: crate::types::PresenceStatus::Active,
            is_bot: false,
        },
    })
}

/// Fetch spaces via proto API calls through Chrome session.
fn fetch_spaces_via_chrome_session(
    session: &mut session::Session,
) -> (Vec<crate::types::Space>, crate::types::User) {
    use prost::Message;

    // Get self user
    let mut self_user_id = String::new();
    let req = proto::GetSelfUserStatusRequest {
        request_header: Some(convert::tests_make_header()),
    };
    match session.call_api("get_self_user_status", &req.encode_to_vec()) {
        Ok(bytes) => {
            eprintln!("  get_self_user_status: {} bytes", bytes.len());
            if let Ok(resp) = proto::GetSelfUserStatusResponse::decode(bytes::Bytes::from(bytes)) {
                if let Some(uid) = resp
                    .user_status
                    .as_ref()
                    .and_then(|s| s.user_id.as_ref())
                    .and_then(|u| u.id.clone())
                {
                    self_user_id = uid;
                    eprintln!("  self_user: {self_user_id}");
                }
            }
        }
        Err(e) => eprintln!("  get_self_user_status FAILED: {e}"),
    }

    // Resolve our own display name and email via get_members
    let mut self_name = "Me".to_string();
    let mut self_email = None;
    if !self_user_id.is_empty() {
        let members_req = proto::GetMembersRequest {
            request_header: Some(convert::tests_make_header()),
            member_ids: vec![proto::MemberId {
                user_id: Some(proto::UserId {
                    id: Some(self_user_id.clone()),
                    ..Default::default()
                }),
                roster_id: None,
                email: None,
            }],
            membership_ids: Vec::new(),
        };
        if let Ok(bytes) = session.call_api("get_members", &members_req.encode_to_vec()) {
            if let Ok(resp) = proto::GetMembersResponse::decode(bytes::Bytes::from(bytes)) {
                if let Some(member) = resp.members.first() {
                    if let Some(u) = &member.user {
                        if let Some(name) = &u.name {
                            self_name = name.clone();
                        }
                        if let Some(email) = &u.email {
                            self_email = Some(email.clone());
                        }
                    }
                }
            }
        }
        eprintln!(
            "  self: {} <{}>",
            self_name,
            self_email.as_deref().unwrap_or("")
        );
    }

    let self_user = crate::types::User {
        id: crate::types::UserId {
            platform: PlatformId::GoogleChat,
            id: session.interner.intern(if self_user_id.is_empty() {
                "self"
            } else {
                &self_user_id
            }),
        },
        display_name: self_name,
        email: self_email,
        avatar_url: None,
        presence: crate::types::PresenceStatus::Active,
        is_bot: false,
    };

    // Use paginated_world to discover all spaces (rooms + DMs).
    // This is the proper API — catch_up_user hits ABORTED_CUTOFF_EXCEEDED
    // on busy accounts.
    let pw_req = proto::PaginatedWorldRequest {
        request_header: Some(convert::tests_make_header()),
        world_section_requests: vec![
            proto::WorldSectionRequest {
                page_size: Some(50),
                world_section: Some(proto::WorldSection {
                    world_section_type: Some(14), // ALL_DIRECT_MESSAGE_EVERYONE
                }),
                ..Default::default()
            },
            proto::WorldSectionRequest {
                page_size: Some(50),
                world_section: Some(proto::WorldSection {
                    world_section_type: Some(8), // ALL_ROOMS
                }),
                ..Default::default()
            },
        ],
        fetch_from_user_spaces: Some(true),
        fetch_snippets_for_unnamed_rooms: Some(true),
        ..Default::default()
    };
    let mut spaces = Vec::new();
    match session.call_api("paginated_world", &pw_req.encode_to_vec()) {
        Err(e) => eprintln!("  paginated_world FAILED: {e}"),
        Ok(bytes) => {
            eprintln!("  paginated_world: {} response bytes", bytes.len());
            if let Ok(resp) = proto::PaginatedWorldResponse::decode(bytes::Bytes::from(bytes)) {
                // Collect items from section responses
                let mut items: Vec<&proto::WorldItemLite> = Vec::new();
                for section in &resp.world_section_responses {
                    items.extend(section.world_items.iter());
                }
                // Also check top-level world_items
                items.extend(resp.world_items.iter());

                eprintln!("  {} world items", items.len());

                for item in &items {
                    let gid = match &item.group_id {
                        Some(g) => g,
                        None => continue,
                    };

                    let (id_str, is_dm) = if let Some(ref sid) = gid.space_id {
                        (sid.space_id.clone().unwrap_or_default(), false)
                    } else if let Some(ref did) = gid.dm_id {
                        let raw = did.dm_id.clone().unwrap_or_default();
                        (format!("dm/{raw}"), true)
                    } else {
                        continue;
                    };

                    if id_str.is_empty() {
                        continue;
                    }

                    // Use room_name from WorldItemLite if available
                    let name = item.room_name.clone().unwrap_or_else(|| id_str.clone());

                    let interned = session.interner.intern(&id_str);
                    spaces.push(crate::types::Space {
                        id: crate::types::SpaceId {
                            platform: PlatformId::GoogleChat,
                            id: interned,
                        },
                        name,
                        kind: if is_dm {
                            crate::types::SpaceKind::DirectMessage
                        } else {
                            crate::types::SpaceKind::Room
                        },
                        platform: PlatformId::GoogleChat,
                        unread_count: 0,
                        last_activity: crate::types::Timestamp::ZERO,
                        sort_timestamp: crate::types::Timestamp::ZERO,
                        typing_users: Vec::new(),
                    });
                }
            }
        }
    }
    // If paginated_world returned empty, fall back to DOM extraction.
    // The SPA tab is already loaded and the sidebar should be rendered.
    if spaces.is_empty() {
        eprintln!("  paginated_world empty, trying DOM extraction...");
        match extract_spaces_from_page(session) {
            Ok(InboundEvent::WorldSync {
                spaces: dom_spaces, ..
            }) => {
                eprintln!("  DOM: {} spaces", dom_spaces.len());
                spaces = dom_spaces;
            }
            Ok(_) => {}
            Err(e) => eprintln!("  DOM extraction failed: {e}"),
        }
    }
    eprintln!("  {} spaces loaded", spaces.len());
    (spaces, self_user)
}

/// Fetch all spaces via proto API calls (no DOM extraction needed).
///
/// 1. `get_self_user_status` → our user ID
/// 2. `catch_up_user` → recent events across all spaces → space IDs
/// 3. `get_group` (batch) → space names, types, details
fn fetch_spaces_via_proto(
    session: &mut direct::DirectSession,
) -> (Vec<crate::types::Space>, crate::types::User) {
    use prost::Message;

    // Step 1: Get self user
    let mut self_user_id = String::new();
    let req = proto::GetSelfUserStatusRequest {
        request_header: Some(convert::tests_make_header()),
    };
    if let Ok(bytes) = session.call_api("get_self_user_status", &req.encode_to_vec()) {
        if let Ok(resp) = proto::GetSelfUserStatusResponse::decode(bytes::Bytes::from(bytes)) {
            if let Some(uid) = resp
                .user_status
                .as_ref()
                .and_then(|s| s.user_id.as_ref())
                .and_then(|u| u.id.clone())
            {
                self_user_id = uid;
                eprintln!("  self_user: {self_user_id}");
            }
        }
    }

    let self_user = crate::types::User {
        id: crate::types::UserId {
            platform: PlatformId::GoogleChat,
            id: session.interner.intern(if self_user_id.is_empty() {
                "self"
            } else {
                &self_user_id
            }),
        },
        display_name: "Me".to_owned(),
        email: None,
        avatar_url: None,
        presence: crate::types::PresenceStatus::Active,
        is_bot: false,
    };

    // Step 2: catch_up_user → extract unique space IDs from recent events
    let now_usec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0);
    let thirty_days_usec: i64 = 30 * 24 * 3600 * 1_000_000;
    let from_usec = now_usec.saturating_sub(thirty_days_usec);
    let catchup_req = proto::CatchUpUserRequest {
        request_header: Some(convert::tests_make_header()),
        range: Some(proto::CatchUpRange {
            from_revision_timestamp: Some(from_usec),
            to_revision_timestamp: Some(now_usec),
        }),
        page_size: Some(200),
        cutoff_size: Some(1000),
    };

    let mut group_ids: Vec<(proto::GroupId, String, bool)> = Vec::new(); // (gid, id_str, is_dm)
    let mut seen = std::collections::HashSet::new();

    if let Ok(bytes) = session.call_api("catch_up_user", &catchup_req.encode_to_vec()) {
        if let Ok(resp) = proto::CatchUpResponse::decode(bytes::Bytes::from(bytes)) {
            eprintln!("  catch_up_user: {} events", resp.events.len());
            for event in &resp.events {
                if let Some(gid) = &event.group_id {
                    let (id_str, is_dm) = if let Some(ref sid) = gid.space_id {
                        (sid.space_id.clone().unwrap_or_default(), false)
                    } else if let Some(ref did) = gid.dm_id {
                        let raw = did.dm_id.clone().unwrap_or_default();
                        (format!("dm/{raw}"), true)
                    } else {
                        continue;
                    };
                    if id_str.is_empty() || seen.contains(&id_str) {
                        continue;
                    }
                    seen.insert(id_str.clone());
                    group_ids.push((gid.clone(), id_str, is_dm));
                }
            }
        }
    }
    eprintln!("  {} unique spaces from events", group_ids.len());

    // Step 3: get_group for each space → resolve names
    let mut spaces = Vec::new();
    for (gid, id_str, is_dm) in &group_ids {
        let get_req = proto::GetGroupRequest {
            request_header: Some(convert::tests_make_header()),
            group_id: Some(gid.clone()),
            fetch_options: vec![1, 4], // minimal: MEMBERS, INCLUDE_SNIPPET
            user_not_older_than: None,
            group_not_older_than: None,
            include_invite_dms: None,
        };

        let name = if let Ok(bytes) = session.call_api("get_group", &get_req.encode_to_vec()) {
            if let Ok(resp) = proto::GetGroupResponse::decode(bytes::Bytes::from(bytes)) {
                resp.group
                    .as_ref()
                    .and_then(|g| g.name.clone())
                    .unwrap_or_else(|| id_str.clone())
            } else {
                id_str.clone()
            }
        } else {
            id_str.clone()
        };

        let interned = session.interner.intern(id_str);
        spaces.push(crate::types::Space {
            id: crate::types::SpaceId {
                platform: PlatformId::GoogleChat,
                id: interned,
            },
            name,
            kind: if *is_dm {
                crate::types::SpaceKind::DirectMessage
            } else {
                crate::types::SpaceKind::Room
            },
            platform: PlatformId::GoogleChat,
            unread_count: 0,
            last_activity: crate::types::Timestamp::ZERO,
            sort_timestamp: crate::types::Timestamp::ZERO,
            typing_users: Vec::new(),
        });
    }

    eprintln!("  {} spaces with names resolved", spaces.len());
    (spaces, self_user)
}

/// Set up BrowserChannel on a fully independent Chrome tab.
///
/// Everything — register, SID acquisition, and subsequent long-polling —
/// happens on a dedicated tab that never touches the SPA or API tabs.
/// This eliminates CDP contention that previously caused timeouts.
pub fn setup_browserchannel(
    session: &session::Session,
) -> Result<channel::StreamingContext, crate::error::AuthError> {
    let browser = session
        .tokens
        .browser
        .as_ref()
        .ok_or(crate::error::AuthError::SessionFetch("no browser".into()))?;

    // Create a dedicated tab for BrowserChannel.
    let bc_tab = browser.new_tab().map_err(|e| {
        crate::error::AuthError::SessionFetch(format!("new_tab for BrowserChannel: {e}"))
    })?;

    // Navigate to chat.google.com/u/0/ so cookies (including auth and
    // path-scoped cookies) are available for fetch() on this tab.
    bc_tab
        .navigate_to("https://chat.google.com/u/0/")
        .map_err(|e| crate::error::AuthError::SessionFetch(format!("navigate BC tab: {e}")))?;

    // Wait for navigation + SSO redirect to complete.
    let _ = bc_tab.wait_until_navigated();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let url = bc_tab.get_url();
        if url.contains("chat.google.com") {
            break;
        }
        if std::time::Instant::now() > deadline {
            return Err(crate::error::AuthError::SessionFetch(format!(
                "BC tab stuck on SSO: {}",
                &url[..url.len().min(60)]
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    // Strip the SPA to reduce memory, but keep the page on chat.google.com
    // (same-origin required for cookies in fetch).
    let _ = bc_tab.evaluate(
        "document.documentElement.innerHTML = '<body>tchat-bc</body>'; \
         navigator.serviceWorker.getRegistrations().then(r => r.forEach(x => x.unregister()));",
        false,
    );
    std::thread::sleep(std::time::Duration::from_millis(300));

    // CDP keepalive: the headless_chrome crate's WebSocket connection dies
    // after ~5 minutes without CDP traffic. During each 3s fetch() cycle,
    // no CDP messages flow. This interval triggers a title change every 15s
    // which generates a CDP Page.lifecycleEvent, keeping the connection alive.
    let _ = bc_tab.evaluate(
        "let _k=0; setInterval(() => { document.title = 'bc-' + (++_k); }, 15000);",
        false,
    );
    eprintln!("  BrowserChannel tab ready");

    // Register: GET /webchannel/register — sets session cookies.
    // Done on the BC tab itself so we don't touch the SPA tab.
    let register_js = r#"(async () => {
        try {
            const resp = await fetch("https://chat.google.com/u/0/webchannel/register?ignore_compass_cookie=1", {
                credentials: 'include',
                headers: { 'X-Goog-AuthUser': '0' }
            });
            const text = await resp.text();
            return JSON.stringify({status: resp.status, size: text.length});
        } catch(e) { return JSON.stringify({error: e.message}); }
    })()"#;
    let reg_result = bc_tab
        .evaluate(register_js, true)
        .map_err(|e| crate::error::AuthError::SessionFetch(format!("register: {e}")))?;
    let reg_text = reg_result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_owned()))
        .unwrap_or_default();
    eprintln!("  BC register: {reg_text}");

    // Acquire SID: first long-poll with SID=null.
    // The SID comes from the response body in a ["c","SID_VALUE",...] pattern.
    let zx = session::Session::random_zx();
    let sid_url = format!(
        "https://chat.google.com/u/0/webchannel/events?\
         VER=8&RID=1&CVER=22&zx={zx}&t=1&SID=null&\
         %24req=count%3D1%26ofs%3D0%26req0_data%3D%255B%255D"
    );
    let sid_js = format!(
        r#"(async () => {{
            try {{
                const ctrl = new AbortController();
                setTimeout(() => ctrl.abort(), 10000);
                const resp = await fetch("{sid_url}", {{
                    credentials: 'include',
                    headers: {{ 'X-Goog-AuthUser': '0' }},
                    signal: ctrl.signal
                }});
                const hdr = resp.headers.get('X-HTTP-Initial-Response');
                if (hdr) return JSON.stringify({{sid_header: hdr}});
                const text = await resp.text();
                return JSON.stringify({{body: text.substring(0, 2000)}});
            }} catch(e) {{ return JSON.stringify({{error: e.message}}); }}
        }})()"#
    );
    let sid_result = bc_tab
        .evaluate(&sid_js, true)
        .map_err(|e| crate::error::AuthError::SessionFetch(format!("acquire SID: {e}")))?;
    let sid_text = sid_result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_owned()))
        .ok_or(crate::error::AuthError::SessionFetch(
            "SID: empty response".into(),
        ))?;
    let sid_resp: serde_json::Value = serde_json::from_str(&sid_text)
        .map_err(|e| crate::error::AuthError::SessionFetch(format!("SID parse: {e}")))?;

    // Extract SID from header or body
    let sid = if let Some(hdr) = sid_resp.get("sid_header").and_then(|v| v.as_str()) {
        session::parse_sid_from_register(hdr)?
    } else if let Some(body) = sid_resp.get("body").and_then(|v| v.as_str()) {
        session::extract_sid_from_stream(body)
            .ok_or(crate::error::AuthError::MissingField("SID in body"))?
    } else {
        let err = sid_resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(crate::error::AuthError::SessionFetch(format!(
            "SID fetch failed: {err}"
        )));
    };

    eprintln!("  BrowserChannel SID: {}...", &sid[..sid.len().min(12)]);
    Ok(channel::StreamingContext { tab: bc_tab, sid })
}

/// Clean up a space name extracted from the SPA sidebar.
///
/// The SPA wraps names with state prefixes like "Unread " and appends
/// extras like " Pinned conversations". Strip these to get the actual
/// space/room name for display.
fn clean_space_name(raw: &str) -> String {
    let mut s = raw.trim();
    // Strip known prefix patterns
    for prefix in &["Unread ", "Muted ", "Active "] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest;
            break;
        }
    }
    // Strip known suffix patterns
    for suffix in &[
        " Pinned conversations",
        " Conversations",
        " conversations",
        " Pinned",
    ] {
        if let Some(rest) = s.strip_suffix(suffix) {
            s = rest;
            break;
        }
    }
    s.trim().to_owned()
}

#[cfg(test)]
mod mod_tests {
    use super::clean_space_name;

    #[test]
    fn clean_strips_unread_prefix() {
        assert_eq!(
            clean_space_name("Unread BI Layer Internal Team"),
            "BI Layer Internal Team"
        );
    }

    #[test]
    fn clean_strips_pinned_suffix() {
        assert_eq!(
            clean_space_name("Unread Datahub Infrastructure Space Pinned conversations"),
            "Datahub Infrastructure Space"
        );
    }

    #[test]
    fn clean_preserves_plain_name() {
        assert_eq!(clean_space_name("general"), "general");
    }

    #[test]
    fn clean_handles_empty() {
        assert_eq!(clean_space_name(""), "");
    }
}
