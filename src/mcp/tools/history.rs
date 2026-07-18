//! `remindi_history` handler.

use serde_json::{Value, json};

use crate::{
    mcp::{
        McpServer,
        schemas::{EventType, HistoryInput},
    },
    remindi::HistoryRequest,
};

use super::{HandlerError, parse, request_id, structured};

pub(crate) async fn handle(
    server: &McpServer,
    actor: &crate::remindi::Actor,
    arguments: Value,
) -> Result<rmcp::model::CallToolResult, HandlerError> {
    let input: HistoryInput = parse(arguments)?;
    let event_types = input
        .event_types
        .into_iter()
        .map(|event| match event {
            EventType::Created => crate::remindi::EventType::Created,
            EventType::Checked => crate::remindi::EventType::Checked,
            EventType::BecameDue => crate::remindi::EventType::BecameDue,
            EventType::BecameOverdue => crate::remindi::EventType::BecameOverdue,
            EventType::ConditionEvaluated => crate::remindi::EventType::ConditionEvaluated,
            EventType::OccurrenceAdvanced => crate::remindi::EventType::OccurrenceAdvanced,
            EventType::Snoozed => crate::remindi::EventType::Snoozed,
            EventType::Updated => crate::remindi::EventType::Updated,
            EventType::Completed => crate::remindi::EventType::Completed,
            EventType::Cancelled => crate::remindi::EventType::Cancelled,
            EventType::DeliveryAttempted => crate::remindi::EventType::DeliveryAttempted,
            EventType::DeliverySucceeded => crate::remindi::EventType::DeliverySucceeded,
            EventType::DeliveryFailed => crate::remindi::EventType::DeliveryFailed,
        })
        .collect();
    let request = HistoryRequest {
        remindi_id: input.remindi_id,
        after_sequence: input
            .after_sequence
            .map(i64::try_from)
            .transpose()
            .map_err(|_| HandlerError::Validation)?,
        event_types,
        limit: usize::from(input.limit),
        cursor: input.cursor,
    };
    let page = server.service().history(actor, request).await?;
    structured(
        &request_id(actor),
        json!({
            "events": page.items,
            "completion_evidence": page.evidence.into_iter().collect::<Vec<_>>(),
            "next_cursor": page.next_cursor
        }),
    )
}
