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
    transaction.commit().await?;
    Ok(())
}

fn migration_identity(migration: &Migration) -> String {
    let digest = Sha256::digest(migration.sql.as_bytes());
    format!("{}:{digest:x}", migration.name)
}
