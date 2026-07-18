use std::collections::{HashMap, HashSet};

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use super::DatabaseError;

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: include_str!("../../migrations/0001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "admin_webui",
        sql: include_str!("../../migrations/0002_admin_webui.sql"),
    },
];

const RUNTIME_DEFAULTS: &[(&str, &str)] = &[
    ("scheduler.poll_interval_seconds", "30"),
    ("scheduler.lease_seconds", "90"),
    ("adapters.timeout_seconds", "5"),
    ("adapters.max_concurrency", "8"),
    ("recurrence.max_catch_up_occurrences", "10"),
    ("remindi.default_overdue_seconds", "0"),
    ("remindi.max_snooze_seconds", "31536000"),
    ("idempotency.retention_days", "30"),
    ("backups.interval_seconds", "86400"),
    ("backups.retention_count", "14"),
    ("backups.upload_max_bytes", "1073741824"),
];
const ADAPTER_NAMES: &[&str] = &[
    "observation_window_ended",
    "http_health",
    "tcp_reachable",
    "file_exists",
];
const SERVICE_COMPONENTS: &[&str] = &["mcp", "scheduler"];

pub(super) async fn apply(pool: &SqlitePool) -> Result<(), DatabaseError> {
    let mut transaction = pool.begin_with("BEGIN EXCLUSIVE").await?;
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at TEXT NOT NULL
             ) STRICT;",
    )
    .execute(transaction.as_mut())
    .await?;

    let applied: Vec<(i64, String)> =
        sqlx::query_as("SELECT version, name FROM schema_migrations ORDER BY version")
            .fetch_all(transaction.as_mut())
            .await?;
    if let Some((version, _)) = applied.last()
        && *version > MIGRATIONS.len() as i64
    {
        return Err(DatabaseError::NewerSchema(*version));
    }

    for migration in MIGRATIONS {
        let expected_name = migration_identity(migration);
        if let Some((_, actual_name)) = applied
            .iter()
            .find(|(version, _)| *version == migration.version)
        {
            if actual_name != &expected_name {
                return Err(DatabaseError::MigrationDrift(migration.version));
            }
            continue;
        }

        sqlx::raw_sql(migration.sql)
            .execute(transaction.as_mut())
            .await?;
        sqlx::query(
            "INSERT INTO schema_migrations(version, name, applied_at)
                 VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .bind(migration.version)
        .bind(expected_name)
        .execute(transaction.as_mut())
        .await?;
    }
    seed_and_validate(&mut transaction).await?;
    transaction.commit().await?;
    Ok(())
}

fn migration_identity(migration: &Migration) -> String {
    let digest = Sha256::digest(migration.sql.as_bytes());
    format!("{}:{digest:x}", migration.name)
}

async fn seed_and_validate(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), DatabaseError> {
    for (key, value) in RUNTIME_DEFAULTS {
        sqlx::query(
            "INSERT INTO runtime_settings(setting_key, value_json, updated_at, updated_by)
             VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system')
             ON CONFLICT(setting_key) DO NOTHING",
        )
        .bind(key)
        .bind(value)
        .execute(transaction.as_mut())
        .await?;
    }
    for name in ADAPTER_NAMES {
        sqlx::query(
            "INSERT INTO adapter_configs(
                adapter_name, enabled, config_json, updated_at, updated_by
             ) VALUES (?, 0, '{}', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system')
             ON CONFLICT(adapter_name) DO NOTHING",
        )
        .bind(name)
        .execute(transaction.as_mut())
        .await?;
    }
    for component in SERVICE_COMPONENTS {
        sqlx::query(
            "INSERT INTO service_runtime(
                component, desired_state, updated_at, updated_by
             ) VALUES (?, 'running', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'system')
             ON CONFLICT(component) DO NOTHING",
        )
        .bind(component)
        .execute(transaction.as_mut())
        .await?;
    }

    validate_runtime_settings(transaction).await?;
    validate_adapters(transaction).await?;
    validate_service_runtime(transaction).await
}

async fn validate_runtime_settings(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), DatabaseError> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT setting_key, value_json FROM runtime_settings")
            .fetch_all(transaction.as_mut())
            .await?;
    let allowed = RUNTIME_DEFAULTS
        .iter()
        .map(|(key, _)| *key)
        .collect::<HashSet<_>>();
    if rows.len() != allowed.len() || rows.iter().any(|(key, _)| !allowed.contains(key.as_str())) {
        return Err(DatabaseError::InvalidBootstrapRows);
    }

    let mut values = HashMap::new();
    for (key, encoded) in rows {
        let value = serde_json::from_str::<Value>(&encoded)
            .ok()
            .and_then(|value| value.as_i64())
            .ok_or(DatabaseError::InvalidBootstrapRows)?;
        let valid = if key == "remindi.default_overdue_seconds" {
            value >= 0
        } else {
            value > 0
        };
        if !valid {
            return Err(DatabaseError::InvalidBootstrapRows);
        }
        values.insert(key, value);
    }
    let poll = values["scheduler.poll_interval_seconds"];
    let lease = values["scheduler.lease_seconds"];
    if lease <= poll.saturating_mul(2) {
        return Err(DatabaseError::InvalidBootstrapRows);
    }
    Ok(())
}

async fn validate_adapters(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), DatabaseError> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT adapter_name, config_json FROM adapter_configs")
            .fetch_all(transaction.as_mut())
            .await?;
    let allowed = ADAPTER_NAMES.iter().copied().collect::<HashSet<_>>();
    if rows.len() != allowed.len()
        || rows.iter().any(|(name, config)| {
            !allowed.contains(name.as_str())
                || !serde_json::from_str::<Value>(config).is_ok_and(|value| value.is_object())
        })
    {
        return Err(DatabaseError::InvalidBootstrapRows);
    }
    Ok(())
}

async fn validate_service_runtime(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), DatabaseError> {
    let components: Vec<String> =
        sqlx::query_scalar("SELECT component FROM service_runtime ORDER BY component")
            .fetch_all(transaction.as_mut())
            .await?;
    let allowed = SERVICE_COMPONENTS.iter().copied().collect::<HashSet<_>>();
    if components.len() != allowed.len()
        || components
            .iter()
            .any(|component| !allowed.contains(component.as_str()))
    {
        return Err(DatabaseError::InvalidBootstrapRows);
    }
    Ok(())
}
