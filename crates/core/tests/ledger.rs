use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

use relaybook_core::{EventKind, Ledger, Sections, Status, replay};

#[test]
fn replay_matches_projection_and_progress_is_once_per_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");

    ledger.create_task("auth", "Add auth").expect("create");
    ledger.add_step("auth", "hash passwords").expect("step");
    ledger.claim_task("auth", "cursor").expect("claim");
    ledger
        .report_progress("auth", "halfway", Some("abc123"))
        .expect("progress");
    ledger
        .log_decision("auth", "use argon2", "claude-code")
        .expect("decision");

    let mid = ledger.context(Some("auth"), 50).expect("context");
    let replayed_mid = replay(&mid.events).expect("replay");
    assert_eq!(replayed_mid[0].status, Status::Claimed);
    assert_eq!(replayed_mid[0].claimed_by.as_deref(), Some("cursor"));
    assert!(
        mid.events
            .iter()
            .any(|event| event.kind == EventKind::Progress)
    );

    ledger.complete_task("auth").expect("done");
    let again = ledger.complete_task("auth").expect("done again");
    assert_eq!(again.status, Status::Done);

    let ctx = ledger.context(Some("auth"), 50).expect("context");
    let replayed = replay(&ctx.events).expect("replay");
    let task = &ctx.tasks[0];
    assert_eq!(task.steps[0].description, "hash passwords");
    assert_eq!(replayed[0].title, task.task.title);
    assert_eq!(replayed[0].status, task.task.status);
    assert_eq!(replayed[0].claimed_by, task.task.claimed_by);
    assert_eq!(
        replayed[0].decisions[0].rationale,
        task.decisions[0].rationale
    );
    assert_eq!(
        replayed[0].decisions[0].provenance,
        task.decisions[0].provenance
    );
    assert_eq!(
        ctx.events
            .iter()
            .filter(|event| event.kind == EventKind::StatusChanged)
            .count(),
        1
    );

    let before = ctx.events.len();
    let err = ledger
        .report_progress("auth", "again", Some("abc123"))
        .expect_err("duplicate progress");
    assert_eq!(err.to_string(), "already reported for abc123");
    let after = ledger.context(Some("auth"), 50).expect("context");
    assert_eq!(after.events.len(), before);

    ledger
        .report_progress("auth", "no commit", None)
        .expect("progress");
    ledger
        .report_progress("auth", "still no commit", None)
        .expect("second progress without a commit");
}

#[test]
fn two_claims_one_winner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.create_task("auth", "Add auth").expect("create");
    drop(ledger);

    let barrier = Arc::new(Barrier::new(2));
    let first = spawn_claim(path.clone(), "alpha", Arc::clone(&barrier));
    let second = spawn_claim(path.clone(), "beta", barrier);
    let results = [
        first.join().expect("thread"),
        second.join().expect("thread"),
    ];

    let wins = results.iter().filter(|result| result.is_ok()).count();
    let losses = results.iter().filter(|result| result.is_err()).count();
    assert_eq!(wins, 1, "{results:?}");
    assert_eq!(losses, 1, "{results:?}");
    let loss = results
        .iter()
        .find_map(|result| result.as_ref().err())
        .expect("loss");
    assert_eq!(loss.to_string(), "already claimed");

    let ledger = Ledger::open(&path).expect("open");
    let ctx = ledger.context(Some("auth"), 20).expect("context");
    let by = ctx.tasks[0].task.claimed_by.as_deref();
    assert!(by == Some("alpha") || by == Some("beta"), "{by:?}");
    assert_eq!(
        ctx.events
            .iter()
            .filter(|event| event.kind == EventKind::Claimed)
            .count(),
        1
    );
}

#[test]
fn block_task_from_open_or_claimed_and_rejects_done() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");

    assert_eq!(
        ledger
            .block_task("missing", "stuck")
            .expect_err("missing")
            .to_string(),
        "no such task: missing"
    );

    ledger.create_task("auth", "Add auth").expect("create");
    let blocked = ledger
        .block_task("auth", "waiting on schema")
        .expect("block");
    assert_eq!(blocked.status, Status::Blocked);
    let again = ledger.block_task("auth", "still waiting").expect("repeat");
    assert_eq!(again.status, Status::Blocked);

    let ctx = ledger.context(Some("auth"), 20).expect("context");
    let changes: Vec<_> = ctx
        .events
        .iter()
        .filter(|event| event.kind == EventKind::StatusChanged)
        .collect();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].payload["to"], "blocked");
    assert_eq!(changes[0].payload["from"], "open");
    assert_eq!(changes[0].payload["reason"], "waiting on schema");
    let replayed = replay(&ctx.events).expect("replay");
    assert_eq!(replayed[0].status, Status::Blocked);

    ledger
        .create_task("ledger", "Open the ledger")
        .expect("create");
    ledger.claim_task("ledger", "cursor").expect("claim");
    let claimed = ledger
        .block_task("ledger", "needs a decision")
        .expect("block claimed");
    assert_eq!(claimed.status, Status::Blocked);
    assert_eq!(claimed.claimed_by.as_deref(), Some("cursor"));

    ledger.create_task("ship", "Ship it").expect("create");
    ledger.claim_task("ship", "cursor").expect("claim");
    ledger.complete_task("ship").expect("done");
    assert_eq!(
        ledger
            .block_task("ship", "too late")
            .expect_err("done")
            .to_string(),
        "cannot block a done task"
    );
}

#[test]
fn dependencies_gate_claims_and_reject_cycles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");

    ledger.create_task("base", "Base").expect("create");
    ledger.create_task("other", "Other").expect("create");
    let feature = ledger
        .create_task_depending(
            "feature",
            "Feature",
            &["base".to_string(), "other".to_string()],
        )
        .expect("create");
    assert_eq!(
        feature.depends_on,
        vec!["base".to_string(), "other".to_string()]
    );
    assert_eq!(
        ledger
            .claim_task("feature", "cursor")
            .expect_err("waiting")
            .to_string(),
        "waiting on: base, other"
    );

    ledger.claim_task("base", "cursor").expect("claim");
    ledger.complete_task("base").expect("done");
    assert_eq!(
        ledger
            .claim_task("feature", "cursor")
            .expect_err("still waiting")
            .to_string(),
        "waiting on: other"
    );
    ledger.claim_task("other", "cursor").expect("claim");
    ledger.complete_task("other").expect("done");
    let claimed = ledger.claim_task("feature", "cursor").expect("claim");
    assert_eq!(claimed.status, Status::Claimed);
    assert!(claimed.needs.is_empty());

    ledger.add_dependency("base", "other").expect("link");
    assert_eq!(
        ledger
            .add_dependency("other", "base")
            .expect_err("cycle")
            .to_string(),
        "dependency cycle: other -> base -> other"
    );
    assert_eq!(
        ledger
            .add_dependency("base", "base")
            .expect_err("self")
            .to_string(),
        "cannot depend on itself"
    );
    assert_eq!(
        ledger
            .add_dependency("base", "missing")
            .expect_err("missing")
            .to_string(),
        "no such task: missing"
    );

    let again = ledger.add_dependency("base", "other").expect("repeat");
    assert_eq!(again.depends_on, vec!["other".to_string()]);
    ledger
        .remove_dependency("base", "missing-link")
        .expect_err("missing task");
    ledger
        .remove_dependency("base", "feature")
        .expect("missing link");
    let unlinked = ledger.remove_dependency("base", "other").expect("unlink");
    assert!(unlinked.depends_on.is_empty());

    let ctx = ledger.context(Some("feature"), 20).expect("context");
    let replayed = replay(&ctx.events).expect("replay");
    let feature_state = replayed
        .iter()
        .find(|task| task.slug == "feature")
        .expect("feature");
    assert_eq!(
        feature_state.depends_on,
        vec!["base".to_string(), "other".to_string()]
    );
    let ready = ledger.list_tasks(None, true, false).expect("ready");
    assert!(ready.iter().all(|task| task.slug != "feature"));

    assert_eq!(
        ledger
            .create_task_depending("loop", "Loop", &["missing".to_string()])
            .expect_err("missing dep")
            .to_string(),
        "no such task: missing"
    );
    assert_eq!(
        ledger
            .context(Some("loop"), 1)
            .expect_err("rolled back")
            .to_string(),
        "no such task: loop"
    );
}

#[test]
fn rename_updates_any_status_and_skips_an_unchanged_title() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");

    ledger.create_task("auth", "Add auth").expect("create");
    let renamed = ledger.rename_task("auth", "  Add login  ").expect("rename");
    assert_eq!(renamed.slug, "auth");
    assert_eq!(renamed.title, "Add login");
    let ctx = ledger.context(Some("auth"), 20).expect("context");
    let renames = ctx
        .events
        .iter()
        .filter(|event| event.kind == EventKind::TaskRenamed)
        .count();
    assert_eq!(renames, 1);
    assert_eq!(ctx.events[0].payload["from"], "Add auth");
    assert_eq!(ctx.events[0].payload["to"], "Add login");
    let replayed = replay(&ctx.events).expect("replay");
    assert_eq!(replayed[0].title, "Add login");

    let again = ledger.rename_task("auth", "Add login").expect("same title");
    assert_eq!(again.title, "Add login");
    let after = ledger.context(Some("auth"), 20).expect("context");
    let renames_after = after
        .events
        .iter()
        .filter(|event| event.kind == EventKind::TaskRenamed)
        .count();
    assert_eq!(renames_after, 1);

    assert_eq!(
        ledger
            .rename_task("auth", "   ")
            .expect_err("blank")
            .to_string(),
        "title is required"
    );
    assert_eq!(
        ledger
            .rename_task("missing", "Title")
            .expect_err("missing")
            .to_string(),
        "no such task: missing"
    );

    ledger.claim_task("auth", "cursor").expect("claim");
    ledger.complete_task("auth").expect("done");
    let done = ledger
        .rename_task("auth", "Add login later")
        .expect("rename done");
    assert_eq!(done.status, Status::Done);
    assert_eq!(done.title, "Add login later");
}

#[test]
fn unblock_restores_the_owner_and_ignores_other_statuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");

    ledger.create_task("auth", "Add auth").expect("create");
    ledger.claim_task("auth", "cursor").expect("claim");
    ledger.block_task("auth", "waiting").expect("block");
    let restored = ledger
        .unblock_task("auth", Some("schema landed"))
        .expect("unblock");
    assert_eq!(restored.status, Status::Claimed);
    assert_eq!(restored.claimed_by.as_deref(), Some("cursor"));
    let ctx = ledger.context(Some("auth"), 20).expect("context");
    let change = ctx
        .events
        .iter()
        .find(|event| event.payload["to"] == "claimed")
        .expect("event");
    assert_eq!(change.payload["from"], "blocked");
    assert_eq!(change.payload["note"], "schema landed");
    let replayed = replay(&ctx.events).expect("replay");
    assert_eq!(replayed[0].status, Status::Claimed);

    let again = ledger.unblock_task("auth", None).expect("noop");
    assert_eq!(again.status, Status::Claimed);
    let after = ledger.context(Some("auth"), 20).expect("context");
    let claimed_changes = after
        .events
        .iter()
        .filter(|event| event.payload["to"] == "claimed")
        .count();
    assert_eq!(claimed_changes, 1);

    ledger.create_task("open", "Open").expect("create");
    ledger.block_task("open", "stuck").expect("block");
    let opened = ledger.unblock_task("open", None).expect("open");
    assert_eq!(opened.status, Status::Open);
    assert!(opened.claimed_by.is_none());

    assert_eq!(
        ledger
            .unblock_task("missing", None)
            .expect_err("missing")
            .to_string(),
        "no such task: missing"
    );
}

#[test]
fn edit_step_updates_text_and_keeps_a_checked_step() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.create_task("auth", "Add auth").expect("create");
    ledger.add_step("auth", "hash passwords").expect("step");
    ledger.check_step("auth", 1).expect("check");
    let before = ledger.context(Some("auth"), 20).expect("context");
    let edited = ledger
        .edit_step("auth", 1, "  hash passwords with argon2  ")
        .expect("edit");
    assert!(edited.done);
    assert_eq!(edited.description, "hash passwords with argon2");
    let again = ledger
        .edit_step("auth", 1, "hash passwords with argon2")
        .expect("noop");
    assert_eq!(again.description, edited.description);
    let after = ledger.context(Some("auth"), 20).expect("context");
    assert_eq!(after.events.len(), before.events.len());
    assert_eq!(
        ledger
            .edit_step("auth", 1, "   ")
            .expect_err("blank")
            .to_string(),
        "text is required"
    );
    assert_eq!(
        ledger
            .edit_step("missing", 1, "text")
            .expect_err("task")
            .to_string(),
        "no such task: missing"
    );
    assert_eq!(
        ledger
            .edit_step("auth", 9, "text")
            .expect_err("step")
            .to_string(),
        "no such step: 9"
    );
}

#[test]
fn oversized_report_rolls_back_the_done_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.create_task("auth", "Add auth").expect("create");
    ledger.claim_task("auth", "cursor").expect("claim");
    let huge = "x".repeat(1600);
    let err = ledger
        .finish_task(
            "auth",
            Some(Sections {
                changed: vec![huge],
                verified: Vec::new(),
                not_verified: Vec::new(),
                deviations: Vec::new(),
                review: Vec::new(),
            }),
            None,
        )
        .expect_err("cap");
    assert!(err.to_string().starts_with("report is "));
    assert!(err.to_string().contains("limit 1500"));
    let task = ledger.context(Some("auth"), 5).expect("context");
    assert_eq!(task.tasks[0].task.status, Status::Claimed);

    ledger
        .finish_task(
            "auth",
            Some(Sections {
                changed: vec!["shipped the report".to_string()],
                verified: vec!["cargo test".to_string()],
                not_verified: Vec::new(),
                deviations: Vec::new(),
                review: vec!["check the inbox".to_string()],
            }),
            None,
        )
        .expect("done");
    let again = ledger
        .finish_task("auth", None, None)
        .expect("already done");
    assert_eq!(again.status, Status::Done);
    let conn = rusqlite::Connection::open(&path).expect("open");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM reports", [], |row| row.get(0))
        .expect("count");
    assert_eq!(count, 1);

    let note = ledger.add_note("cursor", "repo is private").expect("note");
    assert!(note > 0);
    let too_long = "n".repeat(500);
    assert!(ledger.add_note("cursor", &too_long).is_err());
}

#[test]
fn done_on_an_open_task_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.create_task("auth", "Add auth").expect("create");
    let err = ledger
        .finish_task(
            "auth",
            Some(Sections {
                changed: vec!["shipped".to_string()],
                verified: Vec::new(),
                not_verified: Vec::new(),
                deviations: Vec::new(),
                review: Vec::new(),
            }),
            None,
        )
        .expect_err("open");
    assert_eq!(
        err.to_string(),
        "claim it first: relaybook task claim auth --by <agent>"
    );
    let task = ledger.context(Some("auth"), 20).expect("context");
    assert_eq!(task.tasks[0].task.status, Status::Open);
    let conn = rusqlite::Connection::open(&path).expect("open");
    let reports: i64 = conn
        .query_row("SELECT COUNT(*) FROM reports", [], |row| row.get(0))
        .expect("reports");
    assert_eq!(reports, 0);
    let done_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE kind = 'status_changed'",
            [],
            |row| row.get(0),
        )
        .expect("events");
    assert_eq!(done_events, 0);
}

#[test]
fn finish_task_records_commit_sha_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.create_task("auth", "Add auth").expect("create");
    ledger.claim_task("auth", "cursor").expect("claim");
    ledger
        .finish_task("auth", None, Some("abc123"))
        .expect("done");
    assert_eq!(
        ledger.report_commit_sha("auth").expect("sha").as_deref(),
        Some("abc123")
    );
    ledger
        .finish_task("auth", None, Some("def456"))
        .expect("again");
    assert_eq!(
        ledger.report_commit_sha("auth").expect("sha").as_deref(),
        Some("abc123")
    );
    let conn = rusqlite::Connection::open(&path).expect("open");
    let (commit, base, graph): (Option<String>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT commit_sha, base_sha, graph_sha FROM reports WHERE kind = 'task'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("report");
    assert_eq!(commit.as_deref(), Some("abc123"));
    assert_eq!(base, None);
    assert_eq!(graph, None);
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM reports WHERE kind = 'task'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(count, 1);
}

#[test]
fn finish_task_revalidate_holds_the_write_lock_and_rolls_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.create_task("auth", "Add auth").expect("create");
    ledger.claim_task("auth", "cursor").expect("claim");

    let db = path.clone();
    let err = ledger
        .finish_task_with("auth", None, Some("abc123"), move || {
            let conn = rusqlite::Connection::open(&db).expect("open");
            conn.busy_timeout(std::time::Duration::from_millis(0))
                .expect("busy");
            match conn.execute_batch("BEGIN IMMEDIATE") {
                Err(rusqlite::Error::SqliteFailure(ffi, _))
                    if ffi.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    Err(relaybook_core::Error::StaleHead)
                }
                Err(err) => panic!("unexpected lock error: {err}"),
                Ok(()) => panic!("revalidate ran without the write lock"),
            }
        })
        .expect_err("rolled back");
    assert_eq!(err.to_string(), "HEAD changed before the report was stored");
    let task = ledger.context(Some("auth"), 20).expect("context");
    assert_eq!(task.tasks[0].task.status, Status::Claimed);
    let conn = rusqlite::Connection::open(&path).expect("open");
    let reports: i64 = conn
        .query_row("SELECT COUNT(*) FROM reports", [], |row| row.get(0))
        .expect("reports");
    assert_eq!(reports, 0);
}

#[test]
fn claim_task_revalidate_holds_the_write_lock_and_rolls_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.create_task("auth", "Add auth").expect("create");

    let db = path.clone();
    let err = ledger
        .claim_task_with("auth", "cursor", move || {
            let conn = rusqlite::Connection::open(&db).expect("open");
            conn.busy_timeout(std::time::Duration::from_millis(0))
                .expect("busy");
            match conn.execute_batch("BEGIN IMMEDIATE") {
                Err(rusqlite::Error::SqliteFailure(ffi, _))
                    if ffi.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    Err(relaybook_core::Error::DirtyWorktree("race.txt".to_string()))
                }
                Err(err) => panic!("unexpected lock error: {err}"),
                Ok(()) => panic!("revalidate ran without the write lock"),
            }
        })
        .expect_err("rolled back");
    assert_eq!(err.to_string(), "working tree is not clean: race.txt");
    let task = ledger.context(Some("auth"), 20).expect("context");
    assert_eq!(task.tasks[0].task.status, Status::Open);
    assert_eq!(task.tasks[0].task.claimed_by, None);
    let conn = rusqlite::Connection::open(&path).expect("open");
    let claimed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE kind = 'claimed'",
            [],
            |row| row.get(0),
        )
        .expect("events");
    assert_eq!(claimed, 0);
}

#[test]
fn record_review_revalidate_holds_the_write_lock_and_rolls_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.create_task("auth", "Add auth").expect("create");
    ledger.claim_task("auth", "cursor").expect("claim");
    ledger
        .finish_task("auth", None, Some("abc123"))
        .expect("done");
    ledger
        .start_review(
            "task",
            Some("auth"),
            None,
            "main",
            "codex",
            None,
            "abc123",
            None,
            "fp",
        )
        .expect("start");
    let body =
        r#"{"findings":"none","security":"ok","tests_checked":"cargo test","not_checked":"none"}"#;
    let db = path.clone();
    let err = ledger
        .record_review_with(
            "task",
            Some("auth"),
            "codex",
            "abc123",
            "abc123",
            "fp",
            "changes_requested",
            body,
            false,
            None,
            move || {
                let conn = rusqlite::Connection::open(&db).expect("open");
                conn.busy_timeout(std::time::Duration::from_millis(0))
                    .expect("busy");
                match conn.execute_batch("BEGIN IMMEDIATE") {
                    Err(rusqlite::Error::SqliteFailure(ffi, _))
                        if ffi.code == rusqlite::ErrorCode::DatabaseBusy =>
                    {
                        Err(relaybook_core::Error::InvalidSetting(
                            "refs/remotes/origin/main exists".to_string(),
                        ))
                    }
                    Err(err) => panic!("unexpected lock error: {err}"),
                    Ok(()) => panic!("revalidate ran without the write lock"),
                }
            },
        )
        .expect_err("rolled back");
    assert_eq!(err.to_string(), "refs/remotes/origin/main exists");
    let pending = ledger
        .latest_review("task", Some("auth"))
        .expect("review")
        .expect("row");
    assert_eq!(pending.verdict, None);
    assert_eq!(pending.recorded_at, None);
    let ctx = ledger.context(Some("auth"), 20).expect("context");
    assert_eq!(ctx.tasks[0].task.status, Status::Done);
    let recorded = ledger
        .record_review(
            "task",
            Some("auth"),
            "codex",
            "abc123",
            "abc123",
            "fp",
            "approved",
            body,
            false,
        )
        .expect("still pending");
    assert_eq!(recorded.verdict.as_deref(), Some("approved"));
    let ctx = ledger.context(Some("auth"), 20).expect("context");
    assert_eq!(ctx.tasks[0].task.status, Status::Done);
}

#[test]
fn unconfirmed_classic_write_cannot_overwrite_concurrent_gated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.set_setting("reviewer", "codex").expect("reviewer");
    assert_eq!(ledger.settings().expect("settings").workflow, "classic");

    let barrier = Arc::new(Barrier::new(2));
    let writer_path = path.clone();
    let writer_barrier = Arc::clone(&barrier);
    let writer = thread::spawn(move || {
        writer_barrier.wait();
        let mut other = Ledger::open(&writer_path).expect("open");
        other.set_setting("workflow", "gated").expect("gate");
        assert_eq!(other.settings().expect("settings").workflow, "gated");
        drop(other);
        writer_barrier.wait();
    });

    barrier.wait();
    barrier.wait();
    for value in [
        "classic",
        " classic",
        "classic ",
        " classic ",
        "\tclassic\n",
    ] {
        let err = ledger
            .set_setting("workflow", value)
            .expect_err("unconfirmed leave");
        assert_eq!(
            err.to_string(),
            "leaving workflow=gated requires confirmation at a TTY"
        );
        assert_eq!(ledger.settings().expect("settings").workflow, "gated");
    }
    ledger
        .set_setting_with("workflow", " classic ", true)
        .expect("confirmed leave");
    assert_eq!(ledger.settings().expect("settings").workflow, "classic");
    writer.join().expect("writer");
}

fn spawn_claim(
    path: PathBuf,
    by: &'static str,
    barrier: Arc<Barrier>,
) -> thread::JoinHandle<Result<relaybook_core::Task, relaybook_core::Error>> {
    thread::spawn(move || {
        barrier.wait();
        let mut ledger = Ledger::open(&path).expect("open");
        ledger.claim_task("auth", by)
    })
}

const OWNED: &str = "a gated run owns this; use relaybook approve / run --resume";
const TASK_REVIEW: &str =
    r#"{"findings":"none","security":"ok","tests_checked":"cargo test","not_checked":"none"}"#;
const RELEASE_REVIEW: &str = r#"{"findings":"none","security":"ok","tests_checked":"cargo test","not_checked":"none","prd_covered":true,"gaps":"none"}"#;

fn report_verdict(path: &std::path::Path, slug: &str) -> Option<String> {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.query_row(
        "SELECT r.verdict FROM reports AS r
         JOIN tasks AS t ON t.id = r.task_id
         WHERE t.slug = ?1
         ORDER BY r.id DESC
         LIMIT 1",
        [slug],
        |row| row.get(0),
    )
    .expect("verdict")
}

#[test]
fn stale_run_save_does_not_overwrite_a_newer_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.set_setting("reviewer", "codex").expect("reviewer");
    ledger.set_setting("workflow", "gated").expect("gated");
    let mut run = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("run");
    let mut stale = run.clone();
    run.status = "complete".to_string();
    run.stage = "done".to_string();
    run.stop_reason = Some("https://example.test/pr/1".to_string());
    ledger.save_run(&mut run).expect("complete");
    stale.status = "stopped".to_string();
    stale.stop_reason = Some("lost".to_string());
    let err = ledger.save_run(&mut stale).expect_err("stale");
    assert_eq!(err.to_string(), "run changed concurrently");
    let stored = ledger.latest_run().expect("latest").expect("row");
    assert_eq!(stored.status, "complete");
    assert_eq!(stored.stage, "done");
    assert_eq!(
        stored.stop_reason.as_deref(),
        Some("https://example.test/pr/1")
    );
}

#[test]
fn ownership_writes_see_a_gated_run_that_appeared_before_the_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.set_setting("reviewer", "codex").expect("reviewer");
    ledger.set_setting("workflow", "gated").expect("gated");
    for slug in ["owned", "fresh", "acceptme", "accepttoo", "free"] {
        ledger.create_task(slug, slug).expect("task");
    }
    for slug in ["acceptme", "accepttoo"] {
        ledger
            .claim_task_anchored(slug, "cursor", None, || Ok(Some("base".to_string())))
            .expect("claim");
        ledger
            .finish_task(slug, None, Some("abc123"))
            .expect("done");
        ledger
            .start_review(
                "task",
                Some(slug),
                None,
                "feature",
                "codex",
                None,
                "abc123",
                None,
                "fp",
            )
            .expect("start");
        ledger
            .record_review(
                "task",
                Some(slug),
                "codex",
                "abc123",
                "abc123",
                "fp",
                "approved",
                TASK_REVIEW,
                false,
            )
            .expect("record");
    }
    ledger
        .start_review(
            "task",
            Some("owned"),
            None,
            "feature",
            "codex",
            None,
            "abc123",
            None,
            "fp",
        )
        .expect("pending task");
    ledger
        .start_review(
            "release", None, None, "feature", "codex", None, "abc123", None, "fp",
        )
        .expect("pending release");
    assert!(ledger.active_run().expect("active").is_none());

    let barrier = Arc::new(Barrier::new(2));
    let worker_path = path.clone();
    let worker_barrier = Arc::clone(&barrier);
    let worker = thread::spawn(move || {
        worker_barrier.wait();
        let mut other = Ledger::open(&worker_path).expect("open");
        let mut run = other
            .start_run("prd.md", "digest", "feature", Some("gated"))
            .expect("run");
        run.stage = "gate_plan".to_string();
        run.status = "waiting".to_string();
        other.save_run(&mut run).expect("wait");
        let conn = rusqlite::Connection::open(&worker_path).expect("open");
        for slug in ["owned", "fresh", "acceptme", "accepttoo"] {
            conn.execute(
                "UPDATE tasks SET run_id = ?1 WHERE slug = ?2",
                rusqlite::params![run.id, slug],
            )
            .expect("link");
        }
        worker_barrier.wait();
    });
    barrier.wait();
    barrier.wait();
    worker.join().expect("worker");

    let err = ledger
        .set_setting_with("workflow", "classic", true)
        .expect_err("leave");
    assert_eq!(err.to_string(), OWNED);
    assert_eq!(ledger.settings().expect("settings").workflow, "gated");
    ledger.set_setting("mode", "auto").expect("other settings");

    let err = ledger
        .start_review(
            "task",
            Some("fresh"),
            None,
            "feature",
            "codex",
            None,
            "abc123",
            None,
            "fp",
        )
        .expect_err("fresh");
    assert_eq!(err.to_string(), OWNED);
    assert!(
        ledger
            .latest_review("task", Some("fresh"))
            .expect("review")
            .is_none()
    );

    let release_id = ledger
        .latest_review("release", None)
        .expect("release")
        .expect("pending")
        .id;
    let err = ledger
        .record_review(
            "release",
            None,
            "codex",
            "abc123",
            "abc123",
            "fp",
            "approved",
            RELEASE_REVIEW,
            false,
        )
        .expect_err("record release");
    assert_eq!(err.to_string(), OWNED);
    let release = ledger
        .latest_review("release", None)
        .expect("release")
        .expect("row");
    assert_eq!(release.id, release_id);
    assert_eq!(release.verdict, None);

    let err = ledger
        .start_review(
            "release", None, None, "feature", "codex", None, "abc123", None, "fp",
        )
        .expect_err("start release");
    assert_eq!(err.to_string(), OWNED);
    assert_eq!(
        ledger
            .latest_review("release", None)
            .expect("release")
            .expect("row")
            .id,
        release_id
    );

    let err = ledger
        .record_review(
            "task",
            Some("owned"),
            "codex",
            "abc123",
            "abc123",
            "fp",
            "approved",
            TASK_REVIEW,
            false,
        )
        .expect_err("record owned");
    assert_eq!(err.to_string(), OWNED);
    let owned = ledger
        .latest_review("task", Some("owned"))
        .expect("review")
        .expect("row");
    assert_eq!(owned.verdict, None);

    let err = ledger
        .accept_gated_report("acceptme", "abc123", || Ok(()))
        .expect_err("accept gated");
    assert_eq!(err.to_string(), OWNED);
    let err = ledger.accept_report("accepttoo").expect_err("accept");
    assert_eq!(err.to_string(), OWNED);
    assert_eq!(report_verdict(&path, "acceptme"), None);
    assert_eq!(report_verdict(&path, "accepttoo"), None);

    ledger
        .start_review(
            "task",
            Some("free"),
            None,
            "feature",
            "codex",
            None,
            "abc123",
            None,
            "fp",
        )
        .expect("free task is not owned");
}

const RELEASE_BODY: &str = r#"{"findings":"none","security":"ok","tests_checked":"cargo test","not_checked":"none","prd_covered":true,"gaps":"none"}"#;

#[test]
fn authorize_release_derives_run_id_and_refuses_a_live_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.set_setting("reviewer", "codex").expect("reviewer");
    ledger.set_setting("workflow", "gated").expect("gated");
    ledger
        .start_review(
            "release", None, None, "feature", "codex", None, "abc", None, "fp",
        )
        .expect("start");
    ledger
        .record_review(
            "release",
            None,
            "codex",
            "abc",
            "abc",
            "fp",
            "approved",
            RELEASE_BODY,
            false,
        )
        .expect("record");
    let mut run = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("run");
    let err = ledger
        .authorize_release("feature", "abc", "abc", "maintainer", None)
        .expect_err("active run stores no null id");
    assert_eq!(err.to_string(), OWNED);
    let err = ledger
        .authorize_release("feature", "abc", "abc", "maintainer", Some(run.id))
        .expect_err("active run");
    assert_eq!(err.to_string(), OWNED);
    let auths: i64 = rusqlite::Connection::open(&path)
        .expect("open")
        .query_row("SELECT COUNT(*) FROM release_authorizations", [], |row| {
            row.get(0)
        })
        .expect("count");
    assert_eq!(auths, 0);

    run.stage = "gate_release".to_string();
    run.status = "waiting".to_string();
    ledger.save_run(&mut run).expect("wait");
    let auth = ledger
        .authorize_release("feature", "abc", "abc", "maintainer", None)
        .expect("authorize");
    assert_eq!(auth.run_id, Some(run.id));
}

#[test]
fn claim_run_rejects_a_different_run_while_publishing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.set_setting("reviewer", "codex").expect("reviewer");
    ledger.set_setting("workflow", "gated").expect("gated");
    let mut older = ledger
        .start_run("old.md", "digest", "feature", Some("gated"))
        .expect("older");
    older.status = "stopped".to_string();
    ledger.save_run(&mut older).expect("stop");
    let mut newer = ledger
        .start_run("new.md", "digest", "feature", Some("gated"))
        .expect("newer");
    newer.stage = "publish".to_string();
    ledger.save_run(&mut newer).expect("publish");
    older.status = "active".to_string();
    let err = ledger.claim_run(&mut older).expect_err("resume");
    assert_eq!(err.to_string(), "a run is already publishing");
    let stored = ledger.run_by_id(older.id).expect("older");
    assert_eq!(stored.status, "stopped");
    let publisher = ledger.run_by_id(newer.id).expect("newer");
    assert_eq!(publisher.status, "active");
    assert_eq!(publisher.stage, "publish");
}

fn review_count(path: &std::path::Path, slug: &str) -> i64 {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.query_row(
        "SELECT COUNT(*) FROM reviews AS r
         JOIN tasks AS t ON t.id = r.task_id
         WHERE t.slug = ?1",
        [slug],
        |row| row.get(0),
    )
    .expect("reviews")
}

#[test]
fn review_and_accept_refuse_a_task_owned_by_an_older_live_gated_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.set_setting("reviewer", "codex").expect("reviewer");
    ledger.create_task("owned", "Owned").expect("owned");
    ledger.create_task("free", "Free").expect("free");
    ledger.claim_task("owned", "cursor").expect("claim");
    ledger
        .finish_task("owned", None, Some("abc123"))
        .expect("done");
    ledger
        .start_review(
            "task",
            Some("owned"),
            None,
            "feature",
            "codex",
            None,
            "abc123",
            None,
            "fp",
        )
        .expect("start");
    ledger
        .record_review(
            "task",
            Some("owned"),
            "codex",
            "abc123",
            "abc123",
            "fp",
            "approved",
            TASK_REVIEW,
            false,
        )
        .expect("record");
    let reviews_before = review_count(&path, "owned");
    assert_eq!(reviews_before, 1);

    let older = ledger
        .start_run("old.md", "digest", "feature", Some("gated"))
        .expect("older");
    let conn = rusqlite::Connection::open(&path).expect("open");
    conn.execute(
        "UPDATE tasks SET run_id = ?1 WHERE slug = 'owned'",
        [older.id],
    )
    .expect("link");
    drop(conn);
    let newer = ledger
        .start_run("new.md", "digest", "feature", Some("classic"))
        .expect("classic");
    assert!(newer.id > older.id);
    assert_eq!(
        ledger.active_run().expect("active").expect("newest").id,
        newer.id
    );

    let err = ledger
        .start_review(
            "task",
            Some("owned"),
            None,
            "feature",
            "codex",
            None,
            "abc123",
            None,
            "fp",
        )
        .expect_err("review");
    assert_eq!(err.to_string(), OWNED);
    assert_eq!(review_count(&path, "owned"), reviews_before);

    let err = ledger
        .accept_gated_report("owned", "abc123", || Ok(()))
        .expect_err("accept gated");
    assert_eq!(err.to_string(), OWNED);
    let err = ledger.accept_report("owned").expect_err("accept");
    assert_eq!(err.to_string(), OWNED);
    assert_eq!(report_verdict(&path, "owned"), None);

    let err = ledger
        .set_setting_with("workflow", "gated", true)
        .expect_err("workflow");
    assert_eq!(err.to_string(), OWNED);
    assert_eq!(ledger.settings().expect("settings").workflow, "classic");

    ledger
        .start_review(
            "task",
            Some("free"),
            None,
            "feature",
            "codex",
            None,
            "abc123",
            None,
            "fp",
        )
        .expect("free task is not owned");

    let mut older = ledger.run_by_id(older.id).expect("reload");
    older.status = "stopped".to_string();
    ledger.save_run(&mut older).expect("stop");
    ledger.accept_report("owned").expect("stopped owner");
    assert_eq!(report_verdict(&path, "owned").as_deref(), Some("accepted"));
}

#[test]
fn two_connections_cannot_insert_or_activate_a_second_live_gated_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    {
        let mut ledger = Ledger::initialize(&path).expect("init");
        ledger.set_setting("reviewer", "codex").expect("reviewer");
        ledger.set_setting("workflow", "gated").expect("gated");
    }
    let barrier = Arc::new(Barrier::new(2));
    let spawn = |path: PathBuf, barrier: Arc<Barrier>| {
        thread::spawn(move || {
            barrier.wait();
            let mut ledger = Ledger::open(&path).expect("open");
            ledger.start_run("race.md", "digest", "feature", Some("gated"))
        })
    };
    let first = spawn(path.clone(), Arc::clone(&barrier));
    let second = spawn(path.clone(), barrier);
    let first = first.join().expect("first");
    let second = second.join().expect("second");
    let (winner, loser) = match (first, second) {
        (Ok(run), Err(err)) => (run, err),
        (Err(err), Ok(run)) => (run, err),
        (Ok(left), Ok(right)) => panic!("both inserts landed: {} and {}", left.id, right.id),
        (Err(left), Err(right)) => panic!("both inserts failed: {left} / {right}"),
    };
    assert_eq!(loser.to_string(), "a gated run is already live");

    let mut left = Ledger::open(&path).expect("left");
    let mut right = Ledger::open(&path).expect("right");
    let version: i64 = rusqlite::Connection::open(&path)
        .expect("open")
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version");
    assert_eq!(version, 8);
    let index: String = rusqlite::Connection::open(&path)
        .expect("open")
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'runs_one_live_gated'",
            [],
            |row| row.get(0),
        )
        .expect("index");
    assert!(index.contains("workflow = 'gated'"), "{index}");

    let mut winner = left.run_by_id(winner.id).expect("winner");
    winner.status = "stopped".to_string();
    left.save_run(&mut winner).expect("stop winner");
    let mut parked = right
        .start_run("old.md", "digest", "feature", Some("gated"))
        .expect("parked");
    parked.status = "stopped".to_string();
    right.save_run(&mut parked).expect("park");
    let mut live = left
        .start_run("new.md", "digest", "feature", Some("gated"))
        .expect("live");
    parked.status = "active".to_string();
    let err = right.save_run(&mut parked).expect_err("activate");
    assert_eq!(err.to_string(), "a gated run is already live");
    assert_eq!(
        right.run_by_id(parked.id).expect("parked").status,
        "stopped"
    );
    assert_eq!(left.run_by_id(live.id).expect("live").status, "active");

    live.status = "paused".to_string();
    left.save_run(&mut live).expect("pause");
    let err = right
        .start_run("paused.md", "digest", "feature", Some("gated"))
        .expect_err("paused slot");
    assert_eq!(err.to_string(), "a gated run is already live");
    live.status = "waiting".to_string();
    left.save_run(&mut live).expect("wait");
    let err = right
        .start_run("waiting.md", "digest", "feature", Some("gated"))
        .expect_err("waiting slot");
    assert_eq!(err.to_string(), "a gated run is already live");
    live.status = "stopped".to_string();
    live.stage = "publishing".to_string();
    left.save_run(&mut live).expect("claim held");
    let err = right
        .start_run("claim.md", "digest", "feature", Some("gated"))
        .expect_err("publish claim");
    assert_eq!(err.to_string(), "a gated run is already live");
    live.stage = "publish".to_string();
    left.save_run(&mut live).expect("release claim");

    left.set_setting_with("workflow", "classic", true)
        .expect("classic");
    let classic = left
        .start_run("c1.md", "digest", "feature", Some("classic"))
        .expect("classic");
    let other = right
        .start_run("c2.md", "digest", "feature", Some("classic"))
        .expect("second classic");
    let alongside = left
        .start_run("g.md", "digest", "feature", Some("gated"))
        .expect("gated beside classic");
    let err = right
        .start_run("g2.md", "digest", "feature", Some("gated"))
        .expect_err("second gated");
    assert_eq!(err.to_string(), "a gated run is already live");
    assert_eq!(left.run_by_id(classic.id).expect("c1").status, "active");
    assert_eq!(right.run_by_id(other.id).expect("c2").status, "active");
    assert_eq!(
        left.run_by_id(alongside.id).expect("gated").status,
        "active"
    );
    let version: i64 = rusqlite::Connection::open(&path)
        .expect("open")
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version");
    assert_eq!(version, 8);
}

#[test]
fn publishing_refuses_complete_assign_and_reclaim_and_the_publisher_save_lands() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.set_setting("reviewer", "codex").expect("reviewer");
    ledger.set_setting("workflow", "gated").expect("gated");
    let mut publisher = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("run");
    publisher.stage = "publishing".to_string();
    ledger.save_run(&mut publisher).expect("claim");
    let stamp = publisher.write_stamp;

    let err = ledger.request_run_complete().expect_err("complete");
    assert_eq!(
        err.to_string(),
        "a publish claim is in progress; complete does not change it"
    );
    ledger.create_task("extra", "Extra").expect("task");
    let err = ledger.assign_run("extra", publisher.id).expect_err("add");
    assert_eq!(err.to_string(), "a publish claim is in progress");
    let err = ledger.claim_run(&mut publisher).expect_err("reclaim");
    assert_eq!(err.to_string(), "a publish claim is in progress");
    let stored = ledger.run_by_id(publisher.id).expect("stored");
    assert_eq!(stored.write_stamp, stamp);
    assert!(!stored.complete_requested);
    assert_eq!(stored.stage, "publishing");
    assert_eq!(stored.status, "active");

    publisher.status = "complete".to_string();
    publisher.stage = "done".to_string();
    publisher.stop_reason = Some("https://example.test/pr/1".to_string());
    ledger.save_run(&mut publisher).expect("publisher success");
    let done = ledger.run_by_id(publisher.id).expect("done");
    assert_eq!(done.status, "complete");
    assert_eq!(done.stage, "done");
    assert!(done.write_stamp > stamp);

    let mut failed = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("second");
    failed.stage = "publishing".to_string();
    ledger.save_run(&mut failed).expect("claim");
    let failed_stamp = failed.write_stamp;
    let err = ledger
        .request_run_complete()
        .expect_err("complete during failure");
    assert_eq!(
        err.to_string(),
        "a publish claim is in progress; complete does not change it"
    );
    assert_eq!(
        ledger.run_by_id(failed.id).expect("held").write_stamp,
        failed_stamp
    );
    failed.status = "stopped".to_string();
    failed.stage = "publish".to_string();
    failed.stop_reason = Some("git push failed: push refused".to_string());
    ledger.save_run(&mut failed).expect("publisher failure");
    let stopped = ledger.run_by_id(failed.id).expect("stopped");
    assert_eq!(stopped.status, "stopped");
    assert_eq!(stopped.stage, "publish");
    assert_eq!(
        stopped.stop_reason.as_deref(),
        Some("git push failed: push refused")
    );
    assert!(stopped.write_stamp > failed_stamp);
}

#[test]
fn publishing_refuses_a_reloaded_save_and_the_publisher_success_and_failure_land() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    ledger.set_setting("reviewer", "codex").expect("reviewer");
    ledger.set_setting("workflow", "gated").expect("gated");
    let mut publisher = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("run");
    publisher.stage = "publishing".to_string();
    ledger.save_run(&mut publisher).expect("claim");
    let stamp = publisher.write_stamp;

    let mut other = Ledger::open(&path).expect("other");
    let mut outsider = other.run_by_id(publisher.id).expect("reload");
    outsider.status = "stopped".to_string();
    outsider.stage = "publish".to_string();
    outsider.stop_reason = Some("not the publisher".to_string());
    let err = other.save_run(&mut outsider).expect_err("non-publisher");
    assert_eq!(err.to_string(), "a publish claim is in progress");
    let stored = ledger.run_by_id(publisher.id).expect("stored");
    assert_eq!(stored.write_stamp, stamp);
    assert_eq!(stored.stage, "publishing");
    assert_eq!(stored.status, "active");
    assert_eq!(stored.stop_reason, None);

    publisher.status = "complete".to_string();
    publisher.stage = "done".to_string();
    publisher.stop_reason = Some("https://example.test/pr/1".to_string());
    ledger.save_run(&mut publisher).expect("publisher success");
    let done = ledger.run_by_id(publisher.id).expect("done");
    assert_eq!(done.status, "complete");
    assert_eq!(done.stage, "done");
    assert_eq!(
        done.stop_reason.as_deref(),
        Some("https://example.test/pr/1")
    );
    assert!(done.write_stamp > stamp);

    let mut failed = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("second");
    failed.stage = "publishing".to_string();
    ledger.save_run(&mut failed).expect("claim");
    let failed_stamp = failed.write_stamp;
    let mut outsider = other.run_by_id(failed.id).expect("reload failure");
    outsider.status = "complete".to_string();
    outsider.stage = "done".to_string();
    let err = other
        .save_run(&mut outsider)
        .expect_err("non-publisher during failure");
    assert_eq!(err.to_string(), "a publish claim is in progress");
    assert_eq!(
        ledger.run_by_id(failed.id).expect("held").write_stamp,
        failed_stamp
    );
    assert_eq!(
        ledger.run_by_id(failed.id).expect("held").stage,
        "publishing"
    );
    failed.status = "stopped".to_string();
    failed.stage = "publish".to_string();
    failed.stop_reason = Some("git push failed: push refused".to_string());
    ledger.save_run(&mut failed).expect("publisher failure");
    let stopped = ledger.run_by_id(failed.id).expect("stopped");
    assert_eq!(stopped.status, "stopped");
    assert_eq!(stopped.stage, "publish");
    assert_eq!(
        stopped.stop_reason.as_deref(),
        Some("git push failed: push refused")
    );
    assert!(stopped.write_stamp > failed_stamp);
}

#[test]
fn accept_all_rechecks_gated_workflow_inside_the_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    seed_approved_report(&mut ledger, "owned");
    let held = Arc::new(Barrier::new(2));
    let writer = spawn_accept_holder(
        path.clone(),
        held.clone(),
        "UPDATE settings SET value = 'gated' WHERE key = 'workflow'",
    );
    held.wait();
    let err = ledger.accept_all_reports().expect_err("gated");
    assert_eq!(
        err.to_string(),
        "report accept --all is unavailable when workflow=gated"
    );
    writer.join().expect("holder");
    assert_eq!(report_verdict(&path, "owned"), None);
}

#[test]
fn accept_all_rechecks_live_run_ownership_inside_the_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    let mut ledger = Ledger::initialize(&path).expect("init");
    seed_approved_report(&mut ledger, "owned");
    let run = ledger
        .start_run("prd.md", "digest", "feature", Some("gated"))
        .expect("run");
    let mut stopped = ledger.run_by_id(run.id).expect("load");
    stopped.status = "stopped".to_string();
    ledger.save_run(&mut stopped).expect("stop");
    rusqlite::Connection::open(&path)
        .expect("open")
        .execute(
            "UPDATE tasks SET run_id = ?1 WHERE slug = 'owned'",
            [run.id],
        )
        .expect("link");
    let held = Arc::new(Barrier::new(2));
    let writer = spawn_accept_holder(
        path.clone(),
        held.clone(),
        "UPDATE runs SET status = 'active', write_stamp = write_stamp + 1 WHERE workflow = 'gated'",
    );
    held.wait();
    let err = ledger.accept_all_reports().expect_err("owned");
    assert_eq!(err.to_string(), OWNED);
    writer.join().expect("holder");
    assert_eq!(report_verdict(&path, "owned"), None);
    assert_eq!(ledger.settings().expect("settings").workflow, "classic");
}

fn seed_approved_report(ledger: &mut Ledger, slug: &str) {
    ledger.set_setting("reviewer", "codex").expect("reviewer");
    ledger.set_setting("workflow", "classic").expect("classic");
    ledger.create_task(slug, "Owned").expect("task");
    ledger.claim_task(slug, "cursor").expect("claim");
    ledger
        .finish_task(slug, None, Some("abc123"))
        .expect("done");
    ledger
        .start_review(
            "task",
            Some(slug),
            None,
            "feature",
            "codex",
            None,
            "abc123",
            None,
            "fp",
        )
        .expect("start");
    ledger
        .record_review(
            "task",
            Some(slug),
            "codex",
            "abc123",
            "abc123",
            "fp",
            "approved",
            TASK_REVIEW,
            false,
        )
        .expect("record");
}

fn spawn_accept_holder(
    path: PathBuf,
    barrier: Arc<Barrier>,
    sql: &'static str,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let conn = rusqlite::Connection::open(&path).expect("open");
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .expect("timeout");
        conn.execute_batch("BEGIN IMMEDIATE").expect("begin");
        conn.execute(sql, []).expect("switch");
        barrier.wait();
        thread::sleep(std::time::Duration::from_millis(400));
        conn.execute_batch("COMMIT").expect("commit");
    })
}
