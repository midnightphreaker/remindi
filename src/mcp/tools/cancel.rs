//! `remindi_cancel` handler.

use serde_json::{Value, json};

use crate::{
    mcp::{McpServer, schemas::CancelInput},
    remindi::CancelRequest,
};

use super::{HandlerError, parse, request_id, structured};

pub(crate) async fn handle(
    server: &McpServer,
    actor: &crate::remindi::Actor,
    arguments: Value,
) -> Result<rmcp::model::CallToolResult, HandlerError> {
    let input: CancelInput = parse(arguments)?;
    let result = server
        .service()
        .cancel(
            actor,
            CancelRequest {
                remindi_id: input.remindi_id,
                expected_version: input.expected_version,
                reason: input.reason,
                idempotency_key: input.idempotency_key,
            },
        )
        .await?;
    structured(
        &request_id(actor),
        json!({"remindi": {
            "id": result.remindi.id,
            "state": result.remindi.state,
            "version": result.remindi.version
        }}),
    )
}
