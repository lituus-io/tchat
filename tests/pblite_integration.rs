//! Integration tests for the pblite codec with full request/response proto structures.
//!
//! These tests verify that the pblite codec correctly handles the wire format
//! for the Google Chat API calls that tchat actually uses.

use prost::Message;

use tchat::platform::googlechat::pblite;
use tchat::platform::googlechat::proto;

// ─────────────────── Request roundtrips ───────────────────

#[test]
fn paginated_world_request_roundtrip() {
    let req = proto::PaginatedWorldRequest {
        request_header: Some(proto::RequestHeader {
            client_type: Some(3), // WEB
            client_version: None,
            trace_id: None,
            locale: Some("en".into()),
            client_feature_capabilities: None,
        }),
        world_section_requests: vec![
            proto::WorldSectionRequest {
                page_size: Some(100),
                world_section: Some(proto::WorldSection {
                    world_section_type: Some(7), // AllDirectMessagePeople
                }),
                ..Default::default()
            },
            proto::WorldSectionRequest {
                page_size: Some(100),
                world_section: Some(proto::WorldSection {
                    world_section_type: Some(8), // AllRooms
                }),
                ..Default::default()
            },
        ],
        fetch_from_user_spaces: Some(true),
        ..Default::default()
    };

    let wire = req.encode_to_vec();
    let pblite_json = pblite::wire_to_pblite(&wire).unwrap();
    let wire_back = pblite::pblite_to_wire(&pblite_json).unwrap();
    let decoded = proto::PaginatedWorldRequest::decode(wire_back).unwrap();

    assert_eq!(decoded.world_section_requests.len(), 2);
    assert_eq!(decoded.world_section_requests[0].page_size, Some(100));
    assert_eq!(decoded.fetch_from_user_spaces, Some(true));
    let header = decoded.request_header.unwrap();
    assert_eq!(header.client_type, Some(3));
    assert_eq!(header.locale, Some("en".into()));
}

#[test]
fn create_message_request_with_thread_roundtrip() {
    let req = proto::CreateMessageRequest {
        request_header: Some(proto::RequestHeader {
            client_type: Some(3),
            client_version: None,
            trace_id: None,
            locale: Some("en".into()),
            client_feature_capabilities: None,
        }),
        parent_id: Some(proto::MessageParentId {
            topic_id: Some(proto::TopicId {
                group_id: Some(proto::GroupId {
                    space_id: Some(proto::SpaceId {
                        space_id: Some("spaces/AAAA_bbbb".into()),
                    }),
                    dm_id: None,
                }),
                topic_id: Some("topic_xyz123".into()),
            }),
        }),
        text_body: Some("Hello, this is a threaded reply!".into()),
        annotations: Vec::new(),
        local_id: Some("local_abc".into()),
        message_id: None,
        message_info: Some(proto::MessageInfo {
            accept_format_annotations: Some(true),
            reply_to: None,
        }),
    };

    let wire = req.encode_to_vec();
    let pblite_json = pblite::wire_to_pblite(&wire).unwrap();
    let wire_back = pblite::pblite_to_wire(&pblite_json).unwrap();
    let decoded = proto::CreateMessageRequest::decode(wire_back).unwrap();

    assert_eq!(
        decoded.text_body,
        Some("Hello, this is a threaded reply!".into())
    );
    assert_eq!(decoded.local_id, Some("local_abc".into()));
    let parent = decoded.parent_id.unwrap();
    let topic = parent.topic_id.unwrap();
    assert_eq!(topic.topic_id, Some("topic_xyz123".into()));
    let gid = topic.group_id.unwrap();
    assert_eq!(
        gid.space_id.unwrap().space_id,
        Some("spaces/AAAA_bbbb".into())
    );
    let info = decoded.message_info.unwrap();
    assert_eq!(info.accept_format_annotations, Some(true));
}

#[test]
fn catch_up_group_request_roundtrip() {
    let req = proto::CatchUpGroupRequest {
        request_header: Some(proto::RequestHeader {
            client_type: Some(3),
            client_version: None,
            trace_id: None,
            locale: Some("en".into()),
            client_feature_capabilities: None,
        }),
        group_id: Some(proto::GroupId {
            space_id: None,
            dm_id: Some(proto::DmId {
                dm_id: Some("dm/ABCDEF123456".into()),
            }),
        }),
        range: Some(proto::CatchUpRange {
            from_revision_timestamp: Some(1700000000000000),
            to_revision_timestamp: Some(1700099999999999),
        }),
        page_size: Some(50),
        cutoff_size: Some(200),
    };

    let wire = req.encode_to_vec();
    let pblite_json = pblite::wire_to_pblite(&wire).unwrap();
    let wire_back = pblite::pblite_to_wire(&pblite_json).unwrap();
    let decoded = proto::CatchUpGroupRequest::decode(wire_back).unwrap();

    assert_eq!(decoded.page_size, Some(50));
    assert_eq!(decoded.cutoff_size, Some(200));
    let gid = decoded.group_id.unwrap();
    assert_eq!(gid.dm_id.unwrap().dm_id, Some("dm/ABCDEF123456".into()));
    assert!(gid.space_id.is_none());
    let range = decoded.range.unwrap();
    assert_eq!(range.from_revision_timestamp, Some(1700000000000000));
    assert_eq!(range.to_revision_timestamp, Some(1700099999999999));
}

#[test]
fn update_reaction_request_roundtrip() {
    let req = proto::UpdateReactionRequest {
        request_header: Some(proto::RequestHeader {
            client_type: Some(3),
            ..Default::default()
        }),
        message_id: Some(proto::MessageId {
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(proto::GroupId {
                        space_id: Some(proto::SpaceId {
                            space_id: Some("spaces/room1".into()),
                        }),
                        dm_id: None,
                    }),
                    topic_id: Some("topic_1".into()),
                }),
            }),
            message_id: Some("msg_12345".into()),
        }),
        emoji: Some(proto::Emoji {
            unicode: Some("\u{1F44D}".into()), // 👍
            custom_emoji: None,
        }),
        option: Some(1), // ADD
    };

    let wire = req.encode_to_vec();
    let pblite_json = pblite::wire_to_pblite(&wire).unwrap();
    let wire_back = pblite::pblite_to_wire(&pblite_json).unwrap();
    let decoded = proto::UpdateReactionRequest::decode(wire_back).unwrap();

    assert_eq!(decoded.option, Some(1));
    let emoji = decoded.emoji.unwrap();
    assert_eq!(emoji.unicode, Some("\u{1F44D}".into()));
    let mid = decoded.message_id.unwrap();
    assert_eq!(mid.message_id, Some("msg_12345".into()));
}

#[test]
fn set_typing_state_request_roundtrip() {
    let req = proto::SetTypingStateRequest {
        request_header: Some(proto::RequestHeader {
            client_type: Some(3),
            locale: Some("en".into()),
            ..Default::default()
        }),
        state: Some(1), // TYPING
        context: Some(proto::TypingContext {
            group_id: Some(proto::GroupId {
                space_id: Some(proto::SpaceId {
                    space_id: Some("spaces/myroom".into()),
                }),
                dm_id: None,
            }),
            topic_id: None,
        }),
    };

    let wire = req.encode_to_vec();
    let pblite_json = pblite::wire_to_pblite(&wire).unwrap();
    let wire_back = pblite::pblite_to_wire(&pblite_json).unwrap();
    let decoded = proto::SetTypingStateRequest::decode(wire_back).unwrap();

    assert_eq!(decoded.state, Some(1));
    let ctx = decoded.context.unwrap();
    let gid = ctx.group_id.unwrap();
    assert_eq!(gid.space_id.unwrap().space_id, Some("spaces/myroom".into()));
}

#[test]
fn mark_group_readstate_request_roundtrip() {
    let req = proto::MarkGroupReadstateRequest {
        request_header: Some(proto::RequestHeader {
            client_type: Some(3),
            ..Default::default()
        }),
        id: Some(proto::GroupId {
            space_id: Some(proto::SpaceId {
                space_id: Some("spaces/room_abc".into()),
            }),
            dm_id: None,
        }),
        last_read_time: Some(1700000000000000),
    };

    let wire = req.encode_to_vec();
    let pblite_json = pblite::wire_to_pblite(&wire).unwrap();
    let wire_back = pblite::pblite_to_wire(&pblite_json).unwrap();
    let decoded = proto::MarkGroupReadstateRequest::decode(wire_back).unwrap();

    assert_eq!(decoded.last_read_time, Some(1700000000000000));
    let gid = decoded.id.unwrap();
    assert_eq!(
        gid.space_id.unwrap().space_id,
        Some("spaces/room_abc".into())
    );
}

// ─────────────────── Response decode tests ───────────────────

/// Simulate decoding a PaginatedWorldResponse from server pblite JSON.
#[test]
fn decode_world_response_from_pblite() {
    // Simulate what the server would send back as pblite JSON
    // PaginatedWorldResponse fields:
    //   1: repeated WorldSectionResponse
    //   2: world_consistency_token (string)
    //   3: user_revision (ReadRevision)
    //   4: repeated WorldItemLite
    //
    // WorldItemLite fields:
    //   1: group_id (GroupId)
    //   3: sort_timestamp (int64)
    //   5: room_name (string)
    let pblite_json = serde_json::json!([
        // field 1: world_section_responses (empty)
        [],
        // field 2: world_consistency_token
        "token_abc123",
        // field 3: user_revision (ReadRevision { timestamp })
        [1700000000],
        // field 4: repeated WorldItemLite — two items
        [
            // WorldItemLite 1: GroupId at field 1, sort_ts at field 3, name at field 5
            [
                // GroupId: SpaceId at field 1
                [
                    // SpaceId: space_id at field 1
                    ["spaces/room_alpha"]
                ],
                null,
                1700000001,
                null,
                "Alpha Room"
            ],
            // WorldItemLite 2
            [[["spaces/room_beta"]], null, 1700000002, null, "Beta Room"]
        ]
    ]);

    let wire = pblite::pblite_to_wire(&pblite_json).unwrap();
    let resp = proto::PaginatedWorldResponse::decode(wire).unwrap();

    assert_eq!(resp.world_consistency_token, Some("token_abc123".into()));
    assert_eq!(resp.world_items.len(), 2);
    assert_eq!(resp.world_items[0].room_name, Some("Alpha Room".into()));
    assert_eq!(resp.world_items[0].sort_timestamp, Some(1700000001));
    assert_eq!(resp.world_items[1].room_name, Some("Beta Room".into()));

    let gid0 = resp.world_items[0].group_id.as_ref().unwrap();
    assert_eq!(
        gid0.space_id.as_ref().unwrap().space_id,
        Some("spaces/room_alpha".into())
    );
}

/// Simulate decoding a CatchUpResponse with message events.
#[test]
fn decode_catch_up_response_with_message_from_pblite() {
    // CatchUpResponse fields:
    //   1: repeated Event
    //   2: status (enum)
    //
    // Event fields:
    //   1: group_id
    //   3: type (enum EventType)
    //   4: body (EventBody)
    //
    // EventBody fields:
    //   6: message_posted (MessageEvent)
    //   12: event_type
    //
    // MessageEvent fields:
    //   1: message (Message)
    //
    // Message fields:
    //   1: id (MessageId)
    //   2: creator (User)
    //   3: create_time
    //   10: text_body
    let event_pblite = serde_json::json!([
        // Event.group_id (field 1)
        [["spaces/test_room"]],
        null,
        // Event.type (field 3)
        6, // MESSAGE_POSTED
        // Event.body (field 4 = EventBody)
        [
            null,
            null,
            null,
            null,
            null,
            // EventBody.message_posted (field 6 = MessageEvent)
            [
                // MessageEvent.message (field 1 = Message)
                [
                    // Message.id (field 1 = MessageId)
                    [null, "msg_history_001"],
                    // Message.creator (field 2 = User)
                    [
                        // User.user_id (field 1)
                        ["user_sender_123"]
                    ],
                    // Message.create_time (field 3)
                    1700000500000000_i64,
                    null,
                    null,
                    null,
                    null,
                    null,
                    null,
                    // Message.text_body (field 10)
                    "Hello from the past!"
                ]
            ]
        ]
    ]);

    // For a single repeated message, the pblite format places the message
    // fields directly at the position (indistinguishable from a nested message).
    // With multiple repeated entries, it would be [[event1], [event2], ...].
    let resp_pblite = serde_json::json!([
        // CatchUpResponse.events (field 1) — single Event directly
        event_pblite,
        // CatchUpResponse.status (field 2)
        1 // COMPLETED
    ]);

    let wire = pblite::pblite_to_wire(&resp_pblite).unwrap();
    let resp = proto::CatchUpResponse::decode(wire).unwrap();

    assert_eq!(resp.status, Some(1));
    assert_eq!(resp.events.len(), 1);

    let event = &resp.events[0];
    assert_eq!(event.r#type, Some(6));

    let body = event.body.as_ref().unwrap();
    let msg_event = body.message_posted.as_ref().unwrap();
    let msg = msg_event.message.as_ref().unwrap();
    assert_eq!(msg.text_body, Some("Hello from the past!".into()));
    assert_eq!(msg.create_time, Some(1700000500000000));

    let mid = msg.id.as_ref().unwrap();
    assert_eq!(mid.message_id, Some("msg_history_001".into()));

    let creator = msg.creator.as_ref().unwrap();
    let uid = creator.user_id.as_ref().unwrap();
    assert_eq!(uid.id, Some("user_sender_123".into()));
}
