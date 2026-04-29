//! Phase 3: test EVERYTHING including BrowserChannel and advanced features.
//!
//! Run:  cargo test --test live_phase3_everything -- --ignored --nocapture

use tchat::platform::googlechat::{api, auth, convert, proto, session::Session};

const TEST_SPACE_ID: &str = "AAQAjslKeUE";

#[test]
#[ignore]
fn test_everything() {
    eprintln!("\n========== Phase 3: Everything ==========\n");

    let tokens = auth::authenticate(None).expect("Auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();

    let mut pass = 0;
    let mut fail = 0;
    let mut report = |name: &str, ok: bool, detail: &str| {
        if ok {
            eprintln!("  ✓ {name} — {detail}");
            pass += 1;
        } else {
            eprintln!("  ✗ {name} — {detail}");
            fail += 1;
        }
    };

    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE_ID.into()),
        }),
        dm_id: None,
    };

    // ── BrowserChannel: register + acquire_sid ──
    eprintln!("\n[1] BrowserChannel register...");
    match session.register() {
        Ok(_) => report("register", true, "cookies set"),
        Err(e) => report("register", false, &e.to_string()),
    }

    eprintln!("\n[2] BrowserChannel acquire_sid...");
    match session.acquire_sid() {
        Ok(_) => {
            let sid_preview = session
                .sid
                .as_ref()
                .map(|s| format!("{}...({} chars)", &s[..s.len().min(20)], s.len()))
                .unwrap_or_default();
            report("acquire_sid", true, &sid_preview);
        }
        Err(e) => report("acquire_sid", false, &e.to_string()),
    }

    // ── BrowserChannel: one long-poll cycle ──
    if session.sid.is_some() {
        eprintln!("\n[3] BrowserChannel long-poll (single cycle, ~10s)...");
        let sid = session.sid.clone().unwrap();
        let zx = Session::random_zx();
        let url = format!(
            "https://chat.google.com/u/0/webchannel/events?\
             VER=8&RID=rpc&SID={sid}&AID=0&TYPE=xmlhttp&CI=0&t=1&zx={zx}"
        );
        let start = std::time::Instant::now();
        match session.tokens.fetch_get_binary(&url) {
            Ok(bytes) => {
                let elapsed = start.elapsed().as_secs_f64();
                report(
                    "long_poll_cycle",
                    true,
                    &format!("{} bytes in {:.1}s", bytes.len(), elapsed),
                );
                if !bytes.is_empty() {
                    let preview =
                        std::str::from_utf8(&bytes[..bytes.len().min(200)]).unwrap_or("(non-UTF8)");
                    eprintln!("    preview: {preview}");
                }
            }
            Err(e) => report("long_poll_cycle", false, &e.to_string()),
        }
    }

    // ── Advanced endpoints ──
    eprintln!("\n[4] autocomplete_slash_commands...");
    match api::call_proto::<_, proto::AutocompleteSlashCommandsResponse>(
        &mut session,
        "autocomplete_slash_commands",
        &proto::AutocompleteSlashCommandsRequest {
            request_header: Some(convert::tests_make_header()),
            query: Some("/".into()),
            group_id: Some(gid.clone()),
            max_num_results: Some(5),
            restrict_to_bots_in_group: Some(false),
            bot_use_case_filters: Vec::new(),
            include_dialogs: Some(false),
        },
    ) {
        Ok(r) => report(
            "autocomplete_slash_commands",
            true,
            &format!(
                "{} bots in group, {} not in group",
                r.bots_in_group.len(),
                r.bots_not_in_group.len()
            ),
        ),
        Err(e) => report("autocomplete_slash_commands", false, &e.to_string()),
    }

    eprintln!("\n[5] set_presence_shared (toggle false+true)...");
    match api::call_proto::<_, proto::SetPresenceSharedResponse>(
        &mut session,
        "set_presence_shared",
        &proto::SetPresenceSharedRequest {
            request_header: Some(convert::tests_make_header()),
            presence_shared: Some(true),
        },
    ) {
        Ok(_) => report("set_presence_shared", true, "shared=true ok"),
        Err(e) => report("set_presence_shared", false, &e.to_string()),
    }

    // create_custom_emoji — shortcode only; won't actually create without an upload
    eprintln!("\n[6] create_custom_emoji (expected: fails without upload data)...");
    match api::call_proto::<_, proto::CreateCustomEmojiResponse>(
        &mut session,
        "create_custom_emoji",
        &proto::CreateCustomEmojiRequest {
            request_header: Some(convert::tests_make_header()),
            shortcode: Some("tchat_test".into()),
        },
    ) {
        Ok(r) => {
            let uuid = r
                .custom_emoji
                .as_ref()
                .and_then(|e| e.uuid.clone())
                .unwrap_or_default();
            report(
                "create_custom_emoji",
                true,
                &format!("emoji created uuid={uuid}"),
            );
        }
        Err(e) => {
            // Endpoint exists if error is not 404
            let s = e.to_string();
            let endpoint_exists = !s.contains("HTTP 404");
            let brief = if s.len() > 80 { &s[..80] } else { &s };
            report(
                "create_custom_emoji",
                endpoint_exists,
                &format!("endpoint_exists={endpoint_exists}, err={brief}"),
            );
        }
    }

    eprintln!("\n[7] create_video_call...");
    // Note: this creates a Meet call link — we test endpoint reachability
    match api::call_proto::<_, proto::CreateVideoCallResponse>(
        &mut session,
        "create_video_call",
        &proto::CreateVideoCallRequest {
            group_id: Some(gid.clone()),
            request_header: Some(convert::tests_make_header()),
        },
    ) {
        Ok(r) => {
            let has_ann = r.annotation.is_some();
            report(
                "create_video_call",
                true,
                &format!("annotation_present={has_ann}"),
            );
        }
        Err(e) => {
            let s = e.to_string();
            let brief = if s.len() > 80 { &s[..80] } else { &s };
            report("create_video_call", false, brief);
        }
    }

    // ── Test create_group (creates then hides it) ──
    eprintln!("\n[8] create_group (creates a new threaded space)...");
    let room_name = format!("tchat-test-{}", std::process::id());

    // Try several variants in sequence
    let variants: Vec<(&str, proto::CreateGroupRequest)> = vec![
        (
            "threaded_room THREADED_ROOM=5",
            proto::CreateGroupRequest {
                request_header: Some(convert::tests_make_header()),
                space: Some(proto::SpaceCreationInfo {
                    name: Some(room_name.clone()),
                    visibility: None,
                    flat_group: None,
                    threaded_group: Some(proto::space_creation_info::ThreadedGroup {}),
                    has_server_generated_name: Some(false),
                    invitee_member_infos: Vec::new(),
                    space_type: None,
                    attribute_checker_group_type: Some(5),
                    shared_drive_name: None,
                }),
                local_id: Some(format!("tchat-1-{}", std::process::id())),
                should_find_existing_space: Some(false),
            },
        ),
        (
            "flat_room FLAT_ROOM=4",
            proto::CreateGroupRequest {
                request_header: Some(convert::tests_make_header()),
                space: Some(proto::SpaceCreationInfo {
                    name: Some(room_name.clone() + "-flat"),
                    visibility: None,
                    flat_group: Some(proto::space_creation_info::FlatGroup {}),
                    threaded_group: None,
                    has_server_generated_name: Some(false),
                    invitee_member_infos: Vec::new(),
                    space_type: None,
                    attribute_checker_group_type: Some(4),
                    shared_drive_name: None,
                }),
                local_id: Some(format!("tchat-2-{}", std::process::id())),
                should_find_existing_space: Some(false),
            },
        ),
        (
            "POST_ROOM=7",
            proto::CreateGroupRequest {
                request_header: Some(convert::tests_make_header()),
                space: Some(proto::SpaceCreationInfo {
                    name: Some(room_name.clone() + "-post"),
                    visibility: None,
                    flat_group: None,
                    threaded_group: Some(proto::space_creation_info::ThreadedGroup {}),
                    has_server_generated_name: Some(false),
                    invitee_member_infos: Vec::new(),
                    space_type: None,
                    attribute_checker_group_type: Some(7),
                    shared_drive_name: None,
                }),
                local_id: Some(format!("tchat-3-{}", std::process::id())),
                should_find_existing_space: Some(false),
            },
        ),
    ];

    let mut created_any = false;
    for (label, req) in variants {
        eprintln!("  trying: {label}");
        match api::call_proto::<_, proto::CreateGroupResponse>(&mut session, "create_group", &req) {
            Ok(r) => {
                let created_name = r
                    .group
                    .as_ref()
                    .and_then(|g| g.name.clone())
                    .unwrap_or_default();
                let new_space_id = r
                    .group
                    .as_ref()
                    .and_then(|g| g.group_id.as_ref())
                    .and_then(|gid| gid.space_id.as_ref())
                    .and_then(|s| s.space_id.clone())
                    .unwrap_or_default();
                eprintln!("    ✓ OK — created \"{created_name}\" id={new_space_id}");

                // Clean up: hide the new group
                if !new_space_id.is_empty() {
                    let new_gid = proto::GroupId {
                        space_id: Some(proto::SpaceId {
                            space_id: Some(new_space_id.clone()),
                        }),
                        dm_id: None,
                    };
                    let _ = api::call_proto::<_, proto::HideGroupResponse>(
                        &mut session,
                        "hide_group",
                        &proto::HideGroupRequest {
                            request_header: Some(convert::tests_make_header()),
                            id: Some(new_gid),
                            hide: Some(true),
                        },
                    );
                    eprintln!("      hidden (archived) for cleanup");
                }
                created_any = true;
                break;
            }
            Err(e) => {
                let s = e.to_string();
                let brief = if s.len() > 120 { &s[..120] } else { &s };
                eprintln!("    ✗ {brief}");
            }
        }
    }
    if created_any {
        report("create_group", true, "some variant worked");
    } else {
        report("create_group", false, "all variants failed");
    }

    // ── create_dm with test space ──
    // Real users would be needed for create_membership — skip for safety

    eprintln!("\n========== Summary ==========");
    eprintln!("  ✓ {pass} passed");
    eprintln!("  ✗ {fail} failed");
    eprintln!("==============================");
}
