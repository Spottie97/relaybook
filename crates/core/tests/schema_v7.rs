use std::path::{Path, PathBuf};

use relaybook_core::lifecycle::{ReleasePhase, TaskPhase};
use relaybook_core::{Ledger, Status};
use rusqlite::Connection;

fn task_review_body() -> &'static str {
    r#"{"findings":[],"security":"ok","tests_checked":[],"not_checked":[]}"#
}

fn release_review_body() -> &'static str {
    r#"{"findings":[],"security":"ok","tests_checked":[],"not_checked":[],"prd_covered":true,"gaps":[]}"#
}

fn user_version(path: &Path) -> i64 {
    let conn = Connection::open(path).expect("open");
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version")
}

fn table_count(path: &Path, name: &str) -> i64 {
    let conn = Connection::open(path).expect("open");
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get(0),
    )
    .expect("table")
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

fn fresh_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    (dir, path)
}

fn assert_v7_tables(path: &Path) {
    for table in ["reviews", "graph_builds", "release_authorizations"] {
        assert_eq!(table_count(path, table), 1, "{table}");
    }
    let columns = column_names(path, "reports");
    for column in ["commit_sha", "base_sha", "graph_sha"] {
        assert!(columns.iter().any(|name| name == column), "{column}");
    }
}

fn insert_v6_report(path: &Path, author: &str, body: &str) {
    let conn = Connection::open(path).expect("open");
    conn.execute(
        "INSERT INTO reports (task_id, kind, author, body, bytes, created_at)
         VALUES (NULL, 'note', ?1, ?2, ?3, '2026-01-01T00:00:00.000Z')",
        rusqlite::params![author, body, body.len() as i64],
    )
    .expect("v6 insert");
    let stored: (String, Option<String>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT body, commit_sha, base_sha, graph_sha
             FROM reports WHERE author = ?1 ORDER BY id DESC LIMIT 1",
            [author],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read");
    assert_eq!(stored.0, body);
    assert_eq!(stored.1, None);
    assert_eq!(stored.2, None);
    assert_eq!(stored.3, None);
}

#[test]
fn fresh_ledger_is_schema_v7() {
    let (_dir, path) = fresh_path();
    let ledger = Ledger::initialize(&path).expect("init");
    drop(ledger);
    assert_eq!(user_version(&path), 7);
    assert_v7_tables(&path);
    insert_v6_report(&path, "orca", r#"{"text":"hi"}"#);
}

#[test]
fn empty_database_migrates_to_v7() {
    let (_dir, path) = fresh_path();
    Connection::open(&path)
        .expect("create")
        .execute_batch("")
        .ok();
    let ledger = Ledger::open(&path).expect("migrate");
    drop(ledger);
    assert_eq!(user_version(&path), 7);
    assert_v7_tables(&path);
    insert_v6_report(&path, "orca", r#"{"text":"hi"}"#);
}

#[test]
fn v1_ledger_migrates_to_v7_without_changing_events() {
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
                kind TEXT NOT NULL,
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
        .expect("insert");
    }
    let ledger = Ledger::open(&path).expect("migrate");
    drop(ledger);
    assert_eq!(user_version(&path), 7);
    assert_v7_tables(&path);
    let conn = Connection::open(&path).expect("reopen");
    let stored: (i64, String) = conn
        .query_row("SELECT id, payload FROM events", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("event");
    assert_eq!(stored.0, 7);
    assert_eq!(stored.1, payload);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .expect("count");
    assert_eq!(count, 1);
}

#[test]
fn v6_ledger_migrates_to_v7_with_payloads_unchanged() {
    let (_dir, path) = fresh_path();
    let payload = r#"{"slug":"auth","title":"Add auth"}"#;
    let body =
        r#"{"changed":["schema"],"verified":[],"not_verified":[],"deviations":[],"review":[]}"#;
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
                updated_at TEXT NOT NULL,
                run_id INTEGER
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
                reviewed_at TEXT,
                verdict TEXT,
                review_note TEXT
            );
            ",
        )
        .expect("v6 schema");
        conn.pragma_update(None, "user_version", 6)
            .expect("version");
        conn.execute(
            "INSERT INTO tasks (id, slug, title, status, created_at, updated_at)
             VALUES (1, 'auth', 'Add auth', 'done', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .expect("task");
        conn.execute(
            "INSERT INTO events (id, ts, kind, payload)
             VALUES (4, '2026-01-01T00:00:00.000Z', 'task_created', ?1)",
            [payload],
        )
        .expect("event");
        conn.execute(
            "INSERT INTO reports (id, task_id, kind, author, body, bytes, created_at)
             VALUES (9, 1, 'task', 'cursor', ?1, ?2, '2026-01-01T00:00:00.000Z')",
            rusqlite::params![body, body.len() as i64],
        )
        .expect("report");
    }

    let ledger = Ledger::open(&path).expect("migrate");
    drop(ledger);
    assert_eq!(user_version(&path), 7);
    assert_v7_tables(&path);

    let conn = Connection::open(&path).expect("reopen");
    let event: String = conn
        .query_row("SELECT payload FROM events WHERE id = 4", [], |row| {
            row.get(0)
        })
        .expect("event");
    assert_eq!(event, payload);
    let report: (
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT id, author, body, commit_sha, base_sha, graph_sha FROM reports WHERE id = 9",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("report");
    assert_eq!(report.0, 9);
    assert_eq!(report.1, "cursor");
    assert_eq!(report.2, body);
    assert_eq!(report.3, None);
    assert_eq!(report.4, None);
    assert_eq!(report.5, None);
    drop(conn);

    insert_v6_report(&path, "orca", r#"{"text":"still works"}"#);
    let ledger = Ledger::open(&path).expect("second open");
    drop(ledger);
    assert_eq!(user_version(&path), 7);
    let conn = Connection::open(&path).expect("reopen");
    let again: String = conn
        .query_row("SELECT body FROM reports WHERE id = 9", [], |row| {
            row.get(0)
        })
        .expect("body");
    assert_eq!(again, body);
}

#[test]
fn reviews_scope_requires_a_task_exactly_when_scope_is_task() {
    let (_dir, path) = fresh_path();
    let task_id = {
        let mut ledger = Ledger::initialize(&path).expect("init");
        ledger.create_task("auth", "Add auth").expect("task").id
    };
    let conn = Connection::open(&path).expect("open");
    conn.pragma_update(None, "foreign_keys", "ON").expect("fk");

    conn.execute(
        "INSERT INTO reviews (scope, task_id, branch, reviewer, commit_sha, ledger_mark, started_at)
         VALUES ('task', ?1, 'feature', 'codex', 'abc', '{}', '2026-01-01T00:00:00.000Z')",
        [task_id],
    )
    .expect("task review");
    conn.execute(
        "INSERT INTO reviews (scope, task_id, branch, reviewer, commit_sha, ledger_mark, started_at)
         VALUES ('release', NULL, 'feature', 'codex', 'abc', '{}', '2026-01-01T00:00:00.000Z')",
        [],
    )
    .expect("release review");

    let missing_task = conn.execute(
        "INSERT INTO reviews (scope, task_id, branch, reviewer, commit_sha, ledger_mark, started_at)
         VALUES ('task', NULL, 'feature', 'codex', 'abc', '{}', '2026-01-01T00:00:00.000Z')",
        [],
    );
    assert!(missing_task.is_err(), "task scope requires task_id");
    let task_on_release = conn.execute(
        "INSERT INTO reviews (scope, task_id, branch, reviewer, commit_sha, ledger_mark, started_at)
         VALUES ('release', ?1, 'feature', 'codex', 'abc', '{}', '2026-01-01T00:00:00.000Z')",
        [task_id],
    );
    assert!(task_on_release.is_err(), "release scope rejects task_id");
    let bad_scope = conn.execute(
        "INSERT INTO reviews (scope, task_id, branch, reviewer, commit_sha, ledger_mark, started_at)
         VALUES ('note', NULL, 'feature', 'codex', 'abc', '{}', '2026-01-01T00:00:00.000Z')",
        [],
    );
    assert!(bad_scope.is_err(), "scope check");
}

#[test]
fn review_record_checks_sha_fingerprint_ledger_and_body() {
    let (_dir, mut ledger) = done_task();
    let err = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "aaa",
            "aaa",
            "fp",
            "approved",
            task_review_body(),
            false,
        )
        .expect_err("no start");
    assert_eq!(err.to_string(), "no pending review");

    let missing_task = ledger
        .start_review(
            "task", None, None, "feature", "codex", None, "aaa", None, "fp",
        )
        .expect_err("task required");
    assert_eq!(
        missing_task.to_string(),
        "task is required for a task review"
    );
    let err = ledger
        .start_review(
            "release",
            Some("auth"),
            None,
            "feature",
            "codex",
            None,
            "aaa",
            None,
            "fp",
        )
        .expect_err("no task");
    assert_eq!(err.to_string(), "a release review has no task");

    ledger
        .start_review(
            "task",
            Some("auth"),
            None,
            "feature",
            "codex",
            Some("base"),
            "aaa",
            Some("graph"),
            "fp",
        )
        .expect("start");

    let mismatch = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "aaa",
            "bbb",
            "fp",
            "approved",
            task_review_body(),
            false,
        )
        .expect_err("sha");
    assert_eq!(mismatch.to_string(), "sha does not match HEAD");

    let moved = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "bbb",
            "bbb",
            "fp",
            "approved",
            task_review_body(),
            false,
        )
        .expect_err("head");
    assert_eq!(moved.to_string(), "reviewer changed the repository");

    let fingerprint = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "aaa",
            "aaa",
            "other",
            "approved",
            task_review_body(),
            false,
        )
        .expect_err("fingerprint");
    assert_eq!(fingerprint.to_string(), "reviewer changed the repository");

    let bad_json = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "aaa",
            "aaa",
            "fp",
            "approved",
            "[]",
            false,
        )
        .expect_err("array");
    assert_eq!(bad_json.to_string(), "review body must be a JSON object");

    let missing = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "aaa",
            "aaa",
            "fp",
            "approved",
            r#"{"findings":[],"security":"ok","tests_checked":[]}"#,
            false,
        )
        .expect_err("key");
    assert_eq!(missing.to_string(), "review body is missing not_checked");

    let padding = "x".repeat(1600);
    let huge =
        format!(r#"{{"findings":"{padding}","security":"","tests_checked":"","not_checked":""}}"#);
    let too_big = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "aaa",
            "aaa",
            "fp",
            "approved",
            &huge,
            false,
        )
        .expect_err("size");
    assert_eq!(
        too_big.to_string(),
        format!("review is {} bytes; limit 1500", huge.len())
    );

    ledger
        .log_decision("auth", "use argon2", "claude-code")
        .expect("decision");
    let dirty = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "aaa",
            "aaa",
            "fp",
            "approved",
            task_review_body(),
            false,
        )
        .expect_err("ledger");
    assert_eq!(dirty.to_string(), "ledger changed during review");

    ledger.create_task("ship", "Ship it").expect("task");
    ledger.claim_task("ship", "cursor").expect("claim");
    ledger.finish_task("ship", None).expect("done");
    let before = ledger
        .context(Some("ship"), 50)
        .expect("context")
        .events
        .len();
    ledger
        .start_review(
            "task",
            Some("ship"),
            None,
            "feature",
            "codex",
            Some("base"),
            "ccc",
            Some("graph"),
            "fp",
        )
        .expect("start");
    let invalid = ledger
        .record_review(
            "task",
            Some("ship"),
            "codex",
            "ccc",
            "ccc",
            "fp",
            "maybe",
            task_review_body(),
            false,
        )
        .expect_err("verdict");
    assert_eq!(invalid.to_string(), "invalid verdict: maybe");
    let recorded = ledger
        .record_review(
            "task",
            Some("ship"),
            "claude",
            "ccc",
            "ccc",
            "fp",
            "approved",
            task_review_body(),
            true,
        )
        .expect("record");
    assert_eq!(recorded.verdict.as_deref(), Some("approved"));
    assert!(recorded.reviewer_override);
    assert_eq!(recorded.reviewer, "claude");
    assert_eq!(recorded.fingerprint, "fp");
    assert_eq!(recorded.base_sha.as_deref(), Some("base"));
    assert_eq!(recorded.graph_sha.as_deref(), Some("graph"));
    assert_eq!(recorded.commit_sha, "ccc");
    assert!(recorded.report_id.is_some());
    assert!(recorded.recorded_at.is_some());
    assert_eq!(
        recorded.body.expect("body")["security"],
        serde_json::json!("ok")
    );
    let latest = ledger
        .latest_review("task", Some("ship"))
        .expect("latest")
        .expect("row");
    assert_eq!(latest.id, recorded.id);
    let after = ledger
        .context(Some("ship"), 50)
        .expect("context")
        .events
        .len();
    assert_eq!(after, before);
    assert_eq!(
        ledger.context(Some("ship"), 20).expect("context").tasks[0]
            .task
            .status,
        Status::Done
    );
    let again = ledger
        .record_review(
            "task",
            Some("ship"),
            "codex",
            "ccc",
            "ccc",
            "fp",
            "approved",
            task_review_body(),
            false,
        )
        .expect_err("second");
    assert_eq!(again.to_string(), "no pending review");
}

#[test]
fn changes_requested_reopens_a_done_task() {
    let (_dir, mut ledger) = done_task();
    ledger
        .start_review(
            "task",
            Some("auth"),
            None,
            "feature",
            "codex",
            None,
            "aaa",
            None,
            "fp",
        )
        .expect("start");
    let review = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "aaa",
            "aaa",
            "fp",
            "changes-requested",
            task_review_body(),
            false,
        )
        .expect("record");
    assert_eq!(review.verdict.as_deref(), Some("changes_requested"));
    let task = &ledger.context(Some("auth"), 20).expect("context").tasks[0].task;
    assert_eq!(task.status, Status::Open);
    assert_eq!(task.reopened.as_deref(), Some("changes requested"));
    let life = ledger.lifecycle("aaa", &[]).expect("lifecycle");
    assert_eq!(task_phase(&life, "auth"), Some(TaskPhase::Planned));
}

#[test]
fn record_graph_build_validates_step_and_status() {
    let (_dir, mut ledger) = done_task();
    let err = ledger
        .record_graph_build("extract", None, None, "abc", "built", None)
        .expect_err("step");
    assert_eq!(err.to_string(), "invalid graph step: extract");
    let err = ledger
        .record_graph_build("plan", None, None, "abc", "pending", None)
        .expect_err("status");
    assert_eq!(err.to_string(), "invalid graph status: pending");
    let skipped = ledger
        .record_graph_build("plan", None, None, "abc", "skipped", Some("no graph"))
        .expect("skipped");
    assert_eq!(skipped.step, "plan");
    assert_eq!(skipped.status, "skipped");
    assert_eq!(skipped.detail.as_deref(), Some("no graph"));
    assert_eq!(skipped.commit_sha, "abc");
    assert_eq!(skipped.task_id, None);
    let task_id = ledger.context(Some("auth"), 5).expect("context").tasks[0]
        .task
        .id;
    let built = ledger
        .record_graph_build("release-review", Some("auth"), None, "abc", "built", None)
        .expect("built");
    assert_eq!(built.status, "built");
    assert_eq!(built.task_id, Some(task_id));
    assert_eq!(built.step, "release-review");
}

#[test]
fn authorize_release_requires_matching_sha_and_approved_review() {
    let (_dir, mut ledger) = done_task();
    let missing = ledger
        .authorize_release("feature", "sha-head", "sha-head", "maintainer", None)
        .expect_err("no review");
    assert_eq!(missing.to_string(), "no approved release review");

    ledger
        .start_review(
            "release",
            None,
            None,
            "feature",
            "codex",
            None,
            "sha-head",
            Some("graph"),
            "fp",
        )
        .expect("start");
    let incomplete = ledger
        .record_review(
            "release",
            None,
            "codex",
            "sha-head",
            "sha-head",
            "fp",
            "approved",
            task_review_body(),
            false,
        )
        .expect_err("body");
    assert_eq!(incomplete.to_string(), "review body is missing prd_covered");
    ledger
        .record_review(
            "release",
            None,
            "codex",
            "sha-head",
            "sha-head",
            "fp",
            "approved",
            release_review_body(),
            false,
        )
        .expect("record");

    let mismatch = ledger
        .authorize_release("feature", "sha-head", "other", "maintainer", None)
        .expect_err("sha");
    assert_eq!(mismatch.to_string(), "sha does not match HEAD");
    let other = ledger
        .authorize_release("feature", "other", "other", "maintainer", None)
        .expect_err("head");
    assert_eq!(other.to_string(), "no approved release review");

    let authorization = ledger
        .authorize_release("feature", "sha-head", "sha-head", "maintainer", None)
        .expect("authorize");
    assert_eq!(authorization.id, 1);
    assert_eq!(authorization.commit_sha, "sha-head");
    assert_eq!(authorization.branch, "feature");
    assert_eq!(authorization.authorized_by, "maintainer");
    assert_eq!(authorization.run_id, None);
    assert!(authorization.review_id >= 1);
}

#[test]
fn lifecycle_covers_every_state_and_invalidates_release() {
    assert_eq!(TaskPhase::Planned.as_str(), "planned");
    assert_eq!(TaskPhase::Dispatched.as_str(), "dispatched");
    assert_eq!(TaskPhase::Implemented.as_str(), "implemented");
    assert_eq!(TaskPhase::Reviewed.as_str(), "reviewed");
    assert_eq!(TaskPhase::Accepted.as_str(), "accepted");
    assert_eq!(ReleasePhase::ReleaseReviewed.as_str(), "release-reviewed");
    assert_eq!(ReleasePhase::SignedOff.as_str(), "signed-off");

    let (dir, path) = fresh_path();
    {
        let mut ledger = Ledger::initialize(&path).expect("init");
        ledger.create_task("alpha", "Open").expect("task");
        ledger.create_task("beta", "Blocked").expect("task");
        ledger.block_task("beta", "waiting").expect("block");
        ledger.create_task("gamma", "Claimed").expect("task");
        ledger.claim_task("gamma", "cursor").expect("claim");
        for slug in ["delta", "epsilon", "zeta", "eta"] {
            ledger.create_task(slug, slug).expect("task");
            ledger.claim_task(slug, "cursor").expect("claim");
            ledger.finish_task(slug, None).expect("done");
        }
    }
    set_commit_sha(&path, "delta", "sha-delta");
    set_commit_sha(&path, "epsilon", "sha-epsilon");
    set_commit_sha(&path, "zeta", "sha-zeta");

    let mut ledger = Ledger::open(&path).expect("open");
    record_task(&mut ledger, "epsilon", "sha-epsilon");
    record_task(&mut ledger, "zeta", "sha-zeta");
    ledger.accept_report("zeta").expect("accept");

    let life = ledger.lifecycle("sha-head", &[]).expect("phases");
    assert_eq!(task_phase(&life, "alpha"), Some(TaskPhase::Planned));
    assert_eq!(task_phase(&life, "beta"), Some(TaskPhase::Planned));
    assert_eq!(task_phase(&life, "gamma"), Some(TaskPhase::Dispatched));
    assert_eq!(task_phase(&life, "delta"), Some(TaskPhase::Implemented));
    assert_eq!(task_phase(&life, "epsilon"), Some(TaskPhase::Reviewed));
    assert_eq!(task_phase(&life, "zeta"), Some(TaskPhase::Accepted));
    assert_eq!(task_phase(&life, "eta"), None);
    assert_eq!(life.release, None);

    ledger
        .start_review(
            "release", None, None, "feature", "codex", None, "sha-head", None, "fp",
        )
        .expect("release start");
    ledger
        .record_review(
            "release",
            None,
            "codex",
            "sha-head",
            "sha-head",
            "fp",
            "approved",
            release_review_body(),
            false,
        )
        .expect("release record");

    let stale = ledger
        .lifecycle("sha-next", &shas(&["sha-zeta"]))
        .expect("stale head");
    assert_eq!(stale.release, None);
    assert_eq!(task_phase(&stale, "zeta"), Some(TaskPhase::Accepted));

    let claimed = ledger
        .lifecycle("sha-head", &shas(&["sha-zeta"]))
        .expect("claimed task");
    assert_eq!(task_phase(&claimed, "gamma"), Some(TaskPhase::Dispatched));
    assert_eq!(task_phase(&claimed, "zeta"), Some(TaskPhase::Accepted));
    assert_eq!(claimed.release, None);
    ledger.finish_task("gamma", None).expect("clear claim");

    let blocked = ledger
        .lifecycle("sha-head", &shas(&["sha-delta"]))
        .expect("unaccepted scope");
    assert_eq!(blocked.release, None);

    let reviewed = ledger
        .lifecycle("sha-head", &shas(&["sha-zeta"]))
        .expect("release reviewed");
    assert_eq!(reviewed.release, Some(ReleasePhase::ReleaseReviewed));

    ledger
        .authorize_release("feature", "sha-head", "sha-head", "maintainer", None)
        .expect("authorize");
    let signed = ledger
        .lifecycle("sha-head", &shas(&["sha-delta"]))
        .expect("signed off");
    assert_eq!(signed.release, Some(ReleasePhase::SignedOff));
    assert_eq!(task_phase(&signed, "zeta"), Some(TaskPhase::Accepted));

    let invalidated = ledger
        .lifecycle("sha-next", &shas(&["sha-zeta"]))
        .expect("invalidated");
    assert_eq!(invalidated.release, None);
    assert_eq!(task_phase(&invalidated, "zeta"), Some(TaskPhase::Accepted));
    assert_eq!(
        task_phase(&invalidated, "epsilon"),
        Some(TaskPhase::Reviewed)
    );

    drop(ledger);
    drop(dir);
}

#[test]
fn release_reviewed_requires_no_claimed_task() {
    let (dir, path) = fresh_path();
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.create_task("alpha", "Accepted").expect("task");
    ledger.claim_task("alpha", "cursor").expect("claim");
    ledger.finish_task("alpha", None).expect("done");
    set_commit_sha(&path, "alpha", "sha-alpha");
    record_task(&mut ledger, "alpha", "sha-alpha");
    ledger.accept_report("alpha").expect("accept");
    ledger.create_task("gamma", "Claimed").expect("task");
    ledger.claim_task("gamma", "cursor").expect("claim");
    record_release(&mut ledger, "sha-head", "approved");

    let blocked = ledger
        .lifecycle("sha-head", &shas(&["sha-alpha"]))
        .expect("claimed blocks release");
    assert_eq!(task_phase(&blocked, "alpha"), Some(TaskPhase::Accepted));
    assert_eq!(task_phase(&blocked, "gamma"), Some(TaskPhase::Dispatched));
    assert_eq!(blocked.release, None);

    ledger.finish_task("gamma", None).expect("clear claim");
    let reviewed = ledger
        .lifecycle("sha-head", &shas(&["sha-alpha"]))
        .expect("release reviewed");
    assert_eq!(reviewed.release, Some(ReleasePhase::ReleaseReviewed));
    drop(ledger);
    drop(dir);
}

#[test]
fn later_release_rejection_blocks_release_at_same_sha() {
    let (_dir, mut ledger) = done_task();
    let approval = record_release(&mut ledger, "sha-head", "approved");
    ledger
        .start_review(
            "release", None, None, "feature", "codex", None, "sha-head", None, "fp",
        )
        .expect("newer pending");
    let pending = ledger
        .authorize_release("feature", "sha-head", "sha-head", "maintainer", None)
        .expect_err("pending is not approval");
    assert_eq!(pending.to_string(), "no approved release review");
    assert_eq!(
        ledger.lifecycle("sha-head", &[]).expect("pending").release,
        None
    );

    let rejected = ledger
        .record_review(
            "release",
            None,
            "codex",
            "sha-head",
            "sha-head",
            "fp",
            "changes-requested",
            release_review_body(),
            false,
        )
        .expect("reject");
    assert!(rejected.id > approval.id);
    let rejected_auth = ledger
        .authorize_release("feature", "sha-head", "sha-head", "maintainer", None)
        .expect_err("newer rejection");
    assert_eq!(rejected_auth.to_string(), "no approved release review");
    assert_eq!(
        ledger.lifecycle("sha-head", &[]).expect("rejected").release,
        None
    );

    let again = record_release(&mut ledger, "sha-head", "approved");
    assert_eq!(
        ledger
            .lifecycle("sha-head", &[])
            .expect("reapproved")
            .release,
        Some(ReleasePhase::ReleaseReviewed)
    );
    let authorization = ledger
        .authorize_release("feature", "sha-head", "sha-head", "maintainer", None)
        .expect("later approval");
    assert_eq!(authorization.review_id, again.id);
}

#[test]
fn repeated_review_start_rejects_superseded_session() {
    let (_dir, mut ledger) = done_task();
    ledger
        .start_review(
            "task",
            Some("auth"),
            None,
            "feature",
            "codex",
            None,
            "aaa",
            None,
            "fp",
        )
        .expect("first");
    ledger
        .start_review(
            "task",
            Some("auth"),
            None,
            "feature",
            "codex",
            None,
            "aaa",
            None,
            "fp",
        )
        .expect("second");
    let approved = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "aaa",
            "aaa",
            "fp",
            "approved",
            task_review_body(),
            false,
        )
        .expect("record current");
    assert_eq!(approved.verdict.as_deref(), Some("approved"));
    let superseded = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "aaa",
            "aaa",
            "fp",
            "changes-requested",
            task_review_body(),
            false,
        )
        .expect_err("superseded");
    assert_eq!(superseded.to_string(), "no pending review");
    let task = &ledger.context(Some("auth"), 20).expect("context").tasks[0].task;
    assert_eq!(task.status, Status::Done);
    let latest = ledger
        .latest_review("task", Some("auth"))
        .expect("latest")
        .expect("row");
    assert_eq!(latest.id, approved.id);
    assert_eq!(latest.verdict.as_deref(), Some("approved"));
}

fn done_task() -> (tempfile::TempDir, Ledger) {
    let (dir, path) = fresh_path();
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.create_task("auth", "Add auth").expect("task");
    ledger.claim_task("auth", "cursor").expect("claim");
    ledger.finish_task("auth", None).expect("done");
    (dir, ledger)
}

fn record_release(ledger: &mut Ledger, sha: &str, verdict: &str) -> relaybook_core::Review {
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
            verdict,
            release_review_body(),
            false,
        )
        .expect("record")
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

fn set_commit_sha(path: &Path, slug: &str, sha: &str) {
    let conn = Connection::open(path).expect("open");
    let changed = conn
        .execute(
            "UPDATE reports SET commit_sha = ?1
             WHERE id = (
                 SELECT r.id FROM reports AS r
                 JOIN tasks AS t ON t.id = r.task_id
                 WHERE t.slug = ?2 AND r.kind = 'task'
                 ORDER BY r.id DESC
                 LIMIT 1
             )",
            rusqlite::params![sha, slug],
        )
        .expect("sha");
    assert_eq!(changed, 1, "{slug}");
}

fn shas(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn task_phase(life: &relaybook_core::lifecycle::Lifecycle, slug: &str) -> Option<TaskPhase> {
    life.tasks
        .iter()
        .find(|task| task.slug == slug)
        .unwrap_or_else(|| panic!("missing {slug}"))
        .phase
}
