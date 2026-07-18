//! Core Remindi values and deterministic lifecycle rules.

mod evidence;
mod model;
mod recurrence;
mod repository;
mod service;
mod state_machine;

pub use evidence::{EvidenceInput, EvidenceSource, ValidatedEvidence};
pub use model::{
    ActorType, ConditionStatus, DomainError, EventType, EvidenceType, LifecycleEvent, LinkType,
    MissedPolicy, OccurrenceDisposition, Priority, Readiness, RecurrenceSpec, Remindi,
    RemindiEvent, RemindiLink, RemindiState, Trigger, canonical_timestamp, parse_timestamp,
};
pub use recurrence::RecurrenceAdvance;
pub use repository::{CompletionEvidence, HistoryPage, Page};
pub use service::{
    Actor, AddRequest, CancelRequest, CheckRequest, CheckResult, CheckedItem, CompleteRequest,
    HistoryRequest, LinkInput, ListRequest, MutationResult, RemindiService, ServiceError,
    SnoozeRequest, UpdateRequest,
};
