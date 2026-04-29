//! Convert between Google Chat proto types and platform-agnostic types.

use crate::event::{InboundEvent, OutboundCommand};
use crate::types::*;

use super::api::ApiRequest;
use super::proto;
use super::session::Session;

// ─────────────────── Proto → Platform-Agnostic ───────────────────

/// Convert a PaginatedWorldResponse into a WorldSync event.
pub fn world_response_to_event(
    resp: proto::PaginatedWorldResponse,
    session: &mut Session,
) -> InboundEvent {
    let mut spaces = Vec::new();

    // Spaces come from world_section_responses and world_items
    for section in &resp.world_section_responses {
        for item in &section.world_items {
            if let Some(space) = world_item_to_space(item, &mut session.interner) {
                spaces.push(space);
            }
        }
    }
    for item in &resp.world_items {
        if let Some(space) = world_item_to_space(item, &mut session.interner) {
            spaces.push(space);
        }
    }

    let self_user = User {
        id: UserId {
            platform: PlatformId::GoogleChat,
            id: session.interner.intern("self"),
        },
        display_name: "Me".to_owned(),
        email: None,
        avatar_url: None,
        presence: PresenceStatus::Active,
        is_bot: false,
    };

    InboundEvent::WorldSync {
        platform: PlatformId::GoogleChat,
        spaces,
        self_user,
    }
}

fn world_item_to_space(item: &proto::WorldItemLite, interner: &mut IdInterner) -> Option<Space> {
    let group_id = item.group_id.as_ref()?;
    let space_id = group_id_to_space_id(group_id, interner);
    let name = item.room_name.clone().unwrap_or_default();
    let sort_ts = item.sort_timestamp.unwrap_or(0) as u64;

    Some(Space {
        id: space_id,
        name,
        kind: SpaceKind::Room, // Refined via group attributes if available
        platform: PlatformId::GoogleChat,
        unread_count: 0,
        last_activity: Timestamp(sort_ts),
        sort_timestamp: Timestamp(sort_ts),
        typing_users: Vec::new(),
    })
}

/// Convert a CatchUpResponse into a HistoryChunk event.
///
/// CatchUpResponse contains `repeated Event events`, each with an EventBody
/// that may hold a `message_posted` (MessageEvent) or `topic_created`.
/// We extract all messages and return them as a flat HistoryChunk.
pub fn history_response_to_event(
    resp: proto::CatchUpResponse,
    space_id: SpaceId,
    session: &mut Session,
) -> InboundEvent {
    let has_more = resp.status == Some(2); // PAGINATED

    let mut messages = Vec::new();

    for event in &resp.events {
        // Extract from primary body
        if let Some(ref body) = event.body {
            extract_messages_from_body(body, space_id, &mut session.interner, &mut messages);
        }
        // Extract from additional bodies
        for body in &event.bodies {
            extract_messages_from_body(body, space_id, &mut session.interner, &mut messages);
        }
    }

    InboundEvent::HistoryChunk {
        space_id,
        messages,
        has_more,
    }
}

/// Extract messages from a single EventBody.
fn extract_messages_from_body(
    body: &proto::event::EventBody,
    space_id: SpaceId,
    interner: &mut IdInterner,
    out: &mut Vec<Message>,
) {
    // MESSAGE_POSTED events contain a MessageEvent with one Message
    if let Some(ref msg_event) = body.message_posted {
        if let Some(ref proto_msg) = msg_event.message {
            if let Some(msg) = proto_message_to_message(proto_msg, space_id, interner) {
                out.push(msg);
            }
        }
    }

    // TOPIC_CREATED events contain a Topic with replies
    if let Some(ref topic_event) = body.topic_created {
        if let Some(ref topic) = topic_event.topic {
            let thread_id = topic
                .id
                .as_ref()
                .and_then(|tid| tid.topic_id.as_ref().map(|s| TopicId(interner.intern(s))));
            for reply in &topic.replies {
                if let Some(mut msg) = proto_message_to_message(reply, space_id, interner) {
                    if msg.thread_id.is_none() {
                        msg.thread_id = thread_id;
                    }
                    out.push(msg);
                }
            }
        }
    }
}

/// Convert a proto Message to a platform-agnostic Message.
fn proto_message_to_message(
    proto_msg: &proto::Message,
    space_id: SpaceId,
    interner: &mut IdInterner,
) -> Option<Message> {
    let msg_id_str = proto_msg
        .id
        .as_ref()
        .and_then(|id| id.message_id.as_deref())?;
    let msg_id = MessageId(interner.intern(msg_id_str));

    let sender = proto_msg
        .creator
        .as_ref()
        .and_then(|u| u.user_id.as_ref())
        .and_then(|uid| uid.id.as_deref())
        .map(|s| UserId {
            platform: PlatformId::GoogleChat,
            id: interner.intern(s),
        })
        .unwrap_or(UserId {
            platform: PlatformId::GoogleChat,
            id: interner.intern("unknown"),
        });

    let timestamp = Timestamp(proto_msg.create_time.unwrap_or(0) as u64);
    let edit_timestamp = proto_msg.last_edit_time.map(|t| Timestamp(t as u64));

    let text = proto_msg.text_body.clone().unwrap_or_default();

    let thread_id = proto_msg
        .id
        .as_ref()
        .and_then(|id| id.parent_id.as_ref())
        .and_then(|pid| pid.topic_id.as_ref())
        .and_then(|tid| tid.topic_id.as_ref())
        .map(|s| TopicId(interner.intern(s)));

    let message_type = match proto_msg.message_type {
        Some(2) => MessageType::System, // SYSTEM_MESSAGE
        _ => MessageType::User,
    };

    let reactions = proto_msg
        .reactions
        .iter()
        .map(|r| proto_reaction_to_reaction(r))
        .collect();

    Some(Message {
        id: msg_id,
        space_id,
        sender,
        timestamp,
        edit_timestamp,
        text,
        annotations: Vec::new(), // TODO: convert annotations
        reactions,
        thread_id,
        message_type,
        platform: PlatformId::GoogleChat,
    })
}

/// Convert a proto Reaction to a platform-agnostic Reaction.
fn proto_reaction_to_reaction(r: &proto::Reaction) -> Reaction {
    let emoji = r
        .emoji
        .as_ref()
        .map(|e| {
            if let Some(ref unicode) = e.unicode {
                Emoji::Unicode(unicode.clone())
            } else if let Some(ref custom) = e.custom_emoji {
                Emoji::Custom {
                    id: custom.uuid.clone().unwrap_or_default(),
                    shortcode: custom.shortcode.clone().unwrap_or_default(),
                }
            } else {
                Emoji::Unicode("?".to_owned())
            }
        })
        .unwrap_or(Emoji::Unicode("?".to_owned()));

    Reaction {
        emoji,
        count: r.count.unwrap_or(0) as u32,
        includes_self: r.current_user_participated.unwrap_or(false),
    }
}

/// Convert a ListTopicsResponse into a HistoryChunk event.
///
/// Each Topic contains replies — we flatten them into messages with thread_id set.
pub fn list_topics_response_to_event(
    resp: proto::ListTopicsResponse,
    space_id: SpaceId,
    session: &mut Session,
) -> InboundEvent {
    list_topics_response_to_event_with_interner(resp, space_id, &mut session.interner)
}

/// Same as list_topics_response_to_event but takes a bare interner.
pub fn list_topics_response_to_event_with_interner(
    resp: proto::ListTopicsResponse,
    space_id: SpaceId,
    interner: &mut IdInterner,
) -> InboundEvent {
    let has_more = !resp.contains_first_topic.unwrap_or(true);
    let mut messages = Vec::new();

    for topic in &resp.topics {
        let thread_id = topic
            .id
            .as_ref()
            .and_then(|tid| tid.topic_id.as_ref().map(|s| TopicId(interner.intern(s))));

        for reply in &topic.replies {
            if let Some(mut msg) = proto_message_to_message(reply, space_id, interner) {
                if msg.thread_id.is_none() {
                    msg.thread_id = thread_id;
                }
                messages.push(msg);
            }
        }
    }

    InboundEvent::HistoryChunk {
        space_id,
        messages,
        has_more,
    }
}

/// Convert a proto GroupId to a platform-agnostic SpaceId.
pub fn group_id_to_space_id(gid: &proto::GroupId, interner: &mut IdInterner) -> SpaceId {
    let raw = if let Some(ref sid) = gid.space_id {
        sid.space_id.as_deref().unwrap_or("")
    } else if let Some(ref did) = gid.dm_id {
        did.dm_id.as_deref().unwrap_or("")
    } else {
        ""
    };
    SpaceId {
        platform: PlatformId::GoogleChat,
        id: interner.intern(raw),
    }
}

// ─────────────────── OutboundCommand → ApiRequest ───────────────────

/// Convert a platform-agnostic OutboundCommand into a Google Chat ApiRequest.
pub fn command_to_api_request(cmd: OutboundCommand, interner: &IdInterner) -> Option<ApiRequest> {
    match cmd {
        OutboundCommand::SendMessage {
            space_id,
            text,
            thread_id,
        } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            // The server requires parent_id.topic_id to be Some(TopicId).
            // For top-level messages (new thread), TopicId.topic_id = None and
            // TopicId.group_id = the space. For replies, both are set.
            let parent_id = proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(make_group_id(space_str)),
                    topic_id: thread_id.map(|tid| interner.resolve(tid.0).unwrap_or("").to_owned()),
                }),
            };

            Some(ApiRequest::SendMessage(proto::CreateMessageRequest {
                request_header: Some(make_request_header()),
                parent_id: Some(parent_id),
                text_body: Some(text),
                annotations: Vec::new(),
                local_id: Some(format!(
                    "tchat-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_micros())
                        .unwrap_or(0)
                )),
                message_id: None,
                message_info: Some(proto::MessageInfo {
                    accept_format_annotations: Some(true),
                    reply_to: None,
                }),
            }))
        }
        OutboundCommand::SetTyping { space_id, typing } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            let state = if typing { 3 } else { 1 }; // TYPING = 3, STOPPED = 1
            Some(ApiRequest::SetTyping(proto::SetTypingStateRequest {
                request_header: Some(make_request_header()),
                state: Some(state),
                context: Some(proto::TypingContext {
                    group_id: Some(make_group_id(space_str)),
                    topic_id: None,
                }),
            }))
        }
        OutboundCommand::EditMessage {
            space_id: _,
            message_id,
            new_text,
        } => {
            let msg_str = interner.resolve(message_id.0).unwrap_or("");
            Some(ApiRequest::EditMessage(proto::EditMessageRequest {
                request_header: Some(make_request_header()),
                message_id: Some(proto::MessageId {
                    parent_id: Some(proto::MessageParentId { topic_id: None }),
                    message_id: Some(msg_str.to_owned()),
                }),
                text_body: Some(new_text),
                annotations: Vec::new(),
                message_info: None,
            }))
        }
        OutboundCommand::DeleteMessage {
            space_id,
            message_id,
        } => {
            let msg_str = interner.resolve(message_id.0).unwrap_or("");
            let _ = space_id; // MessageId is globally unique
            Some(ApiRequest::DeleteMessage(proto::DeleteMessageRequest {
                request_header: Some(make_request_header()),
                message_id: Some(proto::MessageId {
                    parent_id: Some(proto::MessageParentId { topic_id: None }),
                    message_id: Some(msg_str.to_owned()),
                }),
            }))
        }
        OutboundCommand::FetchHistory {
            space_id,
            before: _,
            count,
        } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            // Use list_topics — verified working with structured topic/reply
            // decoding. catch_up_group works too but returns flat Events that
            // are harder to decode.
            Some(ApiRequest::ListTopics(proto::ListTopicsRequest {
                request_header: Some(make_request_header()),
                group_id: Some(make_group_id(space_str)),
                page_size_for_topics: Some(count as i32),
                page_size_for_replies: Some(5),
                page_size_for_unread_replies: Some(100),
                page_size_for_read_replies: Some(5),
                fetch_options: vec![3, 1, 4], // READ_RECEIPTS, USER, and misc
                user_not_older_than: None,
                group_not_older_than: None,
            }))
        }
        OutboundCommand::MarkRead { space_id, up_to } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            // MarkRead uses a message ID that encodes the read-up-to point.
            // The proto uses last_read_time (timestamp). We look up the message's
            // timestamp by resolving the MessageId — but we don't have access to
            // the store here. Instead, use the current time as a reasonable default.
            // The caller should ideally pass the timestamp directly.
            let _ = up_to;
            Some(ApiRequest::MarkRead(proto::MarkGroupReadstateRequest {
                request_header: Some(make_request_header()),
                id: Some(make_group_id(space_str)),
                last_read_time: None, // Server uses current time if omitted
            }))
        }
        OutboundCommand::AddReaction {
            space_id,
            message_id,
            emoji,
        } => {
            // Verified working against the live server.
            //
            // Two requirements (discovered by comparing with mautrix/googlechat):
            // 1. MessageId structure must include the full topic_id path:
            //      MessageId {
            //        parent_id: MessageParentId {
            //          topic_id: TopicId {
            //            group_id: GroupId { ... },
            //            topic_id: thread_id or message_id,  // MUST be set
            //          }
            //        },
            //        message_id: message_id or thread_id,
            //      }
            //    For top-level messages, topic_id == message_id.
            //
            // 2. The request must use BINARY protobuf (`?rt=b`,
            //    Content-Type: application/x-protobuf), not pblite JSON.
            //    `session.call_api` automatically routes update_reaction to
            //    the binary path.
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            let msg_str = interner.resolve(message_id.0).unwrap_or("");
            Some(ApiRequest::UpdateReaction(proto::UpdateReactionRequest {
                request_header: Some(make_request_header()),
                message_id: Some(proto::MessageId {
                    parent_id: Some(proto::MessageParentId {
                        topic_id: Some(proto::TopicId {
                            group_id: Some(make_group_id(space_str)),
                            topic_id: Some(msg_str.to_owned()),
                        }),
                    }),
                    message_id: Some(msg_str.to_owned()),
                }),
                emoji: Some(emoji_to_proto(&emoji)),
                option: Some(1), // ADD
            }))
        }
        OutboundCommand::RemoveReaction {
            space_id,
            message_id,
            emoji,
        } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            let msg_str = interner.resolve(message_id.0).unwrap_or("");
            Some(ApiRequest::UpdateReaction(proto::UpdateReactionRequest {
                request_header: Some(make_request_header()),
                message_id: Some(proto::MessageId {
                    parent_id: Some(proto::MessageParentId {
                        topic_id: Some(proto::TopicId {
                            group_id: Some(make_group_id(space_str)),
                            topic_id: Some(msg_str.to_owned()),
                        }),
                    }),
                    message_id: Some(msg_str.to_owned()),
                }),
                emoji: Some(emoji_to_proto(&emoji)),
                option: Some(2), // REMOVE
            }))
        }

        // ─────── New endpoints ───────
        OutboundCommand::CreateTopic { space_id, text } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            Some(ApiRequest::CreateTopic(proto::CreateTopicRequest {
                request_header: Some(make_request_header()),
                group_id: Some(make_group_id(space_str)),
                text_body: Some(text),
                annotations: Vec::new(),
                retention_settings: None,
                local_id: Some(format!("tchat-topic-{}", std::process::id())),
                topic_and_message_id: None,
                history_v2: None,
                message_info: None,
                thread_options: None,
            }))
        }

        OutboundCommand::GetUsers { user_ids } => {
            let member_ids: Vec<_> = user_ids
                .iter()
                .map(|uid| {
                    let id_str = interner.resolve(uid.id).unwrap_or("");
                    proto::MemberId {
                        user_id: Some(proto::UserId {
                            id: Some(id_str.to_owned()),
                            ..Default::default()
                        }),
                        roster_id: None,
                        email: None,
                    }
                })
                .collect();
            Some(ApiRequest::GetMembers(proto::GetMembersRequest {
                request_header: Some(make_request_header()),
                member_ids,
                membership_ids: Vec::new(),
            }))
        }

        OutboundCommand::GetUserPresence { user_ids } => {
            Some(ApiRequest::GetUserPresence(proto::GetUserPresenceRequest {
                request_header: Some(make_request_header()),
                user_ids: user_ids
                    .iter()
                    .map(|uid| {
                        let id_str = interner.resolve(uid.id).unwrap_or("");
                        proto::UserId {
                            id: Some(id_str.to_owned()),
                            ..Default::default()
                        }
                    })
                    .collect(),
                include_active_until: Some(true),
                include_user_status: Some(true),
            }))
        }

        OutboundCommand::RefreshSelf => Some(ApiRequest::GetSelfUserStatus(
            proto::GetSelfUserStatusRequest {
                request_header: Some(make_request_header()),
            },
        )),

        OutboundCommand::CreateRoom {
            name,
            invite_user_ids,
        } => {
            // Flat rooms work reliably (verified live). Threaded rooms return
            // 400 — likely need different SpaceCreationInfo fields.
            let invitees: Vec<_> = invite_user_ids
                .iter()
                .map(|uid| {
                    let id_str = interner.resolve(uid.id).unwrap_or("");
                    proto::InviteeMemberInfo {
                        invitee_info: Some(proto::InviteeInfo {
                            user_id: Some(proto::UserId {
                                id: Some(id_str.to_owned()),
                                ..Default::default()
                            }),
                            email: None,
                        }),
                    }
                })
                .collect();
            Some(ApiRequest::CreateGroup(proto::CreateGroupRequest {
                request_header: Some(make_request_header()),
                space: Some(proto::SpaceCreationInfo {
                    name: Some(name),
                    visibility: None,
                    flat_group: Some(proto::space_creation_info::FlatGroup {}),
                    threaded_group: None,
                    has_server_generated_name: Some(false),
                    invitee_member_infos: invitees,
                    space_type: None,
                    attribute_checker_group_type: Some(4), // FLAT_ROOM
                    shared_drive_name: None,
                }),
                local_id: Some(format!("tchat-room-{}", std::process::id())),
                should_find_existing_space: Some(false),
            }))
        }

        OutboundCommand::CreateDm { user_ids } => {
            let members: Vec<_> = user_ids
                .iter()
                .map(|uid| {
                    let id_str = interner.resolve(uid.id).unwrap_or("");
                    proto::UserId {
                        id: Some(id_str.to_owned()),
                        ..Default::default()
                    }
                })
                .collect();
            Some(ApiRequest::CreateDm(proto::CreateDmRequest {
                request_header: Some(make_request_header()),
                fetch_options: Vec::new(),
                members,
                invitees: Vec::new(),
                retention_settings: None,
                local_id: Some(format!("tchat-dm-{}", std::process::id())),
                topic_and_message_id: None,
            }))
        }

        OutboundCommand::AddMembers { space_id, user_ids } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            let member_ids: Vec<_> = user_ids
                .iter()
                .map(|uid| {
                    let id_str = interner.resolve(uid.id).unwrap_or("");
                    proto::MemberId {
                        user_id: Some(proto::UserId {
                            id: Some(id_str.to_owned()),
                            ..Default::default()
                        }),
                        roster_id: None,
                        email: None,
                    }
                })
                .collect();
            Some(ApiRequest::CreateMembership(
                proto::CreateMembershipRequest {
                    request_header: Some(make_request_header()),
                    member_ids,
                    invitee_member_infos: Vec::new(),
                    membership_state: Some(2), // MEMBER_JOINED
                    group_id: Some(make_group_id(space_str)),
                    notification_settings: None,
                },
            ))
        }

        OutboundCommand::RemoveMembers { space_id, user_ids } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            let member_ids: Vec<_> = user_ids
                .iter()
                .map(|uid| {
                    let id_str = interner.resolve(uid.id).unwrap_or("");
                    proto::MemberId {
                        user_id: Some(proto::UserId {
                            id: Some(id_str.to_owned()),
                            ..Default::default()
                        }),
                        roster_id: None,
                        email: None,
                    }
                })
                .collect();
            Some(ApiRequest::RemoveMemberships(
                proto::RemoveMembershipsRequest {
                    request_header: Some(make_request_header()),
                    member_ids,
                    group_id: Some(make_group_id(space_str)),
                    membership_state: Some(3), // MEMBER_NOT_A_MEMBER
                },
            ))
        }

        OutboundCommand::RenameSpace { space_id, new_name } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            Some(ApiRequest::UpdateGroup(proto::UpdateGroupRequest {
                request_header: Some(make_request_header()),
                space_id: Some(proto::SpaceId {
                    space_id: Some(space_str.to_owned()),
                }),
                update_masks: vec![1], // NAME
                name: Some(new_name),
                visibility: None,
            }))
        }

        OutboundCommand::HideSpace { space_id, hide } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            Some(ApiRequest::HideGroup(proto::HideGroupRequest {
                request_header: Some(make_request_header()),
                id: Some(make_group_id(space_str)),
                hide: Some(hide),
            }))
        }

        OutboundCommand::ListSpaceMembers { space_id } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            Some(ApiRequest::ListMembers(proto::ListMembersRequest {
                request_header: Some(make_request_header()),
                space_id: None,
                group_id: Some(make_group_id(space_str)),
                page_size: Some(100),
                page_token: None,
                not_older_than: None,
            }))
        }

        OutboundCommand::SetDndDuration { duration_usec } => {
            Some(ApiRequest::SetDndDuration(proto::SetDndDurationRequest {
                request_header: Some(make_request_header()),
                current_dnd_state: None,
                new_dnd_duration_usec: Some(duration_usec as i64),
                dnd_expiry_timestamp_usec: None,
            }))
        }

        OutboundCommand::SetCustomStatus {
            text,
            emoji,
            expiry_usec,
        } => Some(ApiRequest::SetCustomStatus(proto::SetCustomStatusRequest {
            request_header: Some(make_request_header()),
            custom_status: Some(proto::CustomStatus {
                status_text: Some(text),
                status_emoji: emoji.as_ref().and_then(|e| match e {
                    Emoji::Unicode(s) => Some(s.clone()),
                    _ => None,
                }),
                state_expiry_timestamp_usec: expiry_usec,
                emoji: emoji.as_ref().map(emoji_to_proto),
            }),
            custom_status_expiry_timestamp_usec: expiry_usec,
            custom_status_remaining_duration_usec: None,
        })),

        OutboundCommand::BlockEntity {
            user_id,
            space_id,
            blocked,
            reported,
        } => Some(ApiRequest::BlockEntity(proto::BlockEntityRequest {
            request_header: Some(make_request_header()),
            user_id: user_id.map(|uid| {
                let id_str = interner.resolve(uid.id).unwrap_or("");
                proto::UserId {
                    id: Some(id_str.to_owned()),
                    ..Default::default()
                }
            }),
            group_id: space_id.map(|sid| {
                let id_str = interner.resolve(sid.id).unwrap_or("");
                make_group_id(id_str)
            }),
            blocked: Some(blocked),
            reported: Some(reported),
        })),

        OutboundCommand::SetPresenceShared { shared } => Some(ApiRequest::SetPresenceShared(
            proto::SetPresenceSharedRequest {
                request_header: Some(make_request_header()),
                presence_shared: Some(shared),
            },
        )),

        OutboundCommand::CreateCustomEmoji { shortcode } => Some(ApiRequest::CreateCustomEmoji(
            proto::CreateCustomEmojiRequest {
                request_header: Some(make_request_header()),
                shortcode: Some(shortcode),
            },
        )),

        OutboundCommand::AutocompleteSlashCommands {
            query,
            space_id,
            max_results,
        } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            Some(ApiRequest::AutocompleteSlashCommands(
                proto::AutocompleteSlashCommandsRequest {
                    request_header: Some(make_request_header()),
                    query: Some(query),
                    group_id: Some(make_group_id(space_str)),
                    max_num_results: Some(max_results as i32),
                    restrict_to_bots_in_group: Some(false),
                    bot_use_case_filters: Vec::new(),
                    include_dialogs: Some(false),
                },
            ))
        }

        OutboundCommand::CreateVideoCall { space_id } => {
            let space_str = interner.resolve(space_id.id).unwrap_or("");
            Some(ApiRequest::CreateVideoCall(proto::CreateVideoCallRequest {
                group_id: Some(make_group_id(space_str)),
                request_header: Some(make_request_header()),
            }))
        }

        OutboundCommand::Disconnect => None,
    }
}

/// Convert a GetMembersResponse into a UsersResolved event for the store.
pub fn members_response_to_event(
    resp: proto::GetMembersResponse,
    session: &mut Session,
) -> InboundEvent {
    let mut users = Vec::new();
    for member in resp.members {
        if let Some(u) = member.user {
            if let Some(uid) = &u.user_id {
                if let Some(id_str) = &uid.id {
                    let interned = session.interner.intern(id_str);
                    users.push(User {
                        id: UserId {
                            platform: PlatformId::GoogleChat,
                            id: interned,
                        },
                        display_name: u.name.unwrap_or_default(),
                        email: u.email,
                        avatar_url: u.avatar_url,
                        presence: PresenceStatus::Unknown,
                        is_bot: u.bot_info.is_some(),
                    });
                }
            }
        }
    }
    InboundEvent::UsersResolved { users }
}

/// Convert a GetUserPresenceResponse into a PresenceChanged event(s) — wrapped
/// as a UsersResolved-with-presence by updating each user.
pub fn presence_response_to_event(
    resp: proto::GetUserPresenceResponse,
    session: &mut Session,
) -> InboundEvent {
    // We just emit one PresenceChanged for the first user; for batch updates,
    // the IO loop should iterate and emit multiple events. For simplicity now,
    // we package as UsersResolved with presence overwritten.
    let mut users = Vec::new();
    for p in resp.user_presences {
        if let Some(uid) = &p.user_id {
            if let Some(id_str) = &uid.id {
                let interned = session.interner.intern(id_str);
                let presence = match p.presence.unwrap_or(0) {
                    1 => PresenceStatus::Active,
                    2 => PresenceStatus::Inactive,
                    _ => PresenceStatus::Unknown,
                };
                users.push(User {
                    id: UserId {
                        platform: PlatformId::GoogleChat,
                        id: interned,
                    },
                    display_name: String::new(),
                    email: None,
                    avatar_url: None,
                    presence,
                    is_bot: false,
                });
            }
        }
    }
    InboundEvent::UsersResolved { users }
}

/// Expose for integration tests.
pub fn tests_make_header() -> proto::RequestHeader {
    make_request_header()
}

fn make_request_header() -> proto::RequestHeader {
    proto::RequestHeader {
        client_type: Some(3), // WEB
        client_version: Some(1),
        trace_id: Some(0),
        locale: Some("en".to_owned()),
        client_feature_capabilities: Some(proto::ClientFeatureCapabilities {
            // Match the web client: set critical capabilities to FULLY_SUPPORTED (2)
            spaces_level_for_testing: None,
            dms_level_for_testing: None,
            post_rooms_level: None,
            spam_room_invites_level: Some(2),
            tombstone_level: Some(2),
            rich_text_viewing_level: None,
            threaded_spaces_level: Some(2),
            flat_named_room_topic_ordering_by_creation_time_level: Some(2),
            target_audience_level: Some(2),
            group_scoped_capabilities_level: Some(2),
            activity_feed_level: None,
            roster_as_member_support_level: Some(2),
            tombstone_in_dms_and_ufrs_level: Some(2),
            quoted_message_support_level: Some(2),
            render_announcement_spaces_level: Some(2),
            dark_launch_space_support: Some(2),
            avoid_http_400_error_support_level: Some(2),
            custom_hyperlink_level: Some(2),
            snippets_for_named_rooms: Some(2),
            can_add_continuous_direct_add_groups: Some(2),
            drive_smart_chip_level: Some(2),
            gsuite_integration_in_native_renderer_level: Some(2),
            mentions_shortcut_level: Some(2),
            starred_shortcut_level: Some(2),
            search_snippet_and_keyword_highlight_level: Some(2),
            app_section_level: None,
            create_thread_on_message_send_level: None,
            can_handle_batch_reaction_update: Some(2),
            longer_group_snippets_level: Some(2),
            muting_groups_level: None,
            muting_write_level: None,
            add_existing_members_level: None,
            request_to_join_level: Some(2),
            threads_in_home_level: Some(2),
            enable_all_features: None,
        }),
    }
}

fn emoji_to_proto(emoji: &Emoji) -> proto::Emoji {
    match emoji {
        Emoji::Unicode(s) => proto::Emoji {
            unicode: Some(s.clone()),
            custom_emoji: None,
        },
        Emoji::Custom { id, shortcode } => proto::Emoji {
            unicode: None,
            custom_emoji: Some(proto::CustomEmoji {
                uuid: Some(id.clone()),
                shortcode: Some(shortcode.clone()),
                ..Default::default()
            }),
        },
    }
}

fn make_group_id(space_str: &str) -> proto::GroupId {
    if space_str.starts_with("dm/") || space_str.contains('@') {
        // DM: strip "dm/" prefix if present — the API uses bare IDs
        let id = space_str.strip_prefix("dm/").unwrap_or(space_str);
        proto::GroupId {
            space_id: None,
            dm_id: Some(proto::DmId {
                dm_id: Some(id.to_owned()),
            }),
        }
    } else {
        // Space: strip "space/" or "spaces/" prefix if present
        let id = space_str
            .strip_prefix("space/")
            .or_else(|| space_str.strip_prefix("spaces/"))
            .unwrap_or(space_str);
        proto::GroupId {
            space_id: Some(proto::SpaceId {
                space_id: Some(id.to_owned()),
            }),
            dm_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_id_to_space_id_with_space() {
        let mut interner = IdInterner::new();
        let gid = proto::GroupId {
            space_id: Some(proto::SpaceId {
                space_id: Some("spaces/abc123".into()),
            }),
            dm_id: None,
        };
        let sid = group_id_to_space_id(&gid, &mut interner);
        assert_eq!(sid.platform, PlatformId::GoogleChat);
        assert_eq!(interner.resolve(sid.id), Some("spaces/abc123"));
    }

    #[test]
    fn group_id_to_space_id_with_dm() {
        let mut interner = IdInterner::new();
        let gid = proto::GroupId {
            space_id: None,
            dm_id: Some(proto::DmId {
                dm_id: Some("dm/xyz".into()),
            }),
        };
        let sid = group_id_to_space_id(&gid, &mut interner);
        assert_eq!(interner.resolve(sid.id), Some("dm/xyz"));
    }

    #[test]
    fn command_to_api_request_set_typing() {
        let mut interner = IdInterner::new();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: interner.intern("spaces/test"),
        };
        let cmd = OutboundCommand::SetTyping {
            space_id,
            typing: true,
        };
        let req = command_to_api_request(cmd, &interner);
        assert!(matches!(req, Some(ApiRequest::SetTyping(_))));
    }

    #[test]
    fn command_to_api_request_disconnect_returns_none() {
        let interner = IdInterner::new();
        let req = command_to_api_request(OutboundCommand::Disconnect, &interner);
        assert!(req.is_none());
    }

    #[test]
    fn make_request_header_sets_web_client() {
        let header = make_request_header();
        assert_eq!(header.client_type, Some(3));
        assert_eq!(header.locale, Some("en".to_owned()));
    }

    #[test]
    fn make_group_id_space() {
        let gid = make_group_id("spaces/abc");
        assert!(gid.space_id.is_some());
        assert!(gid.dm_id.is_none());
    }

    #[test]
    fn make_group_id_dm() {
        let gid = make_group_id("dm/xyz");
        assert!(gid.dm_id.is_some());
        assert!(gid.space_id.is_none());
    }

    #[test]
    fn command_to_api_request_send_message() {
        let mut interner = IdInterner::new();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: interner.intern("spaces/test"),
        };
        let cmd = OutboundCommand::SendMessage {
            space_id,
            text: "hello world".to_owned(),
            thread_id: None,
        };
        let req = command_to_api_request(cmd, &interner);
        match req {
            Some(ApiRequest::SendMessage(r)) => {
                assert_eq!(r.text_body, Some("hello world".into()));
                assert!(r.request_header.is_some());
            }
            _ => panic!("Expected correct variant"),
        }
    }

    #[test]
    fn command_to_api_request_edit_message() {
        let mut interner = IdInterner::new();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: interner.intern("spaces/test"),
        };
        let msg_id = crate::types::MessageId(interner.intern("msg_123"));
        let cmd = OutboundCommand::EditMessage {
            space_id,
            message_id: msg_id,
            new_text: "edited text".to_owned(),
        };
        let req = command_to_api_request(cmd, &interner);
        match req {
            Some(ApiRequest::EditMessage(r)) => {
                assert_eq!(r.text_body, Some("edited text".into()));
                assert!(r.message_id.is_some());
            }
            _ => panic!("Expected EditMessage"),
        }
    }

    #[test]
    fn command_to_api_request_delete_message() {
        let mut interner = IdInterner::new();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: interner.intern("spaces/test"),
        };
        let msg_id = crate::types::MessageId(interner.intern("msg_456"));
        let cmd = OutboundCommand::DeleteMessage {
            space_id,
            message_id: msg_id,
        };
        let req = command_to_api_request(cmd, &interner);
        match req {
            Some(ApiRequest::DeleteMessage(r)) => {
                let mid = r.message_id.unwrap();
                assert_eq!(mid.message_id, Some("msg_456".into()));
            }
            _ => panic!("Expected DeleteMessage"),
        }
    }

    #[test]
    fn command_to_api_request_fetch_history() {
        let mut interner = IdInterner::new();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: interner.intern("spaces/history"),
        };
        let cmd = OutboundCommand::FetchHistory {
            space_id,
            before: Timestamp(1000000),
            count: 50,
        };
        let req = command_to_api_request(cmd, &interner);
        match req {
            Some(ApiRequest::ListTopics(r)) => {
                assert!(r.group_id.is_some());
                assert_eq!(r.page_size_for_topics, Some(50));
            }
            _ => panic!("Expected ListTopics"),
        }
    }

    #[test]
    fn world_response_to_event_empty() {
        let tokens = crate::platform::googlechat::auth::Tokens {
            browser: None,
            xsrf_token: None,
            cookie_header: "SID=test".into(),
            sapisid: None,
            dynamite_token: None,
            dynamite_expiry: std::time::Instant::now(),
            raw_cookies: "SID=test".into(),
        };
        let mut session = crate::platform::googlechat::session::Session::new(tokens);

        let resp = proto::PaginatedWorldResponse {
            world_section_responses: Vec::new(),
            world_consistency_token: None,
            user_revision: None,
            world_items: Vec::new(),
        };
        let event = world_response_to_event(resp, &mut session);
        match event {
            InboundEvent::WorldSync { spaces, .. } => {
                assert!(spaces.is_empty());
            }
            _ => panic!("Expected WorldSync"),
        }
    }

    #[test]
    fn world_response_to_event_with_spaces() {
        let tokens = crate::platform::googlechat::auth::Tokens {
            browser: None,
            xsrf_token: None,
            cookie_header: "SID=test".into(),
            sapisid: None,
            dynamite_token: None,
            dynamite_expiry: std::time::Instant::now(),
            raw_cookies: "SID=test".into(),
        };
        let mut session = crate::platform::googlechat::session::Session::new(tokens);

        let resp = proto::PaginatedWorldResponse {
            world_section_responses: Vec::new(),
            world_consistency_token: None,
            user_revision: None,
            world_items: vec![proto::WorldItemLite {
                group_id: Some(proto::GroupId {
                    space_id: Some(proto::SpaceId {
                        space_id: Some("spaces/abc".into()),
                    }),
                    dm_id: None,
                }),
                group_revision: None,
                sort_timestamp: Some(1000),
                read_state: None,
                room_name: Some("Test Room".into()),
                ..Default::default()
            }],
        };
        let event = world_response_to_event(resp, &mut session);
        match event {
            InboundEvent::WorldSync { spaces, .. } => {
                assert_eq!(spaces.len(), 1);
                assert_eq!(spaces[0].name, "Test Room");
                assert_eq!(spaces[0].sort_timestamp, Timestamp(1000));
            }
            _ => panic!("Expected WorldSync"),
        }
    }

    // ─────── MarkRead and Reaction command tests ───────

    #[test]
    fn command_to_api_request_mark_read() {
        let mut interner = IdInterner::new();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: interner.intern("spaces/test"),
        };
        let msg_id = crate::types::MessageId(interner.intern("msg_123"));
        let cmd = OutboundCommand::MarkRead {
            space_id,
            up_to: msg_id,
        };
        let req = command_to_api_request(cmd, &interner);
        assert!(matches!(req, Some(ApiRequest::MarkRead(_))));
    }

    #[test]
    fn command_to_api_request_add_reaction() {
        let mut interner = IdInterner::new();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: interner.intern("spaces/test"),
        };
        let msg_id = crate::types::MessageId(interner.intern("msg_react"));
        let cmd = OutboundCommand::AddReaction {
            space_id,
            message_id: msg_id,
            emoji: Emoji::Unicode("👍".to_owned()),
        };
        let req = command_to_api_request(cmd, &interner);
        match req {
            Some(ApiRequest::UpdateReaction(r)) => {
                assert_eq!(r.option, Some(1)); // ADD
                let emoji = r.emoji.unwrap();
                assert_eq!(emoji.unicode, Some("👍".into()));
            }
            _ => panic!("Expected UpdateReaction"),
        }
    }

    #[test]
    fn command_to_api_request_remove_reaction() {
        let mut interner = IdInterner::new();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: interner.intern("spaces/test"),
        };
        let msg_id = crate::types::MessageId(interner.intern("msg_react"));
        let cmd = OutboundCommand::RemoveReaction {
            space_id,
            message_id: msg_id,
            emoji: Emoji::Unicode("😀".to_owned()),
        };
        let req = command_to_api_request(cmd, &interner);
        match req {
            Some(ApiRequest::UpdateReaction(r)) => {
                assert_eq!(r.option, Some(2)); // REMOVE
            }
            _ => panic!("Expected UpdateReaction"),
        }
    }

    // ─────── History response conversion tests ───────

    fn make_test_session() -> crate::platform::googlechat::session::Session {
        let tokens = crate::platform::googlechat::auth::Tokens {
            browser: None,
            xsrf_token: None,
            cookie_header: "SID=test".into(),
            sapisid: None,
            dynamite_token: None,
            dynamite_expiry: std::time::Instant::now(),
            raw_cookies: "SID=test".into(),
        };
        crate::platform::googlechat::session::Session::new(tokens)
    }

    #[test]
    fn history_response_empty() {
        let mut session = make_test_session();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: session.interner.intern("spaces/test"),
        };

        let resp = proto::CatchUpResponse {
            events: Vec::new(),
            status: Some(1), // COMPLETED
            group_data: None,
        };
        let event = history_response_to_event(resp, space_id, &mut session);
        match event {
            InboundEvent::HistoryChunk {
                messages, has_more, ..
            } => {
                assert!(messages.is_empty());
                assert!(!has_more);
            }
            _ => panic!("Expected HistoryChunk"),
        }
    }

    #[test]
    fn history_response_with_messages() {
        let mut session = make_test_session();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: session.interner.intern("spaces/test"),
        };

        let resp = proto::CatchUpResponse {
            events: vec![proto::Event {
                group_id: Some(proto::GroupId {
                    space_id: Some(proto::SpaceId {
                        space_id: Some("spaces/test".into()),
                    }),
                    dm_id: None,
                }),
                r#type: Some(6), // MESSAGE_POSTED
                body: Some(proto::event::EventBody {
                    message_posted: Some(proto::MessageEvent {
                        message: Some(proto::Message {
                            id: Some(proto::MessageId {
                                parent_id: None,
                                message_id: Some("msg_001".into()),
                            }),
                            creator: Some(proto::User {
                                user_id: Some(proto::UserId {
                                    id: Some("user_alice".into()),
                                    ..Default::default()
                                }),
                                name: Some("Alice".into()),
                                ..Default::default()
                            }),
                            create_time: Some(1700000000),
                            text_body: Some("Hello from history!".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            status: Some(2), // PAGINATED
            group_data: None,
        };
        let event = history_response_to_event(resp, space_id, &mut session);
        match event {
            InboundEvent::HistoryChunk {
                messages,
                has_more,
                space_id: sid,
            } => {
                assert_eq!(sid, space_id);
                assert!(has_more);
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].text, "Hello from history!");
                assert_eq!(messages[0].timestamp, Timestamp(1700000000));
            }
            _ => panic!("Expected HistoryChunk"),
        }
    }

    #[test]
    fn proto_message_to_message_extracts_reactions() {
        let mut interner = IdInterner::new();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: interner.intern("spaces/test"),
        };

        let proto_msg = proto::Message {
            id: Some(proto::MessageId {
                parent_id: None,
                message_id: Some("msg_react".into()),
            }),
            creator: Some(proto::User {
                user_id: Some(proto::UserId {
                    id: Some("user_1".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            create_time: Some(100000),
            text_body: Some("react test".into()),
            reactions: vec![proto::Reaction {
                emoji: Some(proto::Emoji {
                    unicode: Some("👍".into()),
                    custom_emoji: None,
                }),
                count: Some(3),
                current_user_participated: Some(true),
                create_timestamp: None,
            }],
            ..Default::default()
        };

        let msg = proto_message_to_message(&proto_msg, space_id, &mut interner).unwrap();
        assert_eq!(msg.reactions.len(), 1);
        assert_eq!(msg.reactions[0].count, 3);
        assert!(msg.reactions[0].includes_self);
        match &msg.reactions[0].emoji {
            Emoji::Unicode(s) => assert_eq!(s, "👍"),
            _ => panic!("Expected Unicode emoji"),
        }
    }

    #[test]
    fn make_group_id_at_sign_is_dm() {
        let gid = make_group_id("user@example.com");
        assert!(gid.dm_id.is_some());
        assert!(gid.space_id.is_none());
    }
}
