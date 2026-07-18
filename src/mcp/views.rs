use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::remindi::{
    CompletionEvidence, DomainError, EventType, RecurrenceSpec, Remindi, RemindiEvent, Trigger,
    canonical_timestamp, parse_timestamp,
};

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(transparent)]
pub(crate) struct CanonicalTimestamp(#[schemars(extend("format" = "date-time"))] pub(crate) String);

impl TryFrom<OffsetDateTime> for CanonicalTimestamp {
    type Error = DomainError;

    fn try_from(value: OffsetDateTime) -> Result<Self, Self::Error> {
        canonical_timestamp(value).map(Self)
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TriggerView {
    AtTime {
        at: CanonicalTimestamp,
    },
    AfterElapsed {
        after_seconds: u64,
    },
    Interval {
        first_at: CanonicalTimestamp,
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
        manual_check_at: Option<CanonicalTimestamp>,
    },
}

impl TryFrom<Trigger> for TriggerView {
    type Error = DomainError;

    fn try_from(value: Trigger) -> Result<Self, Self::Error> {
        Ok(match value {
            Trigger::AtTime { at } => Self::AtTime { at: at.try_into()? },
            Trigger::AfterElapsed { after_seconds } => Self::AfterElapsed { after_seconds },
            Trigger::Interval {
                first_at,
                every_seconds,
            } => Self::Interval {
                first_at: first_at.try_into()?,
                every_seconds,
            },
            Trigger::NextSession => Self::NextSession,
            Trigger::NextContinuation => Self::NextContinuation,
            Trigger::GoalActive { goal_id } => Self::GoalActive { goal_id },
            Trigger::Condition {
                adapter,
                parameters,
                poll_interval_seconds,
                manual_check_at,
            } => Self::Condition {
                adapter,
                parameters,
                poll_interval_seconds,
                manual_check_at: optional_timestamp(manual_check_at)?,
            },
        })
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub(crate) struct RecurrenceView {
    pub(crate) every_seconds: u64,
    pub(crate) missed_policy: String,
    pub(crate) max_occurrences: Option<u64>,
    pub(crate) end_at: Option<CanonicalTimestamp>,
}

impl TryFrom<RecurrenceSpec> for RecurrenceView {
    type Error = DomainError;

    fn try_from(value: RecurrenceSpec) -> Result<Self, Self::Error> {
        Ok(Self {
            every_seconds: value.every_seconds,
            missed_policy: match value.missed_policy {
                crate::remindi::MissedPolicy::Coalesce => "coalesce",
                crate::remindi::MissedPolicy::CatchUp => "catch_up",
                crate::remindi::MissedPolicy::Skip => "skip",
            }
            .to_owned(),
            max_occurrences: value.max_occurrences,
            end_at: optional_timestamp(value.end_at)?,
        })
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub(crate) struct RemindiView {
    pub(crate) id: Uuid,
    pub(crate) project_id: String,
    pub(crate) task_id: Option<String>,
    pub(crate) message: String,
    pub(crate) instructions: Option<String>,
    pub(crate) state: String,
    pub(crate) priority: String,
    pub(crate) trigger: TriggerView,
    pub(crate) recurrence: Option<RecurrenceView>,
    pub(crate) next_fire_at: Option<CanonicalTimestamp>,
    pub(crate) next_evaluation_at: Option<CanonicalTimestamp>,
    pub(crate) original_next_fire_at: Option<CanonicalTimestamp>,
    pub(crate) due_since: Option<CanonicalTimestamp>,
    pub(crate) snooze_until: Option<CanonicalTimestamp>,
    pub(crate) snoozed_from_state: Option<String>,
    pub(crate) overdue_after_seconds: u64,
    pub(crate) occurrence_no: u64,
    pub(crate) source_session_id: Option<String>,
    pub(crate) source_task_lineage_id: Option<String>,
    pub(crate) last_checked_at: Option<CanonicalTimestamp>,
    pub(crate) last_condition_status: Option<String>,
    pub(crate) last_condition_detail: Option<String>,
    pub(crate) snooze_count: u64,
    pub(crate) version: u64,
    pub(crate) created_at: CanonicalTimestamp,
    pub(crate) updated_at: CanonicalTimestamp,
    pub(crate) completed_at: Option<CanonicalTimestamp>,
    pub(crate) cancelled_at: Option<CanonicalTimestamp>,
}

impl TryFrom<Remindi> for RemindiView {
    type Error = DomainError;

    fn try_from(value: Remindi) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            project_id: value.project_id,
            task_id: value.task_id,
            message: value.message,
            instructions: value.instructions,
            state: value.state.to_string(),
            priority: value.priority.to_string(),
            trigger: value.trigger.try_into()?,
            recurrence: value.recurrence.map(TryInto::try_into).transpose()?,
            next_fire_at: optional_timestamp(value.next_fire_at)?,
            next_evaluation_at: optional_timestamp(value.next_evaluation_at)?,
            original_next_fire_at: optional_timestamp(value.original_next_fire_at)?,
            due_since: optional_timestamp(value.due_since)?,
            snooze_until: optional_timestamp(value.snooze_until)?,
            snoozed_from_state: value.snoozed_from_state.map(|state| state.to_string()),
            overdue_after_seconds: value.overdue_after_seconds,
            occurrence_no: value.occurrence_no,
            source_session_id: value.source_session_id,
            source_task_lineage_id: value.source_task_lineage_id,
            last_checked_at: optional_timestamp(value.last_checked_at)?,
            last_condition_status: value.last_condition_status.map(|status| status.to_string()),
            last_condition_detail: value.last_condition_detail,
            snooze_count: value.snooze_count,
            version: value.version,
            created_at: value.created_at.try_into()?,
            updated_at: value.updated_at.try_into()?,
            completed_at: optional_timestamp(value.completed_at)?,
            cancelled_at: optional_timestamp(value.cancelled_at)?,
        })
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub(crate) struct EventView {
    pub(crate) sequence: Option<i64>,
    pub(crate) event_id: Uuid,
    pub(crate) remindi_id: Uuid,
    pub(crate) event_type: String,
    pub(crate) actor_type: String,
    pub(crate) actor_id: String,
    pub(crate) request_id: Option<String>,
    pub(crate) occurred_at: CanonicalTimestamp,
    pub(crate) prior_version: Option<u64>,
    pub(crate) new_version: Option<u64>,
    pub(crate) details: Value,
}

impl TryFrom<RemindiEvent> for EventView {
    type Error = DomainError;

    fn try_from(value: RemindiEvent) -> Result<Self, Self::Error> {
        Ok(Self {
            sequence: value.sequence,
            event_id: value.event_id,
            remindi_id: value.remindi_id,
            event_type: value.event_type.to_string(),
            actor_type: value.actor_type.to_string(),
            actor_id: value.actor_id,
            request_id: value.request_id,
            occurred_at: value.occurred_at.try_into()?,
            prior_version: value.prior_version,
            new_version: value.new_version,
            details: normalize_event_details(value.event_type, value.details)?,
        })
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub(crate) struct CompletionEvidenceView {
    pub(crate) id: Uuid,
    pub(crate) remindi_id: Uuid,
    pub(crate) evidence_type: String,
    pub(crate) summary: String,
    pub(crate) reference_uri: Option<String>,
    pub(crate) content_hash: Option<String>,
    pub(crate) observed_at: CanonicalTimestamp,
    pub(crate) recorded_at: CanonicalTimestamp,
    pub(crate) recorded_by: String,
    pub(crate) metadata: Option<Value>,
}

impl TryFrom<CompletionEvidence> for CompletionEvidenceView {
    type Error = DomainError;

    fn try_from(value: CompletionEvidence) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            remindi_id: value.remindi_id,
            evidence_type: value.evidence_type.to_string(),
            summary: value.summary,
            reference_uri: value.reference_uri,
            content_hash: value.content_hash,
            observed_at: value.observed_at.try_into()?,
            recorded_at: value.recorded_at.try_into()?,
            recorded_by: value.recorded_by,
            metadata: value.metadata,
        })
    }
}

fn optional_timestamp(
    value: Option<OffsetDateTime>,
) -> Result<Option<CanonicalTimestamp>, DomainError> {
    value.map(TryInto::try_into).transpose()
}

fn normalize_event_details(
    event_type: EventType,
    mut details: Value,
) -> Result<Value, DomainError> {
    match event_type {
        EventType::Snoozed => {
            normalize_object_timestamp(&mut details, "prior_next_fire_at")?;
            normalize_object_timestamp(&mut details, "snooze_until")?;
        }
        EventType::OccurrenceAdvanced => {
            normalize_object_timestamp(&mut details, "previous_schedule")?;
            normalize_object_timestamp(&mut details, "next_schedule")?;
        }
        EventType::Updated => {
            normalize_nested_trigger_summary(&mut details, "before_trigger")?;
            normalize_nested_trigger_summary(&mut details, "after_trigger")?;
        }
        _ => {
            normalize_object_timestamp(&mut details, "next_fire_at")?;
            normalize_object_timestamp(&mut details, "next_evaluation_at")?;
        }
    }
    Ok(details)
}

fn normalize_nested_trigger_summary(details: &mut Value, key: &str) -> Result<(), DomainError> {
    let Some(summary) = details.get_mut(key) else {
        return Ok(());
    };
    normalize_object_timestamp(summary, "next_fire_at")?;
    normalize_object_timestamp(summary, "next_evaluation_at")
}

fn normalize_object_timestamp(object: &mut Value, key: &str) -> Result<(), DomainError> {
    let Some(value) = object.get_mut(key) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let instant = match value {
        Value::String(timestamp) => parse_timestamp(timestamp)?,
        Value::Array(_) => serde_json::from_value::<OffsetDateTime>(value.take())
            .map_err(|_| DomainError::InvalidPersistedValue)?,
        _ => return Err(DomainError::InvalidPersistedValue),
    };
    *value = Value::String(canonical_timestamp(instant)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::macros::datetime;

    use super::*;
    use crate::remindi::{
        ActorType, ConditionStatus, EventType, MissedPolicy, Priority, RemindiState,
    };

    fn event(event_type: EventType, details: Value) -> RemindiEvent {
        RemindiEvent {
            sequence: Some(1),
            event_id: Uuid::nil(),
            remindi_id: Uuid::nil(),
            event_type,
            actor_type: ActorType::Agent,
            actor_id: "agent-a".into(),
            request_id: Some("request-a".into()),
            occurred_at: datetime!(2026-07-19 06:00:00.123456 UTC),
            prior_version: Some(1),
            new_version: Some(2),
            details,
        }
    }

    #[test]
    fn event_view_normalizes_every_owned_legacy_timestamp_path() {
        let legacy = serde_json::to_value(datetime!(2026-07-19 06:00:00.123456 UTC))
            .expect("legacy timestamp");
        let snoozed = EventView::try_from(event(
            EventType::Snoozed,
            json!({
                "prior_next_fire_at": legacy,
                "snooze_until": "2026-07-19T16:00:00.123456+10:00"
            }),
        ))
        .expect("snoozed view");
        assert_eq!(
            snoozed.details,
            json!({
                "prior_next_fire_at": "2026-07-19T06:00:00.123Z",
                "snooze_until": "2026-07-19T06:00:00.123Z"
            })
        );

        let legacy = serde_json::to_value(datetime!(2026-07-19 06:00:00.123456 UTC))
            .expect("legacy timestamp");
        let advanced = EventView::try_from(event(
            EventType::OccurrenceAdvanced,
            json!({"previous_schedule": legacy, "next_schedule": null}),
        ))
        .expect("advanced view");
        assert_eq!(
            advanced.details,
            json!({"previous_schedule": "2026-07-19T06:00:00.123Z", "next_schedule": null})
        );

        let legacy = serde_json::to_value(datetime!(2026-07-19 06:00:00.123456 UTC))
            .expect("legacy timestamp");
        let updated = EventView::try_from(event(
            EventType::Updated,
            json!({
                "before_trigger": {
                    "next_fire_at": legacy,
                    "next_evaluation_at": null
                },
                "after_trigger": {
                    "next_fire_at": "2026-07-19T06:00:00Z",
                    "next_evaluation_at": null
                }
            }),
        ))
        .expect("updated view");
        assert_eq!(
            updated.details,
            json!({
                "before_trigger": {
                    "next_fire_at": "2026-07-19T06:00:00.123Z",
                    "next_evaluation_at": null
                },
                "after_trigger": {
                    "next_fire_at": "2026-07-19T06:00:00.000Z",
                    "next_evaluation_at": null
                }
            })
        );
    }

    #[test]
    fn event_view_does_not_rewrite_unowned_json() {
        let opaque = json!({
            "parameters": {
                "next_fire_at": [2026, 199, 6, 0, 0, 0, 0, 0, 0]
            }
        });
        let view = EventView::try_from(event(EventType::ConditionEvaluated, opaque.clone()))
            .expect("condition view");

        assert_eq!(view.details, opaque);
    }

    #[test]
    fn item_view_formats_every_direct_and_nested_timestamp() {
        let instant = datetime!(2026-07-19 06:00:00.123456 UTC);
        let item = Remindi {
            id: Uuid::nil(),
            owner_id: "owner-a".into(),
            project_id: "project-a".into(),
            task_id: Some("task-a".into()),
            message: "Inspect timestamps".into(),
            instructions: Some("Check every field".into()),
            state: RemindiState::Completed,
            priority: Priority::High,
            trigger: Trigger::Interval {
                first_at: instant,
                every_seconds: 3600,
            },
            recurrence: Some(RecurrenceSpec {
                every_seconds: 3600,
                missed_policy: MissedPolicy::CatchUp,
                max_occurrences: Some(2),
                end_at: Some(instant),
            }),
            next_fire_at: Some(instant),
            next_evaluation_at: Some(instant),
            original_next_fire_at: Some(instant),
            due_since: Some(instant),
            snooze_until: Some(instant),
            snoozed_from_state: Some(RemindiState::Due),
            overdue_after_seconds: 60,
            occurrence_no: 1,
            source_session_id: Some("session-a".into()),
            source_task_lineage_id: Some("lineage-a".into()),
            last_checked_at: Some(instant),
            last_condition_status: Some(ConditionStatus::Satisfied),
            last_condition_detail: Some("ready".into()),
            snooze_count: 1,
            version: 2,
            created_at: instant,
            updated_at: instant,
            completed_at: Some(instant),
            cancelled_at: Some(instant),
        };
        let value = serde_json::to_value(RemindiView::try_from(item).expect("item view"))
            .expect("item JSON");
        for key in [
            "next_fire_at",
            "next_evaluation_at",
            "original_next_fire_at",
            "due_since",
            "snooze_until",
            "last_checked_at",
            "created_at",
            "updated_at",
            "completed_at",
            "cancelled_at",
        ] {
            assert_eq!(value[key], "2026-07-19T06:00:00.123Z", "{key}");
        }
        assert_eq!(value["trigger"]["first_at"], "2026-07-19T06:00:00.123Z");
        assert_eq!(value["recurrence"]["end_at"], "2026-07-19T06:00:00.123Z");
        assert!(value.get("owner_id").is_none());
    }

    #[test]
    fn condition_trigger_view_preserves_parameters_and_formats_manual_check() {
        let parameters = json!({
            "next_fire_at": [2026, 200, 6, 0, 0, 0, 0, 0, 0],
            "timestamp": "caller-owned"
        });
        let value = serde_json::to_value(
            TriggerView::try_from(Trigger::Condition {
                adapter: "http_status".into(),
                parameters: parameters.clone(),
                poll_interval_seconds: Some(60),
                manual_check_at: Some(datetime!(2026-07-19 16:00:00.123456 +10:00)),
            })
            .expect("trigger view"),
        )
        .expect("trigger JSON");

        assert_eq!(value["parameters"], parameters);
        assert_eq!(value["manual_check_at"], "2026-07-19T06:00:00.123Z");
    }
}
