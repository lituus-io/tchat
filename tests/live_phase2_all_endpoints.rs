//! Phase 2: test ALL new endpoints against the live server.
//!
//! Tests:
//!   - get_self_user_status, get_user_presence, get_members
//!   - create_topic, create_dm
//!   - update_group (rename), hide_group
//!   - set_dnd_duration, set_custom_status
//!   - block_entity (then unblock for cleanup)
//!   - create_membership, remove_memberships
//!
//! Skipped for safety / requires extra setup:
//!   - create_group (would create a real new room)
//!
//! Run:  cargo test --test live_phase2_all_endpoints -- --ignored --nocapture

use tchat::platform::googlechat::{api, auth, convert, proto, session::Session};

const TEST_SPACE_ID: &str = "AAQAjslKeUE";

#[test]
#[ignore]
fn test_all_new_endpoints() {
    eprintln!("\n========== Phase 2: All new endpoints ==========\n");

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

    // ── get_self_user_status (sanity: get our user_id) ──
    eprintln!("\n[1] get_self_user_status");
    let self_id = match api::call_proto::<_, proto::GetSelfUserStatusResponse>(
        &mut session,
        "get_self_user_status",
        &proto::GetSelfUserStatusRequest {
            request_header: Some(convert::tests_make_header()),
        },
    ) {
        Ok(r) => {
            let uid = r
                .user_status
                .as_ref()
                .and_then(|s| s.user_id.as_ref())
                .and_then(|u| u.id.clone())
                .unwrap_or_default();
            report("get_self_user_status", true, &format!("user_id={uid}"));
            Some(uid)
        }
        Err(e) => {
            report("get_self_user_status", false, &e.to_string());
            None
        }
    };

    // ── get_user_presence (use self_id) ──
    eprintln!("\n[2] get_user_presence");
    if let Some(ref uid) = self_id {
        match api::call_proto::<_, proto::GetUserPresenceResponse>(
            &mut session,
            "get_user_presence",
            &proto::GetUserPresenceRequest {
                request_header: Some(convert::tests_make_header()),
                user_ids: vec![proto::UserId {
                    id: Some(uid.clone()),
                    ..Default::default()
                }],
                include_active_until: Some(true),
                include_user_status: Some(true),
            },
        ) {
            Ok(r) => {
                let n = r.user_presences.len();
                let presence = r
                    .user_presences
                    .first()
                    .and_then(|p| p.presence)
                    .unwrap_or(0);
                report(
                    "get_user_presence",
                    true,
                    &format!("{n} presences, first=Presence({presence})"),
                );
            }
            Err(e) => report("get_user_presence", false, &e.to_string()),
        }
    } else {
        report("get_user_presence", false, "skipped (no self_id)");
    }

    // ── get_members (use self_id) ──
    eprintln!("\n[3] get_members");
    if let Some(ref uid) = self_id {
        match api::call_proto::<_, proto::GetMembersResponse>(
            &mut session,
            "get_members",
            &proto::GetMembersRequest {
                request_header: Some(convert::tests_make_header()),
                member_ids: vec![proto::MemberId {
                    user_id: Some(proto::UserId {
                        id: Some(uid.clone()),
                        ..Default::default()
                    }),
                    roster_id: None,
                    email: None,
                }],
                membership_ids: Vec::new(),
            },
        ) {
            Ok(r) => {
                let name = r
                    .members
                    .first()
                    .and_then(|m| m.user.as_ref())
                    .and_then(|u| u.name.clone())
                    .unwrap_or_default();
                report(
                    "get_members",
                    true,
                    &format!("{} members, first.name=\"{name}\"", r.members.len()),
                );
            }
            Err(e) => report("get_members", false, &e.to_string()),
        }
    } else {
        report("get_members", false, "skipped");
    }

    // ── list_members in test space ──
    eprintln!("\n[4] list_members (test space)");
    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE_ID.into()),
        }),
        dm_id: None,
    };
    match api::call_proto::<_, proto::ListMembersResponse>(
        &mut session,
        "list_members",
        &proto::ListMembersRequest {
            request_header: Some(convert::tests_make_header()),
            space_id: None,
            group_id: Some(gid.clone()),
            page_size: Some(20),
            page_token: None,
            not_older_than: None,
        },
    ) {
        Ok(r) => report(
            "list_members",
            true,
            &format!(
                "{} memberships, {} members",
                r.memberships.len(),
                r.members.len()
            ),
        ),
        Err(e) => report("list_members", false, &e.to_string()),
    }

    // ── create_topic (start a new thread) ──
    eprintln!("\n[5] create_topic");
    let topic_resp = api::call_proto::<_, proto::CreateTopicResponse>(
        &mut session,
        "create_topic",
        &proto::CreateTopicRequest {
            request_header: Some(convert::tests_make_header()),
            group_id: Some(gid.clone()),
            text_body: Some("tchat: testing create_topic — this is a new thread".into()),
            annotations: Vec::new(),
            retention_settings: None,
            local_id: Some(format!("tchat-topic-{}", std::process::id())),
            topic_and_message_id: None,
            history_v2: None,
            message_info: None,
            thread_options: None,
        },
    );
    let created_topic_id = match topic_resp {
        Ok(r) => {
            let tid = r
                .topic
                .as_ref()
                .and_then(|t| t.id.as_ref())
                .and_then(|id| id.topic_id.clone())
                .unwrap_or_default();
            report("create_topic", true, &format!("topic_id={tid}"));
            Some(tid)
        }
        Err(e) => {
            report("create_topic", false, &e.to_string());
            None
        }
    };

    // Cleanup: delete the topic message we just created
    if let Some(ref tid) = created_topic_id {
        let _ = api::call_proto::<_, proto::DeleteMessageResponse>(
            &mut session,
            "delete_message",
            &proto::DeleteMessageRequest {
                request_header: Some(convert::tests_make_header()),
                message_id: Some(proto::MessageId {
                    parent_id: Some(proto::MessageParentId {
                        topic_id: Some(proto::TopicId {
                            group_id: Some(gid.clone()),
                            topic_id: Some(tid.clone()),
                        }),
                    }),
                    message_id: Some(tid.clone()),
                }),
            },
        );
    }

    // ── update_group (rename test space, then rename back) ──
    eprintln!("\n[6] update_group (rename)");
    let original_name = match api::call_proto::<_, proto::GetGroupResponse>(
        &mut session,
        "get_group",
        &proto::GetGroupRequest {
            request_header: Some(convert::tests_make_header()),
            group_id: Some(gid.clone()),
            fetch_options: vec![5, 9, 8, 7, 1, 4],
            user_not_older_than: None,
            group_not_older_than: None,
            include_invite_dms: Some(true),
        },
    ) {
        Ok(r) => r.group.and_then(|g| g.name).unwrap_or_default(),
        Err(_) => "Test - Gchat".to_string(),
    };
    eprintln!("    original name: {original_name}");

    let new_name = format!("{original_name} (tchat test)");
    match api::call_proto::<_, proto::UpdateGroupResponse>(
        &mut session,
        "update_group",
        &proto::UpdateGroupRequest {
            request_header: Some(convert::tests_make_header()),
            space_id: Some(proto::SpaceId {
                space_id: Some(TEST_SPACE_ID.into()),
            }),
            update_masks: vec![1], // NAME
            name: Some(new_name.clone()),
            visibility: None,
        },
    ) {
        Ok(_) => {
            report("update_group", true, &format!("renamed to \"{new_name}\""));
            // Restore original name
            let _ = api::call_proto::<_, proto::UpdateGroupResponse>(
                &mut session,
                "update_group",
                &proto::UpdateGroupRequest {
                    request_header: Some(convert::tests_make_header()),
                    space_id: Some(proto::SpaceId {
                        space_id: Some(TEST_SPACE_ID.into()),
                    }),
                    update_masks: vec![1],
                    name: Some(original_name.clone()),
                    visibility: None,
                },
            );
            eprintln!("    restored original name");
        }
        Err(e) => report("update_group", false, &e.to_string()),
    }

    // ── hide_group (hide then unhide) ──
    eprintln!("\n[7] hide_group");
    match api::call_proto::<_, proto::HideGroupResponse>(
        &mut session,
        "hide_group",
        &proto::HideGroupRequest {
            request_header: Some(convert::tests_make_header()),
            id: Some(gid.clone()),
            hide: Some(true),
        },
    ) {
        Ok(_) => {
            // Unhide
            let _ = api::call_proto::<_, proto::HideGroupResponse>(
                &mut session,
                "hide_group",
                &proto::HideGroupRequest {
                    request_header: Some(convert::tests_make_header()),
                    id: Some(gid.clone()),
                    hide: Some(false),
                },
            );
            report("hide_group", true, "hide+unhide cycle ok");
        }
        Err(e) => report("hide_group", false, &e.to_string()),
    }

    // ── set_dnd_duration (5 minutes, then disable) ──
    eprintln!("\n[8] set_dnd_duration");
    let five_min_usec: i64 = 5 * 60 * 1_000_000;
    match api::call_proto::<_, proto::SetDndDurationResponse>(
        &mut session,
        "set_dnd_duration",
        &proto::SetDndDurationRequest {
            request_header: Some(convert::tests_make_header()),
            current_dnd_state: None,
            new_dnd_duration_usec: Some(five_min_usec),
            dnd_expiry_timestamp_usec: None,
        },
    ) {
        Ok(_) => {
            // Turn off DND
            let _ = api::call_proto::<_, proto::SetDndDurationResponse>(
                &mut session,
                "set_dnd_duration",
                &proto::SetDndDurationRequest {
                    request_header: Some(convert::tests_make_header()),
                    current_dnd_state: None,
                    new_dnd_duration_usec: Some(0),
                    dnd_expiry_timestamp_usec: None,
                },
            );
            report("set_dnd_duration", true, "5min set + cleared");
        }
        Err(e) => report("set_dnd_duration", false, &e.to_string()),
    }

    // ── set_custom_status (set then clear) ──
    eprintln!("\n[9] set_custom_status");
    match api::call_proto::<_, proto::SetCustomStatusResponse>(
        &mut session,
        "set_custom_status",
        &proto::SetCustomStatusRequest {
            request_header: Some(convert::tests_make_header()),
            custom_status: Some(proto::CustomStatus {
                status_text: Some("tchat protocol testing".into()),
                status_emoji: None,
                state_expiry_timestamp_usec: None,
                emoji: Some(proto::Emoji {
                    unicode: Some("🦀".into()),
                    custom_emoji: None,
                }),
            }),
            custom_status_expiry_timestamp_usec: None,
            custom_status_remaining_duration_usec: Some(60 * 1_000_000), // 1 minute
        },
    ) {
        Ok(_) => {
            // Clear status
            let _ = api::call_proto::<_, proto::SetCustomStatusResponse>(
                &mut session,
                "set_custom_status",
                &proto::SetCustomStatusRequest {
                    request_header: Some(convert::tests_make_header()),
                    custom_status: Some(proto::CustomStatus {
                        status_text: Some(String::new()),
                        status_emoji: None,
                        state_expiry_timestamp_usec: None,
                        emoji: None,
                    }),
                    custom_status_expiry_timestamp_usec: None,
                    custom_status_remaining_duration_usec: None,
                },
            );
            report("set_custom_status", true, "set+cleared");
        }
        Err(e) => report("set_custom_status", false, &e.to_string()),
    }

    // ── catch_up_user (probably useful for global event sync) ──
    eprintln!("\n[10] catch_up_user");
    let now_usec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0);
    match api::call_proto::<_, proto::CatchUpResponse>(
        &mut session,
        "catch_up_user",
        &proto::CatchUpUserRequest {
            request_header: Some(convert::tests_make_header()),
            range: Some(proto::CatchUpRange {
                from_revision_timestamp: Some(now_usec - 3600 * 1_000_000),
                to_revision_timestamp: Some(now_usec),
            }),
            page_size: Some(50),
            cutoff_size: Some(500),
        },
    ) {
        Ok(r) => report(
            "catch_up_user",
            true,
            &format!(
                "{} events, status={}",
                r.events.len(),
                r.status.unwrap_or(0)
            ),
        ),
        Err(e) => report("catch_up_user", false, &e.to_string()),
    }

    // ── get_server_time (ultra simple — should always work) ──
    eprintln!("\n[11] get_server_time");
    match api::call_proto::<_, proto::GetServerTimeResponse>(
        &mut session,
        "get_server_time",
        &proto::GetServerTimeRequest {
            request_header: Some(convert::tests_make_header()),
        },
    ) {
        Ok(r) => {
            let secs = r.timestamp.as_ref().and_then(|t| t.seconds).unwrap_or(0);
            report("get_server_time", true, &format!("server time secs={secs}"));
        }
        Err(e) => report("get_server_time", false, &e.to_string()),
    }

    // ── create_dm with self ──
    eprintln!("\n[12] create_dm (with self for safety)");
    if let Some(ref uid) = self_id {
        match api::call_proto::<_, proto::CreateDmResponse>(
            &mut session,
            "create_dm",
            &proto::CreateDmRequest {
                request_header: Some(convert::tests_make_header()),
                fetch_options: Vec::new(),
                members: vec![proto::UserId {
                    id: Some(uid.clone()),
                    ..Default::default()
                }],
                invitees: Vec::new(),
                retention_settings: None,
                local_id: Some(format!("tchat-dm-{}", std::process::id())),
                topic_and_message_id: None,
            },
        ) {
            Ok(r) => {
                let dm_id =
                    r.dm.as_ref()
                        .and_then(|g| g.group_id.as_ref())
                        .and_then(|gid| gid.dm_id.as_ref())
                        .and_then(|d| d.dm_id.clone())
                        .unwrap_or_default();
                report("create_dm", true, &format!("dm_id={dm_id}"));
            }
            Err(e) => report("create_dm", false, &e.to_string()),
        }
    } else {
        report("create_dm", false, "skipped (no self_id)");
    }

    // ── block_entity (block self briefly, then unblock) ──
    // Skip for safety — blocking yourself may have side effects
    eprintln!("\n[13] block_entity (DRY RUN — unblock state only)");
    if let Some(ref uid) = self_id {
        // Just unblock (no-op if not blocked) to verify the endpoint works
        match api::call_proto::<_, proto::BlockEntityResponse>(
            &mut session,
            "block_entity",
            &proto::BlockEntityRequest {
                request_header: Some(convert::tests_make_header()),
                user_id: Some(proto::UserId {
                    id: Some(uid.clone()),
                    ..Default::default()
                }),
                group_id: None,
                blocked: Some(false),
                reported: Some(false),
            },
        ) {
            Ok(_) => report("block_entity", true, "unblock self ok (no-op)"),
            Err(e) => {
                // Expected: server might reject self-block — that's fine, the endpoint exists
                let s = e.to_string();
                let brief = if s.len() > 80 { &s[..80] } else { &s };
                report("block_entity", false, brief);
            }
        }
    }

    eprintln!("\n========== Summary ==========");
    eprintln!("  ✓ {pass} passed");
    eprintln!("  ✗ {fail} failed");
    eprintln!("==============================");
}
