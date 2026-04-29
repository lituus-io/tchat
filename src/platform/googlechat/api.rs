//! Typed REST API calls to Google Chat endpoints.
//!
//! All requests are proxied through Chrome's page context via
//! `session.call_api()` → `Tokens.fetch_post()` → Chrome `fetch()`.

use crate::error::ApiError;

use super::session::Session;

/// Internal request type for the API thread's queue.
pub enum ApiRequest {
    // Existing
    PaginatedWorld(super::proto::PaginatedWorldRequest),
    SendMessage(super::proto::CreateMessageRequest),
    EditMessage(super::proto::EditMessageRequest),
    DeleteMessage(super::proto::DeleteMessageRequest),
    ListMessages(super::proto::ListMessagesRequest),
    ListTopics(super::proto::ListTopicsRequest),
    CatchUpGroup(super::proto::CatchUpGroupRequest),
    SetTyping(super::proto::SetTypingStateRequest),
    MarkRead(super::proto::MarkGroupReadstateRequest),
    UpdateReaction(super::proto::UpdateReactionRequest),
    GetGroup(super::proto::GetGroupRequest),
    // New: User operations
    GetMembers(super::proto::GetMembersRequest),
    GetUserPresence(super::proto::GetUserPresenceRequest),
    GetSelfUserStatus(super::proto::GetSelfUserStatusRequest),
    // New: Space management
    CreateGroup(super::proto::CreateGroupRequest),
    CreateDm(super::proto::CreateDmRequest),
    CreateMembership(super::proto::CreateMembershipRequest),
    RemoveMemberships(super::proto::RemoveMembershipsRequest),
    UpdateGroup(super::proto::UpdateGroupRequest),
    HideGroup(super::proto::HideGroupRequest),
    ListMembers(super::proto::ListMembersRequest),
    // New: Topic
    CreateTopic(super::proto::CreateTopicRequest),
    // New: User settings
    SetDndDuration(super::proto::SetDndDurationRequest),
    SetCustomStatus(super::proto::SetCustomStatusRequest),
    BlockEntity(super::proto::BlockEntityRequest),
    SetPresenceShared(super::proto::SetPresenceSharedRequest),
    // New: Advanced features
    CreateCustomEmoji(super::proto::CreateCustomEmojiRequest),
    AutocompleteSlashCommands(super::proto::AutocompleteSlashCommandsRequest),
    CreateVideoCall(super::proto::CreateVideoCallRequest),
}

/// Make a typed protobuf API call through Chrome's fetch().
pub fn call_proto<Req, Resp>(
    session: &mut Session,
    endpoint: &str,
    request: &Req,
) -> Result<Resp, ApiError>
where
    Req: prost::Message,
    Resp: prost::Message + Default,
{
    let body = request.encode_to_vec();

    let resp_bytes = session
        .call_api(endpoint, &body)
        .map_err(|e| ApiError::Http(e.to_string()))?;

    Resp::decode(bytes::Bytes::from(resp_bytes)).map_err(ApiError::ProtoDecode)
}

/// The API loop runs on its own thread, consuming ApiRequests.
pub fn api_loop(
    mut session: Session,
    rx: crossbeam::channel::Receiver<ApiRequest>,
    inbound_tx: crossbeam::channel::Sender<crate::event::InboundEvent>,
) {
    for req in rx {
        let result = match req {
            ApiRequest::PaginatedWorld(r) => call_proto::<_, super::proto::PaginatedWorldResponse>(
                &mut session,
                "paginated_world",
                &r,
            )
            .map(|resp| Some(super::convert::world_response_to_event(resp, &mut session))),
            ApiRequest::SendMessage(r) => call_proto::<_, super::proto::CreateMessageResponse>(
                &mut session,
                "create_message",
                &r,
            )
            .map(|_| None),
            ApiRequest::EditMessage(r) => {
                call_proto::<_, super::proto::EditMessageResponse>(&mut session, "edit_message", &r)
                    .map(|_| None)
            }
            ApiRequest::DeleteMessage(r) => call_proto::<_, super::proto::DeleteMessageResponse>(
                &mut session,
                "delete_message",
                &r,
            )
            .map(|_| None),
            ApiRequest::ListMessages(r) => call_proto::<_, super::proto::ListMessagesResponse>(
                &mut session,
                "list_messages",
                &r,
            )
            .map(|_| None),
            ApiRequest::SetTyping(r) => call_proto::<_, super::proto::SetTypingStateResponse>(
                &mut session,
                "set_typing_state",
                &r,
            )
            .map(|_| None),
            ApiRequest::MarkRead(r) => call_proto::<_, super::proto::MarkGroupReadstateResponse>(
                &mut session,
                "mark_group_readstate",
                &r,
            )
            .map(|_| None),
            ApiRequest::CatchUpGroup(r) => {
                call_proto::<_, super::proto::CatchUpResponse>(&mut session, "catch_up_group", &r)
                    .map(|_| None)
            }
            ApiRequest::ListTopics(r) => {
                call_proto::<_, super::proto::ListTopicsResponse>(&mut session, "list_topics", &r)
                    .map(|_| None)
            }
            ApiRequest::UpdateReaction(r) => call_proto::<_, super::proto::UpdateReactionResponse>(
                &mut session,
                "update_reaction",
                &r,
            )
            .map(|_| None),
            ApiRequest::GetGroup(r) => {
                call_proto::<_, super::proto::GetGroupResponse>(&mut session, "get_group", &r)
                    .map(|_| None)
            }
            ApiRequest::GetMembers(r) => {
                call_proto::<_, super::proto::GetMembersResponse>(&mut session, "get_members", &r)
                    .map(|_| None)
            }
            ApiRequest::GetUserPresence(r) => {
                call_proto::<_, super::proto::GetUserPresenceResponse>(
                    &mut session,
                    "get_user_presence",
                    &r,
                )
                .map(|_| None)
            }
            ApiRequest::GetSelfUserStatus(r) => call_proto::<
                _,
                super::proto::GetSelfUserStatusResponse,
            >(
                &mut session, "get_self_user_status", &r
            )
            .map(|_| None),
            ApiRequest::CreateGroup(r) => {
                call_proto::<_, super::proto::CreateGroupResponse>(&mut session, "create_group", &r)
                    .map(|_| None)
            }
            ApiRequest::CreateDm(r) => {
                call_proto::<_, super::proto::CreateDmResponse>(&mut session, "create_dm", &r)
                    .map(|_| None)
            }
            ApiRequest::CreateMembership(r) => call_proto::<
                _,
                super::proto::CreateMembershipResponse,
            >(&mut session, "create_membership", &r)
            .map(|_| None),
            ApiRequest::RemoveMemberships(r) => call_proto::<
                _,
                super::proto::RemoveMembershipsResponse,
            >(
                &mut session, "remove_memberships", &r
            )
            .map(|_| None),
            ApiRequest::UpdateGroup(r) => {
                call_proto::<_, super::proto::UpdateGroupResponse>(&mut session, "update_group", &r)
                    .map(|_| None)
            }
            ApiRequest::HideGroup(r) => {
                call_proto::<_, super::proto::HideGroupResponse>(&mut session, "hide_group", &r)
                    .map(|_| None)
            }
            ApiRequest::ListMembers(r) => {
                call_proto::<_, super::proto::ListMembersResponse>(&mut session, "list_members", &r)
                    .map(|_| None)
            }
            ApiRequest::CreateTopic(r) => {
                call_proto::<_, super::proto::CreateTopicResponse>(&mut session, "create_topic", &r)
                    .map(|_| None)
            }
            ApiRequest::SetDndDuration(r) => call_proto::<_, super::proto::SetDndDurationResponse>(
                &mut session,
                "set_dnd_duration",
                &r,
            )
            .map(|_| None),
            ApiRequest::SetCustomStatus(r) => {
                call_proto::<_, super::proto::SetCustomStatusResponse>(
                    &mut session,
                    "set_custom_status",
                    &r,
                )
                .map(|_| None)
            }
            ApiRequest::BlockEntity(r) => {
                call_proto::<_, super::proto::BlockEntityResponse>(&mut session, "block_entity", &r)
                    .map(|_| None)
            }
            ApiRequest::SetPresenceShared(r) => call_proto::<
                _,
                super::proto::SetPresenceSharedResponse,
            >(
                &mut session, "set_presence_shared", &r
            )
            .map(|_| None),
            ApiRequest::CreateCustomEmoji(r) => call_proto::<
                _,
                super::proto::CreateCustomEmojiResponse,
            >(
                &mut session, "create_custom_emoji", &r
            )
            .map(|_| None),
            ApiRequest::AutocompleteSlashCommands(r) => {
                call_proto::<_, super::proto::AutocompleteSlashCommandsResponse>(
                    &mut session,
                    "autocomplete_slash_commands",
                    &r,
                )
                .map(|_| None)
            }
            ApiRequest::CreateVideoCall(r) => {
                call_proto::<_, super::proto::CreateVideoCallResponse>(
                    &mut session,
                    "create_video_call",
                    &r,
                )
                .map(|_| None)
            }
        };

        match result {
            Ok(Some(event)) => {
                let _ = inbound_tx.send(event);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("API call failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_auth_detection() {
        assert!(ApiError::AuthExpired.is_auth_expired());
        assert!(ApiError::HttpStatus {
            status: 401,
            body: "unauthorized".into()
        }
        .is_auth_expired());
        assert!(!ApiError::HttpStatus {
            status: 500,
            body: "error".into()
        }
        .is_auth_expired());
    }
}
