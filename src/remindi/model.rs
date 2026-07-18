use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use time::{
    OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339, macros::format_description,
};
use uuid::Uuid;

/// A domain validation or transition failure safe to expose at an API boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("timestamp must include Z or an explicit UTC offset")]
    TimestampOffsetRequired,
    #[error("timestamp is not valid RFC 3339")]
    InvalidTimestamp,
    #[error("timestamp could not be formatted")]
    TimestampFormatting,
    #[error("terminal Remindi state is irreversible")]
    TerminalState,
    #[error("snooze requires a due or overdue Remindi item")]
    SnoozeRequiresReadyState,
    #[error("snooze deadline must be in the future")]
    SnoozeMustBeFuture,
    #[error("a non-empty reason is required")]
    ReasonRequired,
    #[error("completion evidence is required")]
    CompletionEvidenceRequired,
    #[error("recurrence interval must be between 60 and 31536000 seconds")]
    InvalidRecurrenceInterval,
    #[error("maximum occurrences must be between 1 and 1000000")]
    InvalidMaxOccurrences,
    #[error("the final recurrence must be completed or cancelled")]
    FinalOccurrence,
    #[error("catch-up limit must be at least one")]
    InvalidCatchUpLimit,
    #[error("evidence summary must be meaningful and at most 4096 characters")]
    EmptyEvidenceAssertion,
    #[error("evidence requires a stable URI or SHA-256 content hash")]
    StableEvidenceReferenceRequired,
    #[error("evidence reference URI is invalid")]
    InvalidEvidenceReference,
    #[error("evidence reference must not contain credentials")]
    EvidenceReferenceContainsCredentials,
    #[error("content hash must use SHA-256")]
    InvalidContentHash,
    #[error("evidence observation timestamp is too far in the future")]
    EvidenceObservedInFuture,
    #[error("evidence metadata must be an object")]
    EvidenceMetadataMustBeObject,
    #[error("an adapter trigger result is not completion evidence")]
    AdapterResultIsNotCompletionEvidence,
    #[error("condition parameters must be an object")]
    ConditionParametersMustBeObject,
    #[error("elapsed trigger duration must be between 1 and 31536000 seconds")]
    InvalidElapsedDuration,
    #[error("interval trigger duration must be between 60 and 31536000 seconds")]
    InvalidTriggerInterval,
    #[error("condition adapter name is invalid")]
    InvalidConditionAdapter,
    #[error("goal identifier must be between 1 and 512 characters")]
    InvalidGoalId,
    #[error("condition polling interval must be between 30 and 86400 seconds")]
    InvalidConditionPollInterval,
    #[error("recurrence is allowed only for an interval trigger")]
    RecurrenceRequiresIntervalTrigger,
    #[error("recurrence interval must match the interval trigger")]
    RecurrenceIntervalMismatch,
}

/// Parses offset-bearing RFC 3339 input and normalizes it to UTC.
pub fn parse_timestamp(value: &str) -> Result<OffsetDateTime, DomainError> {
    if !has_explicit_offset(value) {
        return Err(DomainError::TimestampOffsetRequired);
    }
    OffsetDateTime::parse(value, &Rfc3339)
        .map(|timestamp| timestamp.to_offset(UtcOffset::UTC))
        .map_err(|_| DomainError::InvalidTimestamp)
}

/// Formats an instant as canonical UTC RFC 3339 with millisecond precision.
pub fn canonical_timestamp(value: OffsetDateTime) -> Result<String, DomainError> {
    const FORMAT: &[time::format_description::FormatItem<'static>] =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

    value
        .to_offset(UtcOffset::UTC)
        .format(FORMAT)
        .map_err(|_| DomainError::TimestampFormatting)
}

fn has_explicit_offset(value: &str) -> bool {
    value.ends_with('Z')
        || value
            .get(value.len().saturating_sub(6)..)
            .is_some_and(|tail| {
                matches!(tail.as_bytes().first(), Some(b'+' | b'-'))
                    && tail.as_bytes().get(3) == Some(&b':')
            })
}

macro_rules! string_enum {
    ($(#[$meta:meta])* $visibility:vis enum $name:ident { $($variant:ident),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
        #[serde(rename_all = "snake_case")]
        $visibility enum $name {
            $($variant),+
        }
    };
}

string_enum!(
    /// Persistent lifecycle state.
    pub enum RemindiState {
        Scheduled,
        Due,
        Overdue,
        Snoozed,
        Completed,
        Cancelled,
    }
);

impl RemindiState {
    /// Whether this state permits completion or cancellation.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Scheduled | Self::Due | Self::Overdue | Self::Snoozed
        )
    }

    /// Whether this state cannot transition further in version 1.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

string_enum!(
    /// Priority used for ready-item ordering.
    pub enum Priority {
        Low,
        Normal,
        High,
        Critical,
    }
);

string_enum!(
    /// Why an item is returned by a readiness check.
    pub enum Readiness {
        Due,
        Overdue,
        ManualVerification,
    }
);

/// Fixed recurrence missed-occurrence policy.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissedPolicy {
    #[default]
    Coalesce,
    CatchUp,
    Skip,
}

string_enum!(
    /// Explicit disposition of a recurring occurrence.
    pub enum OccurrenceDisposition {
        Acknowledged,
        Skipped,
    }
);

string_enum!(
    /// Completion evidence category.
    pub enum EvidenceType {
        Observation,
        TestResult,
        Artifact,
        LogReference,
        ChangeReference,
        UserConfirmation,
        ExternalReference,
    }
);

string_enum!(
    /// Immutable Remindi audit event category.
    pub enum EventType {
        Created,
        Checked,
        BecameDue,
        BecameOverdue,
        ConditionEvaluated,
        OccurrenceAdvanced,
        Snoozed,
        Updated,
        Completed,
        Cancelled,
        DeliveryAttempted,
        DeliverySucceeded,
        DeliveryFailed,
    }
);

string_enum!(
    /// Authenticated or internal audit actor category.
    pub enum ActorType {
        User,
        Agent,
        Scheduler,
        System,
    }
);

string_enum!(
    /// Supported association category.
    pub enum LinkType {
        Goal,
        Memory,
        Issue,
        Url,
        Artifact,
    }
);

string_enum!(
    /// Lifecycle point supplied by an agent check.
    pub enum LifecycleEvent {
        TaskStart,
        Checkpoint,
        Continuation,
        FinalReview,
    }
);

string_enum!(
    /// Last known condition-adapter result.
    pub enum ConditionStatus {
        Satisfied,
        Unsatisfied,
        Unknown,
        Error,
    }
);

/// One of the seven version-1 trigger classes.
#[derive(Clone, Debug, PartialEq)]
pub enum Trigger {
    AtTime {
        at: OffsetDateTime,
    },
    AfterElapsed {
        after_seconds: u64,
    },
    Interval {
        first_at: OffsetDateTime,
        every_seconds: u64,
    },
    NextSession,
    NextContinuation,
    GoalActive {
        goal_id: String,
    },
    Condition {
        adapter: String,
        parameters: Value,
        poll_interval_seconds: Option<u64>,
        manual_check_at: Option<OffsetDateTime>,
    },
}

impl Trigger {
    /// Validates trigger-specific source bounds before persistence.
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::AtTime { .. } | Self::NextSession | Self::NextContinuation => Ok(()),
            Self::AfterElapsed { after_seconds } => {
                if (1..=31_536_000).contains(after_seconds) {
                    Ok(())
                } else {
                    Err(DomainError::InvalidElapsedDuration)
                }
            }
            Self::Interval { every_seconds, .. } => {
                if (60..=31_536_000).contains(every_seconds) {
                    Ok(())
                } else {
                    Err(DomainError::InvalidTriggerInterval)
                }
            }
            Self::GoalActive { goal_id } => {
                if (1..=512).contains(&goal_id.chars().count()) {
                    Ok(())
                } else {
                    Err(DomainError::InvalidGoalId)
                }
            }
            Self::Condition {
                adapter,
                parameters,
                poll_interval_seconds,
                ..
            } => {
                if !valid_adapter_name(adapter) {
                    return Err(DomainError::InvalidConditionAdapter);
                }
                if !parameters.is_object() {
                    return Err(DomainError::ConditionParametersMustBeObject);
                }
                if poll_interval_seconds.is_some_and(|seconds| !(30..=86_400).contains(&seconds)) {
                    return Err(DomainError::InvalidConditionPollInterval);
                }
                Ok(())
            }
        }
    }
}

fn valid_adapter_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && name.len() <= 64
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Fixed-interval recurrence constraints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecurrenceSpec {
    pub every_seconds: u64,
    pub missed_policy: MissedPolicy,
    pub max_occurrences: Option<u64>,
    pub end_at: Option<OffsetDateTime>,
}

impl RecurrenceSpec {
    /// A convenient valid hourly recurrence.
    #[must_use]
    pub const fn every_hour() -> Self {
        Self {
            every_seconds: 3600,
            missed_policy: MissedPolicy::Coalesce,
            max_occurrences: None,
            end_at: None,
        }
    }
}

/// The domain representation of one persisted Remindi item.
#[derive(Clone, Debug, PartialEq)]
pub struct Remindi {
    pub id: Uuid,
    pub owner_id: String,
    pub project_id: String,
    pub task_id: Option<String>,
    pub message: String,
    pub instructions: Option<String>,
    pub state: RemindiState,
    pub priority: Priority,
    pub trigger: Trigger,
    pub recurrence: Option<RecurrenceSpec>,
    pub next_fire_at: Option<OffsetDateTime>,
    pub next_evaluation_at: Option<OffsetDateTime>,
    pub original_next_fire_at: Option<OffsetDateTime>,
    pub due_since: Option<OffsetDateTime>,
    pub snooze_until: Option<OffsetDateTime>,
    pub snoozed_from_state: Option<RemindiState>,
    pub overdue_after_seconds: u64,
    pub occurrence_no: u64,
    pub source_session_id: Option<String>,
    pub source_task_lineage_id: Option<String>,
    pub last_checked_at: Option<OffsetDateTime>,
    pub last_condition_status: Option<ConditionStatus>,
    pub last_condition_detail: Option<String>,
    pub snooze_count: u64,
    pub version: u64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
    pub cancelled_at: Option<OffsetDateTime>,
}

/// One association attached to a Remindi item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemindiLink {
    pub link_type: LinkType,
    pub value: String,
    pub created_at: OffsetDateTime,
}

/// One append-only lifecycle event.
#[derive(Clone, Debug, PartialEq)]
pub struct RemindiEvent {
    pub sequence: Option<i64>,
    pub event_id: Uuid,
    pub remindi_id: Uuid,
    pub event_type: EventType,
    pub actor_type: ActorType,
    pub actor_id: String,
    pub request_id: Option<String>,
    pub occurred_at: OffsetDateTime,
    pub prior_version: Option<u64>,
    pub new_version: Option<u64>,
    pub details: Value,
}
