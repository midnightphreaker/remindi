//! `remindi_snooze` handler.

use std::time::Duration;

use serde_json::{Value, json};

use crate::{
    mcp::{McpServer, schemas::SnoozeInput},
    remindi::SnoozeRequest,
};

use super::{HandlerError, parse, request_id, structured, timestamp};

pub(crate) async fn handle(
    server: &McpServer,
    actor: &crate::remindi::Actor,
    arguments: Value,
) -> Result<rmcp::model::CallToolResult, HandlerError> {
    let input: SnoozeInput = parse(arguments)?;
    let result = server
        .service()
        .snooze(
            actor,
            SnoozeRequest {
                remindi_id: input.remindi_id,
                expected_version: input.expected_version,
                snooze_until: timestamp(&input.snooze_until)?,
                reason: input.reason,
                idempotency_key: input.idempotency_key,
            },
            Duration::from_secs(31_536_000),
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
