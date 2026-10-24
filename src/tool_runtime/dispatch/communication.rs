//! Communication tool family. Admission is owned by the canonical pipeline.
use crate::auth::AuthContext;
use crate::tool_runtime::{ToolCall, ToolResult, ToolRuntime};
impl ToolRuntime {
    pub(super) async fn dispatch_communication_family(
        &self,
        call: ToolCall,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        match call {
            ToolCall::CreateAgentIdentity {
                handle,
                display_name,
                description,
                specialty_labels,
                idempotency_key,
            } => self.communication.create_agent_identity(
                auth,
                handle,
                display_name,
                description,
                specialty_labels,
                idempotency_key,
            ),

            ToolCall::ListAgentIdentities {
                agent_id,
                offset,
                limit,
            } => self
                .communication
                .list_agent_identities(auth, agent_id, offset, limit),

            ToolCall::UpdateAgentIdentity {
                agent_id,
                expected_profile_revision,
                handle,
                display_name,
                description,
                specialty_labels,
            } => self.communication.update_agent_identity(
                auth,
                agent_id,
                expected_profile_revision,
                handle,
                display_name,
                description,
                specialty_labels,
            ),

            ToolCall::AttachAgentEndpoint {
                agent_id,
                host,
                client_attachment_id,
                idempotency_key,
            } => self.communication.attach_agent_endpoint(
                auth,
                agent_id,
                host,
                client_attachment_id,
                idempotency_key,
            ),

            ToolCall::DetachAgentEndpoint { endpoint_id } => {
                self.communication.detach_agent_endpoint(auth, endpoint_id)
            }

            ToolCall::CreateConversation {
                title,
                agent_ids,
                idempotency_key,
            } => self
                .communication
                .create_conversation(auth, title, agent_ids, idempotency_key),

            ToolCall::ListConversations {
                agent_id,
                endpoint_id,
                expected_controller_generation,
                offset,
                limit,
            } => self.communication.list_conversations(
                auth,
                agent_id,
                endpoint_id,
                expected_controller_generation,
                offset,
                limit,
            ),

            ToolCall::ReadConversation {
                conversation_id,
                agent_id,
                endpoint_id,
                expected_controller_generation,
                after_seq,
                limit,
            } => self.communication.read_conversation(
                auth,
                conversation_id,
                agent_id,
                endpoint_id,
                expected_controller_generation,
                after_seq,
                limit,
            ),

            ToolCall::PostConversationMessage {
                conversation_id,
                body,
                author_agent_id,
                endpoint_id,
                expected_controller_generation,
                recipient_agent_ids,
                reply_to,
                idempotency_key,
                wake_reply_id,
                reply_operation_index,
            } => self.communication.post_conversation_message(
                auth,
                conversation_id,
                body,
                author_agent_id,
                endpoint_id,
                expected_controller_generation,
                recipient_agent_ids,
                reply_to,
                idempotency_key,
                wake_reply_id,
                reply_operation_index,
            ),

            ToolCall::ListAgentInbox {
                agent_id,
                endpoint_id,
                expected_controller_generation,
                after_delivery_order,
                limit,
            } => self.communication.list_agent_inbox(
                auth,
                agent_id,
                endpoint_id,
                expected_controller_generation,
                after_delivery_order,
                limit,
            ),

            ToolCall::ConsumeAgentDeliveries {
                agent_id,
                endpoint_id,
                expected_controller_generation,
                delivery_ids,
            } => self.communication.consume_agent_deliveries(
                auth,
                agent_id,
                endpoint_id,
                expected_controller_generation,
                delivery_ids,
            ),

            ToolCall::BootstrapAgentConversation {
                agent_id,
                endpoint_id,
                expected_controller_generation,
                conversation_id,
                wake_id,
                activation_idempotency_key,
            } => self.communication.bootstrap_agent_conversation(
                auth,
                agent_id,
                endpoint_id,
                expected_controller_generation,
                conversation_id,
                wake_id,
                activation_idempotency_key,
            ),

            ToolCall::ConsumeAgentWake {
                agent_id,
                endpoint_id,
                expected_controller_generation,
                wake_id,
                consume_token,
            } => self.communication.consume_agent_wake(
                auth,
                agent_id,
                endpoint_id,
                expected_controller_generation,
                wake_id,
                consume_token,
            ),

            _ => unreachable!("typed call routed to the wrong communication handler"),
        }
    }
}
