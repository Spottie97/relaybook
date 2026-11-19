use std::path::{Path, PathBuf};

use relaybook_core::{Error, Ledger, Status};
use rusqlite::{Connection, Transaction, TransactionBehavior};

const GUARD: &str = "workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)";

fn task_review_body() -> &'static str {
    r#"{"findings":[],"security":"ok","tests_checked":[],"not_checked":[]}"#
}

fn release_review_body() -> &'static str {
    r#"{"findings":[],"security":"ok","tests_checked":[],"not_checked":[],"prd_covered":true,"gaps":[]}"#
}

fn fresh_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    (dir, path)
}

fn user_version(path: &Path) -> i64 {
    let conn = Connection::open(path).expect("open");
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version")
}

fn column_names(path: &Path, table: &str) -> Vec<String> {
    let conn = Connection::open(path).expect("open");
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .expect("columns");
    rows.map(|row| row.expect("column")).collect()
}

fn trigger_names(path: &Path) -> Vec<String> {
    let conn = Connection::open(path).expect("open");
    let mut statement = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'trigger' ORDER BY name")
        .expect("prepare");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("triggers");
    rows.map(|row| row.expect("trigger")).collect()
}

fn task_status(path: &Path, slug: &str) -> String {
    let conn = Connection::open(path).expect("open");
    conn.query_row("SELECT status FROM tasks WHERE slug = ?1", [slug], |row| {
        row.get(0)
    })
    .expect("status")
}

fn report_count(path: &Path) -> i64 {
    let conn = Connection::open(path).expect("open");
    conn.query_row("SELECT COUNT(*) FROM reports", [], |row| row.get(0))
        .expect("reports")
}

fn report_verdict(path: &Path) -> Option<String> {
    let conn = Connection::open(path).expect("open");
    conn.query_row(
        "SELECT verdict FROM reports WHERE kind = 'task' ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get(0),
    )
    .expect("verdict")
}

fn run_count(path: &Path) -> i64 {
    let conn = Connection::open(path).expect("open");
    conn.query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .expect("runs")
}

fn assert_guard(err: Error) {
    assert_eq!(err.to_string(), format!("sqlite: {GUARD}"));
}

fn assert_rusqlite_guard(err: rusqlite::Error) {
    assert_guard(Error::from(err));
}

fn refused(conn: &mut Connection, body: impl FnOnce(&Transaction) -> Result<(), rusqlite::Error>) {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("begin");
    let err = body(&tx).expect_err("refused");
    assert_rusqlite_guard(err);
    tx.rollback().expect("rollback");
}

fn gate(ledger: &mut Ledger) {
    ledger.set_setting("reviewer", "codex").expect("reviewer");
    ledger.set_setting("workflow", "gated").expect("workflow");
}

fn record_task(ledger: &mut Ledger, slug: &str, sha: &str) {
    ledger
        .start_review(
            "task",
            Some(slug),
            None,
            "feature",
            "codex",
            None,
            sha,
            None,
            "fp",
        )
        .expect("start");
    ledger
        .record_review(
            "task",
            Some(slug),
            "codex",
            sha,
            sha,
            "fp",
            "approved",
            task_review_body(),
            false,
        )
        .expect("record");
}

fn record_release(ledger: &mut Ledger, sha: &str) {
    ledger
        .start_review(
            "release", None, None, "feature", "codex", None, sha, None, "fp",
        )
        .expect("start");
    ledger
        .record_review(
            "release",
            None,
            "codex",
            sha,
            sha,
            "fp",
            "approved",
            release_review_body(),
            false,
        )
        .expect("record");
}

#[test]
fn fresh_ledger_is_v8_with_guards() {
    let (_dir, path) = fresh_path();
    let ledger = Ledger::initialize(&path).expect("init");
    drop(ledger);
    assert_eq!(user_version(&path), 8);
    let runs = column_names(&path, "runs");
    assert!(runs.iter().any(|name| name == "workflow"));
    assert!(runs.iter().any(|name| name == "write_stamp"));
    let auth = column_names(&path, "release_authorizations");
    assert!(auth.iter().any(|name| name == "method"));
    assert!(auth.iter().any(|name| name == "evidence"));
    let conn = Connection::open(&path).expect("open");
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal");
    assert_eq!(journal, "wal");
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'remote_checks'",
            [],
            |row| row.get(0),
        )
        .expect("remote_checks");
    assert_eq!(tables, 1);
    conn.execute(
        "INSERT INTO remote_checks (context, branch, head_sha, status, override, created_at)
         VALUES ('publish', 'main', 'abc', 'present', 0, '2026-01-01T00:00:00.000Z')",
        [],
    )
    .expect("valid remote check");
    assert!(
        conn.execute(
            "INSERT INTO remote_checks (context, branch, head_sha, status, created_at)
             VALUES ('publish', 'main', 'abc', 'nope', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO remote_checks (context, branch, head_sha, status, override, created_at)
             VALUES ('publish', 'main', 'abc', 'local', 2, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .is_err()
    );
    drop(conn);
    let names = trigger_names(&path);
    assert_eq!(
        names,
        vec![
            "relaybook_guard_claim_base_sha".to_string(),
            "relaybook_guard_rereview_authorize".to_string(),
            "relaybook_guard_rereview_publish".to_string(),
            "relaybook_guard_run_insert".to_string(),
            "relaybook_guard_run_update".to_string(),
            "relaybook_guard_task_accept".to_string(),
            "relaybook_guard_task_report_insert".to_string(),
        ]
    );
    let conn = Connection::open(&path).expect("open");
    for name in &names {
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
                [name],
                |row| row.get(0),
            )
            .expect("sql");
        if name == "relaybook_guard_rereview_authorize"
            || name == "relaybook_guard_rereview_publish"
        {
            assert!(sql.contains("codex re-review pending"), "{name}");
            assert!(sql.contains("selected.run_id"), "{name}");
        } else {
            assert!(sql.contains(GUARD), "{name}");
        }
        assert!(sql.contains("SELECT RAISE"), "{name}");
    }
}

#[test]
fn existing_v8_ledger_gains_the_live_run_index_without_a_version_bump() {
    let (_dir, path) = fresh_path();
    drop(Ledger::initialize(&path).expect("init"));
    assert_eq!(user_version(&path), 8);
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute("DROP INDEX runs_one_live_gated", [])
            .expect("drop");
        let missing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'runs_one_live_gated'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(missing, 0);
    }
    let ledger = Ledger::open(&path).expect("reopen");
    assert!(!ledger.guards_restored());
    drop(ledger);
    assert_eq!(user_version(&path), 8);
    let conn = Connection::open(&path).expect("open");
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'runs_one_live_gated'",
            [],
            |row| row.get(0),
        )
        .expect("index");
    assert!(sql.contains("workflow = 'gated'"), "{sql}");
    assert!(sql.contains("stage = 'publishing'"), "{sql}");
}

#[test]
fn existing_v8_ledger_gains_the_review_token_without_a_version_bump() {
    let (_dir, path) = fresh_path();
    drop(Ledger::initialize(&path).expect("init"));
    assert_eq!(user_version(&path), 8);
    assert!(column_names(&path, "reviews").contains(&"invocation_token".to_string()));
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute("DROP INDEX reviews_invocation_token", [])
            .expect("drop index");
        conn.execute("ALTER TABLE reviews DROP COLUMN invocation_token", [])
            .expect("drop column");
    }
    assert!(!column_names(&path, "reviews").contains(&"invocation_token".to_string()));
    let ledger = Ledger::open(&path).expect("reopen");
    assert!(!ledger.guards_restored());
    drop(ledger);
    assert_eq!(user_version(&path), 8);
    assert!(column_names(&path, "reviews").contains(&"invocation_token".to_string()));
    let conn = Connection::open(&path).expect("open");
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'reviews_invocation_token'",
            [],
            |row| row.get(0),
        )
        .expect("index");
    assert!(sql.contains("invocation_token IS NOT NULL"), "{sql}");
    drop(conn);
    drop(Ledger::open(&path).expect("second open"));
    assert_eq!(user_version(&path), 8);
}

#[test]
fn v7_ledger_migrates_to_v8_with_payloads_unchanged() {
    let (_dir, path) = fresh_path();
    let event = r#"{"slug":"auth","title":"Add auth"}"#;
    let body = r#"{"changed":["schema"]}"#;
    let review = r#"{"findings":[]}"#;
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "
            CREATE TABLE tasks (
                id INTEGER PRIMARY KEY,
                slug TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                status TEXT NOT NULL,
                claimed_by TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE events (
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
                base_sha TEXT,
                graph_sha TEXT
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
            CREATE TABLE reviews (
                id INTEGER PRIMARY KEY,
                scope TEXT NOT NULL,
                task_id INTEGER,
                branch TEXT NOT NULL,
                reviewer TEXT NOT NULL,
                reviewer_override INTEGER NOT NULL DEFAULT 0,
                commit_sha TEXT NOT NULL,
                ledger_mark TEXT NOT NULL,
                verdict TEXT,
                body TEXT,
                started_at TEXT NOT NULL
            );
            CREATE TABLE release_authorizations (
                id INTEGER PRIMARY KEY,
                branch TEXT NOT NULL,
                commit_sha TEXT NOT NULL,
                review_id INTEGER NOT NULL,
                authorized_by TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                updated_by TEXT NOT NULL
            );
            ",
        )
        .expect("v7 schema");
        conn.pragma_update(None, "user_version", 7)
            .expect("version");
        conn.execute(
            "INSERT INTO tasks (id, slug, title, status, created_at, updated_at)
             VALUES (1, 'auth', 'Add auth', 'done', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .expect("task");
        conn.execute(
            "INSERT INTO events (id, ts, kind, payload) VALUES (7, '2026-01-01T00:00:00.000Z', 'task_created', ?1)",
            [event],
        )
        .expect("event");
        conn.execute(
            "INSERT INTO reports (id, task_id, kind, author, body, bytes, created_at, commit_sha)
             VALUES (1, 1, 'task', 'cursor', ?1, ?2, '2026-01-01T00:00:00.000Z', 'abc')",
            rusqlite::params![body, body.len() as i64],
        )
        .expect("report");
        conn.execute(
            "INSERT INTO runs (id, prd_path, prd_sha256, branch, stage, cycle, agent_calls, status, started_at)
             VALUES (1, 'prd.md', 'digest', 'feature', 'plan', 0, 0, 'paused', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .expect("run");
        conn.execute(
            "INSERT INTO reviews (
                id, scope, task_id, branch, reviewer, commit_sha, ledger_mark, verdict, body, started_at
             ) VALUES (1, 'task', 1, 'feature', 'codex', 'abc', 'mark', 'approved', ?1, '2026-01-01T00:00:00.000Z')",
            [review],
        )
        .expect("review");
        conn.execute(
            "INSERT INTO release_authorizations (id, branch, commit_sha, review_id, authorized_by, created_at)
             VALUES (1, 'feature', 'abc', 1, 'maintainer', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .expect("authorization");
        conn.execute(
            "INSERT INTO settings (key, value, updated_at, updated_by)
             VALUES ('workflow', 'gated', '2026-01-01T00:00:00.000Z', 'orca')",
            [],
        )
        .expect("setting");
    }
    let ledger = Ledger::open(&path).expect("migrate");
    drop(ledger);
    assert_eq!(user_version(&path), 8);
    assert_eq!(trigger_names(&path).len(), 7);
    let conn = Connection::open(&path).expect("reopen");
    let stored_event: String = conn
        .query_row("SELECT payload FROM events WHERE id = 7", [], |row| {
            row.get(0)
        })
        .expect("event");
    assert_eq!(stored_event, event);
    let stored_report: (String, Option<String>) = conn
        .query_row(
            "SELECT body, commit_sha FROM reports WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("report");
    assert_eq!(stored_report.0, body);
    assert_eq!(stored_report.1.as_deref(), Some("abc"));
    let stored_run: (String, String, Option<String>, i64) = conn
        .query_row(
            "SELECT prd_path, status, workflow, write_stamp FROM runs WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("run");
    assert_eq!(stored_run.0, "prd.md");
    assert_eq!(stored_run.1, "paused");
    assert_eq!(stored_run.2, None);
    assert_eq!(stored_run.3, 0);
    let stored_review: (String, Option<String>) = conn
        .query_row(
            "SELECT body, invocation_token FROM reviews WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("review");
    assert_eq!(stored_review.0, review);
    assert_eq!(stored_review.1, None);
    let stored_auth: (String, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT authorized_by, method, evidence FROM release_authorizations WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("authorization");
    assert_eq!(stored_auth.0, "maintainer");
    assert_eq!(stored_auth.1, None);
    assert_eq!(stored_auth.2, None);
    let workflow: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'workflow'",
            [],
            |row| row.get(0),
        )
        .expect("workflow");
    assert_eq!(workflow, "gated");
    let checks: i64 = conn
        .query_row("SELECT COUNT(*) FROM remote_checks", [], |row| row.get(0))
        .expect("checks");
    assert_eq!(checks, 0);
}

#[test]
fn v1_and_v6_migrate_to_v8() {
    let (_dir, path) = fresh_path();
    let payload = r#"{"slug":"auth","title":"Add auth"}"#;
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "
            CREATE TABLE tasks (
                id INTEGER PRIMARY KEY,
                slug TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                status TEXT NOT NULL,
                claimed_by TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE events (
                id INTEGER PRIMARY KEY,
                ts TEXT NOT NULL,
                kind TEXT NOT NULL CHECK (
                    kind IN ('task_created', 'claimed', 'progress', 'decision', 'status_changed')
                ),
                payload TEXT NOT NULL
            );
            ",
        )
        .expect("v1 schema");
        conn.pragma_update(None, "user_version", 1)
            .expect("version");
        conn.execute(
            "INSERT INTO events (id, ts, kind, payload)
             VALUES (7, '2026-01-01T00:00:00.000Z', 'task_created', ?1)",
            [payload],
        )
        .expect("event");
    }
    drop(Ledger::open(&path).expect("migrate v1"));
    assert_eq!(user_version(&path), 8);
    assert_eq!(trigger_names(&path).len(), 7);
    let conn = Connection::open(&path).expect("reopen");
    let stored: String = conn
        .query_row("SELECT payload FROM events WHERE id = 7", [], |row| {
            row.get(0)
        })
        .expect("event");
    assert_eq!(stored, payload);
    drop(conn);

    let (_dir, path) = fresh_path();
    let body = r#"{"text":"note"}"#;
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "
            CREATE TABLE events (
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
                created_at TEXT NOT NULL
            );
            ",
        )
        .expect("v6 schema");
        conn.pragma_update(None, "user_version", 6)
            .expect("version");
        conn.execute(
            "INSERT INTO events (id, ts, kind, payload)
             VALUES (3, '2026-01-01T00:00:00.000Z', 'task_created', ?1)",
            [payload],
        )
        .expect("event");
        conn.execute(
            "INSERT INTO reports (id, task_id, kind, author, body, bytes, created_at)
             VALUES (1, NULL, 'note', 'orca', ?1, ?2, '2026-01-01T00:00:00.000Z')",
            rusqlite::params![body, body.len() as i64],
        )
        .expect("report");
    }
    drop(Ledger::open(&path).expect("migrate v6"));
    assert_eq!(user_version(&path), 8);
    assert_eq!(trigger_names(&path).len(), 7);
    let columns = column_names(&path, "reports");
    for column in ["commit_sha", "base_sha", "graph_sha"] {
        assert!(columns.iter().any(|name| name == column), "{column}");
    }
    let conn = Connection::open(&path).expect("reopen");
    let stored_event: String = conn
        .query_row("SELECT payload FROM events WHERE id = 3", [], |row| {
            row.get(0)
        })
        .expect("event");
    assert_eq!(stored_event, payload);
    let stored_report: (String, Option<String>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT body, commit_sha, base_sha, graph_sha FROM reports WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("report");
    assert_eq!(stored_report.0, body);
    assert_eq!(stored_report.1, None);
    assert_eq!(stored_report.2, None);
    assert_eq!(stored_report.3, None);
}

#[test]
fn v033_statements_are_refused_on_gated_v8() {
    let (_dir, path) = fresh_path();
    {
        let mut ledger = Ledger::initialize(&path).expect("init");
        gate(&mut ledger);
        ledger.create_task("auth", "Add auth").expect("auth");
        ledger.create_task("other", "Other").expect("other");
        assert_guard(
            ledger
                .claim_task("other", "cursor")
                .expect_err("classic claim"),
        );
        assert_eq!(task_status(&path, "other"), "open");
        ledger
            .claim_task_anchored("auth", "cursor", None, || Ok(Some("base".to_string())))
            .expect("anchored claim");
        assert_guard(
            ledger
                .finish_task("auth", None, None)
                .expect_err("null commit"),
        );
    }
    assert_eq!(task_status(&path, "auth"), "claimed");
    assert_eq!(report_count(&path), 0);

    let mut conn = Connection::open(&path).expect("open");
    let task_id: i64 = conn
        .query_row("SELECT id FROM tasks WHERE slug = 'auth'", [], |row| {
            row.get(0)
        })
        .expect("id");
    let reports_before = report_count(&path);
    refused(&mut conn, |tx| {
        tx.execute(
            "UPDATE tasks SET status = 'done', updated_at = ?1 WHERE id = ?2",
            rusqlite::params!["2026-01-01T00:00:00.000Z", task_id],
        )?;
        tx.execute(
            "INSERT INTO events (ts, kind, payload) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "2026-01-01T00:00:00.000Z",
                "status_changed",
                r#"{"slug":"auth","from":"claimed","to":"done"}"#
            ],
        )?;
        tx.execute(
            "INSERT INTO reports (task_id, kind, author, body, bytes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                task_id,
                "task",
                "cursor",
                "{}",
                2_i64,
                "2026-01-01T00:00:00.000Z"
            ],
        )?;
        Ok(())
    });
    assert_eq!(task_status(&path, "auth"), "claimed");
    assert_eq!(report_count(&path), reports_before);

    refused(&mut conn, |tx| {
        tx.execute(
            "INSERT INTO reports (task_id, kind, author, body, bytes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                task_id,
                "task",
                "cursor",
                "{}",
                2_i64,
                "2026-01-01T00:00:00.000Z"
            ],
        )?;
        Ok(())
    });
    refused(&mut conn, |tx| {
        tx.execute(
            "INSERT INTO events (ts, kind, payload) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "2026-01-01T00:00:00.000Z",
                "claimed",
                r#"{"slug":"other","by":"cursor"}"#
            ],
        )?;
        Ok(())
    });
    conn.execute(
        "INSERT INTO events (ts, kind, payload) VALUES (?1, 'claimed', ?2)",
        rusqlite::params![
            "2026-01-01T00:00:00.000Z",
            r#"{"slug":"other","by":"cursor","base_sha":null}"#
        ],
    )
    .expect("json null base_sha");
    drop(conn);

    {
        let mut ledger = Ledger::open(&path).expect("open");
        ledger
            .finish_task("auth", None, Some("abc"))
            .expect("done with sha");
    }
    assert_eq!(task_status(&path, "auth"), "done");
    assert_eq!(report_verdict(&path), None);
    let mut conn = Connection::open(&path).expect("open");
    refused(&mut conn, |tx| {
        tx.execute(
            "UPDATE reports SET reviewed_at = ?1, verdict = 'accepted'
             WHERE task_id = ?2 AND reviewed_at IS NULL",
            rusqlite::params!["2026-01-01T00:00:00.000Z", task_id],
        )?;
        Ok(())
    });
    refused(&mut conn, |tx| {
        tx.execute(
            "UPDATE reports SET reviewed_at = ?1, verdict = 'accepted' WHERE reviewed_at IS NULL",
            ["2026-01-01T00:00:00.000Z"],
        )?;
        Ok(())
    });
    drop(conn);
    assert_eq!(report_verdict(&path), None);
    assert_eq!(task_status(&path, "auth"), "done");

    {
        let mut ledger = Ledger::open(&path).expect("open");
        assert_guard(
            ledger
                .start_run("prd.md", "digest", "feature", None)
                .expect_err("null workflow"),
        );
    }
    assert_eq!(run_count(&path), 0);
    let mut conn = Connection::open(&path).expect("open");
    refused(&mut conn, |tx| {
        tx.execute(
            "INSERT INTO runs (
                prd_path, prd_sha256, branch, stage, cycle, agent_calls, est_cost_usd,
                status, started_at, failure_streak, complete_requested
             ) VALUES (?1, ?2, ?3, 'plan', 0, 0, 0, 'active', ?4, 0, 0)",
            rusqlite::params!["prd.md", "digest", "feature", "2026-01-01T00:00:00.000Z"],
        )?;
        Ok(())
    });
    drop(conn);
    assert_eq!(run_count(&path), 0);

    let run = {
        let mut ledger = Ledger::open(&path).expect("open");
        ledger
            .start_run("prd.md", "digest", "feature", Some("gated"))
            .expect("gated run")
    };
    let mut conn = Connection::open(&path).expect("open");
    let stamp: i64 = conn
        .query_row(
            "SELECT write_stamp FROM runs WHERE id = ?1",
            [run.id],
            |row| row.get(0),
        )
        .expect("stamp");
    assert_eq!(stamp, 0);
    refused(&mut conn, |tx| {
        tx.execute(
            "UPDATE runs SET
                stage = ?1, cycle = ?2, agent_calls = ?3, est_cost_usd = ?4, status = ?5,
                stop_reason = ?6, failure_streak = ?7, current_slug = ?8, complete_requested = ?9
             WHERE id = ?10",
            rusqlite::params![
                "execute",
                1_i64,
                0_i64,
                0_f64,
                "active",
                None::<String>,
                0_i64,
                None::<String>,
                0_i64,
                run.id
            ],
        )?;
        Ok(())
    });
    let after: (String, i64) = conn
        .query_row(
            "SELECT stage, write_stamp FROM runs WHERE id = ?1",
            [run.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("unchanged");
    assert_eq!(after.0, "plan");
    assert_eq!(after.1, 0);
    drop(conn);
    {
        let mut ledger = Ledger::open(&path).expect("open");
        let mut saved = ledger.active_run().expect("active").expect("row");
        saved.stage = "execute".to_string();
        ledger.save_run(&mut saved).expect("save");
    }
    let conn = Connection::open(&path).expect("open");
    let bumped: (String, i64) = conn
        .query_row(
            "SELECT stage, write_stamp FROM runs WHERE id = ?1",
            [run.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("bumped");
    assert_eq!(bumped.0, "execute");
    assert_eq!(bumped.1, 1);
}

#[test]
fn v033_statements_still_work_on_classic_v8() {
    let (_dir, path) = fresh_path();
    {
        let mut ledger = Ledger::initialize(&path).expect("init");
        ledger.create_task("auth", "Add auth").expect("auth");
        ledger.claim_task("auth", "cursor").expect("claim");
        ledger.finish_task("auth", None, None).expect("done");
    }
    assert_eq!(task_status(&path, "auth"), "done");
    assert_eq!(report_count(&path), 1);
    let conn = Connection::open(&path).expect("open");
    let task_id: i64 = conn
        .query_row("SELECT id FROM tasks WHERE slug = 'auth'", [], |row| {
            row.get(0)
        })
        .expect("id");
    conn.execute(
        "INSERT INTO reports (task_id, kind, author, body, bytes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            task_id,
            "task",
            "cursor",
            "{}",
            2_i64,
            "2026-01-01T00:00:00.000Z"
        ],
    )
    .expect("report insert");
    conn.execute(
        "INSERT INTO events (ts, kind, payload) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            "2026-01-01T00:00:00.000Z",
            "claimed",
            r#"{"slug":"auth","by":"cursor"}"#
        ],
    )
    .expect("claim event");
    conn.execute(
        "UPDATE reports SET reviewed_at = ?1, verdict = 'accepted'
         WHERE task_id = ?2 AND reviewed_at IS NULL",
        rusqlite::params!["2026-01-01T00:00:00.000Z", task_id],
    )
    .expect("accept");
    let accepted: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM reports WHERE task_id = ?1 AND verdict = 'accepted'",
            [task_id],
            |row| row.get(0),
        )
        .expect("accepted");
    assert!(accepted >= 1);
    conn.execute(
        "INSERT INTO runs (
            prd_path, prd_sha256, branch, stage, cycle, agent_calls, est_cost_usd,
            status, started_at, failure_streak, complete_requested
         ) VALUES (?1, ?2, ?3, 'plan', 0, 0, 0, 'active', ?4, 0, 0)",
        rusqlite::params!["prd.md", "digest", "feature", "2026-01-01T00:00:00.000Z"],
    )
    .expect("run insert");
    let run_id: i64 = conn
        .query_row("SELECT id FROM runs ORDER BY id DESC LIMIT 1", [], |row| {
            row.get(0)
        })
        .expect("run id");
    conn.execute(
        "UPDATE runs SET
            stage = ?1, cycle = ?2, agent_calls = ?3, est_cost_usd = ?4, status = ?5,
            stop_reason = ?6, failure_streak = ?7, current_slug = ?8, complete_requested = ?9
         WHERE id = ?10",
        rusqlite::params![
            "execute",
            1_i64,
            0_i64,
            0_f64,
            "paused",
            None::<String>,
            0_i64,
            None::<String>,
            0_i64,
            run_id
        ],
    )
    .expect("run update");
    let workflow: Option<String> = conn
        .query_row("SELECT workflow FROM runs WHERE id = ?1", [run_id], |row| {
            row.get(0)
        })
        .expect("workflow");
    assert_eq!(workflow, None);
    drop(conn);
    {
        let mut ledger = Ledger::open(&path).expect("open");
        let run = ledger
            .start_run("prd.md", "digest", "feature", None)
            .expect("start");
        let mut saved = run;
        saved.stage = "execute".to_string();
        ledger.save_run(&mut saved).expect("save");
    }
    let conn = Connection::open(&path).expect("open");
    let stamp: i64 = conn
        .query_row(
            "SELECT write_stamp FROM runs ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("stamp");
    assert_eq!(stamp, 1);
}

#[test]
fn classic_setting_still_refuses_unstamped_update_of_gated_run() {
    let (_dir, path) = fresh_path();
    let run_id = {
        let mut ledger = Ledger::initialize(&path).expect("init");
        ledger.set_setting("reviewer", "codex").expect("reviewer");
        ledger.set_setting("workflow", "gated").expect("gated");
        ledger.create_task("auth", "Add auth").expect("task");
        let mut run = ledger
            .start_run("prd.md", "digest", "feature", Some("gated"))
            .expect("run");
        // Leaving gated is refused while this run is live. Stop it, change
        // the setting, then make the same gated row active again.
        run.status = "stopped".to_string();
        ledger.save_run(&mut run).expect("stop");
        ledger
            .set_setting_with("workflow", "classic", true)
            .expect("classic");
        run.status = "active".to_string();
        ledger.save_run(&mut run).expect("reactivate");
        run.id
    };
    let mut conn = Connection::open(&path).expect("open");
    let setting: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'workflow'",
            [],
            |row| row.get(0),
        )
        .expect("setting");
    assert_eq!(setting, "classic");
    let stored_workflow: String = conn
        .query_row("SELECT workflow FROM runs WHERE id = ?1", [run_id], |row| {
            row.get(0)
        })
        .expect("workflow");
    assert_eq!(stored_workflow, "gated");
    let trigger_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'relaybook_guard_run_update'",
            [],
            |row| row.get(0),
        )
        .expect("trigger");
    assert!(
        trigger_sql.contains("OLD.workflow = 'gated'"),
        "{trigger_sql}"
    );
    assert!(
        trigger_sql.contains("(SELECT value FROM settings WHERE key = 'workflow') = 'gated'"),
        "{trigger_sql}"
    );
    assert!(
        trigger_sql.contains("NEW.write_stamp IS OLD.write_stamp"),
        "{trigger_sql}"
    );
    let task_id: i64 = conn
        .query_row("SELECT id FROM tasks WHERE slug = 'auth'", [], |row| {
            row.get(0)
        })
        .expect("task");
    refused(&mut conn, |tx| {
        tx.execute(
            "UPDATE runs SET
                stage = ?1, cycle = ?2, agent_calls = ?3, est_cost_usd = ?4, status = ?5,
                stop_reason = ?6, failure_streak = ?7, current_slug = ?8, complete_requested = ?9
             WHERE id = ?10",
            rusqlite::params![
                "execute",
                1_i64,
                0_i64,
                0_f64,
                "active",
                None::<String>,
                0_i64,
                None::<String>,
                0_i64,
                run_id
            ],
        )?;
        Ok(())
    });
    let stage: String = conn
        .query_row("SELECT stage FROM runs WHERE id = ?1", [run_id], |row| {
            row.get(0)
        })
        .expect("stage");
    assert_eq!(stage, "plan");
    conn.execute(
        "INSERT INTO reports (task_id, kind, author, body, bytes, created_at)
         VALUES (?1, 'task', 'cursor', '{}', 2, '2026-01-01T00:00:00.000Z')",
        [task_id],
    )
    .expect("classic report insert");
    drop(conn);
    {
        let mut ledger = Ledger::open(&path).expect("open");
        let mut saved = ledger.active_run().expect("active").expect("row");
        saved.stage = "execute".to_string();
        ledger.save_run(&mut saved).expect("stamped save");
    }
    let conn = Connection::open(&path).expect("open");
    let stored: (String, i64, String) = conn
        .query_row(
            "SELECT stage, write_stamp, workflow FROM runs WHERE id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("saved");
    assert_eq!(stored.0, "execute");
    // insert 0, stop 1, reactivate 2, this save 3.
    assert_eq!(stored.1, 3);
    assert_eq!(stored.2, "gated");
}

#[test]
fn gated_core_flow_passes_the_guards() {
    let (_dir, path) = fresh_path();
    let mut ledger = Ledger::initialize(&path).expect("init");
    gate(&mut ledger);
    ledger.create_task("auth", "Add auth").expect("task");
    ledger
        .claim_task_anchored("auth", "cursor", None, || Ok(Some("base".to_string())))
        .expect("claim");
    let done = ledger
        .finish_task_anchored("auth", None, Some("abc"), Some("base"), None, || Ok(()))
        .expect("done");
    assert_eq!(done.status, Status::Done);
    record_task(&mut ledger, "auth", "abc");
    ledger
        .accept_gated_report("auth", "abc", || Ok(()))
        .expect("accept");
    record_release(&mut ledger, "abc");
    ledger
        .authorize_release("feature", "abc", "abc", "maintainer", None)
        .expect("authorize");
    let mut run = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("run");
    run.stage = "execute".to_string();
    ledger.save_run(&mut run).expect("save");
    drop(ledger);
    let conn = Connection::open(&path).expect("open");
    let stored: (String, i64) = conn
        .query_row(
            "SELECT workflow, write_stamp FROM runs ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("run");
    assert_eq!(stored.0, "gated");
    assert_eq!(stored.1, 1);
    let method: Option<String> = conn
        .query_row("SELECT method FROM release_authorizations", [], |row| {
            row.get(0)
        })
        .expect("method");
    assert_eq!(method, None);
    let verdict: String = conn
        .query_row(
            "SELECT verdict FROM reports WHERE kind = 'task' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("verdict");
    assert_eq!(verdict, "accepted");
}

#[test]
fn missing_guard_is_restored_on_open() {
    let (_dir, path) = fresh_path();
    drop(Ledger::initialize(&path).expect("init"));
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute("DROP TRIGGER relaybook_guard_run_insert", [])
            .expect("drop");
    }
    let ledger = Ledger::open(&path).expect("restore classic");
    assert!(ledger.guards_restored());
    assert!(!Ledger::reported_restored_guard());
    drop(ledger);
    assert!(
        trigger_names(&path)
            .iter()
            .any(|name| name == "relaybook_guard_run_insert")
    );
    let again = Ledger::open(&path).expect("present");
    assert!(!again.guards_restored());
    drop(again);

    let (_dir, path) = fresh_path();
    {
        let mut ledger = Ledger::initialize(&path).expect("init");
        gate(&mut ledger);
    }
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute("DROP TRIGGER relaybook_guard_task_accept", [])
            .expect("drop");
    }
    let ledger = Ledger::open(&path).expect("restore gated");
    assert!(ledger.guards_restored());
    assert!(Ledger::reported_restored_guard());
    drop(ledger);
    assert!(
        trigger_names(&path)
            .iter()
            .any(|name| name == "relaybook_guard_task_accept")
    );
    let again = Ledger::open(&path).expect("present");
    assert!(!again.guards_restored());
}

#[test]
fn newer_than_schema_is_refused() {
    let (_dir, path) = fresh_path();
    drop(Ledger::initialize(&path).expect("init"));
    {
        let conn = Connection::open(&path).expect("open");
        conn.pragma_update(None, "user_version", 9)
            .expect("version");
    }
    let opened = match Ledger::open(&path) {
        Ok(_) => panic!("read-write open of a newer ledger succeeded"),
        Err(err) => err,
    };
    assert_eq!(opened.to_string(), "this ledger needs a newer relaybook");
    let readonly = match Ledger::open_readonly(&path) {
        Ok(_) => panic!("read-only open of a newer ledger succeeded"),
        Err(err) => err,
    };
    assert_eq!(readonly.to_string(), "this ledger needs a newer relaybook");
    assert_eq!(user_version(&path), 9);
    assert_eq!(trigger_names(&path).len(), 7);
}

#[test]
fn fallback_schema_idempotent_at_v8() {
    let (_dir, path) = fresh_path();
    drop(Ledger::initialize(&path).expect("init"));
    assert_eq!(user_version(&path), 8);
    let again = Ledger::open(&path).expect("second open");
    drop(again);
    assert_eq!(user_version(&path), 8);
    let columns = column_names(&path, "reviews");
    for name in ["reviewer_model", "fallback_reason", "rereview_of"] {
        assert!(columns.iter().any(|column| column == name), "{name}");
    }
    assert!(
        trigger_names(&path)
            .iter()
            .any(|name| name == "relaybook_guard_rereview_authorize")
    );
    {
        let conn = Connection::open(&path).expect("open");
        conn.pragma_update(None, "user_version", 9)
            .expect("version");
    }
    let opened = match Ledger::open(&path) {
        Ok(_) => panic!("newer ledger opened"),
        Err(err) => err,
    };
    assert_eq!(opened.to_string(), "this ledger needs a newer relaybook");
}

#[test]
fn rereview_trigger_blocks_authorization_insert() {
    let (_dir, path) = fresh_path();
    let mut ledger = Ledger::initialize(&path).expect("init");
    gate(&mut ledger);
    let run = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("run");
    drop(ledger);
    let conn = Connection::open(&path).expect("open");
    conn.execute(
        "INSERT INTO tasks (slug, title, status, created_at, updated_at, run_id)
         VALUES ('work', 'Work', 'done', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', ?1)",
        [run.id],
    )
    .expect("task");
    let task_id: i64 = conn
        .query_row("SELECT id FROM tasks WHERE slug = 'work'", [], |row| {
            row.get(0)
        })
        .expect("task id");
    conn.execute(
        "INSERT INTO reviews (
            scope, task_id, run_id, branch, reviewer, base_sha, commit_sha, ledger_mark,
            verdict, body, bytes, started_at, recorded_at
         ) VALUES (
            'task', ?1, ?2, 'feature', 'claude', 'a', 'b', 'mark',
            'approved', '{}', 2, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z'
         )",
        rusqlite::params![task_id, run.id],
    )
    .expect("review");
    let review_id = conn.last_insert_rowid();
    let err = conn
        .execute(
            "INSERT INTO release_authorizations (
                run_id, branch, commit_sha, review_id, authorized_by, created_at
             ) VALUES (?1, 'feature', 'b', ?2, 'maintainer', '2026-01-01T00:00:00.000Z')",
            rusqlite::params![run.id, review_id],
        )
        .expect_err("trigger");
    assert!(err.to_string().contains("codex re-review pending"), "{err}");
}

#[test]
fn retry_after_from_error_is_ignored_and_advisory_iso_is_clamped() {
    let (_dir, path) = fresh_path();
    let ledger = Ledger::initialize(&path).expect("init");
    let past = ledger
        .resolve_retry_after(Some("2000-01-01T00:00:00.000Z"), 60)
        .expect("past");
    assert_eq!(past.1, "backoff");
    let far = ledger
        .resolve_retry_after(Some("2999-01-01T00:00:00.000Z"), 60)
        .expect("far");
    assert_eq!(far.1, "backoff");
    let soon = ledger.backoff_deadline(1).expect("soon");
    let kept = ledger.resolve_retry_after(Some(&soon), 60).expect("kept");
    assert_eq!(kept.1, "codex");
    assert_eq!(kept.0, soon);
    let missing = ledger.resolve_retry_after(None, 60).expect("missing");
    assert_eq!(missing.1, "backoff");
}

fn claude_release_review(conn: &Connection, run_id: i64, sha: &str) -> i64 {
    let mark = r#"{"events":null,"reports":null,"decisions":null,"steps":null,"tasks_updated_at":null,"fingerprint":""}"#;
    conn.execute(
        "INSERT INTO reviews (
            scope, run_id, branch, reviewer, base_sha, commit_sha, ledger_mark, verdict, started_at
         ) VALUES (
            'release', ?1, 'feature', 'claude', 'base', ?2, ?3, 'approved', '2026-01-01T00:00:00.000Z'
         )",
        rusqlite::params![run_id, sha, mark],
    )
    .expect("review");
    conn.last_insert_rowid()
}

fn stop_run(ledger: &mut Ledger, run_id: i64) {
    let mut run = ledger.run_by_id(run_id).expect("run");
    run.status = "stopped".to_string();
    ledger.save_run(&mut run).expect("stop");
}

/// The trigger and the Rust check both refuse a stopped run when the caller
/// supplies no run id. Coverage comes from the selected release review.
#[test]
fn stopped_run_null_run_id_blocks_authorization_in_sql_and_rust() {
    let (_dir, path) = fresh_path();
    let run_id = {
        let mut ledger = Ledger::initialize(&path).expect("init");
        gate(&mut ledger);
        ledger
            .set_setting("fallback_rereview", "before-release")
            .expect("policy");
        let run = ledger
            .start_run("prd.md", "digest", "feature", Some("gated"))
            .expect("run");
        stop_run(&mut ledger, run.id);
        run.id
    };
    let review_id = {
        let conn = Connection::open(&path).expect("open");
        claude_release_review(&conn, run_id, "abc")
    };
    {
        let conn = Connection::open(&path).expect("open");
        let err = conn
            .execute(
                "INSERT INTO release_authorizations (
                    run_id, branch, commit_sha, review_id, authorized_by, created_at
                 ) VALUES (NULL, 'feature', 'abc', ?1, 'maintainer', '2026-01-01T00:00:00.000Z')",
                [review_id],
            )
            .expect_err("trigger");
        assert!(
            err.to_string().contains("codex re-review pending"),
            "trigger: {err}"
        );
    }
    let mut ledger = Ledger::open(&path).expect("open");
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute("DROP TRIGGER relaybook_guard_rereview_authorize", [])
            .expect("drop trigger");
    }
    let err = ledger
        .authorize_release("feature", "abc", "abc", "maintainer", None)
        .expect_err("rust");
    assert!(
        err.to_string().contains("codex re-review pending"),
        "rust check: {err}"
    );
    drop(ledger);
    let conn = Connection::open(&path).expect("open");
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM release_authorizations", [], |row| {
            row.get(0)
        })
        .expect("count");
    assert_eq!(rows, 0);
}

/// An older trigger that only reads `NEW.run_id` is replaced on open.
/// `user_version` stays 8. A null `run_id` then refuses.
#[test]
fn old_rereview_trigger_is_replaced_at_v8() {
    let (_dir, path) = fresh_path();
    let run_id = {
        let mut ledger = Ledger::initialize(&path).expect("init");
        gate(&mut ledger);
        let run = ledger
            .start_run("prd.md", "digest", "feature", Some("gated"))
            .expect("run");
        stop_run(&mut ledger, run.id);
        run.id
    };
    assert_eq!(user_version(&path), 8);
    let review_id = {
        let conn = Connection::open(&path).expect("open");
        conn.execute(
            "DROP TRIGGER IF EXISTS relaybook_guard_rereview_authorize",
            [],
        )
        .expect("drop");
        conn.execute_batch(
            "
            CREATE TRIGGER relaybook_guard_rereview_authorize
            BEFORE INSERT ON release_authorizations
            WHEN NEW.run_id IS NOT NULL
             AND EXISTS (
                SELECT 1 FROM reviews AS sonnet
                WHERE sonnet.reviewer = 'claude'
                  AND sonnet.verdict = 'approved'
                  AND sonnet.run_id = NEW.run_id
             )
            BEGIN
              SELECT RAISE(ABORT, 'codex re-review pending');
            END;
            ",
        )
        .expect("old trigger");
        let review_id = claude_release_review(&conn, run_id, "abc");
        conn.execute(
            "INSERT INTO release_authorizations (
                run_id, branch, commit_sha, review_id, authorized_by, created_at
             ) VALUES (NULL, 'feature', 'abc', ?1, 'maintainer', '2026-01-01T00:00:00.000Z')",
            [review_id],
        )
        .expect("old trigger allows a null run id");
        conn.execute("DELETE FROM release_authorizations", [])
            .expect("clear");
        review_id
    };
    drop(Ledger::open(&path).expect("upgrade"));
    assert_eq!(user_version(&path), 8);
    let conn = Connection::open(&path).expect("open");
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'trigger' AND name = 'relaybook_guard_rereview_authorize'",
            [],
            |row| row.get(0),
        )
        .expect("sql");
    assert!(sql.contains("selected.run_id"), "{sql}");
    let err = conn
        .execute(
            "INSERT INTO release_authorizations (
                run_id, branch, commit_sha, review_id, authorized_by, created_at
             ) VALUES (NULL, 'feature', 'abc', ?1, 'maintainer', '2026-01-01T00:00:00.000Z')",
            [review_id],
        )
        .expect_err("upgraded trigger");
    assert!(err.to_string().contains("codex re-review pending"), "{err}");
}

/// The stamp-incrementing `UPDATE` from the R13 binary at `dd292e9`
/// (`crates/core/src/runs.rs` `save`, the statement at line 172). Resume
/// from `gate_release` binds stage `publish` and calls `claim_run`
/// (`crates/relaybook/src/runloop.rs:369`). `publish` then binds stage
/// `publishing` and calls `claim_run` again (`runloop.rs:2258`).
fn r13_save(
    conn: &Connection,
    id: i64,
    stage: &str,
    status: &str,
) -> Result<usize, rusqlite::Error> {
    let (cycle, calls, cost, streak, slug, complete, stamp): (
        i64,
        i64,
        f64,
        i64,
        Option<String>,
        i64,
        i64,
    ) = conn.query_row(
        "SELECT cycle, agent_calls, est_cost_usd, failure_streak, current_slug,
                complete_requested, write_stamp
         FROM runs WHERE id = ?1",
        [id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;
    conn.execute(
        "UPDATE runs SET
            stage = ?1, cycle = ?2, agent_calls = ?3, est_cost_usd = ?4, status = ?5,
            stop_reason = ?6, failure_streak = ?7, current_slug = ?8, complete_requested = ?9,
            write_stamp = write_stamp + 1
         WHERE id = ?10 AND write_stamp = ?11",
        rusqlite::params![
            stage,
            cycle,
            calls,
            cost,
            status,
            None::<String>,
            streak,
            slug,
            complete,
            id,
            stamp
        ],
    )
}

/// Authorize a Sonnet release while re-review is off, tighten the policy,
/// then replay the R13 update sequence. The move into `publishing` raises
/// and the row stays at `publish`.
#[test]
fn r13_stamp_update_cannot_enter_publishing_while_rereview_is_pending() {
    let (_dir, path) = fresh_path();
    let run_id = {
        let mut ledger = Ledger::initialize(&path).expect("init");
        gate(&mut ledger);
        ledger
            .set_setting_with("fallback_rereview", "off", true)
            .expect("off");
        let run = ledger
            .start_run("prd.md", "digest", "feature", Some("gated"))
            .expect("run");
        let mut waiting = ledger.run_by_id(run.id).expect("load");
        waiting.stage = "gate_release".to_string();
        waiting.status = "waiting".to_string();
        ledger.save_run(&mut waiting).expect("gate");
        run.id
    };
    assert_eq!(user_version(&path), 8);
    let review_id = {
        let conn = Connection::open(&path).expect("open");
        claude_release_review(&conn, run_id, "abc")
    };
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute(
            "INSERT INTO release_authorizations (
                run_id, branch, commit_sha, review_id, authorized_by, created_at
             ) VALUES (?1, 'feature', 'abc', ?2, 'maintainer', '2026-01-01T00:00:00.000Z')",
            rusqlite::params![run_id, review_id],
        )
        .expect("authorize while off");
    }
    {
        let mut ledger = Ledger::open(&path).expect("open");
        ledger
            .set_setting("fallback_rereview", "before-release")
            .expect("tighten");
    }
    let conn = Connection::open(&path).expect("open");
    r13_save(&conn, run_id, "publish", "active").expect("R13 claim of stage publish");
    let err = r13_save(&conn, run_id, "publishing", "active").expect_err("R13 publishing update");
    assert!(err.to_string().contains("codex re-review pending"), "{err}");
    let stage: String = conn
        .query_row("SELECT stage FROM runs WHERE id = ?1", [run_id], |row| {
            row.get(0)
        })
        .expect("stage");
    assert_eq!(stage, "publish");
    assert_eq!(user_version(&path), 8);
}

fn probe_event_count(path: &Path, kind: &str) -> i64 {
    let conn = Connection::open(path).expect("open");
    conn.query_row(
        "SELECT COUNT(*) FROM reviewer_events WHERE kind = ?1",
        [kind],
        |row| row.get(0),
    )
    .expect("events")
}

fn assert_probe_halt(path: &Path, outcome: relaybook_core::ProbeOutcome) {
    let mut ledger = Ledger::open(path).expect("open");
    let run = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("run");
    let claim = ledger
        .begin_probe(run.id, run.write_stamp, "plan", 1)
        .expect("begin");
    ledger.create_task("extra", "Extra").expect("mutate");
    let written = ledger
        .record_probe(
            run.id,
            claim.generation,
            &claim.token,
            "plan",
            &claim.ledger_mark,
            outcome,
        )
        .expect("record");
    match written {
        relaybook_core::ProbeOutcome::Halt { reason } => {
            assert_eq!(reason, relaybook_core::LEDGER_CHANGED_DURING_REVIEW);
        }
        other => panic!("mutated probe wrote {other:?}"),
    }
    let stored = ledger.run_by_id(run.id).expect("stored");
    assert_eq!(stored.status, "stopped");
    assert_eq!(
        stored.stop_reason.as_deref(),
        Some(relaybook_core::LEDGER_CHANGED_DURING_REVIEW)
    );
    drop(ledger);
    assert_eq!(probe_event_count(path, "probe_ok"), 0);
    assert_eq!(probe_event_count(path, "switched_to_codex"), 0);
    assert_eq!(probe_event_count(path, "probe_failed"), 1);
}

#[test]
fn probe_record_halts_every_outcome_when_the_ledger_changes() {
    for outcome in [
        relaybook_core::ProbeOutcome::Ok {
            version: "codex-cli 0.169.0".to_string(),
        },
        relaybook_core::ProbeOutcome::Limited {
            retry_after: "2026-01-01T01:00:00.000Z".to_string(),
            source: "backoff".to_string(),
            evidence: "limit".to_string(),
            version: "codex-cli 0.169.0".to_string(),
        },
        relaybook_core::ProbeOutcome::Failed {
            evidence: "plain".to_string(),
            version: "codex-cli 0.169.0".to_string(),
        },
        relaybook_core::ProbeOutcome::Halt {
            reason: "reviewer edited the worktree".to_string(),
        },
    ] {
        let (_dir, path) = fresh_path();
        drop(Ledger::initialize(&path).expect("init"));
        assert_probe_halt(&path, outcome);
    }
}

#[test]
fn probe_record_keeps_ok_when_the_mark_matches() {
    let (_dir, path) = fresh_path();
    let mut ledger = Ledger::initialize(&path).expect("init");
    let run = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("run");
    let claim = ledger
        .begin_probe(run.id, run.write_stamp, "plan", 1)
        .expect("begin");
    let written = ledger
        .record_probe(
            run.id,
            claim.generation,
            &claim.token,
            "plan",
            &claim.ledger_mark,
            relaybook_core::ProbeOutcome::Ok {
                version: "codex-cli 0.169.0".to_string(),
            },
        )
        .expect("record");
    assert!(matches!(written, relaybook_core::ProbeOutcome::Ok { .. }));
    let stored = ledger.run_by_id(run.id).expect("stored");
    assert_eq!(stored.status, "active");
    assert_eq!(stored.stop_reason, None);
    drop(ledger);
    assert_eq!(probe_event_count(&path, "probe_ok"), 1);
    assert_eq!(probe_event_count(&path, "switched_to_codex"), 1);
}

#[test]
fn publishing_claim_reads_the_current_rereview_policy() {
    let (_dir, path) = fresh_path();
    let mut ledger = Ledger::initialize(&path).expect("init");
    gate(&mut ledger);
    ledger
        .set_setting("fallback_rereview", "before-release")
        .expect("policy");
    let run = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("run");
    drop(ledger);
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute(
            "INSERT INTO tasks (slug, title, status, created_at, updated_at, run_id)
             VALUES ('work', 'Work', 'done', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', ?1)",
            [run.id],
        )
        .expect("task");
        let task_id: i64 = conn
            .query_row("SELECT id FROM tasks WHERE slug = 'work'", [], |row| {
                row.get(0)
            })
            .expect("task id");
        conn.execute(
            "INSERT INTO reviews (
                scope, task_id, run_id, branch, reviewer, base_sha, commit_sha, ledger_mark,
                verdict, started_at
             ) VALUES (
                'task', ?1, ?2, 'feature', 'claude', 'base', 'abc',
                '{\"events\":null,\"reports\":null,\"decisions\":null,\"steps\":null,\"tasks_updated_at\":null,\"fingerprint\":\"\"}',
                'approved', '2026-01-01T00:00:00.000Z'
             )",
            rusqlite::params![task_id, run.id],
        )
        .expect("review");
    }
    let mut ledger = Ledger::open(&path).expect("open");
    let stamp = ledger.run_by_id(run.id).expect("stamp").write_stamp;
    let mut publishing = ledger.run_by_id(run.id).expect("run");
    publishing.stage = "publishing".to_string();
    let err = ledger.claim_run(&mut publishing).expect_err("blocked");
    assert!(err.to_string().contains("codex re-review pending"), "{err}");
    let stored = ledger.run_by_id(run.id).expect("unchanged");
    assert_eq!(stored.write_stamp, stamp);
    assert_eq!(stored.stage, "plan");
    stop_run(&mut ledger, run.id);
    ledger
        .set_setting_with("fallback_rereview", "off", true)
        .expect("off");
    let mut again = ledger.run_by_id(run.id).expect("again");
    again.stage = "publishing".to_string();
    ledger
        .claim_run(&mut again)
        .expect("current policy allows the claim");
    assert_eq!(again.stage, "publishing");
}
