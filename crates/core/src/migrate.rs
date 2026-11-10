use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::error::Error;
use crate::schema::{MIGRATE_V1_TO_V2, MIGRATE_V2_TO_V3, MIGRATE_V3_TO_V4, SCHEMA, SCHEMA_VERSION};

pub(crate) fn migrate(conn: &mut Connection) -> Result<(), Error> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    let has_events = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'events'",
            [],
            |_| Ok(1_i64),
        )
        .optional()?
        .is_some();
    if !has_events {
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if version < 2 {
        tx.execute_batch(MIGRATE_V1_TO_V2)?;
    }
    if version < 3 {
        tx.execute_batch(MIGRATE_V2_TO_V3)?;
    }
    if version < 4 {
        tx.execute_batch(MIGRATE_V3_TO_V4)?;
    }
    if version < 5 {
        if table_exists(&tx, "tasks")? && !column_exists(&tx, "tasks", "run_id")? {
            tx.execute("ALTER TABLE tasks ADD COLUMN run_id INTEGER", [])?;
        }
        if table_exists(&tx, "runs")? && !column_exists(&tx, "runs", "failure_streak")? {
            tx.execute_batch(
                "ALTER TABLE runs ADD COLUMN failure_streak INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE runs ADD COLUMN current_slug TEXT;
                 ALTER TABLE runs ADD COLUMN complete_requested INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool, Error> {
    let found = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(1_i64),
        )
        .optional()?;
    Ok(found.is_some())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, Error> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}
