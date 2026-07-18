//! `remindi_complete` handler.

use std::time::Duration;

use serde_json::{Value, json};

use crate::{
    mcp::{
        McpServer,
        schemas::{CompleteInput, EvidenceType},
    },
    remindi::{CompleteRequest, EvidenceInput, EvidenceSource},
};

use super::{HandlerError, parse, request_id, structured, timestamp};

pub(crate) async fn handle(
    server: &McpServer,
    actor: &crate::remindi::Actor,
    arguments: Value,
) -> Result<rmcp::model::CallToolResult, HandlerError> {
    let input: CompleteInput = parse(arguments)?;
    let evidence_type = match input.evidence.evidence_type {
        EvidenceType::Observation => crate::remindi::EvidenceType::Observation,
        EvidenceType::TestResult => crate::remindi::EvidenceType::TestResult,
        EvidenceType::Artifact => crate::remindi::EvidenceType::Artifact,
        EvidenceType::LogReference => crate::remindi::EvidenceType::LogReference,
        EvidenceType::ChangeReference => crate::remindi::EvidenceType::ChangeReference,
        EvidenceType::UserConfirmation => crate::remindi::EvidenceType::UserConfirmation,
        EvidenceType::ExternalReference => crate::remindi::EvidenceType::ExternalReference,
    };
    let request = CompleteRequest {
        remindi_id: input.remindi_id,
        expected_version: input.expected_version,
        evidence: EvidenceInput {
            evidence_type,
            summary: input.evidence.summary,
            reference_uri: input.evidence.reference_uri,
            content_hash: input.evidence.content_hash,
            observed_at: timestamp(&input.evidence.observed_at)?,
            metadata: input.evidence.metadata,
            source: EvidenceSource::AuthenticatedActor,
        },
        completion_note: input.completion_note,
        idempotency_key: input.idempotency_key,
    };
    let result = server
        .service()
        .complete(actor, request, Duration::from_secs(300))
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
