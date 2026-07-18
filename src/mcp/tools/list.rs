//! `remindi_list` handler.

use serde_json::Value;

use crate::{
    mcp::{
        McpServer,
        schemas::{ListInput, RemindiState, TriggerType},
        views::RemindiView,
    },
    remindi::ListRequest,
};

use super::{HandlerError, PageData, parse, request_id, structured};

pub(crate) async fn handle(
    server: &McpServer,
    actor: &crate::remindi::Actor,
    arguments: Value,
) -> Result<rmcp::model::CallToolResult, HandlerError> {
    let input: ListInput = parse(arguments)?;
    let states = input
        .states
        .into_iter()
        .map(|state| match state {
            RemindiState::Scheduled => crate::remindi::RemindiState::Scheduled,
            RemindiState::Due => crate::remindi::RemindiState::Due,
            RemindiState::Overdue => crate::remindi::RemindiState::Overdue,
            RemindiState::Snoozed => crate::remindi::RemindiState::Snoozed,
            RemindiState::Completed => crate::remindi::RemindiState::Completed,
            RemindiState::Cancelled => crate::remindi::RemindiState::Cancelled,
        })
        .collect();
    let trigger_types = input
        .trigger_types
        .into_iter()
        .map(|trigger| match trigger {
            TriggerType::AtTime => "at_time",
            TriggerType::AfterElapsed => "after_elapsed",
            TriggerType::Interval => "interval",
            TriggerType::NextSession => "next_session",
            TriggerType::NextContinuation => "next_continuation",
            TriggerType::GoalActive => "goal_active",
            TriggerType::Condition => "condition",
        })
        .map(str::to_owned)
        .collect();
    let request = ListRequest {
        project_id: input.project_id,
        task_id: input.task_id,
        states,
        trigger_types,
        linked_goal_id: input.linked_goal_id,
        linked_memory_hash: input.linked_memory_hash,
        limit: usize::from(input.limit),
        cursor: input.cursor,
    };
    let page = server.service().list(actor, request).await?;
    let items = page
        .items
        .into_iter()
        .map(RemindiView::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| HandlerError::Serialization)?;
    structured(
        &request_id(actor),
        PageData {
            items,
            next_cursor: page.next_cursor,
        },
    )
}
