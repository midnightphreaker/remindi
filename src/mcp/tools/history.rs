//! `remindi_history` handler.

use serde_json::Value;

use crate::{
    mcp::{
        McpServer,
        schemas::{EventType, HistoryInput},
        views::{CompletionEvidenceView, EventView},
    },
    remindi::HistoryRequest,
};

use super::{HandlerError, HistoryData, parse, request_id, structured};

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
    let events = page
        .items
        .into_iter()
        .map(EventView::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| HandlerError::Serialization)?;
    let completion_evidence = page
        .evidence
        .map(CompletionEvidenceView::try_from)
        .transpose()
        .map_err(|_| HandlerError::Serialization)?
        .into_iter()
        .collect();
    structured(
        &request_id(actor),
        HistoryData {
            events,
            completion_evidence,
            next_cursor: page.next_cursor,
        },
    )
}
