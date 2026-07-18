CREATE TABLE runtime_settings (
    setting_key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL CHECK (json_valid(value_json)),
    version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE adapter_configs (
    adapter_name TEXT PRIMARY KEY CHECK (adapter_name IN ('observation_window_ended', 'http_health', 'tcp_reachable', 'file_exists')),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    config_json TEXT NOT NULL CHECK (json_valid(config_json)),
    version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE service_runtime (
    component TEXT PRIMARY KEY CHECK (component IN ('mcp', 'scheduler')),
    desired_state TEXT NOT NULL CHECK (desired_state IN ('running', 'stopped')),
    version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE backup_records (
    id TEXT PRIMARY KEY,
    file_name TEXT NOT NULL UNIQUE,
    source TEXT NOT NULL CHECK (source IN ('manual', 'automatic', 'upload', 'pre_restore')),
    status TEXT NOT NULL CHECK (status IN ('ready', 'invalid', 'restored', 'expired', 'failed')),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    size_bytes INTEGER NOT NULL CHECK (size_bytes > 0),
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    created_at TEXT NOT NULL,
    verified_at TEXT,
    created_by TEXT NOT NULL,
    details_json TEXT NOT NULL CHECK (json_valid(details_json))
) STRICT;

CREATE TABLE admin_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL CHECK (event_type IN ('login_succeeded', 'login_failed', 'logout', 'runtime_setting_updated', 'adapter_config_updated', 'workload_started', 'workload_stopped', 'workload_restarted', 'backup_created', 'backup_uploaded', 'backup_verified', 'backup_expired', 'restore_started', 'restore_succeeded', 'restore_failed')),
    actor_id TEXT NOT NULL,
    request_id TEXT,
    occurred_at TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('succeeded', 'rejected', 'failed')),
    details_json TEXT NOT NULL CHECK (json_valid(details_json))
) STRICT;

CREATE INDEX idx_backups_created ON backup_records(created_at, status);
CREATE INDEX idx_admin_events_sequence ON admin_events(sequence);

INSERT INTO runtime_settings(setting_key, value_json, updated_at, updated_by) VALUES
    ('scheduler.poll_interval_seconds', '30', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('scheduler.lease_seconds', '90', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('adapters.timeout_seconds', '5', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('adapters.max_concurrency', '8', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('recurrence.max_catch_up_occurrences', '10', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('remindi.default_overdue_seconds', '0', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('remindi.max_snooze_seconds', '31536000', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('idempotency.retention_days', '30', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('backups.interval_seconds', '86400', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('backups.retention_count', '14', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('backups.upload_max_bytes', '1073741824', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system');

INSERT INTO adapter_configs(adapter_name, enabled, config_json, updated_at, updated_by) VALUES
    ('observation_window_ended', 0, '{}', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('http_health', 0, '{}', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('tcp_reachable', 0, '{}', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('file_exists', 0, '{}', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system');

INSERT INTO service_runtime(component, desired_state, updated_at, updated_by) VALUES
    ('mcp', 'running', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system'),
    ('scheduler', 'running', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system');
