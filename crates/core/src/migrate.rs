use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::error::Error;
use crate::schema::{
    GUARD_MESSAGE, GUARD_NAMES, GUARDS, MIGRATE_V1_TO_V2, MIGRATE_V2_TO_V3, MIGRATE_V3_TO_V4,
    MIGRATE_V5_TO_V6, MIGRATE_V6_TO_V7, MIGRATE_V7_TO_V8, SCHEMA, SCHEMA_VERSION,
};

pub(crate) fn migrate(conn: &mut Connection) -> Result<(), Error> {
    let version = user_version(conn)?;
    if version > SCHEMA_VERSION {
        return Err(newer_ledger());
    }
    if version == SCHEMA_VERSION {
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
        apply_current_schema(conn)?;
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
    if version < 6 && table_exists(&tx, "events")? {
        tx.execute_batch(MIGRATE_V5_TO_V6)?;
    }
    if version < 7 {
        if table_exists(&tx, "reports")? && !column_exists(&tx, "reports", "commit_sha")? {
            tx.execute("ALTER TABLE reports ADD COLUMN commit_sha TEXT", [])?;
        }
        if table_exists(&tx, "reports")? && !column_exists(&tx, "reports", "base_sha")? {
            tx.execute("ALTER TABLE reports ADD COLUMN base_sha TEXT", [])?;
        }
        if table_exists(&tx, "reports")? && !column_exists(&tx, "reports", "graph_sha")? {
            tx.execute("ALTER TABLE reports ADD COLUMN graph_sha TEXT", [])?;
        }
        tx.execute_batch(MIGRATE_V6_TO_V7)?;
    }
    if version < 8 {
        if table_exists(&tx, "runs")? && !column_exists(&tx, "runs", "workflow")? {
            tx.execute("ALTER TABLE runs ADD COLUMN workflow TEXT", [])?;
        }
        if table_exists(&tx, "runs")? && !column_exists(&tx, "runs", "write_stamp")? {
            tx.execute(
                "ALTER TABLE runs ADD COLUMN write_stamp INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if table_exists(&tx, "release_authorizations")?
            && !column_exists(&tx, "release_authorizations", "method")?
        {
            tx.execute(
                "ALTER TABLE release_authorizations ADD COLUMN method TEXT",
                [],
            )?;
        }
        if table_exists(&tx, "release_authorizations")?
            && !column_exists(&tx, "release_authorizations", "evidence")?
        {
            tx.execute(
                "ALTER TABLE release_authorizations ADD COLUMN evidence TEXT",
                [],
            )?;
        }
        tx.execute_batch(MIGRATE_V7_TO_V8)?;
        tx.execute_batch(GUARDS)?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn apply_current_schema(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(SCHEMA)?;
    conn.execute_batch(GUARDS)?;
    Ok(())
}

pub(crate) fn user_version(conn: &Connection) -> Result<i64, Error> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

pub(crate) fn newer_ledger() -> Error {
    Error::InvalidSetting("this ledger needs a newer relaybook".to_string())
}

/// Recreate any missing schema-8 trigger. Returns whether one was missing.
pub(crate) fn ensure_guards(conn: &Connection) -> Result<bool, Error> {
    debug_assert!(
        GUARDS.contains(GUARD_MESSAGE),
        "guard SQL must raise {GUARD_MESSAGE}"
    );
    let mut missing = false;
    for name in GUARD_NAMES {
        if !trigger_exists(conn, name)? {
            missing = true;
            break;
        }
    }
    if missing {
        conn.execute_batch(GUARDS)?;
    }
    Ok(missing)
}

fn trigger_exists(conn: &Connection, name: &str) -> Result<bool, Error> {
    let found = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            [name],
            |_| Ok(1_i64),
        )
        .optional()?;
    Ok(found.is_some())
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
