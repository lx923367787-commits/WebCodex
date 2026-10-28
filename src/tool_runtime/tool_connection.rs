use super::{ToolResult, ToolRuntime};
use crate::auth::AuthContext;
use crate::runner_http::{requested_by_from_auth, runner_access_from_auth, RunnerFeature};
use serde_json::json;
use std::time::Duration;
use webcodex_core::tool_connection::{
    ToolConnectionRequest, ToolConnectionResponse, TOOL_CONNECTION_MAX_BYTES,
};

impl ToolRuntime {
    pub(crate) async fn dispatch_tool_connection(
        &self,
        client_id: &str,
        operation: ToolConnectionRequest,
        auth: Option<&AuthContext>,
    ) -> ToolResult {
        if let Err(error) = operation.validate() {
            return ToolResult::err(error);
        }
        let access = runner_access_from_auth(auth);
        let runner = match self
            .runner_registry
            .get_runner_semantic_view_checked_for_auth(client_id, access.as_ref())
            .await
        {
            Ok(runner) => runner,
            Err(_) => return ToolResult::err("Runner is unavailable"),
        };
        if !runner.supports(RunnerFeature::ToolConnections) {
            return ToolResult::err(
                "This Runner needs an update before it can save tool connections",
            );
        }
        let instance = runner.view.runner_instance_id;
        let (request_id, receiver) = match self
            .runner_registry
            .enqueue_tool_connection(
                client_id,
                &instance,
                operation,
                access.as_ref(),
                requested_by_from_auth(auth),
            )
            .await
        {
            Ok(value) => value,
            Err(error) => return ToolResult::err(error),
        };
        let response = match tokio::time::timeout(Duration::from_secs(125), receiver).await {
            Ok(Ok(response)) => response,
            _ => {
                let dispatched = self
                    .runner_registry
                    .cancel_request_dispatch_state(&request_id)
                    .await;
                return ToolResult::err_with_output(
                    "Connection management response was lost; inspect status before repeating",
                    json!({"execution_state":if dispatched == Some(false) {"not_started"} else {"outcome_unknown"}}),
                );
            }
        };
        if response.error.is_some() || response.exit_code != Some(0) {
            return ToolResult::err(
                "Runner could not return connection status; inspect status before repeating",
            );
        }
        let parsed = response
            .stdout
            .as_deref()
            .filter(|v| v.len() <= TOOL_CONNECTION_MAX_BYTES)
            .and_then(|v| serde_json::from_str::<ToolConnectionResponse>(v).ok());
        let Some(parsed) = parsed else {
            return ToolResult::err("Invalid connection response; inspect status before repeating");
        };
        if let Err(error) = self
            .runner_registry
            .replace_mcp_gateway_inventory(
                client_id,
                &instance,
                access.as_ref(),
                parsed.revision,
                parsed.inventory,
            )
            .await
        {
            return ToolResult::err_with_output(
                "Connection state needs refreshing",
                json!({"message":error,"recovery":"status"}),
            );
        }
        let output = json!({"changed":parsed.changed,"message":parsed.message,"connections":parsed.connections});
        if parsed.success {
            ToolResult::ok(output)
        } else {
            ToolResult::err_with_output(parsed.message, output)
        }
    }
}
