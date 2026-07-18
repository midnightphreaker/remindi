use time::{Duration, OffsetDateTime};

use crate::remindi::{
    ConditionStatus, DomainError, EventType, LifecycleEvent, Readiness, Remindi, RemindiState,
    Trigger,
};

/// Explicit agent lifecycle context used by context-sensitive triggers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckContext {
    pub session_id: Option<String>,
    pub task_lineage_id: Option<String>,
    pub lifecycle_event: LifecycleEvent,
    pub active_goal_ids: Vec<String>,
}

/// A condition result supplied by the adapter layer, or absence of an evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConditionEvaluation {
    NotEvaluated,
    Satisfied,
    Unsatisfied,
    Unknown,
    Error,
}

/// Readiness and transition events produced by one deterministic evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationResult {
    pub readiness: Option<Readiness>,
    pub events: Vec<EventType>,
}

/// Evaluates an item without completing or advancing a recurring occurrence.
pub fn evaluate(
    remindi: &mut Remindi,
    now: OffsetDateTime,
    context: &CheckContext,
    condition: ConditionEvaluation,
) -> Result<EvaluationResult, DomainError> {
    if remindi.state.is_terminal() {
        return Err(DomainError::TerminalState);
    }
    remindi.last_checked_at = Some(now);

    if remindi.state == RemindiState::Snoozed {
        return evaluate_snooze(remindi, now);
    }
    if remindi.state == RemindiState::Overdue {
        return Ok(ready(Readiness::Overdue, vec![]));
    }
    if remindi.state == RemindiState::Due {
        return evaluate_due(remindi, now, vec![]);
    }

    let (satisfied, readiness, mut events) =
        trigger_satisfaction(remindi, now, context, condition)?;
    if !satisfied {
        return Ok(EvaluationResult {
            readiness: None,
            events,
        });
    }

    remindi.state = RemindiState::Due;
    remindi.due_since = Some(now);
    remindi.updated_at = now;
    remindi.version += 1;
    events.push(EventType::BecameDue);
    Ok(ready(readiness, events))
}

fn trigger_satisfaction(
    remindi: &mut Remindi,
    now: OffsetDateTime,
    context: &CheckContext,
    condition: ConditionEvaluation,
) -> Result<(bool, Readiness, Vec<EventType>), DomainError> {
    let standard = Readiness::Due;
    let result = match &remindi.trigger {
        Trigger::AtTime { .. } | Trigger::AfterElapsed { .. } | Trigger::Interval { .. } => (
            remindi.next_fire_at.is_some_and(|anchor| now >= anchor),
            standard,
            vec![],
        ),
        Trigger::NextSession => (
            nonempty(context.session_id.as_deref())
                && context.session_id.as_deref() != remindi.source_session_id.as_deref(),
            standard,
            vec![],
        ),
        Trigger::NextContinuation => (
            context.lifecycle_event == LifecycleEvent::Continuation
                && nonempty(context.session_id.as_deref())
                && context.session_id.as_deref() != remindi.source_session_id.as_deref()
                && context.task_lineage_id.as_deref() == remindi.source_task_lineage_id.as_deref()
                && remindi.source_task_lineage_id.is_some(),
            standard,
            vec![],
        ),
        Trigger::GoalActive { goal_id } => (
            context
                .active_goal_ids
                .iter()
                .any(|active| active == goal_id),
            standard,
            vec![],
        ),
        Trigger::Condition {
            parameters,
            manual_check_at,
            ..
        } => {
            if !parameters.is_object() {
                return Err(DomainError::ConditionParametersMustBeObject);
            }
            remindi.last_condition_status = match condition {
                ConditionEvaluation::NotEvaluated => None,
                ConditionEvaluation::Satisfied => Some(ConditionStatus::Satisfied),
                ConditionEvaluation::Unsatisfied => Some(ConditionStatus::Unsatisfied),
                ConditionEvaluation::Unknown => Some(ConditionStatus::Unknown),
                ConditionEvaluation::Error => Some(ConditionStatus::Error),
            };
            let evaluated = condition != ConditionEvaluation::NotEvaluated;
            let manual = condition != ConditionEvaluation::Satisfied
                && manual_check_at.is_some_and(|deadline| now >= deadline);
            (
                condition == ConditionEvaluation::Satisfied || manual,
                if manual {
                    Readiness::ManualVerification
                } else {
                    standard
                },
                if evaluated {
                    vec![EventType::ConditionEvaluated]
                } else {
                    vec![]
                },
            )
        }
    };
    Ok(result)
}

fn evaluate_snooze(
    remindi: &mut Remindi,
    now: OffsetDateTime,
) -> Result<EvaluationResult, DomainError> {
    let Some(deadline) = remindi.snooze_until else {
        return Err(DomainError::SnoozeRequiresReadyState);
    };
    if now < deadline {
        return Ok(EvaluationResult {
            readiness: None,
            events: vec![],
        });
    }

    remindi.clear_snooze();
    let grace = i64::try_from(remindi.overdue_after_seconds).unwrap_or(i64::MAX);
    let overdue = remindi
        .due_since
        .is_some_and(|due_since| now >= due_since + Duration::seconds(grace));
    remindi.state = if overdue {
        RemindiState::Overdue
    } else {
        RemindiState::Due
    };
    remindi.updated_at = now;
    remindi.version += 1;
    Ok(ready(
        if overdue {
            Readiness::Overdue
        } else {
            Readiness::Due
        },
        vec![if overdue {
            EventType::BecameOverdue
        } else {
            EventType::BecameDue
        }],
    ))
}

fn evaluate_due(
    remindi: &mut Remindi,
    now: OffsetDateTime,
    mut events: Vec<EventType>,
) -> Result<EvaluationResult, DomainError> {
    let Some(due_since) = remindi.due_since else {
        return Ok(ready(Readiness::Due, events));
    };
    let grace = i64::try_from(remindi.overdue_after_seconds).unwrap_or(i64::MAX);
    if now >= due_since + Duration::seconds(grace) {
        remindi.state = RemindiState::Overdue;
        remindi.updated_at = now;
        remindi.version += 1;
        events.push(EventType::BecameOverdue);
        return Ok(ready(Readiness::Overdue, events));
    }
    Ok(ready(Readiness::Due, events))
}

fn ready(readiness: Readiness, events: Vec<EventType>) -> EvaluationResult {
    EvaluationResult {
        readiness: Some(readiness),
        events,
    }
}

fn nonempty(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}
