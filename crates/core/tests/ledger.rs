use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

use relaybook_core::{EventKind, Ledger, Status, replay};

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

    ledger.complete_task("base").expect("done");
    assert_eq!(
        ledger
            .claim_task("feature", "cursor")
            .expect_err("still waiting")
            .to_string(),
        "waiting on: other"
    );
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
    let ready = ledger.list_tasks(None, true).expect("ready");
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

    ledger.complete_task("auth").expect("done");
    let done = ledger
        .rename_task("auth", "Add login later")
        .expect("rename done");
    assert_eq!(done.status, Status::Done);
    assert_eq!(done.title, "Add login later");
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
