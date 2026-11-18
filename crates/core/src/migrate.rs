use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::error::Error;
use crate::schema::{
    GUARD_MESSAGE, GUARD_NAMES, GUARDS, MIGRATE_V1_TO_V2, MIGRATE_V2_TO_V3, MIGRATE_V3_TO_V4,
    MIGRATE_V5_TO_V6, MIGRATE_V6_TO_V7, MIGRATE_V7_TO_V8, ONE_LIVE_GATED_INDEX,
    REREVIEW_AUTHORIZE_TRIGGER, REVIEW_INVOCATION_INDEX, REVIEWER_EVENTS_TABLE, SCHEMA,
    SCHEMA_VERSION,
};

pub(crate) fn migrate(conn: &mut Connection) -> Result<(), Error> {
    // This read only skips the write lock when the ledger is already current
    // or already newer. An older ledger is decided again after BEGIN IMMEDIATE.
    // Another connection can commit a newer user_version while this one waits,
    // and migration must use that locked value. Writing SCHEMA_VERSION from
    // the earlier read would relabel the newer ledger.
    let observed = user_version(conn)?;
    if observed > SCHEMA_VERSION {
        return Err(newer_ledger());
    }
    if observed == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    migrate_locked(&tx)?;
    tx.commit()?;
    Ok(())
}

fn migrate_locked(conn: &Connection) -> Result<(), Error> {
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
    if version < 2 {
        conn.execute_batch(MIGRATE_V1_TO_V2)?;
    }
    if version < 3 {
        conn.execute_batch(MIGRATE_V2_TO_V3)?;
    }
    if version < 4 {
        conn.execute_batch(MIGRATE_V3_TO_V4)?;
    }
    if version < 5 {
        if table_exists(conn, "tasks")? && !column_exists(conn, "tasks", "run_id")? {
            conn.execute("ALTER TABLE tasks ADD COLUMN run_id INTEGER", [])?;
        }
        if table_exists(conn, "runs")? && !column_exists(conn, "runs", "failure_streak")? {
            conn.execute_batch(
                "ALTER TABLE runs ADD COLUMN failure_streak INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE runs ADD COLUMN current_slug TEXT;
                 ALTER TABLE runs ADD COLUMN complete_requested INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
    }
    if version < 6 && table_exists(conn, "events")? {
        conn.execute_batch(MIGRATE_V5_TO_V6)?;
    }
    if version < 7 {
        if table_exists(conn, "reports")? && !column_exists(conn, "reports", "commit_sha")? {
            conn.execute("ALTER TABLE reports ADD COLUMN commit_sha TEXT", [])?;
        }
        if table_exists(conn, "reports")? && !column_exists(conn, "reports", "base_sha")? {
            conn.execute("ALTER TABLE reports ADD COLUMN base_sha TEXT", [])?;
        }
        if table_exists(conn, "reports")? && !column_exists(conn, "reports", "graph_sha")? {
            conn.execute("ALTER TABLE reports ADD COLUMN graph_sha TEXT", [])?;
        }
        conn.execute_batch(MIGRATE_V6_TO_V7)?;
    }
    if version < 8 {
        if table_exists(conn, "runs")? && !column_exists(conn, "runs", "workflow")? {
            conn.execute("ALTER TABLE runs ADD COLUMN workflow TEXT", [])?;
        }
        if table_exists(conn, "runs")? && !column_exists(conn, "runs", "write_stamp")? {
            conn.execute(
                "ALTER TABLE runs ADD COLUMN write_stamp INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if table_exists(conn, "release_authorizations")?
            && !column_exists(conn, "release_authorizations", "method")?
        {
            conn.execute(
                "ALTER TABLE release_authorizations ADD COLUMN method TEXT",
                [],
            )?;
        }
        if table_exists(conn, "release_authorizations")?
            && !column_exists(conn, "release_authorizations", "evidence")?
        {
            conn.execute(
                "ALTER TABLE release_authorizations ADD COLUMN evidence TEXT",
                [],
            )?;
        }
        conn.execute_batch(MIGRATE_V7_TO_V8)?;
        conn.execute_batch(GUARDS)?;
        ensure_review_invocation_token(conn)?;
        ensure_reviewer_fallback(conn)?;
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

pub(crate) fn apply_current_schema(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(SCHEMA)?;
    conn.execute_batch(GUARDS)?;
    conn.execute_batch(REVIEWER_EVENTS_TABLE)?;
    conn.execute_batch(REREVIEW_AUTHORIZE_TRIGGER)?;
    Ok(())
}

pub(crate) fn user_version(conn: &Connection) -> Result<i64, Error> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

pub(crate) fn newer_ledger() -> Error {
    Error::InvalidSetting("this ledger needs a newer relaybook".to_string())
}

/// Install the one-live-gated-run index. `IF NOT EXISTS` keeps an existing
/// schema 8 ledger at the same `user_version`.
pub(crate) fn ensure_one_live_gated(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(ONE_LIVE_GATED_INDEX)?;
    Ok(())
}

/// Add `reviews.invocation_token` and its partial unique index.
///
/// `user_version` stays 8. A ledger already at schema 8 gets the column on
/// the next read-write open, and the v7-to-v8 migration adds it too.
///
/// The restore takes `BEGIN IMMEDIATE` when the column or index is missing
/// and this connection is not already in a transaction, then checks both
/// again. Two opens that both saw the column missing wait on that lock
/// instead of both running `ALTER`. A ledger that already has both does
/// not take the write lock.
pub(crate) fn ensure_review_invocation_token(conn: &Connection) -> Result<(), Error> {
    // Skip the write lock when a current ledger already has both. The check
    // runs again after BEGIN IMMEDIATE, so two opens that both saw them
    // missing cannot both ALTER.
    if !invocation_token_restore_needed(conn)? {
        return Ok(());
    }
    if conn.is_autocommit() {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = restore_review_invocation_token(conn);
        return match result {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(err)
            }
        };
    }
    restore_review_invocation_token(conn)
}

fn invocation_token_restore_needed(conn: &Connection) -> Result<bool, Error> {
    if !table_exists(conn, "reviews")? {
        return Ok(false);
    }
    Ok(!column_exists(conn, "reviews", "invocation_token")?
        || !index_exists(conn, "reviews_invocation_token")?)
}

fn restore_review_invocation_token(conn: &Connection) -> Result<(), Error> {
    if table_exists(conn, "reviews")? && !column_exists(conn, "reviews", "invocation_token")? {
        pause_before_adding_invocation_token(conn);
        conn.execute("ALTER TABLE reviews ADD COLUMN invocation_token TEXT", [])?;
    }
    if table_exists(conn, "reviews")? && !index_exists(conn, "reviews_invocation_token")? {
        conn.execute_batch(REVIEW_INVOCATION_INDEX)?;
    }
    Ok(())
}

/// Add the reviewer-fallback columns, event log, and re-review trigger.
///
/// `user_version` stays 8. A ledger already at schema 8 gains them on the
/// next read-write open. The v7-to-v8 migration adds them too.
///
/// The restore takes `BEGIN IMMEDIATE` when something is missing and this
/// connection is not already in a transaction, then checks again. Two opens
/// that both saw a column missing wait on that lock instead of both running
/// `ALTER`.
pub(crate) fn ensure_reviewer_fallback(conn: &Connection) -> Result<(), Error> {
    if !reviewer_fallback_needed(conn)? {
        return Ok(());
    }
    if conn.is_autocommit() {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = restore_reviewer_fallback(conn);
        return match result {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(err)
            }
        };
    }
    restore_reviewer_fallback(conn)
}

fn reviewer_fallback_needed(conn: &Connection) -> Result<bool, Error> {
    if table_exists(conn, "reviews")? {
        for column in ["reviewer_model", "fallback_reason", "rereview_of"] {
            if !column_exists(conn, "reviews", column)? {
                return Ok(true);
            }
        }
    }
    if !table_exists(conn, "reviewer_events")? {
        return Ok(true);
    }
    Ok(!trigger_exists(conn, "relaybook_guard_rereview_authorize")?)
}

fn restore_reviewer_fallback(conn: &Connection) -> Result<(), Error> {
    if table_exists(conn, "reviews")? && !column_exists(conn, "reviews", "reviewer_model")? {
        conn.execute("ALTER TABLE reviews ADD COLUMN reviewer_model TEXT", [])?;
    }
    if table_exists(conn, "reviews")? && !column_exists(conn, "reviews", "fallback_reason")? {
        conn.execute("ALTER TABLE reviews ADD COLUMN fallback_reason TEXT", [])?;
    }
    if table_exists(conn, "reviews")? && !column_exists(conn, "reviews", "rereview_of")? {
        conn.execute(
            "ALTER TABLE reviews ADD COLUMN rereview_of INTEGER REFERENCES reviews(id)",
            [],
        )?;
    }
    if !table_exists(conn, "reviewer_events")? {
        conn.execute_batch(REVIEWER_EVENTS_TABLE)?;
    }
    if !trigger_exists(conn, "relaybook_guard_rereview_authorize")? {
        conn.execute_batch(REREVIEW_AUTHORIZE_TRIGGER)?;
    }
    Ok(())
}

fn index_exists(conn: &Connection, name: &str) -> Result<bool, Error> {
    let found = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [name],
            |_| Ok(1_i64),
        )
        .optional()?;
    Ok(found.is_some())
}

#[cfg(test)]
struct TokenRestoreHold {
    path: String,
    seen: std::sync::atomic::AtomicUsize,
    release: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
fn token_restore_hold() -> &'static std::sync::Mutex<Option<TokenRestoreHold>> {
    static HOLD: std::sync::Mutex<Option<TokenRestoreHold>> = std::sync::Mutex::new(None);
    &HOLD
}

fn pause_before_adding_invocation_token(conn: &Connection) {
    #[cfg(test)]
    {
        let Some(path) = conn.path() else {
            return;
        };
        let path = path.to_string();
        {
            let hold = token_restore_hold().lock().expect("token restore hold");
            let Some(hold) = hold.as_ref() else {
                return;
            };
            if hold.path != path {
                return;
            }
            hold.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        for _ in 0..400 {
            let released = token_restore_hold()
                .lock()
                .expect("token restore hold")
                .as_ref()
                .is_none_or(|hold| hold.release.load(std::sync::atomic::Ordering::SeqCst));
            if released {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[cfg(not(test))]
    {
        let _ = conn;
    }
}

/// Recreate any missing schema-8 trigger. Returns whether one was missing.
/// Also installs [`ONE_LIVE_GATED_INDEX`] so a ledger already at version 8
/// gains it on the next read-write open.
pub(crate) fn ensure_guards(conn: &Connection) -> Result<bool, Error> {
    debug_assert!(
        GUARDS.contains(GUARD_MESSAGE),
        "guard SQL must raise {GUARD_MESSAGE}"
    );
    ensure_one_live_gated(conn)?;
    ensure_review_invocation_token(conn)?;
    ensure_reviewer_fallback(conn)?;
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::{migrate, user_version};
    use crate::error::Error;

    fn file() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ledger.db");
        (dir, path)
    }

    fn version_of(path: &Path) -> i64 {
        let conn = Connection::open(path).expect("open");
        user_version(&conn).expect("version")
    }

    fn trigger_count(path: &Path, name: &str) -> i64 {
        let conn = Connection::open(path).expect("open");
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            [name],
            |row| row.get(0),
        )
        .expect("triggers")
    }

    /// Hold the write lock until `migrate` blocks on it, then publish a newer
    /// `user_version` and let the migrator continue.
    fn migrate_after_publishing(
        path: &Path,
        publish: impl FnOnce(&Connection),
    ) -> Result<(), Error> {
        let holder = Connection::open(path).expect("holder");
        holder.execute_batch("BEGIN IMMEDIATE").expect("lock");
        let (blocked_tx, blocked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let opened = path.to_path_buf();
        let handle = thread::spawn(move || {
            let mut conn = Connection::open(&opened).expect("migrator");
            let mut signaled = false;
            conn.busy_handler(Some(move |_| {
                if !signaled {
                    signaled = true;
                    let _ = blocked_tx.send(());
                    let _ = release_rx.recv_timeout(Duration::from_secs(5));
                    return true;
                }
                false
            }))
            .expect("busy handler");
            migrate(&mut conn)
        });
        blocked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("migrator waited for the write lock");
        publish(&holder);
        holder.execute_batch("COMMIT").expect("publish");
        release_tx.send(()).expect("release");
        handle.join().expect("migrator thread")
    }

    #[test]
    fn migrate_refuses_newer_version_published_while_waiting_for_the_write_lock() {
        let (_dir, path) = file();
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE events (
                    id INTEGER PRIMARY KEY,
                    ts TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    payload TEXT NOT NULL
                 );
                 CREATE TABLE reports (
                    id INTEGER PRIMARY KEY,
                    task_id INTEGER,
                    kind TEXT NOT NULL,
                    author TEXT NOT NULL,
                    body TEXT NOT NULL,
                    bytes INTEGER NOT NULL,
                    created_at TEXT NOT NULL,
                    commit_sha TEXT,
                    verdict TEXT
                 );
                 CREATE TABLE runs (
                    id INTEGER PRIMARY KEY,
                    prd_path TEXT NOT NULL,
                    prd_sha256 TEXT NOT NULL,
                    branch TEXT NOT NULL,
                    stage TEXT NOT NULL,
                    cycle INTEGER NOT NULL,
                    agent_calls INTEGER NOT NULL,
                    est_cost_usd REAL NOT NULL DEFAULT 0,
                    status TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    failure_streak INTEGER NOT NULL DEFAULT 0,
                    complete_requested INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE settings (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    updated_by TEXT NOT NULL
                 );
                 CREATE TABLE reviews (
                    id INTEGER PRIMARY KEY,
                    scope TEXT NOT NULL,
                    task_id INTEGER,
                    verdict TEXT,
                    commit_sha TEXT NOT NULL
                 );
                 INSERT INTO events (id, ts, kind, payload)
                 VALUES (1, '2026-01-01T00:00:00.000Z', 'task_created', '{\"slug\":\"auth\"}');",
            )
            .expect("v7 schema");
            conn.pragma_update(None, "user_version", 7)
                .expect("version");
        }
        let result = migrate_after_publishing(&path, |holder| {
            holder
                .pragma_update(None, "user_version", 9)
                .expect("newer");
        });
        assert_eq!(
            result.expect_err("newer ledger").to_string(),
            "this ledger needs a newer relaybook"
        );
        assert_eq!(version_of(&path), 9);
        assert_eq!(trigger_count(&path, "relaybook_guard_run_update"), 0);
        let conn = Connection::open(&path).expect("open");
        let payload: String = conn
            .query_row("SELECT payload FROM events WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("event");
        assert_eq!(payload, r#"{"slug":"auth"}"#);
    }

    #[test]
    fn migrate_refuses_newer_version_on_the_empty_schema_path() {
        let (_dir, path) = file();
        {
            let conn = Connection::open(&path).expect("create");
            assert_eq!(user_version(&conn).expect("version"), 0);
        }
        let result = migrate_after_publishing(&path, |holder| {
            holder
                .pragma_update(None, "user_version", 9)
                .expect("newer");
            holder
                .execute(
                    "CREATE TABLE events (
                        id INTEGER PRIMARY KEY,
                        ts TEXT NOT NULL,
                        kind TEXT NOT NULL,
                        payload TEXT NOT NULL
                     )",
                    [],
                )
                .expect("events");
            holder
                .execute(
                    "INSERT INTO events (id, ts, kind, payload) VALUES (1, 't', 'task_created', '{}')",
                    [],
                )
                .expect("row");
        });
        assert_eq!(
            result.expect_err("newer ledger").to_string(),
            "this ledger needs a newer relaybook"
        );
        assert_eq!(version_of(&path), 9);
        assert_eq!(
            trigger_count(&path, "relaybook_guard_task_report_insert"),
            0
        );
        let conn = Connection::open(&path).expect("open");
        let payload: String = conn
            .query_row("SELECT payload FROM events WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("event");
        assert_eq!(payload, "{}");
    }

    #[test]
    fn concurrent_v8_opens_restore_invocation_token_once() {
        use std::sync::atomic::Ordering;

        use crate::ledger::Ledger;

        let (_dir, path) = file();
        drop(Ledger::initialize(&path).expect("init"));
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute("DROP INDEX reviews_invocation_token", [])
                .expect("drop index");
            conn.execute("ALTER TABLE reviews DROP COLUMN invocation_token", [])
                .expect("drop column");
        }
        let path_text = path.to_string_lossy().to_string();
        {
            let mut hold = super::token_restore_hold().lock().expect("hold");
            *hold = Some(super::TokenRestoreHold {
                path: path_text,
                seen: std::sync::atomic::AtomicUsize::new(0),
                release: std::sync::atomic::AtomicBool::new(false),
            });
        }
        struct ClearHold;
        impl Drop for ClearHold {
            fn drop(&mut self) {
                if let Ok(mut hold) = super::token_restore_hold().lock() {
                    *hold = None;
                }
            }
        }
        let _clear = ClearHold;

        let mut handles = Vec::new();
        for _ in 0..2 {
            let opened = path.clone();
            handles.push(thread::spawn(move || Ledger::open(&opened)));
        }
        let started = std::time::Instant::now();
        loop {
            let seen = super::token_restore_hold()
                .lock()
                .expect("hold")
                .as_ref()
                .expect("hold armed")
                .seen
                .load(Ordering::SeqCst);
            if seen >= 1 || started.elapsed() > Duration::from_secs(5) {
                thread::sleep(Duration::from_millis(200));
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        super::token_restore_hold()
            .lock()
            .expect("hold")
            .as_mut()
            .expect("hold armed")
            .release
            .store(true, Ordering::SeqCst);

        for handle in handles {
            handle.join().expect("thread").expect("concurrent open");
        }
        assert_eq!(version_of(&path), 8);
        let conn = Connection::open(&path).expect("open");
        let columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('reviews') WHERE name = 'invocation_token'",
                [],
                |row| row.get(0),
            )
            .expect("column");
        let indexes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'reviews_invocation_token'",
                [],
                |row| row.get(0),
            )
            .expect("index");
        assert_eq!(columns, 1);
        assert_eq!(indexes, 1);
    }
}
