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
        assert!(sql.contains(GUARD), "{name}");
        assert!(sql.contains("SELECT RAISE"), "{name}");
    }
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
    assert_eq!(trigger_names(&path).len(), 5);
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
    let stored_review: String = conn
        .query_row("SELECT body FROM reviews WHERE id = 1", [], |row| {
            row.get(0)
        })
        .expect("review");
    assert_eq!(stored_review, review);
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
    assert_eq!(trigger_names(&path).len(), 5);
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
    assert_eq!(trigger_names(&path).len(), 5);
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
        ledger.save_run(&saved).expect("save");
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
        ledger.save_run(&saved).expect("save");
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
    ledger.save_run(&run).expect("save");
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
    assert_eq!(trigger_names(&path).len(), 5);
}
