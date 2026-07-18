CREATE TABLE remindi (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    task_id TEXT,
    message TEXT NOT NULL CHECK (length(message) BETWEEN 1 AND 8192),
    instructions TEXT CHECK (instructions IS NULL OR length(instructions) <= 32768),
    state TEXT NOT NULL CHECK (state IN ('scheduled', 'due', 'overdue', 'snoozed', 'completed', 'cancelled')),
    priority TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('low', 'normal', 'high', 'critical')),
    trigger_type TEXT NOT NULL CHECK (trigger_type IN ('at_time', 'after_elapsed', 'interval', 'next_session', 'next_continuation', 'goal_active', 'condition')),
    trigger_spec_json TEXT NOT NULL CHECK (json_valid(trigger_spec_json)),
    recurrence_spec_json TEXT CHECK (recurrence_spec_json IS NULL OR json_valid(recurrence_spec_json)),
    next_fire_at TEXT,
    next_evaluation_at TEXT,
    original_next_fire_at TEXT,
    due_since TEXT,
    snooze_until TEXT,
    snoozed_from_state TEXT CHECK (snoozed_from_state IS NULL OR snoozed_from_state IN ('due', 'overdue')),
    overdue_after_seconds INTEGER NOT NULL DEFAULT 0 CHECK (overdue_after_seconds >= 0),
    occurrence_no INTEGER NOT NULL DEFAULT 1 CHECK (occurrence_no >= 1),
    source_session_id TEXT,
    source_task_lineage_id TEXT,
    last_checked_at TEXT,
    last_condition_status TEXT CHECK (last_condition_status IS NULL OR last_condition_status IN ('satisfied', 'unsatisfied', 'unknown', 'error')),
    last_condition_detail TEXT,
    snooze_count INTEGER NOT NULL DEFAULT 0 CHECK (snooze_count >= 0),
    version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT,
    cancelled_at TEXT,
    CHECK (
        (state = 'completed' AND completed_at IS NOT NULL AND cancelled_at IS NULL)
        OR (state = 'cancelled' AND cancelled_at IS NOT NULL AND completed_at IS NULL)
        OR (state NOT IN ('completed', 'cancelled') AND completed_at IS NULL AND cancelled_at IS NULL)
    ),
    CHECK (
        (state = 'snoozed' AND snooze_until IS NOT NULL AND snoozed_from_state IS NOT NULL)
        OR (state <> 'snoozed' AND snooze_until IS NULL AND snoozed_from_state IS NULL)
    )
) STRICT;

CREATE TABLE remindi_links (
    remindi_id TEXT NOT NULL REFERENCES remindi(id) ON DELETE CASCADE,
    link_type TEXT NOT NULL CHECK (link_type IN ('goal', 'memory', 'issue', 'url', 'artifact')),
    link_value TEXT NOT NULL CHECK (length(link_value) BETWEEN 1 AND 2048),
    created_at TEXT NOT NULL,
    PRIMARY KEY (remindi_id, link_type, link_value)
) STRICT, WITHOUT ROWID;

CREATE TABLE remindi_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    remindi_id TEXT NOT NULL REFERENCES remindi(id) ON DELETE RESTRICT,
    event_type TEXT NOT NULL CHECK (event_type IN ('created', 'checked', 'became_due', 'became_overdue', 'condition_evaluated', 'occurrence_advanced', 'snoozed', 'updated', 'completed', 'cancelled', 'delivery_attempted', 'delivery_succeeded', 'delivery_failed')),
    actor_type TEXT NOT NULL CHECK (actor_type IN ('user', 'agent', 'scheduler', 'system')),
    actor_id TEXT NOT NULL,
    request_id TEXT,
    occurred_at TEXT NOT NULL,
    prior_version INTEGER,
    new_version INTEGER,
    details_json TEXT NOT NULL CHECK (json_valid(details_json))
) STRICT;

CREATE TABLE completion_evidence (
    id TEXT PRIMARY KEY,
    remindi_id TEXT NOT NULL UNIQUE REFERENCES remindi(id) ON DELETE RESTRICT,
    evidence_type TEXT NOT NULL CHECK (evidence_type IN ('observation', 'test_result', 'artifact', 'log_reference', 'change_reference', 'user_confirmation', 'external_reference')),
    summary TEXT NOT NULL CHECK (length(summary) BETWEEN 1 AND 4096),
    reference_uri TEXT,
    content_hash TEXT,
    observed_at TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    recorded_by TEXT NOT NULL,
    metadata_json TEXT CHECK (metadata_json IS NULL OR json_valid(metadata_json)),
    CHECK (reference_uri IS NOT NULL OR content_hash IS NOT NULL)
) STRICT;

CREATE TABLE idempotency_records (
    actor_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    response_json TEXT NOT NULL CHECK (json_valid(response_json)),
    remindi_id TEXT,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    PRIMARY KEY (actor_id, tool_name, idempotency_key)
) STRICT, WITHOUT ROWID;

CREATE TABLE scheduler_leases (
    lease_name TEXT PRIMARY KEY,
    holder_id TEXT NOT NULL,
    acquired_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version >= 1)
) STRICT;

CREATE INDEX idx_remindi_project_state_fire ON remindi(owner_id, project_id, state, next_fire_at);
CREATE INDEX idx_remindi_task_state ON remindi(owner_id, project_id, task_id, state);
CREATE INDEX idx_remindi_trigger_state ON remindi(trigger_type, state, next_fire_at);
CREATE INDEX idx_remindi_condition_evaluation ON remindi(trigger_type, state, next_evaluation_at) WHERE trigger_type = 'condition';
CREATE INDEX idx_remindi_due_since ON remindi(due_since) WHERE state IN ('due', 'overdue');
CREATE INDEX idx_events_remindi_sequence ON remindi_events(remindi_id, sequence);
CREATE INDEX idx_links_lookup ON remindi_links(link_type, link_value, remindi_id);
CREATE INDEX idx_idempotency_expiry ON idempotency_records(expires_at);
