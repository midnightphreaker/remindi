//! `remindi_add` handler.

use serde_json::{Value, json};

use crate::{
    mcp::{McpServer, schemas::AddInput},
    remindi::AddRequest,
};

use super::{HandlerError, link, parse, priority, recurrence, request_id, structured, trigger};

pub(crate) async fn handle(
    server: &McpServer,
    actor: &crate::remindi::Actor,
    arguments: Value,
) -> Result<rmcp::model::CallToolResult, HandlerError> {
    let input: AddInput = parse(arguments)?;
    input
        .validate_semantics()
        .map_err(|_| HandlerError::Validation)?;
    let request = AddRequest {
        project_id: input.project_id,
        task_id: input.task_id,
        message: input.message,
        instructions: input.instructions,
        priority: priority(input.priority),
        trigger: trigger(input.trigger)?,
        recurrence: input.recurrence.map(recurrence).transpose()?,
        overdue_after_seconds: input.overdue_after_seconds,
        links: input.links.into_iter().map(link).collect(),
        session_id: input.session_id,
        task_lineage_id: input.task_lineage_id,
        idempotency_key: input.idempotency_key,
    };
    let result = server.service().add(actor, request).await?;
    structured(
        &request_id(actor),
        json!({"remindi": {
            "id": result.remindi.id,
            "state": result.remindi.state,
            "version": result.remindi.version
        }}),
    )
}
