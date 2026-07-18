//! `remindi_check` handler.

use serde_json::{Value, json};

use crate::{
    mcp::{
        McpServer,
        schemas::{CheckInput, LifecycleEvent},
    },
    remindi::CheckRequest,
};

use super::{HandlerError, parse, request_id, structured};

pub(crate) async fn handle(
    server: &McpServer,
    actor: &crate::remindi::Actor,
    arguments: Value,
) -> Result<rmcp::model::CallToolResult, HandlerError> {
    let input: CheckInput = parse(arguments)?;
    let lifecycle_event = match input.lifecycle_event {
        LifecycleEvent::TaskStart => crate::remindi::LifecycleEvent::TaskStart,
        LifecycleEvent::Checkpoint => crate::remindi::LifecycleEvent::Checkpoint,
        LifecycleEvent::Continuation => crate::remindi::LifecycleEvent::Continuation,
        LifecycleEvent::FinalReview => crate::remindi::LifecycleEvent::FinalReview,
    };
    let request = CheckRequest {
        project_id: input.project_id,
        task_id: input.task_id,
        session_id: input.session_id,
        task_lineage_id: input.task_lineage_id,
        lifecycle_event,
        active_goal_ids: input.active_goal_ids,
        include_scheduled: input.include_scheduled,
        limit: usize::from(input.limit),
        cursor: input.cursor,
    };
    let result = server.service().check(actor, request).await?;
    let checked_at = crate::remindi::canonical_timestamp(result.checked_at)
        .map_err(|_| HandlerError::Serialization)?;
    let items = result
        .items
        .into_iter()
        .map(|item| {
            json!({
                "remindi_id": item.remindi.id,
                "readiness": item.readiness,
                "message": item.remindi.message,
                "occurrence_no": item.remindi.occurrence_no,
                "version": item.remindi.version
            })
        })
        .collect::<Vec<_>>();
    structured(
        &request_id(actor),
        json!({
            "checked_at": checked_at,
            "items": items,
            "next_cursor": result.next_cursor
        }),
    )
}
