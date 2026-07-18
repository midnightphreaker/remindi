use std::time::Duration;

use remindi::{
    remindi::{
        ActorType, DomainError, EventType, EvidenceInput, EvidenceSource, EvidenceType,
        LifecycleEvent, LinkType, MissedPolicy, OccurrenceDisposition, Priority, Readiness,
        RecurrenceSpec, Remindi, RemindiEvent, RemindiLink, RemindiState, Trigger,
        canonical_timestamp, parse_timestamp,
    },
    triggers::{CheckContext, ConditionEvaluation, evaluate},
};
use serde_json::json;
use time::{OffsetDateTime, macros::datetime};
use uuid::Uuid;

fn item(trigger: Trigger) -> Remindi {
    Remindi {
        id: Uuid::nil(),
        owner_id: "owner".into(),
        project_id: "project".into(),
        task_id: None,
        message: "verify the service".into(),
        instructions: None,
        state: RemindiState::Scheduled,
        priority: Priority::Normal,
        trigger,
        recurrence: None,
        next_fire_at: None,
        next_evaluation_at: None,
        original_next_fire_at: None,
        due_since: None,
        snooze_until: None,
        snoozed_from_state: None,
        overdue_after_seconds: 0,
        occurrence_no: 1,
        source_session_id: None,
        source_task_lineage_id: None,
        last_checked_at: None,
        last_condition_status: None,
        last_condition_detail: None,
        snooze_count: 0,
        version: 1,
        created_at: datetime!(2026-07-18 00:00 UTC),
        updated_at: datetime!(2026-07-18 00:00 UTC),
        completed_at: None,
        cancelled_at: None,
    }
}

fn context() -> CheckContext {
    CheckContext {
        session_id: None,
        task_lineage_id: None,
        lifecycle_event: LifecycleEvent::Checkpoint,
        active_goal_ids: vec![],
    }
}

#[test]
fn timestamp_input_requires_offset_and_normalizes_to_utc_milliseconds() {
    let parsed = parse_timestamp("2026-07-19T16:34:56.789456+10:30").expect("valid timestamp");

    assert_eq!(
        canonical_timestamp(parsed).expect("canonical timestamp"),
        "2026-07-19T06:04:56.789Z"
    );
    assert_eq!(
        parse_timestamp("2026-07-19T06:04:56"),
        Err(DomainError::TimestampOffsetRequired)
    );
}

#[test]
fn domain_enums_use_the_exact_external_snake_case_values() {
    assert_eq!(
        serde_json::to_value(RemindiState::Scheduled).expect("state serializes"),
        "scheduled"
    );
    assert_eq!(
        serde_json::to_value(Readiness::ManualVerification).expect("readiness serializes"),
        "manual_verification"
    );
    assert_eq!(
        serde_json::to_value(EvidenceType::TestResult).expect("evidence type serializes"),
        "test_result"
    );
    assert_eq!(
        serde_json::to_value(ActorType::Scheduler).expect("actor type serializes"),
        "scheduler"
    );

    let link = RemindiLink {
        link_type: LinkType::Goal,
        value: "goal-a".into(),
        created_at: datetime!(2026-07-18 00:00 UTC),
    };
    let event = RemindiEvent {
        sequence: Some(1),
        event_id: Uuid::nil(),
        remindi_id: Uuid::nil(),
        event_type: EventType::Created,
        actor_type: ActorType::Agent,
        actor_id: "agent".into(),
        request_id: None,
        occurred_at: datetime!(2026-07-18 00:00 UTC),
        prior_version: None,
        new_version: Some(1),
        details: json!({}),
    };
    assert_eq!(link.link_type, LinkType::Goal);
    assert_eq!(event.new_version, Some(1));
}

#[test]
fn time_trigger_fires_at_the_exact_boundary_without_consuming_recurrence() {
    let fire_at = datetime!(2026-07-19 06:00 UTC);
    let mut remindi = item(Trigger::AtTime { at: fire_at });
    remindi.next_fire_at = Some(fire_at);
    remindi.recurrence = Some(RecurrenceSpec {
        every_seconds: 3600,
        missed_policy: MissedPolicy::Coalesce,
        max_occurrences: None,
        end_at: None,
    });

    let result = evaluate(
        &mut remindi,
        fire_at,
        &context(),
        ConditionEvaluation::NotEvaluated,
    )
    .expect("evaluation succeeds");

    assert_eq!(result.readiness, Some(Readiness::Due));
    assert_eq!(remindi.state, RemindiState::Due);
    assert_eq!(remindi.due_since, Some(fire_at));
    assert_eq!(remindi.occurrence_no, 1);
    assert_eq!(remindi.next_fire_at, Some(fire_at));
}

#[test]
fn overdue_is_measured_from_due_since_and_never_moves_backwards() {
    let due_since = datetime!(2026-07-19 06:00 UTC);
    let mut remindi = item(Trigger::AtTime { at: due_since });
    remindi.state = RemindiState::Due;
    remindi.due_since = Some(due_since);
    remindi.overdue_after_seconds = 60;

    let result = evaluate(
        &mut remindi,
        due_since + time::Duration::seconds(60),
        &context(),
        ConditionEvaluation::NotEvaluated,
    )
    .expect("evaluation succeeds");
    assert_eq!(result.readiness, Some(Readiness::Overdue));
    assert_eq!(remindi.state, RemindiState::Overdue);

    let result = evaluate(
        &mut remindi,
        due_since - time::Duration::hours(1),
        &context(),
        ConditionEvaluation::NotEvaluated,
    )
    .expect("backward clock evaluation succeeds");
    assert_eq!(result.readiness, Some(Readiness::Overdue));
    assert_eq!(remindi.state, RemindiState::Overdue);
}

#[test]
fn next_session_and_continuation_require_the_documented_context() {
    let now = datetime!(2026-07-19 06:00 UTC);
    let mut next_session = item(Trigger::NextSession);
    next_session.source_session_id = Some("session-a".into());
    let result = evaluate(
        &mut next_session,
        now,
        &CheckContext {
            session_id: Some("session-b".into()),
            ..context()
        },
        ConditionEvaluation::NotEvaluated,
    )
    .expect("next session evaluates");
    assert_eq!(result.readiness, Some(Readiness::Due));

    let mut continuation = item(Trigger::NextContinuation);
    continuation.source_session_id = Some("session-a".into());
    continuation.source_task_lineage_id = Some("lineage".into());
    let wrong = evaluate(
        &mut continuation,
        now,
        &CheckContext {
            session_id: Some("session-b".into()),
            task_lineage_id: Some("other".into()),
            lifecycle_event: LifecycleEvent::Continuation,
            active_goal_ids: vec![],
        },
        ConditionEvaluation::NotEvaluated,
    )
    .expect("wrong continuation context evaluates");
    assert_eq!(wrong.readiness, None);

    let right = evaluate(
        &mut continuation,
        now,
        &CheckContext {
            session_id: Some("session-b".into()),
            task_lineage_id: Some("lineage".into()),
            lifecycle_event: LifecycleEvent::Continuation,
            active_goal_ids: vec![],
        },
        ConditionEvaluation::NotEvaluated,
    )
    .expect("matching continuation evaluates");
    assert_eq!(right.readiness, Some(Readiness::Due));
}

#[test]
fn first_supplied_session_fires_when_creation_session_was_absent() {
    let mut remindi = item(Trigger::NextSession);
    let result = evaluate(
        &mut remindi,
        datetime!(2026-07-19 06:00 UTC),
        &CheckContext {
            session_id: Some("session-a".into()),
            ..context()
        },
        ConditionEvaluation::NotEvaluated,
    )
    .expect("next session evaluates");

    assert_eq!(result.readiness, Some(Readiness::Due));
}

#[test]
fn goal_and_condition_triggers_use_explicit_non_consuming_results() {
    let now = datetime!(2026-07-19 06:00 UTC);
    let mut goal = item(Trigger::GoalActive {
        goal_id: "goal-a".into(),
    });
    let goal_result = evaluate(
        &mut goal,
        now,
        &CheckContext {
            active_goal_ids: vec!["goal-a".into()],
            ..context()
        },
        ConditionEvaluation::NotEvaluated,
    )
    .expect("goal evaluates");
    assert_eq!(goal_result.readiness, Some(Readiness::Due));
    assert_ne!(goal.state, RemindiState::Completed);

    let manual_at = now;
    let mut condition = item(Trigger::Condition {
        adapter: "http_health".into(),
        parameters: json!({"target": "service-api"}),
        poll_interval_seconds: Some(300),
        manual_check_at: Some(manual_at),
    });
    let condition_result = evaluate(
        &mut condition,
        now,
        &context(),
        ConditionEvaluation::Unknown,
    )
    .expect("condition evaluates");
    assert_eq!(
        condition_result.readiness,
        Some(Readiness::ManualVerification)
    );
    assert_eq!(condition.state, RemindiState::Due);
}

#[test]
fn snooze_requires_ready_state_reason_and_future_deadline_then_preserves_due_anchor() {
    let now = datetime!(2026-07-19 06:00 UTC);
    let mut scheduled = item(Trigger::NextSession);
    assert_eq!(
        scheduled.snooze(now + time::Duration::hours(1), "later", now),
        Err(DomainError::SnoozeRequiresReadyState)
    );

    let mut due = item(Trigger::NextSession);
    due.state = RemindiState::Due;
    due.due_since = Some(now - time::Duration::minutes(10));
    assert_eq!(
        due.snooze(now, "later", now),
        Err(DomainError::SnoozeMustBeFuture)
    );
    assert_eq!(
        due.snooze(now + time::Duration::hours(1), " ", now),
        Err(DomainError::ReasonRequired)
    );

    due.snooze(now + time::Duration::hours(1), "waiting", now)
        .expect("due item snoozes");
    assert_eq!(due.state, RemindiState::Snoozed);
    assert_eq!(due.snoozed_from_state, Some(RemindiState::Due));
    assert_eq!(due.due_since, Some(now - time::Duration::minutes(10)));

    let result = evaluate(
        &mut due,
        now + time::Duration::hours(1),
        &context(),
        ConditionEvaluation::NotEvaluated,
    )
    .expect("snooze expires");
    assert_eq!(result.readiness, Some(Readiness::Overdue));
    assert_eq!(due.state, RemindiState::Overdue);
    assert_eq!(due.snooze_until, None);
    assert_eq!(due.snoozed_from_state, None);
}

#[test]
fn terminal_states_are_irreversible_and_cancel_is_soft() {
    let now = datetime!(2026-07-19 06:00 UTC);
    let mut remindi = item(Trigger::NextSession);
    remindi.cancel("no longer needed", now).expect("cancels");
    assert_eq!(remindi.state, RemindiState::Cancelled);
    assert_eq!(remindi.cancelled_at, Some(now));
    assert_eq!(
        remindi.cancel("again", now),
        Err(DomainError::TerminalState)
    );
    assert_eq!(
        evaluate(
            &mut remindi,
            now,
            &context(),
            ConditionEvaluation::NotEvaluated
        ),
        Err(DomainError::TerminalState)
    );
}

#[test]
fn recurrence_advances_from_scheduled_anchor_for_each_policy() {
    let now = datetime!(2026-07-19 10:30 UTC);
    let anchor = datetime!(2026-07-19 06:00 UTC);

    let coalesced = RecurrenceSpec {
        every_seconds: 3600,
        missed_policy: MissedPolicy::Coalesce,
        max_occurrences: None,
        end_at: None,
    }
    .advance(anchor, 1, now, OccurrenceDisposition::Acknowledged)
    .expect("coalesce advances");
    assert_eq!(coalesced.next_fire_at, datetime!(2026-07-19 11:00 UTC));
    assert_eq!(coalesced.occurrence_no, 6);
    assert_eq!(coalesced.skipped_count, 0);

    let caught_up = RecurrenceSpec {
        missed_policy: MissedPolicy::CatchUp,
        ..RecurrenceSpec::every_hour()
    }
    .advance(anchor, 1, now, OccurrenceDisposition::Acknowledged)
    .expect("catch-up advances once");
    assert_eq!(caught_up.next_fire_at, datetime!(2026-07-19 07:00 UTC));
    assert_eq!(caught_up.occurrence_no, 2);

    let skipped = RecurrenceSpec {
        missed_policy: MissedPolicy::Skip,
        ..RecurrenceSpec::every_hour()
    }
    .advance(anchor, 1, now, OccurrenceDisposition::Skipped)
    .expect("skip advances");
    assert_eq!(skipped.next_fire_at, datetime!(2026-07-19 11:00 UTC));
    assert_eq!(skipped.occurrence_no, 6);
    assert_eq!(skipped.skipped_count, 4);
}

#[test]
fn recurrence_limits_and_catch_up_cap_are_enforced_without_mutating() {
    let anchor = datetime!(2026-07-19 06:00 UTC);
    let spec = RecurrenceSpec {
        every_seconds: 3600,
        missed_policy: MissedPolicy::CatchUp,
        max_occurrences: Some(2),
        end_at: Some(datetime!(2026-07-20 00:00 UTC)),
    };
    assert_eq!(
        spec.advance(
            datetime!(2026-07-19 07:00 UTC),
            2,
            datetime!(2026-07-19 10:00 UTC),
            OccurrenceDisposition::Acknowledged,
        ),
        Err(DomainError::FinalOccurrence)
    );

    let unlimited = RecurrenceSpec {
        max_occurrences: None,
        end_at: None,
        ..spec
    };
    assert_eq!(
        unlimited
            .ready_occurrences(anchor, 1, datetime!(2026-07-19 20:00 UTC), 3)
            .expect("ready count"),
        3
    );
    assert_eq!(anchor, datetime!(2026-07-19 06:00 UTC));
}

#[test]
fn recurrence_validation_rejects_invalid_bounds() {
    assert_eq!(
        RecurrenceSpec {
            every_seconds: 59,
            ..RecurrenceSpec::every_hour()
        }
        .validate(),
        Err(DomainError::InvalidRecurrenceInterval)
    );
    assert_eq!(
        RecurrenceSpec {
            max_occurrences: Some(0),
            ..RecurrenceSpec::every_hour()
        }
        .validate(),
        Err(DomainError::InvalidMaxOccurrences)
    );
}

#[test]
fn trigger_validation_enforces_source_bounds_and_recurrence_pairing() {
    assert_eq!(
        Trigger::AfterElapsed { after_seconds: 0 }.validate(),
        Err(DomainError::InvalidElapsedDuration)
    );
    assert_eq!(
        Trigger::Condition {
            adapter: "HTTP-health".into(),
            parameters: json!({}),
            poll_interval_seconds: Some(300),
            manual_check_at: None,
        }
        .validate(),
        Err(DomainError::InvalidConditionAdapter)
    );
    assert_eq!(
        RecurrenceSpec::every_hour().validate_for_trigger(&Trigger::AtTime {
            at: datetime!(2026-07-19 06:00 UTC),
        }),
        Err(DomainError::RecurrenceRequiresIntervalTrigger)
    );
    assert_eq!(
        RecurrenceSpec::every_hour().validate_for_trigger(&Trigger::Interval {
            first_at: datetime!(2026-07-19 06:00 UTC),
            every_seconds: 7200,
        }),
        Err(DomainError::RecurrenceIntervalMismatch)
    );
}

#[test]
fn evidence_requires_meaningful_summary_reference_valid_hash_and_bounded_time() {
    let now = datetime!(2026-07-19 06:00 UTC);
    let valid = EvidenceInput {
        evidence_type: EvidenceType::TestResult,
        summary: "37 domain tests passed".into(),
        reference_uri: Some("https://ci.example.test/runs/42".into()),
        content_hash: None,
        observed_at: now,
        metadata: Some(json!({"suite": "domain"})),
        source: EvidenceSource::AuthenticatedActor,
    };
    valid
        .clone()
        .validate(now, Duration::from_secs(300))
        .expect("evidence validates");

    assert_eq!(
        EvidenceInput {
            summary: "done".into(),
            ..valid.clone()
        }
        .validate(now, Duration::from_secs(300)),
        Err(DomainError::EmptyEvidenceAssertion)
    );
    assert_eq!(
        EvidenceInput {
            reference_uri: None,
            content_hash: None,
            ..valid.clone()
        }
        .validate(now, Duration::from_secs(300)),
        Err(DomainError::StableEvidenceReferenceRequired)
    );
    assert_eq!(
        EvidenceInput {
            reference_uri: None,
            content_hash: Some("sha1:deadbeef".into()),
            ..valid.clone()
        }
        .validate(now, Duration::from_secs(300)),
        Err(DomainError::InvalidContentHash)
    );
    assert_eq!(
        EvidenceInput {
            observed_at: now + time::Duration::minutes(6),
            ..valid.clone()
        }
        .validate(now, Duration::from_secs(300)),
        Err(DomainError::EvidenceObservedInFuture)
    );
    assert_eq!(
        EvidenceInput {
            source: EvidenceSource::AdapterTrigger,
            ..valid
        }
        .validate(now, Duration::from_secs(300)),
        Err(DomainError::AdapterResultIsNotCompletionEvidence)
    );
}

#[test]
fn evidence_rejects_embedded_credentials_and_non_object_metadata() {
    let now = OffsetDateTime::UNIX_EPOCH;
    let evidence = EvidenceInput {
        evidence_type: EvidenceType::ExternalReference,
        summary: "external status was verified".into(),
        reference_uri: Some("https://user:secret@example.test/report".into()),
        content_hash: None,
        observed_at: now,
        metadata: Some(json!(["not", "an", "object"])),
        source: EvidenceSource::AuthenticatedActor,
    };
    assert_eq!(
        evidence.clone().validate(now, Duration::ZERO),
        Err(DomainError::EvidenceReferenceContainsCredentials)
    );

    assert_eq!(
        EvidenceInput {
            reference_uri: Some("https://example.test/report".into()),
            ..evidence
        }
        .validate(now, Duration::ZERO),
        Err(DomainError::EvidenceMetadataMustBeObject)
    );
}

#[test]
fn completion_requires_validated_evidence_and_trigger_replacement_resets_snooze() {
    let now = datetime!(2026-07-19 06:00 UTC);
    let evidence = EvidenceInput {
        evidence_type: EvidenceType::Observation,
        summary: "owner confirmed the work outcome".into(),
        reference_uri: Some("urn:remindi:user-confirmation:42".into()),
        content_hash: None,
        observed_at: now,
        metadata: None,
        source: EvidenceSource::AuthenticatedActor,
    }
    .validate(now, Duration::ZERO)
    .expect("evidence validates");
    let mut completed = item(Trigger::NextSession);
    completed
        .complete(&evidence, now)
        .expect("active item completes");
    assert_eq!(completed.state, RemindiState::Completed);
    assert_eq!(completed.completed_at, Some(now));

    let mut snoozed = item(Trigger::NextSession);
    snoozed.state = RemindiState::Due;
    snoozed.due_since = Some(now);
    snoozed
        .snooze(now + time::Duration::hours(1), "waiting", now)
        .expect("item snoozes");
    snoozed
        .replace_trigger(
            Trigger::AtTime {
                at: now + time::Duration::days(1),
            },
            Some(now + time::Duration::days(1)),
            now,
        )
        .expect("trigger replacement succeeds");
    assert_eq!(snoozed.state, RemindiState::Scheduled);
    assert_eq!(snoozed.snooze_until, None);
    assert_eq!(snoozed.due_since, None);

    let mut due = item(Trigger::NextSession);
    due.state = RemindiState::Due;
    due.due_since = Some(now);
    due.replace_trigger(
        Trigger::AtTime {
            at: now + time::Duration::days(2),
        },
        Some(now + time::Duration::days(2)),
        now,
    )
    .expect("ready item trigger updates");
    assert_eq!(due.state, RemindiState::Due);
    assert_eq!(due.due_since, Some(now));
}
