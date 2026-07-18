//! `remindi_update` handler.

use serde_json::{Value, json};

use crate::{
    mcp::{
        McpServer,
        schemas::{OccurrenceDisposition, Patch, UpdateInput},
    },
    remindi::UpdateRequest,
};

use super::{HandlerError, link, parse, priority, recurrence, request_id, structured, trigger};

pub(crate) async fn handle(
    server: &McpServer,
    actor: &crate::remindi::Actor,
    arguments: Value,
) -> Result<rmcp::model::CallToolResult, HandlerError> {
    let input: UpdateInput = parse(arguments)?;
    input
        .validate_semantics()
        .map_err(|_| HandlerError::Validation)?;
    let instructions = match input.instructions {
        Patch::Unset => None,
        Patch::Null => Some(None),
        Patch::Value(value) => Some(Some(value)),
    };
    let recurrence = match input.recurrence {
        Patch::Unset => None,
        Patch::Null => Some(None),
        Patch::Value(value) => Some(Some(recurrence(value)?)),
    };
    let request = UpdateRequest {
        remindi_id: input.remindi_id,
        expected_version: input.expected_version,
        message: input.message,
        instructions,
        priority: input.priority.map(priority),
        trigger: input.trigger.map(trigger).transpose()?,
        recurrence,
        overdue_after_seconds: input.overdue_after_seconds,
        links: input
            .links
            .map(|links| links.into_iter().map(link).collect()),
        occurrence_disposition: input.occurrence_disposition.map(|value| match value {
            OccurrenceDisposition::Acknowledged => {
                crate::remindi::OccurrenceDisposition::Acknowledged
            }
            OccurrenceDisposition::Skipped => crate::remindi::OccurrenceDisposition::Skipped,
        }),
        reason: input.reason,
        idempotency_key: input.idempotency_key,
    };
    let result = server.service().update(actor, request).await?;
    structured(
        &request_id(actor),
        json!({"remindi": {
            "id": result.remindi.id,
            "state": result.remindi.state,
            "version": result.remindi.version
        }}),
    )
}
