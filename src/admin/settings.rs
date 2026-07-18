use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::db::{DatabaseError, DatabaseManager};

use super::AdminError;

/// One source-defined, SQLite-backed safe runtime setting.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeSetting {
    pub key: String,
    pub value: i64,
    pub minimum: i64,
    pub maximum: Option<i64>,
    pub version: i64,
    pub restart_required: bool,
    pub updated_at: String,
    pub updated_by: String,
}

#[derive(Clone, Copy)]
struct SettingDefinition {
    key: &'static str,
    minimum: i64,
}

const DEFINITIONS: &[SettingDefinition] = &[
    SettingDefinition {
        key: "adapters.max_concurrency",
        minimum: 1,
    },
    SettingDefinition {
        key: "adapters.timeout_seconds",
        minimum: 1,
    },
    SettingDefinition {
        key: "backups.interval_seconds",
        minimum: 1,
    },
    SettingDefinition {
        key: "backups.retention_count",
        minimum: 1,
    },
    SettingDefinition {
        key: "backups.upload_max_bytes",
        minimum: 1,
    },
    SettingDefinition {
        key: "idempotency.retention_days",
        minimum: 1,
    },
    SettingDefinition {
        key: "recurrence.max_catch_up_occurrences",
        minimum: 1,
    },
    SettingDefinition {
        key: "remindi.default_overdue_seconds",
        minimum: 0,
    },
    SettingDefinition {
        key: "remindi.max_snooze_seconds",
        minimum: 1,
    },
    SettingDefinition {
        key: "scheduler.lease_seconds",
        minimum: 1,
    },
    SettingDefinition {
        key: "scheduler.poll_interval_seconds",
        minimum: 1,
    },
];

pub(super) fn is_known(key: &str) -> bool {
    definition(key).is_some()
}

fn definition(key: &str) -> Option<SettingDefinition> {
    DEFINITIONS
        .iter()
        .copied()
        .find(|definition| definition.key == key)
}

pub(super) async fn list(database: &DatabaseManager) -> Result<Vec<RuntimeSetting>, AdminError> {
    let mut connection = database.connection().await.map_err(database_error)?;
    let rows = sqlx::query(
        "SELECT setting_key, value_json, version, updated_at, updated_by \
         FROM runtime_settings ORDER BY setting_key",
    )
    .fetch_all(connection.as_mut())
    .await
    .map_err(|_| AdminError::Database)?;

    rows.into_iter()
        .map(|row| {
            let key: String = row.get("setting_key");
            let definition = definition(&key).ok_or(AdminError::Validation)?;
            let encoded: String = row.get("value_json");
            let value = serde_json::from_str::<serde_json::Value>(&encoded)
                .ok()
                .and_then(|value| value.as_i64())
                .ok_or(AdminError::Validation)?;
            Ok(RuntimeSetting {
                key,
                value,
                minimum: definition.minimum,
                // The v1 sources define integer type and minima but no numeric maxima.
                maximum: None,
                // DESIGN 16.2 reloads running consumers and atomically swaps adapters.
                restart_required: false,
                version: row.get("version"),
                updated_at: row.get("updated_at"),
                updated_by: row.get("updated_by"),
            })
        })
        .collect()
}

pub(super) fn validate_candidate(
    settings: &[RuntimeSetting],
    key: &str,
    value: i64,
) -> Result<(), AdminError> {
    let selected = definition(key).ok_or(AdminError::Validation)?;
    if value < selected.minimum {
        return Err(AdminError::Validation);
    }

    let mut values = settings
        .iter()
        .map(|setting| (setting.key.as_str(), setting.value))
        .collect::<BTreeMap<_, _>>();
    values.insert(key, value);

    let poll = values
        .get("scheduler.poll_interval_seconds")
        .copied()
        .ok_or(AdminError::Validation)?;
    let lease = values
        .get("scheduler.lease_seconds")
        .copied()
        .ok_or(AdminError::Validation)?;
    if lease <= poll.saturating_mul(2) {
        return Err(AdminError::Validation);
    }

    usize::try_from(values["adapters.max_concurrency"])
        .map(|_| ())
        .map_err(|_| AdminError::Validation)
}

pub(super) fn updated(
    settings: &[RuntimeSetting],
    key: &str,
    value: i64,
    actor_id: &str,
    occurred_at: String,
) -> Result<RuntimeSetting, AdminError> {
    let mut setting = settings
        .iter()
        .find(|setting| setting.key == key)
        .cloned()
        .ok_or(AdminError::Validation)?;
    setting.value = value;
    setting.version += 1;
    setting.updated_at = occurred_at;
    setting.updated_by = actor_id.to_owned();
    Ok(setting)
}

fn database_error(_: DatabaseError) -> AdminError {
    AdminError::Database
}
