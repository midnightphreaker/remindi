//! Core Remindi values and deterministic lifecycle rules.

mod evidence;
mod model;
mod recurrence;
mod state_machine;

pub use evidence::{EvidenceInput, EvidenceSource, ValidatedEvidence};
pub use model::{
    ActorType, ConditionStatus, DomainError, EventType, EvidenceType, LifecycleEvent, LinkType,
    MissedPolicy, OccurrenceDisposition, Priority, Readiness, RecurrenceSpec, Remindi,
    RemindiEvent, RemindiLink, RemindiState, Trigger, canonical_timestamp, parse_timestamp,
};
pub use recurrence::RecurrenceAdvance;
