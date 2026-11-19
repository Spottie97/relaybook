use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use serde_json::{Value, json};

#[path = "support/conpty.rs"]
mod conpty;

use conpty::ENTER;

#[test]
fn claim_race_has_one_winner() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();

    let alpha = spawn_claim(dir.path(), "alpha");
    let beta = spawn_claim(dir.path(), "beta");
    let alpha_out = alpha.wait_with_output().expect("alpha");
    let beta_out = beta.wait_with_output().expect("beta");
    let successes = [&alpha_out, &beta_out]
        .iter()
        .filter(|output| output.status.success())
        .count();
    assert_eq!(
        successes,
        1,
        "alpha={:?} beta={:?}",
        summarize(&alpha_out),
        summarize(&beta_out)
    );
    let loser = if alpha_out.status.success() {
        &beta_out
    } else {
        &alpha_out
    };
    assert_eq!(loser.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&loser.stderr);
    assert!(stderr.contains("already claimed"), "stderr={stderr}");

    let data = json_data(dir.path(), ["context", "auth", "--json"]);
    let by = data["tasks"][0]["claimed_by"].as_str();
    assert!(by == Some("alpha") || by == Some("beta"));
    let claimed = data["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter(|event| event["kind"] == "claimed")
        .count();
    assert_eq!(claimed, 1);
}

#[test]
fn events_are_append_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success();
    run(
        dir.path(),
        ["progress", "report", "auth", "--note", "started"],
    )
    .assert()
    .success();
    run(
        dir.path(),
        [
            "decision",
            "log",
            "auth",
            "--why",
            "use argon2",
            "--by",
            "claude-code",
        ],
    )
    .assert()
    .success();
    run(dir.path(), ["task", "done", "--no-report", "auth"])
        .assert()
        .success();

    let first = event_rows(dir.path());
    run(dir.path(), ["task", "list"]).assert().success();
    run(dir.path(), ["context"]).assert().success();
    assert_eq!(event_rows(dir.path()), first);

    run(
        dir.path(),
        [
            "decision",
            "log",
            "auth",
            "--why",
            "keep the hash",
            "--by",
            "cursor",
        ],
    )
    .assert()
    .success();
    let after = event_rows(dir.path());
    assert!(after.len() > first.len());
    assert_eq!(&after[..first.len()], first.as_slice());
    assert!(after.last().expect("event").0 > first.last().expect("event").0);
}

#[test]
fn context_json_includes_task_step_decision_and_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(dir.path(), ["step", "add", "auth", "hash passwords"])
        .assert()
        .success();
    run(
        dir.path(),
        [
            "decision",
            "log",
            "auth",
            "--why",
            "use argon2",
            "--by",
            "claude-code",
        ],
    )
    .assert()
    .success();

    let data = json_data(dir.path(), ["context", "auth", "--json"]);
    assert_eq!(data["tasks"][0]["title"], "Add auth");
    assert_eq!(
        data["tasks"][0]["steps"][0]["description"],
        "hash passwords"
    );
    assert_eq!(data["tasks"][0]["decisions"][0]["rationale"], "use argon2");
    let kinds: Vec<&str> = data["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"task_created"));
    assert!(kinds.contains(&"decision"));
}

#[test]
fn block_sets_status_and_rejects_a_done_task() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();

    let data = json_data(
        dir.path(),
        [
            "task",
            "block",
            "auth",
            "--reason",
            "waiting on schema",
            "--json",
        ],
    );
    assert_eq!(data["status"], "blocked");
    assert_eq!(data["slug"], "auth");

    run(dir.path(), ["task", "list", "--status", "blocked"])
        .assert()
        .success()
        .stdout(predicate::str::contains("auth"));

    run(
        dir.path(),
        ["task", "block", "auth", "--reason", "still waiting"],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("blocked auth"));

    let after = json_data(dir.path(), ["context", "auth", "--json"]);
    let changes = after["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter(|event| event["kind"] == "status_changed")
        .count();
    assert_eq!(changes, 1);

    run(dir.path(), ["task", "create", "ship", "--title", "Ship it"])
        .assert()
        .success();
    run(dir.path(), ["task", "claim", "ship", "--by", "cursor"])
        .assert()
        .success();
    run(dir.path(), ["task", "done", "--no-report", "ship"])
        .assert()
        .success();
    run(
        dir.path(),
        ["task", "block", "ship", "--reason", "too late"],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("cannot block a done task"));

    run(dir.path(), ["task", "block", "missing", "--reason", "gone"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("no such task: missing"));
}

#[test]
fn dependencies_show_in_json_and_ready_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["task", "create", "base", "--title", "Base"])
        .assert()
        .success();
    let created = json_data(
        dir.path(),
        [
            "task",
            "create",
            "feature",
            "--title",
            "Feature",
            "--depends-on",
            "base",
            "--json",
        ],
    );
    let created_task = json_data(dir.path(), ["context", "feature", "--json"]);
    assert_eq!(created["slug"], "feature");
    assert_eq!(created_task["tasks"][0]["depends_on"][0], "base");

    run(dir.path(), ["task", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("needs: base"));

    run(dir.path(), ["task", "list", "--ready"])
        .assert()
        .success()
        .stdout(predicate::str::contains("base"))
        .stdout(predicate::str::contains("feature").not());

    run(dir.path(), ["task", "claim", "feature", "--by", "cursor"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("waiting on: base"));

    run(dir.path(), ["task", "claim", "base", "--by", "cursor"])
        .assert()
        .success();
    run(dir.path(), ["task", "done", "--no-report", "base"])
        .assert()
        .success();
    run(dir.path(), ["task", "list", "--ready"])
        .assert()
        .success()
        .stdout(predicate::str::contains("feature"));
    run(dir.path(), ["task", "claim", "feature", "--by", "cursor"])
        .assert()
        .success();

    run(dir.path(), ["task", "depend", "base", "--on", "feature"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("dependency cycle:"));

    run(dir.path(), ["task", "undepend", "feature", "--on", "base"])
        .assert()
        .success();
    let after = json_data(dir.path(), ["context", "feature", "--json"]);
    assert!(
        after["tasks"][0]["depends_on"]
            .as_array()
            .expect("deps")
            .is_empty()
    );
}

#[test]
fn rename_keeps_the_slug_and_shows_the_new_title() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();

    run(
        dir.path(),
        ["task", "rename", "auth", "--title", "Add login"],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("renamed auth"));

    let task = json_data(
        dir.path(),
        ["task", "rename", "auth", "--title", "Add login", "--json"],
    );
    assert_eq!(task["slug"], "auth");
    let titled = json_data(dir.path(), ["context", "auth", "--json"]);
    assert_eq!(titled["tasks"][0]["title"], "Add login");
    let renamed = event_rows(dir.path())
        .into_iter()
        .filter(|row| row.1 == "task_renamed")
        .count();
    assert_eq!(renamed, 1);

    run(dir.path(), ["task", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Add login"));
    run(dir.path(), ["context", "auth"])
        .assert()
        .success()
        .stdout(predicate::str::contains("auth [open] Add login"));

    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success();
    run(dir.path(), ["task", "done", "--no-report", "auth"])
        .assert()
        .success();
    run(
        dir.path(),
        ["task", "rename", "auth", "--title", "Add login later"],
    )
    .assert()
    .success();
    let done = json_data(dir.path(), ["context", "auth", "--json"]);
    assert_eq!(done["tasks"][0]["status"], "done");
    assert_eq!(done["tasks"][0]["title"], "Add login later");

    run(dir.path(), ["task", "rename", "auth", "--title", "   "])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("title is required"));
    run(
        dir.path(),
        ["task", "rename", "missing", "--title", "Title"],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("no such task: missing"));
}

#[test]
fn unblock_returns_claimed_or_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success();
    run(dir.path(), ["task", "block", "auth", "--reason", "waiting"])
        .assert()
        .success();
    let task = json_data(
        dir.path(),
        ["task", "unblock", "auth", "--note", "ready", "--json"],
    );
    assert_eq!(task["status"], "claimed");
    assert_eq!(task["slug"], "auth");
    let held = json_data(dir.path(), ["context", "auth", "--json"]);
    assert_eq!(held["tasks"][0]["claimed_by"], "cursor");
    run(dir.path(), ["task", "unblock", "auth"])
        .assert()
        .success()
        .stdout(predicate::str::contains("unblocked auth"));

    run(dir.path(), ["task", "create", "other", "--title", "Other"])
        .assert()
        .success();
    run(dir.path(), ["task", "block", "other", "--reason", "stuck"])
        .assert()
        .success();
    let opened = json_data(dir.path(), ["task", "unblock", "other", "--json"]);
    assert_eq!(opened["status"], "open");
    run(dir.path(), ["task", "unblock", "missing"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("no such task: missing"));
}

#[test]
fn context_hides_done_tasks_until_asked() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["task", "create", "old", "--title", "Old"])
        .assert()
        .success();
    run(dir.path(), ["task", "claim", "old", "--by", "cursor"])
        .assert()
        .success();
    run(dir.path(), ["task", "done", "--no-report", "old"])
        .assert()
        .success();
    run(dir.path(), ["task", "create", "now", "--title", "Now"])
        .assert()
        .success();

    run(dir.path(), ["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("now [open] Now"))
        .stdout(predicate::str::contains("old [done]").not())
        .stdout(predicate::str::contains(
            "1 done tasks hidden (relaybook context --all)",
        ))
        .stdout(predicate::str::contains(
            "events omitted (relaybook context --events 20)",
        ));

    run(dir.path(), ["context", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("old [done] Old"));

    let data = json_data(dir.path(), ["context", "--json"]);
    assert_eq!(data["hidden_done"], 1);
    assert_eq!(data["tasks"].as_array().expect("tasks").len(), 1);
    let full = json_data(dir.path(), ["context", "old", "--json"]);
    assert_eq!(full["tasks"][0]["slug"], "old");
    assert!(!full["events"].as_array().expect("events").is_empty());
}

#[test]
fn edit_step_accepts_a_leading_dash() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(dir.path(), ["step", "add", "auth", "hash passwords"])
        .assert()
        .success();
    run(dir.path(), ["step", "check", "auth", "1"])
        .assert()
        .success();
    let edited = json_data(
        dir.path(),
        [
            "step",
            "edit",
            "auth",
            "1",
            "--text",
            "--keep the dash",
            "--json",
        ],
    );
    assert_eq!(edited["slug"], "auth");
    assert_eq!(edited["position"], 1);
    assert_eq!(edited["status"], "done");
    run(
        dir.path(),
        ["step", "edit", "auth", "1", "--text", "--keep the dash"],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("edited step 1 on auth"));
    let ctx = json_data(dir.path(), ["context", "auth", "--json"]);
    assert_eq!(
        ctx["tasks"][0]["steps"][0]["description"],
        "--keep the dash"
    );
    run(dir.path(), ["step", "edit", "auth", "1", "--text", "   "])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("text is required"));
    run(
        dir.path(),
        ["step", "edit", "missing", "1", "--text", "text"],
    )
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains("no such task: missing"));
    run(dir.path(), ["step", "edit", "auth", "9", "--text", "text"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("no such step: 9"));
}

#[test]
fn step_add_text_flag_accepts_a_leading_dash() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "--add auth"],
    )
    .assert()
    .success();
    run(
        dir.path(),
        ["step", "add", "auth", "--text", "--keep the dash"],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("added step 1 to auth"));
    run(dir.path(), ["step", "add", "auth", "  hash passwords  "])
        .assert()
        .success();
    run(dir.path(), ["step", "add", "auth", "--text", "  trim me  "])
        .assert()
        .success();
    let ctx = json_data(dir.path(), ["context", "auth", "--json"]);
    let steps = ctx["tasks"][0]["steps"].as_array().expect("steps");
    assert_eq!(steps[0]["description"], "--keep the dash");
    assert_eq!(steps[1]["description"], "hash passwords");
    assert_eq!(steps[2]["description"], "trim me");
    assert_eq!(ctx["tasks"][0]["title"], "--add auth");

    run(dir.path(), ["step", "add", "auth"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "step text is required: pass it as an argument or with --text",
        ));
    run(
        dir.path(),
        ["step", "add", "auth", "both", "--text", "also"],
    )
    .assert()
    .failure()
    .code(2)
    .stderr(predicate::str::contains(
        "pass step text as an argument or with --text, not both",
    ));
    run(dir.path(), ["step", "add", "auth", "--text", "   "])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("description is required"));

    run(
        dir.path(),
        [
            "decision",
            "log",
            "auth",
            "--why",
            "--keep it",
            "--by",
            "cursor",
        ],
    )
    .assert()
    .success();
    run(
        dir.path(),
        ["report", "note", "--by", "cursor", "--text", "--note"],
    )
    .assert()
    .success();
}

#[test]
fn task_list_hides_done_tasks_until_asked() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["task", "create", "old", "--title", "Old"])
        .assert()
        .success();
    run(dir.path(), ["task", "claim", "old", "--by", "cursor"])
        .assert()
        .success();
    run(dir.path(), ["task", "done", "--no-report", "old"])
        .assert()
        .success();
    run(dir.path(), ["task", "create", "now", "--title", "Now"])
        .assert()
        .success();

    run(dir.path(), ["task", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("now"))
        .stdout(predicate::str::contains("old").not())
        .stdout(predicate::str::contains(
            "1 done tasks hidden (relaybook task list --all)",
        ));
    run(dir.path(), ["task", "list", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("old"));
    run(dir.path(), ["task", "list", "--status", "done"])
        .assert()
        .success()
        .stdout(predicate::str::contains("old"))
        .stdout(predicate::str::contains("hidden").not());
    run(dir.path(), ["task", "list", "--ready"])
        .assert()
        .success()
        .stdout(predicate::str::contains("now"))
        .stdout(predicate::str::contains("hidden").not());

    let data = json_data(dir.path(), ["task", "list", "--json"]);
    assert_eq!(data["hidden_done"], 1);
    assert_eq!(data["tasks"].as_array().expect("tasks").len(), 1);
}

#[test]
fn task_done_on_an_open_task_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(dir.path(), ["task", "done", "auth", "--changed", "not yet"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "claim it first: relaybook task claim auth --by <agent>",
        ));
    let data = json_data(dir.path(), ["context", "auth", "--json"]);
    assert_eq!(data["tasks"][0]["status"], "open");
    run(dir.path(), ["inbox"])
        .assert()
        .success()
        .stdout("inbox empty\n");
}

#[test]
fn task_done_stores_a_report_and_rejects_a_long_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success();
    run(dir.path(), ["task", "done", "auth"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("changed is required"));
    run(
        dir.path(),
        [
            "task",
            "done",
            "auth",
            "--changed",
            "added reports",
            "--verified",
            "cargo test",
            "--not-verified",
            "linux install",
            "--deviations",
            "none",
            "--review",
            "inbox ordering",
        ],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("done auth"));
    run(dir.path(), ["task", "done", "auth", "--changed", "again"])
        .assert()
        .success();
    let huge = "x".repeat(1600);
    run(dir.path(), ["task", "create", "big", "--title", "Big"])
        .assert()
        .success();
    run(dir.path(), ["task", "claim", "big", "--by", "cursor"])
        .assert()
        .success();
    command(dir.path())
        .args(["task", "done", "big", "--changed", &huge])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("limit 1500"));
    let still = json_data(dir.path(), ["context", "big", "--json"]);
    assert_eq!(still["tasks"][0]["status"], "claimed");

    run(
        dir.path(),
        ["report", "note", "--by", "cursor", "--text", "spans tasks"],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("noted"));
}

#[test]
fn inbox_accepts_reopens_and_stays_out_of_default_context() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["inbox"])
        .assert()
        .success()
        .stdout("inbox empty\n");

    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(
        dir.path(),
        ["task", "create", "billing", "--title", "Add billing"],
    )
    .assert()
    .success();
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success();
    run(dir.path(), ["task", "claim", "billing", "--by", "cursor"])
        .assert()
        .success();
    run(
        dir.path(),
        ["task", "done", "auth", "--changed", "added reports"],
    )
    .assert()
    .success();
    run(
        dir.path(),
        ["task", "done", "billing", "--changed", "added inbox"],
    )
    .assert()
    .success();
    run(
        dir.path(),
        ["report", "note", "--by", "cursor", "--text", "spans tasks"],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("noted 3"));

    let inbox = run(dir.path(), ["inbox"]).assert().success();
    let inbox = String::from_utf8(inbox.get_output().stdout.clone()).expect("utf8");
    let auth_at = inbox.find("auth Add auth").expect("auth report");
    let billing_at = inbox.find("billing Add billing").expect("billing report");
    let note_at = inbox.find("note 3 by cursor").expect("note");
    assert!(auth_at < billing_at && billing_at < note_at, "{inbox}");
    assert!(inbox.contains("added reports"), "{inbox}");
    assert!(inbox.contains("relaybook report accept <slug>"), "{inbox}");

    let overview = run(dir.path(), ["context"]).assert().success();
    let overview = String::from_utf8(overview.get_output().stdout.clone()).expect("utf8");
    assert!(
        overview.contains("3 unreviewed reports (relaybook inbox)"),
        "{overview}"
    );
    assert!(!overview.contains("added reports"), "{overview}");

    let auth = run(dir.path(), ["context", "auth"]).assert().success();
    let auth = String::from_utf8(auth.get_output().stdout.clone()).expect("utf8");
    assert!(auth.contains("added reports"), "{auth}");

    let data = json_data(dir.path(), ["context", "--json"]);
    assert_eq!(data["unreviewed_reports"], 3);

    run(dir.path(), ["report", "accept", "auth"])
        .assert()
        .success()
        .stdout("accepted auth\n");
    run(dir.path(), ["report", "accept", "--note", "3"])
        .assert()
        .success()
        .stdout("accepted note 3\n");
    run(dir.path(), ["report", "accept", "--all"])
        .assert()
        .success()
        .stdout("accepted 1\n");
    run(dir.path(), ["inbox"])
        .assert()
        .success()
        .stdout("inbox empty\n");

    run(
        dir.path(),
        [
            "task",
            "reopen",
            "billing",
            "--reason",
            "cover the empty inbox",
        ],
    )
    .assert()
    .success()
    .stdout("reopened billing\n");
    run(dir.path(), ["task", "list", "--ready"])
        .assert()
        .success()
        .stdout(predicate::str::contains("billing"))
        .stdout(predicate::str::contains("reopened: cover the empty inbox"));
    run(dir.path(), ["context", "billing"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reopened: cover the empty inbox"));
    run(
        dir.path(),
        ["task", "reopen", "billing", "--reason", "again"],
    )
    .assert()
    .failure()
    .stderr(predicate::str::contains("task is not done"));

    run(dir.path(), ["task", "claim", "billing", "--by", "cursor"])
        .assert()
        .success();
    run(
        dir.path(),
        [
            "task",
            "done",
            "billing",
            "--changed",
            "covered empty inbox",
        ],
    )
    .assert()
    .success();
    run(dir.path(), ["task", "list", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reopened:").not());
    run(dir.path(), ["context", "billing"])
        .assert()
        .success()
        .stdout(predicate::str::contains("covered empty inbox"))
        .stdout(predicate::str::contains("reopened:").not());
}

#[test]
fn hook_prompt_is_one_line_or_silent() {
    let missing = tempfile::tempdir().expect("tempdir");
    run(missing.path(), ["hook", "prompt"])
        .assert()
        .success()
        .stdout("")
        .stderr("");

    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["hook", "prompt"])
        .assert()
        .success()
        .stdout("")
        .stderr("");

    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success();
    run(
        dir.path(),
        ["task", "done", "auth", "--changed", "added the hook"],
    )
    .assert()
    .success();
    run(
        dir.path(),
        ["task", "create", "billing", "--title", "Add billing"],
    )
    .assert()
    .success();
    run(dir.path(), ["task", "claim", "billing", "--by", "cursor"])
        .assert()
        .success();
    run(
        dir.path(),
        ["task", "done", "billing", "--changed", "added the inbox"],
    )
    .assert()
    .success();
    run(dir.path(), ["hook", "prompt"])
        .assert()
        .success()
        .stdout("2 new reports: relaybook inbox\n")
        .stderr("");

    let broken = dir.path().join(".relaybook").join("ledger.db");
    fs::write(&broken, "not a sqlite database").expect("write");
    run(dir.path(), ["hook", "prompt"])
        .assert()
        .success()
        .stdout("")
        .stderr("");
}

#[test]
fn mode_and_config_use_agent_call_limits() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["mode"])
        .assert()
        .success()
        .stdout("manual\n");
    run(dir.path(), ["gates"])
        .assert()
        .success()
        .stdout("plan\n");
    run(dir.path(), ["gates", "sometimes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid gates: sometimes"));
    run(dir.path(), ["gates", "plan,review"])
        .assert()
        .success()
        .stdout("gates plan,review\n");
    run(dir.path(), ["gates"])
        .assert()
        .success()
        .stdout("plan,review\n");
    run(dir.path(), ["run", "a.md", "--prd", "b.md"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pass either a PRD path or --prd"));
    run(dir.path(), ["config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("max_agent_calls 60"))
        .stdout(predicate::str::contains("est_cost_cap_usd unset"))
        .stdout(predicate::str::contains("gates plan"))
        .stdout(predicate::str::contains("reviewer claude"))
        .stdout(predicate::str::contains("workflow classic"));
    run(dir.path(), ["config", "set", "gates", "sometimes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid gates: sometimes"));
    run(dir.path(), ["config", "set", "gates", "plan,review"])
        .assert()
        .success()
        .stdout("gates plan,review\n");
    run(dir.path(), ["config", "set", "max_agent_calls", "15"])
        .assert()
        .success()
        .stdout("max_agent_calls 15\n");
    run(dir.path(), ["config", "set", "est_cost_cap_usd", "5"])
        .assert()
        .success()
        .stdout("est_cost_cap_usd 5.00\n");
    run(dir.path(), ["config", "set", "est_cost_cap_usd", "unset"])
        .assert()
        .success()
        .stdout("est_cost_cap_usd unset\n");
    run(dir.path(), ["config", "set", "reviewer", "codex"])
        .assert()
        .success()
        .stdout("reviewer codex\n");
    run(dir.path(), ["config", "set", "reviewer", "claude"])
        .assert()
        .success()
        .stdout("reviewer claude\n");
    run(dir.path(), ["config", "set", "reviewer", "grok"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid reviewer: grok"));
    run(dir.path(), ["config", "set", "workflow", "classic"])
        .assert()
        .success()
        .stdout("workflow classic\n");
    run(dir.path(), ["config", "set", "workflow", "sometimes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid workflow: sometimes"));
    run(dir.path(), ["mode", "auto"])
        .assert()
        .success()
        .stdout("mode auto\n");
    run(dir.path(), ["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("mode auto"))
        .stdout(predicate::str::contains("no active run"))
        .stdout(predicate::str::contains("cap unset"));

    let db = dir.path().join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(&db).expect("open");
    conn.execute(
        "INSERT INTO runs (prd_path, prd_sha256, branch, stage, cycle, agent_calls, est_cost_usd, status, stop_reason, started_at)
         VALUES ('docs/prd.md', 'abc', 'relaybook/prd', 'review', 3, 9, 4.1, 'waiting', NULL, '2026-11-10T12:00:00.000Z')",
        [],
    )
    .expect("insert");
    drop(conn);

    let status = run(dir.path(), ["status"]).assert().success();
    let status = String::from_utf8(status.get_output().stdout.clone()).expect("utf8");
    assert!(status.contains("calls 9/15"), "{status}");
    assert!(status.contains("est 4.10 USD (estimate)"), "{status}");
    assert!(status.contains("waiting on approval"), "{status}");
    assert!(status.len() < 400, "{}", status.len());
    run(dir.path(), ["hook", "prompt"])
        .assert()
        .success()
        .stdout("waiting for approval: relaybook approve\n");

    let conn = rusqlite::Connection::open(&db).expect("open");
    conn.execute(
        "UPDATE runs SET status = 'paused', stop_reason = 'claude|2026-11-10T18:00:00Z' WHERE id = 1",
        [],
    )
    .expect("update");
    drop(conn);
    run(dir.path(), ["hook", "prompt"])
        .assert()
        .success()
        .stdout(
            "usage limit reached on claude; relaybook run --resume after 2026-11-10T18:00:00Z\n",
        );
    run(dir.path(), ["mode", "manual"])
        .assert()
        .success()
        .stdout("mode manual\n");
    run(dir.path(), ["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "usage limit (claude, until 2026-11-10T18:00:00Z)",
        ));
}

#[test]
fn gated_requires_reviewer_codex() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();

    let config = json_data(dir.path(), ["config", "--json"]);
    assert_eq!(config["reviewer"], json!("claude"));
    assert_eq!(config["workflow"], json!("classic"));
    assert_eq!(config["mode"], json!("manual"));

    run(dir.path(), ["config", "set", "workflow", "gated"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "workflow=gated requires reviewer=codex",
        ));
    let still = json_data(dir.path(), ["config", "--json"]);
    assert_eq!(still["reviewer"], json!("claude"));
    assert_eq!(still["workflow"], json!("classic"));

    run(dir.path(), ["config", "set", "reviewer", "codex"])
        .assert()
        .success()
        .stdout("reviewer codex\n");
    run(dir.path(), ["config", "set", "workflow", "gated"])
        .assert()
        .success()
        .stdout("workflow gated\n");
    let gated = json_data(dir.path(), ["config", "--json"]);
    assert_eq!(gated["reviewer"], json!("codex"));
    assert_eq!(gated["workflow"], json!("gated"));

    run(dir.path(), ["config", "set", "reviewer", "claude"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "workflow=gated requires reviewer=codex",
        ));
    let kept = json_data(dir.path(), ["config", "--json"]);
    assert_eq!(kept["reviewer"], json!("codex"));
    assert_eq!(kept["workflow"], json!("gated"));

    run(dir.path(), ["config", "set", "workflow", "classic"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "leaving workflow=gated requires confirmation at a TTY",
        ));
    let stayed = json_data(dir.path(), ["config", "--json"]);
    assert_eq!(stayed["reviewer"], json!("codex"));
    assert_eq!(stayed["workflow"], json!("gated"));
}

#[test]
fn task_done_records_commit_sha() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_repo(dir.path());
    run(dir.path(), ["init"]).assert().success();
    let config = json_data(dir.path(), ["config", "--json"]);
    assert_eq!(config["workflow"], json!("classic"));
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    fs::write(dir.path().join("wip.txt"), "dirty\n").expect("write");
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success()
        .stdout("claimed auth by cursor\n");
    let data = json_data(
        dir.path(),
        [
            "task",
            "done",
            "auth",
            "--changed",
            "recorded the sha",
            "--json",
        ],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(data["slug"], json!("auth"));
    assert_eq!(data["status"], json!("done"));
    assert_eq!(data["commit_sha"], json!(head));

    let db = dir.path().join(".relaybook").join("ledger.db");
    let (commit, base, graph) = report_shas(&db);
    assert_eq!(commit.as_deref(), Some(head.as_str()));
    assert_eq!(base, None);
    assert_eq!(graph, None);

    run(dir.path(), ["task", "done", "auth", "--changed", "again"])
        .assert()
        .success()
        .stdout("done auth\n");
    let (again, _, _) = report_shas(&db);
    assert_eq!(again.as_deref(), Some(head.as_str()));
}

#[test]
fn gated_claim_and_done_require_clean_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_repo(dir.path());
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(dir.path(), ["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    run(dir.path(), ["config", "set", "workflow", "gated"])
        .assert()
        .success();

    let dirty = dir.path().join("dirty.txt");
    fs::write(&dirty, "wip\n").expect("dirty");
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "working tree is not clean: dirty.txt",
        ));
    let open = json_data(dir.path(), ["context", "auth", "--json"]);
    assert_eq!(open["tasks"][0]["status"], "open");

    fs::remove_file(&dirty).expect("clean");
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success()
        .stdout("claimed auth by cursor\n");

    fs::write(&dirty, "wip\n").expect("dirty again");
    run(dir.path(), ["task", "done", "auth", "--changed", "shipped"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "working tree is not clean: dirty.txt",
        ));
    let claimed = json_data(dir.path(), ["context", "auth", "--json"]);
    assert_eq!(claimed["tasks"][0]["status"], "claimed");

    fs::remove_file(&dirty).expect("clean again");
    let done = json_data(
        dir.path(),
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(done["status"], json!("done"));
    assert_eq!(done["commit_sha"], json!(head));
    let db = dir.path().join(".relaybook").join("ledger.db");
    let (commit, base, graph) = report_shas(&db);
    assert_eq!(commit.as_deref(), Some(head.as_str()));
    // The claim records HEAD as base_sha. graphify-out/ is not gitignored here,
    // so the refresh is not built and graph_sha stays empty.
    assert_eq!(base.as_deref(), Some(head.as_str()));
    assert_eq!(graph, None);
}

#[test]
fn gated_claim_and_done_reject_hidden_status_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_gated_task(dir.path());
    fs::write(dir.path().join(".gitignore"), ".relaybook/\n").expect("gitignore");
    fs::create_dir_all(dir.path().join("a.relaybook")).expect("dir");
    fs::write(dir.path().join("a.relaybook").join("x"), "tracked\n").expect("file");
    fs::write(dir.path().join("src.rs"), "fn main() {}\n").expect("src");
    commit_worktree(dir.path(), "track paths");

    git(dir.path(), &["mv", "-f", "src.rs", ".relaybook/src.rs"]);
    assert_eq!(
        git_stdout(dir.path(), &["status", "--porcelain"]),
        b"R  src.rs -> .relaybook/src.rs\n"
    );
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "working tree is not clean: src.rs",
        ));
    assert_eq!(task_status(dir.path()), "open");
    git(dir.path(), &["reset", "--hard"]);

    fs::write(dir.path().join("a.relaybook").join("x"), "tracked\ndirty\n").expect("edit");
    assert_eq!(
        git_stdout(dir.path(), &["status", "--porcelain"]),
        b" M a.relaybook/x\n"
    );
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "working tree is not clean: a.relaybook/x",
        ));
    assert_eq!(task_status(dir.path()), "open");
    git(dir.path(), &["checkout", "--", "a.relaybook/x"]);

    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success()
        .stdout("claimed auth by cursor\n");

    git(dir.path(), &["mv", "-f", "src.rs", ".relaybook/src.rs"]);
    run(dir.path(), ["task", "done", "auth", "--changed", "shipped"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "working tree is not clean: src.rs",
        ));
    assert_eq!(task_status(dir.path()), "claimed");
    git(dir.path(), &["reset", "--hard"]);

    fs::write(dir.path().join("a.relaybook").join("x"), "tracked\ndirty\n").expect("edit");
    run(dir.path(), ["task", "done", "auth", "--changed", "shipped"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "working tree is not clean: a.relaybook/x",
        ));
    assert_eq!(task_status(dir.path()), "claimed");
    git(dir.path(), &["checkout", "--", "a.relaybook/x"]);

    let done = json_data(
        dir.path(),
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(done["status"], json!("done"));
    assert_eq!(done["commit_sha"], json!(head));
}

#[test]
fn gated_claim_and_done_see_files_hidden_by_git_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_gated_task(dir.path());
    fs::write(dir.path().join(".gitignore"), ".relaybook/\n").expect("gitignore");
    git(dir.path(), &["add", ".gitignore"]);
    commit_index(dir.path(), "ignore ledger");
    git(dir.path(), &["config", "status.showUntrackedFiles", "no"]);
    fs::write(dir.path().join("secret.txt"), "hidden\n").expect("secret");
    assert_eq!(git(dir.path(), &["status", "--porcelain"]), "");
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "working tree is not clean: secret.txt",
        ));
    assert_eq!(task_status(dir.path()), "open");
    fs::remove_file(dir.path().join("secret.txt")).expect("remove");
    git(
        dir.path(),
        &["config", "--unset", "status.showUntrackedFiles"],
    );

    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success()
        .stdout("claimed auth by cursor\n");

    let sub = tempfile::tempdir().expect("sub");
    git(sub.path(), &["init", "-b", "main"]);
    fs::write(sub.path().join("file.txt"), "sub\n").expect("subfile");
    git(sub.path(), &["add", "file.txt"]);
    commit_worktree(sub.path(), "sub");
    git(
        dir.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            sub.path().to_str().expect("utf8"),
            "vendor",
        ],
    );
    commit_index(dir.path(), "vendor");
    fs::write(dir.path().join("vendor").join("file.txt"), "sub\ndirty\n").expect("dirty sub");
    git(dir.path(), &["config", "submodule.vendor.ignore", "all"]);
    git(dir.path(), &["config", "diff.ignoreSubmodules", "all"]);
    assert_eq!(git(dir.path(), &["status", "--porcelain"]), "");
    run(dir.path(), ["task", "done", "auth", "--changed", "shipped"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "working tree is not clean: vendor",
        ));
    assert_eq!(task_status(dir.path()), "claimed");

    git(&dir.path().join("vendor"), &["checkout", "--", "file.txt"]);
    assert_eq!(
        git(
            dir.path(),
            &["status", "--porcelain", "--ignore-submodules=none"]
        ),
        ""
    );
    let done = json_data(
        dir.path(),
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(done["commit_sha"], json!(head));
}

#[test]
fn gated_done_requires_head_commit() {
    let classic = tempfile::tempdir().expect("tempdir");
    git(classic.path(), &["init", "-b", "main"]);
    run(classic.path(), ["init"]).assert().success();
    run(
        classic.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(classic.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success();
    let classic_done = json_data(
        classic.path(),
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    assert_eq!(classic_done["status"], json!("done"));
    assert!(classic_done["commit_sha"].is_null());
    let (commit, _, _) = report_shas(&classic.path().join(".relaybook").join("ledger.db"));
    assert_eq!(commit, None);

    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-b", "main"]);
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(dir.path(), ["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    run(dir.path(), ["config", "set", "workflow", "gated"])
        .assert()
        .success();
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success()
        .stdout("claimed auth by cursor\n");
    run(dir.path(), ["task", "done", "auth", "--changed", "shipped"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "gated completion requires a commit at HEAD",
        ));
    assert_eq!(task_status(dir.path()), "claimed");
    assert_eq!(report_count(dir.path()), 0);

    fs::write(dir.path().join("README.md"), "seed\n").expect("readme");
    git(dir.path(), &["add", "README.md"]);
    commit_index(dir.path(), "seed");
    let done = json_data(
        dir.path(),
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(done["status"], json!("done"));
    assert_eq!(done["commit_sha"], json!(head));
    let again = json_data(
        dir.path(),
        ["task", "done", "auth", "--changed", "again", "--json"],
    );
    assert_eq!(again["commit_sha"], json!(head));
}

#[test]
fn gated_done_rechecks_inside_the_write_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_gated_task(dir.path());
    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success();

    let dirty = during_write_lock(
        dir.path(),
        &["task", "done", "auth", "--changed", "shipped"],
        || {
            fs::write(dir.path().join("race.txt"), "race\n").expect("race");
        },
    );
    assert_eq!(dirty.status.code(), Some(1), "{}", summarize(&dirty));
    let stderr = String::from_utf8_lossy(&dirty.stderr);
    assert!(
        stderr.contains("working tree is not clean: race.txt"),
        "{stderr}"
    );
    assert_eq!(task_status(dir.path()), "claimed");
    assert_eq!(report_count(dir.path()), 0);
    fs::remove_file(dir.path().join("race.txt")).expect("clean");

    let moved = during_write_lock(
        dir.path(),
        &["task", "done", "auth", "--changed", "shipped"],
        || {
            git(
                dir.path(),
                &[
                    "-c",
                    "user.email=relaybook@example.com",
                    "-c",
                    "user.name=Relaybook",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "move",
                ],
            );
        },
    );
    assert_eq!(moved.status.code(), Some(1), "{}", summarize(&moved));
    let stderr = String::from_utf8_lossy(&moved.stderr);
    assert!(
        stderr.contains("HEAD changed before the report was stored"),
        "{stderr}"
    );
    assert_eq!(task_status(dir.path()), "claimed");
    assert_eq!(report_count(dir.path()), 0);

    let done = json_data(
        dir.path(),
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(done["commit_sha"], json!(head));
}

#[test]
fn gated_claim_rechecks_inside_the_write_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_gated_task(dir.path());

    let dirty = during_write_lock(
        dir.path(),
        &["task", "claim", "auth", "--by", "cursor"],
        || {
            fs::write(dir.path().join("race.txt"), "race\n").expect("race");
        },
    );
    assert_eq!(dirty.status.code(), Some(1), "{}", summarize(&dirty));
    let stderr = String::from_utf8_lossy(&dirty.stderr);
    assert!(
        stderr.contains("working tree is not clean: race.txt"),
        "{stderr}"
    );
    assert_eq!(task_status(dir.path()), "open");
    fs::remove_file(dir.path().join("race.txt")).expect("clean");

    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success()
        .stdout("claimed auth by cursor\n");
}

/// A backslash is a legal filename on Unix. Git `-z` keeps it, and folding it
/// to `/` would hide `.relaybook\payload.rs` inside the ledger exclusion.
#[cfg(unix)]
#[test]
fn gated_claim_and_done_reject_unix_backslash_filenames() {
    const PAYLOAD: &str = ".relaybook\\payload.rs";
    const EXTRA: &str = ".relaybook\\extra.rs";
    let dir = tempfile::tempdir().expect("tempdir");
    seed_gated_task(dir.path());
    git(dir.path(), &["config", "core.protectNTFS", "false"]);
    git(dir.path(), &["config", "core.protectHFS", "false"]);

    let refuse = |args: &[&'static str], detail: &str| {
        run(dir.path(), args.iter().copied())
            .assert()
            .failure()
            .code(1)
            .stderr(predicate::str::contains(format!(
                "working tree is not clean: {detail}"
            )));
    };

    fs::write(dir.path().join(PAYLOAD), "untracked\n").expect("untracked");
    assert_git_reports_literal(dir.path(), PAYLOAD);
    refuse(&["task", "claim", "auth", "--by", "cursor"], PAYLOAD);
    assert_eq!(task_status(dir.path()), "open");
    fs::remove_file(dir.path().join(PAYLOAD)).expect("remove untracked");

    fs::write(dir.path().join(PAYLOAD), "tracked\n").expect("tracked");
    git(dir.path(), &["--literal-pathspecs", "add", "--", PAYLOAD]);
    commit_index(dir.path(), "track payload");
    fs::write(dir.path().join(PAYLOAD), "tracked\ndirty\n").expect("modify");
    assert_git_reports_literal(dir.path(), PAYLOAD);
    refuse(&["task", "claim", "auth", "--by", "cursor"], PAYLOAD);
    assert_eq!(task_status(dir.path()), "open");
    git(
        dir.path(),
        &["--literal-pathspecs", "checkout", "--", PAYLOAD],
    );

    run(dir.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success()
        .stdout("claimed auth by cursor\n");

    fs::write(dir.path().join(EXTRA), "extra\n").expect("extra");
    assert_git_reports_literal(dir.path(), EXTRA);
    refuse(&["task", "done", "auth", "--changed", "shipped"], EXTRA);
    assert_eq!(task_status(dir.path()), "claimed");
    assert_eq!(report_count(dir.path()), 0);
    fs::remove_file(dir.path().join(EXTRA)).expect("remove extra");

    fs::write(dir.path().join(PAYLOAD), "tracked\ndirty\n").expect("modify again");
    refuse(&["task", "done", "auth", "--changed", "shipped"], PAYLOAD);
    assert_eq!(task_status(dir.path()), "claimed");
    assert_eq!(report_count(dir.path()), 0);
    git(
        dir.path(),
        &["--literal-pathspecs", "checkout", "--", PAYLOAD],
    );

    let done = json_data(
        dir.path(),
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(done["status"], json!("done"));
    assert_eq!(done["commit_sha"], json!(head));
}

#[test]
fn gated_claim_records_base_and_graph_sha() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_graph_task(dir.path());
    let fake = GraphifyFake::install();
    let claim = fake_data(
        dir.path(),
        &fake,
        "graph",
        ["task", "claim", "auth", "--by", "cursor", "--json"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(claim["status"], json!("claimed"));
    assert_eq!(claim["base_sha"], json!(head));
    assert_eq!(claim["graph_sha"], json!(head));
    assert_eq!(claim["graph_status"], json!("built"));
    let log = fake.log_text();
    assert!(log.lines().any(|line| line == "update ."), "{log}");
    assert!(!log.contains("extract"), "{log}");
    let builds = graph_builds(dir.path());
    assert_eq!(
        builds,
        vec![("implement".to_string(), head, "built".to_string())]
    );
}

#[test]
fn gated_done_refreshes_graph_after_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_graph_task(dir.path());
    let fake = GraphifyFake::install();
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["task", "claim", "auth", "--by", "cursor", "--json"],
    );
    let base = git(dir.path(), &["rev-parse", "HEAD"]);
    fs::write(dir.path().join("work.txt"), "work\n").expect("work");
    git(dir.path(), &["add", "work.txt"]);
    commit_index(dir.path(), "do the work");
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_ne!(base, head);
    let done = fake_data(
        dir.path(),
        &fake,
        "graph",
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    assert_eq!(done["commit_sha"], json!(head));
    assert_eq!(done["base_sha"], json!(base));
    assert_eq!(done["graph_sha"], json!(base));
    let db = dir.path().join(".relaybook").join("ledger.db");
    let (commit, stored_base, stored_graph) = report_shas(&db);
    assert_eq!(commit.as_deref(), Some(head.as_str()));
    assert_eq!(stored_base.as_deref(), Some(base.as_str()));
    assert_eq!(stored_graph.as_deref(), Some(base.as_str()));
    assert_eq!(
        graph_builds(dir.path()),
        vec![
            ("implement".to_string(), base, "built".to_string()),
            ("commit".to_string(), head.clone(), "built".to_string()),
        ]
    );
    let log = fake.log_text();
    assert_eq!(
        log.lines().filter(|line| *line == "update .").count(),
        2,
        "{log}"
    );
    assert!(!log.contains("extract"), "{log}");

    let failed = tempfile::tempdir().expect("tempdir");
    seed_graph_task(failed.path());
    let fail_fake = GraphifyFake::install();
    fake_data(
        failed.path(),
        &fail_fake,
        "graph",
        ["task", "claim", "auth", "--by", "cursor", "--json"],
    );
    fs::write(failed.path().join("work.txt"), "work\n").expect("work");
    git(failed.path(), &["add", "work.txt"]);
    commit_index(failed.path(), "do the work");
    let failed_head = git(failed.path(), &["rev-parse", "HEAD"]);
    fake_data(
        failed.path(),
        &fail_fake,
        "fail",
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    assert_eq!(task_status(failed.path()), "done");
    let (commit, _, _) = report_shas(&failed.path().join(".relaybook").join("ledger.db"));
    assert_eq!(commit.as_deref(), Some(failed_head.as_str()));
    assert_eq!(
        graph_builds(failed.path())
            .into_iter()
            .find(|(step, _, _)| step == "commit")
            .map(|(_, sha, status)| (sha, status)),
        Some((failed_head, "failed".to_string()))
    );
    assert!(fail_fake.log_text().lines().any(|line| line == "update ."));
}

#[test]
fn claim_anchor_survives_a_later_implement_refresh() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_graph_task(dir.path());
    let fake = GraphifyFake::install();
    fake_data(
        dir.path(),
        &fake,
        "graph",
        [
            "graph",
            "refresh",
            "--step",
            "implement",
            "--task",
            "auth",
            "--json",
        ],
    );
    let earlier = git(dir.path(), &["rev-parse", "HEAD"]);
    git(
        dir.path(),
        &[
            "-c",
            "user.email=relaybook@example.com",
            "-c",
            "user.name=Relaybook",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "move",
        ],
    );
    let claim = fake_data(
        dir.path(),
        &fake,
        "graph",
        ["task", "claim", "auth", "--by", "cursor", "--json"],
    );
    let base = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_ne!(earlier, base);
    assert_eq!(claim["base_sha"], json!(base));
    assert_eq!(claim["graph_sha"], json!(base));

    fs::write(dir.path().join("work.txt"), "work\n").expect("work");
    git(dir.path(), &["add", "work.txt"]);
    commit_index(dir.path(), "do the work");
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_ne!(base, head);
    fake_data(
        dir.path(),
        &fake,
        "graph",
        [
            "graph",
            "refresh",
            "--step",
            "implement",
            "--task",
            "auth",
            "--json",
        ],
    );
    let done = fake_data(
        dir.path(),
        &fake,
        "graph",
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    assert_eq!(done["commit_sha"], json!(head));
    assert_eq!(done["base_sha"], json!(base));
    assert_eq!(done["graph_sha"], json!(base));
    let (commit, stored_base, stored_graph) =
        report_shas(&dir.path().join(".relaybook").join("ledger.db"));
    assert_eq!(commit.as_deref(), Some(head.as_str()));
    assert_eq!(stored_base.as_deref(), Some(base.as_str()));
    assert_eq!(stored_graph.as_deref(), Some(base.as_str()));

    let review = fake_data(
        dir.path(),
        &fake,
        "graph",
        ["review", "start", "auth", "--json"],
    );
    assert_eq!(review["base_sha"], json!(base));
    assert_eq!(review["commit_sha"], json!(head));
    assert_eq!(review["range"], json!(format!("{base}..{head}")));
}

#[test]
fn graph_refresh_skips_without_graphify() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_repo(dir.path());
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["graph", "refresh", "--step", "plan"])
        .assert()
        .failure()
        .code(1)
        .stderr("graph refresh requires workflow=gated\n");

    run(dir.path(), ["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    run(dir.path(), ["config", "set", "workflow", "gated"])
        .assert()
        .success();
    let data = json_without_graphify(dir.path(), ["graph", "refresh", "--step", "plan", "--json"]);
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(data["commit_sha"], json!(head));
    assert!(data["graph_sha"].is_null());
    assert_eq!(data["status"], json!("skipped"));
    assert_eq!(data["detail"], json!("no graph"));
    assert_eq!(
        graph_builds(dir.path()),
        vec![("plan".to_string(), head, "skipped".to_string())]
    );
}

#[test]
fn graphify_hiding_helper_matches_production_lookup() {
    let root = tempfile::tempdir().expect("tempdir");
    let shim = root.path().join("shim");
    fs::create_dir_all(&shim).expect("shim");
    let program = shim.join(if cfg!(windows) {
        "graphify.exe"
    } else {
        "graphify"
    });
    fs::write(&program, b"").expect("graphify");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&program).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).expect("chmod");
    }
    let mut raw = OsString::from(&shim);
    raw.push(if cfg!(windows) { ";" } else { ":" });
    if let Some(rest) = std::env::var_os("PATH") {
        raw.push(rest);
    }
    assert!(
        which::which_in("graphify", Some(shim.as_os_str()), root.path()).is_ok(),
        "planted graphify was not visible to which"
    );
    let hidden = path_without_graphify(&raw, root.path());
    assert!(
        std::env::split_paths(&hidden).all(|entry| entry != shim),
        "helper kept {}",
        shim.display()
    );
    for entry in std::env::split_paths(&hidden) {
        assert!(
            which::which_in("graphify", Some(entry.as_os_str()), root.path()).is_err(),
            "remaining PATH entry resolves graphify: {}",
            entry.display()
        );
    }
}

#[test]
#[cfg(windows)]
fn graphify_hiding_helper_drops_cmd_and_bat_shims() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_repo(dir.path());
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    run(dir.path(), ["config", "set", "workflow", "gated"])
        .assert()
        .success();

    let shim_root = tempfile::tempdir().expect("shim");
    let shim = shim_root.path();
    fs::write(shim.join("graphify.cmd"), "@echo off\r\nexit /b 0\r\n").expect("cmd");
    fs::write(shim.join("graphify.bat"), "@echo off\r\nexit /b 0\r\n").expect("bat");
    let mut raw = OsString::from(shim);
    raw.push(";");
    if let Some(rest) = std::env::var_os("PATH") {
        raw.push(rest);
    }
    assert!(
        which::which_in("graphify", Some(shim.as_os_str()), dir.path()).is_ok(),
        "cmd and bat shims were not visible to which"
    );
    let hidden = path_without_graphify(&raw, dir.path());
    assert!(
        std::env::split_paths(&hidden).all(|entry| entry != shim),
        "helper kept {}",
        shim.display()
    );

    let data = json_on_path_without_graphify(
        dir.path(),
        &raw,
        ["graph", "refresh", "--step", "plan", "--json"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(data["commit_sha"], json!(head));
    assert!(data["graph_sha"].is_null());
    assert_eq!(data["status"], json!("skipped"));
    assert_eq!(data["detail"], json!("no graph"));
    assert_eq!(
        graph_builds(dir.path()),
        vec![("plan".to_string(), head, "skipped".to_string())]
    );
}

#[test]
fn graphify_hiding_helper_does_not_add_a_which_dev_dependency() {
    enum Section {
        Dependencies,
        DevDependencies,
        Other,
    }

    let manifest = include_str!("../Cargo.toml");
    let mut section = Section::Other;
    let mut normal_which = false;
    let mut dev_which = false;
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            section = match header {
                "dependencies" => Section::Dependencies,
                "dev-dependencies" => Section::DevDependencies,
                _ => Section::Other,
            };
            continue;
        }
        let Some(key) = line.split_once('=').map(|(key, _)| key.trim()) else {
            continue;
        };
        if key != "which" {
            continue;
        }
        match section {
            Section::Dependencies => normal_which = true,
            Section::DevDependencies => dev_which = true,
            Section::Other => {}
        }
    }
    assert!(
        !dev_which,
        "which is already a normal dependency; a dev-dependency duplicates it"
    );
    assert!(
        normal_which,
        "the hiding helper must use the normal which dependency"
    );
}

#[test]
fn relative_path_entry_resolves_from_nested_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_graph_task(dir.path());
    let mut ignore = fs::read_to_string(dir.path().join(".gitignore")).expect("gitignore");
    ignore.push_str("fakebin/\nsub/\n");
    fs::write(dir.path().join(".gitignore"), ignore).expect("gitignore");
    git(dir.path(), &["add", ".gitignore"]);
    commit_index(dir.path(), "ignore lookup fixtures");

    let fakebin = dir.path().join("fakebin");
    fs::create_dir_all(&fakebin).expect("fakebin");
    let program = fakebin.join(if cfg!(windows) {
        "graphify.exe"
    } else {
        "graphify"
    });
    fs::copy(env!("CARGO_BIN_EXE_fake"), &program).expect("copy fake");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&program).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).expect("chmod");
    }
    let sub = dir.path().join("sub");
    fs::create_dir_all(&sub).expect("sub");
    let log = fakebin.join("graphify.log");

    // `../fakebin` is the entry under test. Later entries stay so `git` resolves.
    let mut entries = vec![PathBuf::from("..").join("fakebin")];
    if let Some(rest) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&rest));
    }
    let path = std::env::join_paths(&entries).expect("path");
    let mut cmd = command(&sub);
    cmd.env("PATH", &path);
    cmd.env("GRAPHIFY_LOG", &log);
    cmd.env("RELAYBOOK_FAKE_GRAPHIFY", "graph");
    let output = cmd
        .args(["graph", "refresh", "--step", "plan", "--json"])
        .output()
        .expect("output");
    assert!(output.status.success(), "{}", summarize(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(envelope["data"]["status"], json!("built"), "{envelope}");
    let logged = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logged.lines().any(|line| line == "update ."),
        "GRAPHIFY_LOG={logged:?}"
    );
}

#[test]
#[cfg(windows)]
fn windows_empty_path_entry_does_not_resolve_cwd_shim() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_repo(dir.path());
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    run(dir.path(), ["config", "set", "workflow", "gated"])
        .assert()
        .success();
    fs::write(
        dir.path().join("graphify.cmd"),
        "@echo off\r\nexit /b 0\r\n",
    )
    .expect("cmd");

    // A leading empty entry used to mean the current directory. which 8 skips it.
    let mut entries = vec![PathBuf::new()];
    if let Some(rest) = std::env::var_os("PATH") {
        for entry in std::env::split_paths(&rest) {
            if which::which_in("graphify", Some(entry.as_os_str()), dir.path()).is_ok() {
                continue;
            }
            entries.push(entry);
        }
    }
    let path = std::env::join_paths(&entries).expect("path");
    let data = json_with_path(
        dir.path(),
        &path,
        ["graph", "refresh", "--step", "plan", "--json"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(data["commit_sha"], json!(head));
    assert!(data["graph_sha"].is_null());
    assert_eq!(data["status"], json!("skipped"));
    assert_eq!(data["detail"], json!("no graph"));
    assert_eq!(
        graph_builds(dir.path()),
        vec![("plan".to_string(), head, "skipped".to_string())]
    );
}

#[test]
fn agent_binary_used_only_when_version_names_cursor() {
    let dir = classic_auto_repo();
    let naming = plant_agent(dir.path(), true);
    assert!(
        which::which_in("agent", Some(naming.bin.as_os_str()), dir.path()).is_ok(),
        "fake agent was not visible to which"
    );
    assert!(
        which::which_in("cursor-agent", Some(naming.path.as_os_str()), dir.path()).is_err(),
        "cursor-agent was still on PATH"
    );
    let output = run_with_agent(dir.path(), &naming);
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    assert!(text.contains("cursor: plan account"), "{text}");
    let cursor_args = fs::read_to_string(naming.log.join("cursor.args")).unwrap_or_default();
    assert!(
        cursor_args.contains("-p"),
        "agent was not used as the cursor CLI: {cursor_args:?}"
    );

    let other = classic_auto_repo();
    let unnamed = plant_agent(other.path(), false);
    assert!(
        which::which_in("agent", Some(unnamed.bin.as_os_str()), other.path()).is_ok(),
        "non-cursor agent was not visible to which"
    );
    assert!(
        which::which_in("cursor-agent", Some(unnamed.path.as_os_str()), other.path()).is_err(),
        "cursor-agent was still on PATH"
    );
    let output = run_with_agent(other.path(), &unnamed);
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("cursor: not on PATH"), "{text}");
    assert!(!text.contains("cursor: plan account"), "{text}");
    assert!(
        !unnamed.log.join("cursor.args").is_file(),
        "rejected agent was still invoked"
    );
}

#[test]
fn flow_never_runs_graphify_extract() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_graph_task(dir.path());
    let fake = GraphifyFake::install();
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["graph", "refresh", "--step", "plan", "--json"],
    );
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["task", "claim", "auth", "--by", "cursor", "--json"],
    );
    fs::write(dir.path().join("work.txt"), "work\n").expect("work");
    git(dir.path(), &["add", "work.txt"]);
    commit_index(dir.path(), "do the work");
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["review", "start", "auth", "--json"],
    );
    let log = fake.log_text();
    assert!(
        log.lines()
            .all(|line| line == "update ." || line.starts_with("affected ")),
        "{log}"
    );
    assert!(
        log.lines().filter(|line| *line == "update .").count() >= 4,
        "{log}"
    );
    assert!(
        log.lines().any(|line| line.starts_with("affected ")),
        "{log}"
    );
    assert!(!log.contains("extract"), "{log}");
    let steps: Vec<String> = graph_builds(dir.path())
        .into_iter()
        .map(|(step, _, status)| {
            assert_eq!(status, "built");
            step
        })
        .collect();
    assert_eq!(
        steps,
        vec![
            "plan".to_string(),
            "implement".to_string(),
            "commit".to_string(),
            "review".to_string(),
        ]
    );
}

#[test]
fn review_start_caps_graph_context() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_graph_task(dir.path());
    let fake = GraphifyFake::install();
    let claim = fake_data(
        dir.path(),
        &fake,
        "affected-large",
        ["task", "claim", "auth", "--by", "cursor", "--json"],
    );
    let base = claim["base_sha"].as_str().expect("base").to_string();
    fs::write(dir.path().join("work.txt"), "work\n").expect("work");
    git(dir.path(), &["add", "work.txt"]);
    commit_index(dir.path(), "do the work");
    fake_data(
        dir.path(),
        &fake,
        "affected-large",
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    let review = fake_data(
        dir.path(),
        &fake,
        "affected-large",
        ["review", "start", "auth", "--json"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    let context = review["graph_context"].as_str().expect("context");
    assert_eq!(context.len(), 4000, "{context}");
    assert!(context.ends_with("\n[truncated]"), "{context}");
    assert!(context.chars().filter(|ch| *ch == 'a').count() > 3000);
    assert_eq!(review["base_sha"], json!(base));
    assert_eq!(review["commit_sha"], json!(head));
    assert_eq!(review["range"], json!(format!("{base}..{head}")));
    assert_eq!(review["graph_sha"], json!(head));
    assert_eq!(review["graph_status"], json!("built"));
    assert_eq!(review["report"]["changed"][0], json!("shipped"));
    let log = fake.log_text();
    assert!(log.lines().any(|line| line == "affected work.txt"), "{log}");
    assert!(!log.contains("extract"), "{log}");
}

#[test]
fn failed_refresh_does_not_inject_a_stale_graph() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_graph_task(dir.path());
    let fake = GraphifyFake::install();
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["task", "claim", "auth", "--by", "cursor", "--json"],
    );
    fs::write(dir.path().join("work.txt"), "work\n").expect("work");
    git(dir.path(), &["add", "work.txt"]);
    commit_index(dir.path(), "do the work");
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    assert!(dir.path().join("graphify-out").join("graph.json").is_file());
    let review = fake_data(
        dir.path(),
        &fake,
        "fail",
        ["review", "start", "auth", "--json"],
    );
    assert!(review["graph_sha"].is_null(), "{review}");
    assert_eq!(review["graph_status"], json!("failed"));
    assert_eq!(review["graph_context"], json!("no graph"));
    assert!(
        !fake
            .log_text()
            .lines()
            .any(|line| line.starts_with("affected ")),
        "{}",
        fake.log_text()
    );
}

#[test]
fn graph_refresh_from_a_nested_directory_uses_the_repository_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_graph_task(dir.path());
    let nested = dir.path().join("crates").join("app");
    fs::create_dir_all(&nested).expect("nested");
    let fake = GraphifyFake::install();
    let cwd_log = fake.outside("cwd.log");
    let root = fs::canonicalize(dir.path()).expect("root");

    let refreshed = fake_data_cwd(
        &nested,
        &fake,
        "graph",
        &cwd_log,
        ["graph", "refresh", "--step", "plan", "--json"],
    );
    assert_eq!(refreshed["status"], json!("built"));
    assert!(dir.path().join("graphify-out").join("graph.json").is_file());
    assert!(!nested.join("graphify-out").exists());

    fake_data_cwd(
        &nested,
        &fake,
        "graph",
        &cwd_log,
        ["task", "claim", "auth", "--by", "cursor", "--json"],
    );
    fs::write(dir.path().join("work.txt"), "work\n").expect("work");
    git(dir.path(), &["add", "work.txt"]);
    commit_index(dir.path(), "do the work");
    fake_data_cwd(
        &nested,
        &fake,
        "graph",
        &cwd_log,
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    let review = fake_data_cwd(
        &nested,
        &fake,
        "graph",
        &cwd_log,
        ["review", "start", "auth", "--json"],
    );
    assert_eq!(review["graph_status"], json!("built"));
    assert!(
        fake.log_text()
            .lines()
            .any(|line| line == "affected work.txt"),
        "{}",
        fake.log_text()
    );
    assert!(!nested.join("graphify-out").exists());
    let logged = fs::read_to_string(&cwd_log).expect("cwd log");
    assert!(!logged.trim().is_empty(), "{logged}");
    for line in logged.lines() {
        assert_eq!(fs::canonicalize(line.trim()).expect(line), root, "{logged}");
    }

    git(dir.path(), &["add", "-f", "--", "graphify-out/graph.json"]);
    commit_index(dir.path(), "track the graph");
    let tracked = fake_data_cwd(
        &nested,
        &fake,
        "graph",
        &cwd_log,
        ["graph", "refresh", "--step", "plan", "--json"],
    );
    assert_eq!(tracked["status"], json!("failed"));
    let detail = tracked["detail"].as_str().expect("detail");
    assert!(detail.contains("graphify-out/ is tracked"), "{detail}");
}

const TASK_REVIEW_TEXT: &str =
    r#"{"findings":"none","security":"ok","tests_checked":"cargo test","not_checked":"none"}"#;

#[test]
fn review_record_rejects_sha_mismatch_or_moved_head() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let _head = commit_gated_work(dir.path(), &fake);
    git(
        dir.path(),
        &[
            "-c",
            "user.email=relaybook@example.com",
            "-c",
            "user.name=Relaybook",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "after the report",
        ],
    );
    let started = fake_output(dir.path(), &fake, "graph", &["review", "start", "auth"]);
    assert_failure(&started, "HEAD does not match the reported commit");
    assert!(task_reviews(dir.path()).is_empty());

    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let pending = task_reviews(dir.path());
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, "codex");
    assert_eq!(pending[0].1, None);
    assert_eq!(pending[0].2, 0);
    assert_eq!(pending[0].3, head);

    fs::write(dir.path().join("extra.txt"), "x\n").expect("extra");
    assert_failure(
        &record_review(dir.path(), "codex", &head, "approved", false),
        "working tree is not clean",
    );
    fs::remove_file(dir.path().join("extra.txt")).expect("remove");
    assert_eq!(task_reviews(dir.path())[0].1, None);

    assert_failure(
        &record_review(dir.path(), "codex", "abc", "approved", false),
        "sha does not match HEAD",
    );
    git(
        dir.path(),
        &[
            "-c",
            "user.email=relaybook@example.com",
            "-c",
            "user.name=Relaybook",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "move HEAD",
        ],
    );
    let moved = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_ne!(moved, head);
    assert_failure(
        &record_review(dir.path(), "codex", &moved, "approved", false),
        "reviewer changed the repository",
    );
    assert_failure(
        &record_review(dir.path(), "codex", &head, "approved", false),
        "sha does not match HEAD",
    );
    assert_eq!(task_reviews(dir.path())[0].1, None);
}

#[test]
fn review_record_rechecks_cleanliness_inside_the_write_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    assert_record_blocked_in_lock(
        dir.path(),
        &head,
        "status",
        "working tree is not clean: race.txt",
        || {
            fs::write(dir.path().join("race.txt"), "race\n").expect("race");
        },
    );
}

#[test]
fn review_record_rechecks_head_inside_the_write_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    assert_record_blocked_in_lock(
        dir.path(),
        &head,
        "status",
        "sha does not match HEAD",
        || {
            git(
                dir.path(),
                &[
                    "-c",
                    "user.email=relaybook@example.com",
                    "-c",
                    "user.name=Relaybook",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "move",
                ],
            );
        },
    );
}

#[test]
fn review_record_rechecks_fingerprint_inside_the_write_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    assert_record_blocked_in_lock(
        dir.path(),
        &head,
        "diff",
        "reviewer changed the repository",
        || {
            fs::write(dir.path().join("work.txt"), "changed\n").expect("change");
        },
    );
}

#[test]
fn review_record_rechecks_origin_inside_the_write_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    assert_record_blocked_in_lock(
        dir.path(),
        &head,
        "status",
        "refs/remotes/origin/main exists",
        || {
            git(
                dir.path(),
                &["update-ref", "refs/remotes/origin/main", &head],
            );
        },
    );
}

fn assert_record_blocked_in_lock(
    dir: &Path,
    head: &str,
    git_arg: &str,
    message: &str,
    mutate: impl FnOnce(),
) {
    let output = during_git_recheck(
        dir,
        &[
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
        ],
        git_arg,
        mutate,
    );
    assert_failure(&output, message);
    let pending = task_reviews(dir);
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].1, None, "{pending:?}");
    assert_eq!(task_status(dir), "done");
}

#[test]
fn review_record_detects_ledger_write_during_review() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    run(
        dir.path(),
        [
            "decision",
            "log",
            "auth",
            "--why",
            "during review",
            "--by",
            "orca",
        ],
    )
    .assert()
    .success();
    assert_failure(
        &record_review(dir.path(), "codex", &head, "approved", false),
        "ledger changed during review",
    );
    assert_eq!(task_reviews(dir.path())[0].1, None);

    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["review", "start", "auth", "--json"],
    );
    let recorded = data_of(
        dir.path(),
        &[
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
            "--json",
        ],
    );
    assert_eq!(recorded["verdict"], json!("approved"));
    assert_eq!(recorded["commit_sha"], json!(head));
    assert_eq!(task_status(dir.path()), "done");
}

#[test]
fn review_record_by_claude_in_gated_needs_tty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let output = command(dir.path())
        .stdin(Stdio::null())
        .args([
            "review",
            "record",
            "auth",
            "--by",
            "claude",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
        ])
        .output()
        .expect("output");
    assert_failure(&output, "claude review requires confirmation at a TTY");
    let pending = task_reviews(dir.path());
    assert_eq!(pending[0].0, "codex");
    assert_eq!(pending[0].1, None);
    assert_eq!(pending[0].2, 0);

    let recorded = data_of(
        dir.path(),
        &[
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
            "--json",
        ],
    );
    assert_eq!(recorded["reviewer"], json!("codex"));
    assert_eq!(recorded["reviewer_override"], json!(false));
}

#[test]
fn review_record_rejects_unconfigured_reviewer() {
    let classic = tempfile::tempdir().expect("tempdir");
    run(classic.path(), ["init"]).assert().success();
    run(
        classic.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
    .assert()
    .success();
    run(classic.path(), ["task", "claim", "auth", "--by", "cursor"])
        .assert()
        .success();
    run(
        classic.path(),
        ["task", "done", "auth", "--changed", "shipped"],
    )
    .assert()
    .success();
    assert_failure(
        &record_review(classic.path(), "codex", "abc", "approved", false),
        "review record requires workflow=gated",
    );
    assert!(task_reviews(classic.path()).is_empty());

    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    assert_failure(
        &record_review(dir.path(), "cursor", &head, "approved", false),
        "unconfigured reviewer: cursor",
    );
    assert_failure(
        &record_review(dir.path(), "orca", &head, "approved", false),
        "unconfigured reviewer: orca",
    );
    assert_eq!(task_reviews(dir.path())[0].1, None);
    let recorded = data_of(
        dir.path(),
        &[
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
            "--json",
        ],
    );
    assert_eq!(recorded["reviewer"], json!("codex"));
    assert_eq!(recorded["reviewer_override"], json!(false));
}

#[test]
fn changes_requested_reopens_task() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let output = record_review(dir.path(), "codex", &head, "changes-requested", false);
    assert!(output.status.success(), "{}", summarize(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("recorded auth changes_requested"),
        "{stdout}"
    );
    assert_eq!(task_status(dir.path()), "open");
    let context = run(dir.path(), ["context", "auth"]).assert().success();
    let text = String::from_utf8(context.get_output().stdout.clone()).expect("utf8");
    assert!(text.contains("reopened: changes requested"), "{text}");
    assert_eq!(
        task_reviews(dir.path())[0].1.as_deref(),
        Some("changes_requested")
    );
    assert_failure(
        &relay_output(dir.path(), &["report", "accept", "auth"]),
        "no approved review at HEAD",
    );
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["task", "claim", "auth", "--by", "cursor", "--json"],
    );
    assert_eq!(task_status(dir.path()), "claimed");
}

#[test]
fn gated_accept_requires_valid_review() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = commit_gated_work(dir.path(), &fake);
    assert_failure(
        &relay_output(dir.path(), &["report", "accept", "auth"]),
        "no approved review at HEAD",
    );
    fs::write(dir.path().join("scratch.txt"), "x\n").expect("scratch");
    assert_failure(
        &relay_output(dir.path(), &["report", "accept", "auth"]),
        "working tree is not clean",
    );
    fs::remove_file(dir.path().join("scratch.txt")).expect("remove");
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["review", "start", "auth", "--json"],
    );
    let pending = data_of(dir.path(), &["context", "auth", "--json"]);
    assert!(pending["latest_review"]["verdict"].is_null());
    assert_eq!(pending["latest_review"]["commit_sha"], json!(head));
    assert_failure(
        &relay_output(dir.path(), &["report", "accept", "auth"]),
        "no approved review at HEAD",
    );
    data_of(
        dir.path(),
        &[
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
            "--json",
        ],
    );
    git(
        dir.path(),
        &[
            "-c",
            "user.email=relaybook@example.com",
            "-c",
            "user.name=Relaybook",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "after approval",
        ],
    );
    assert_failure(
        &relay_output(dir.path(), &["report", "accept", "auth"]),
        "no approved review at HEAD",
    );
    assert_eq!(task_report_verdict(dir.path()), None);
    assert_eq!(task_status(dir.path()), "done");

    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    data_of(
        dir.path(),
        &[
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
            "--json",
        ],
    );
    let view = data_of(dir.path(), &["context", "auth", "--json"]);
    assert_eq!(view["latest_review"]["verdict"], json!("approved"));
    assert_eq!(view["latest_review"]["reviewer"], json!("codex"));
    assert_eq!(view["latest_review"]["commit_sha"], json!(head));
    let (_, _, graph_sha) = report_shas(&dir.path().join(".relaybook").join("ledger.db"));
    assert_eq!(view["graph_sha"], json!(graph_sha.as_deref()));
    let shown = run(dir.path(), ["context", "auth"]).assert().success();
    let shown = String::from_utf8(shown.get_output().stdout.clone()).expect("utf8");
    assert!(
        shown.contains(&format!("review approved by codex at {head}")),
        "{shown}"
    );
    let graph_sha = graph_sha.expect("graph");
    assert!(shown.contains(&format!("graph {graph_sha}")), "{shown}");
    run(dir.path(), ["report", "accept", "auth"])
        .assert()
        .success()
        .stdout("accepted auth\n");
    assert_eq!(task_report_verdict(dir.path()).as_deref(), Some("accepted"));
    assert_failure(
        &relay_output(dir.path(), &["report", "accept", "auth"]),
        "no unreviewed report: auth",
    );
    let note = data_of(
        dir.path(),
        &[
            "report",
            "note",
            "--by",
            "orca",
            "--text",
            "spans tasks",
            "--json",
        ],
    );
    let id = note["id"].as_i64().expect("id").to_string();
    let accepted = relay_output(dir.path(), &["report", "accept", "--note", &id]);
    assert!(accepted.status.success(), "{}", summarize(&accepted));
    assert!(
        String::from_utf8_lossy(&accepted.stdout).contains("accepted note"),
        "{}",
        summarize(&accepted)
    );
    assert_failure(
        &relay_output(dir.path(), &["report", "accept", "--all"]),
        "report accept --all is unavailable when workflow=gated",
    );
}

#[test]
fn gated_refuses_when_origin_branch_exists() {
    let fake = GraphifyFake::install();
    let other = tempfile::tempdir().expect("tempdir");
    let head = prepare_gated_review(other.path(), &fake);
    git(
        other.path(),
        &["update-ref", "refs/remotes/origin/other", &head],
    );
    assert_failure(
        &record_review(other.path(), "codex", &head, "approved", false),
        "refs/remotes/origin/other",
    );
    assert_eq!(task_reviews(other.path())[0].1, None);

    let blocked = tempfile::tempdir().expect("tempdir");
    let head = prepare_gated_review(blocked.path(), &fake);
    git(
        blocked.path(),
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    assert_failure(
        &record_review(blocked.path(), "codex", &head, "approved", false),
        "refs/remotes/origin/main exists",
    );
    assert_eq!(task_reviews(blocked.path())[0].1, None);
    git(
        blocked.path(),
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let recorded = data_of(
        blocked.path(),
        &[
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
            "--json",
        ],
    );
    assert_eq!(recorded["verdict"], json!("approved"));
    assert_eq!(recorded["commit_sha"], json!(head));
}

#[test]
fn remote_check_finds_branch_on_a_remote_not_named_origin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let _upstream = bare_remote(dir.path(), "upstream");
    git(dir.path(), &["push", "upstream", "HEAD:refs/heads/main"]);
    delete_ref_if_present(dir.path(), "refs/remotes/upstream/main");
    delete_ref_if_present(dir.path(), "refs/remotes/upstream/HEAD");
    assert_failure(
        &record_review(dir.path(), "codex", &head, "approved", false),
        "refs/heads/main exists on upstream",
    );
    assert_eq!(task_reviews(dir.path())[0].1, None);
}

#[test]
fn remote_check_finds_commit_pushed_under_another_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let _origin = bare_remote(dir.path(), "origin");
    git(dir.path(), &["push", "origin", "HEAD:refs/heads/other"]);
    delete_ref_if_present(dir.path(), "refs/remotes/origin/other");
    delete_ref_if_present(dir.path(), "refs/remotes/origin/HEAD");
    assert_failure(
        &record_review(dir.path(), "codex", &head, "approved", false),
        "refs/heads/other",
    );
    assert_eq!(task_reviews(dir.path())[0].1, None);
}

#[test]
fn remote_check_refuses_when_release_range_fails_on_a_published_interior_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    seed_graph_task(dir.path());
    git(dir.path(), &["checkout", "-b", "feature"]);
    fs::write(dir.path().join("earlier.txt"), "earlier\n").expect("write");
    git(dir.path(), &["add", "earlier.txt"]);
    commit_index(dir.path(), "earlier");
    let interior = git(dir.path(), &["rev-parse", "HEAD"]);
    let head = finish_task(dir.path(), &fake, "auth", "work.txt");
    assert_ne!(interior, head);
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["review", "start", "auth", "--json"],
    );
    let origin = bare_remote(dir.path(), "origin");
    let spec = format!("{interior}:refs/heads/other");
    git(dir.path(), &["push", "origin", &spec]);
    assert_eq!(
        git(origin.path(), &["rev-parse", "refs/heads/other"]),
        interior
    );
    delete_ref_if_present(dir.path(), "refs/remotes/origin/other");
    delete_ref_if_present(dir.path(), "refs/remotes/origin/HEAD");
    git(dir.path(), &["update-ref", "-d", "refs/heads/main"]);
    assert!(rev_missing(dir.path(), "refs/heads/main"));
    let output = record_review(dir.path(), "codex", &head, "approved", false);
    assert_failure(&output, "remote check failed");
    assert_eq!(task_reviews(dir.path())[0].1, None);
    assert_eq!(latest_remote_status(dir.path()), "unknown");
}

#[test]
fn slash_remote_tracking_ref_at_older_sha_refuses_in_local_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    seed_graph_task(dir.path());
    git(dir.path(), &["checkout", "-b", "feature"]);
    let head = finish_task(dir.path(), &fake, "auth", "work.txt");
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["review", "start", "auth", "--json"],
    );
    let older = git(dir.path(), &["rev-parse", "HEAD~1"]);
    assert_ne!(older, head);
    let _upstream = bare_remote(dir.path(), "team/upstream");
    git(
        dir.path(),
        &["update-ref", "refs/remotes/team/upstream/feature", &older],
    );
    let switched = answer_prompts(
        dir.path(),
        &["config", "set", "remote_check", "local"],
        &[PromptAnswer::Phrase],
    );
    assert!(switched.success, "{}", switched.output);
    assert_failure(
        &record_review(dir.path(), "codex", &head, "approved", false),
        "refs/remotes/team/upstream/feature exists",
    );
    assert_eq!(task_reviews(dir.path())[0].1, None);
}

#[test]
fn remote_check_checks_every_remote() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let _origin = bare_remote(dir.path(), "origin");
    let _backup = bare_remote(dir.path(), "backup");
    let listed = git(dir.path(), &["remote"]);
    let names: Vec<String> = listed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let mut expected = names.clone();
    expected.sort();
    assert_eq!(expected, ["backup".to_string(), "origin".to_string()]);
    let first = names[0].clone();
    let last = names.last().expect("remote").clone();
    assert_ne!(first, last);
    // `git remote` sorts names, so backup is first. Publish only on the last
    // remote: a check that stops after the first remote would accept this review.
    git(dir.path(), &["push", &last, "HEAD:refs/heads/main"]);
    delete_ref_if_present(dir.path(), &format!("refs/remotes/{last}/main"));
    delete_ref_if_present(dir.path(), &format!("refs/remotes/{last}/HEAD"));
    let untouched = git(
        dir.path(),
        &["ls-remote", "--quiet", "--end-of-options", &first],
    );
    assert!(untouched.is_empty(), "{first}: {untouched}");
    let gate = tempfile::tempdir().expect("gate");
    let log_dir = tempfile::tempdir().expect("log");
    let log = log_dir.path().join("git-env.log");
    let output = command_with_git_shim(dir.path(), gate.path(), "not-a-git-command")
        .env("RELAYBOOK_GIT_ENV_LOG", &log)
        .args([
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
        ])
        .output()
        .expect("output");
    assert_failure(&output, &format!("refs/heads/main exists on {last}"));
    assert_eq!(task_reviews(dir.path())[0].1, None);
    let text = fs::read_to_string(&log).expect("env log");
    assert_eq!(ls_remote_targets(&text), names, "{text}");
}

fn ls_remote_targets(log: &str) -> Vec<String> {
    let marker = "ls-remote --quiet --end-of-options ";
    log.lines()
        .filter_map(|line| {
            let argv = line.split_once("argv=")?.1;
            let name = argv.split_once(marker)?.1.split_whitespace().next()?;
            Some(name.to_string())
        })
        .collect()
}

#[test]
fn remote_check_finds_tag_or_pull_ref_into_range() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let _origin = bare_remote(dir.path(), "origin");
    git(dir.path(), &["push", "origin", "HEAD:refs/tags/x"]);
    assert_failure(
        &record_review(dir.path(), "codex", &head, "approved", false),
        "refs/tags/x",
    );
    git(dir.path(), &["push", "origin", ":refs/tags/x"]);
    git(dir.path(), &["push", "origin", "HEAD:refs/pull/1/head"]);
    assert_failure(
        &record_review(dir.path(), "codex", &head, "approved", false),
        "refs/pull/1/head",
    );
    assert_eq!(task_reviews(dir.path())[0].1, None);
}

#[test]
fn stale_tracking_ref_of_deleted_branch_still_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let origin = bare_remote(dir.path(), "origin");
    git(dir.path(), &["push", "origin", "HEAD:refs/heads/main"]);
    git(dir.path(), &["fetch", "origin"]);
    assert!(
        !rev_missing(dir.path(), "refs/remotes/origin/main"),
        "fetch did not leave a tracking ref"
    );
    let bare = origin.path().to_str().expect("utf8");
    git(
        dir.path(),
        &["--git-dir", bare, "update-ref", "-d", "refs/heads/main"],
    );
    assert_failure(
        &record_review(dir.path(), "codex", &head, "approved", false),
        "published earlier; deleting the remote branch does not unpublish it",
    );
    assert_eq!(task_reviews(dir.path())[0].1, None);
}

#[test]
fn unreachable_remote_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    git(
        dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "/nonexistent/relaybook-r7-missing",
        ],
    );
    let output = relay_output(
        dir.path(),
        &[
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
            "--json",
        ],
    );
    assert_json_failure(&output, "origin");
    assert_json_failure(&output, "ls-remote failed");
    assert_eq!(task_reviews(dir.path())[0].1, None);
    let status = data_of(dir.path(), &["release", "status", "--json"]);
    assert_eq!(status["remote"]["remote"], json!("origin"));
    assert_eq!(status["remote"]["status"], json!("unknown"));
}

#[test]
fn remote_check_times_out_and_never_hangs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let _origin = bare_remote(dir.path(), "origin");
    let gate = tempfile::tempdir().expect("gate");
    let started = Instant::now();
    let output = command_with_git_shim(dir.path(), gate.path(), "ls-remote")
        .env("RELAYBOOK_REMOTE_TIMEOUT_SECS", "2")
        .args([
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
        ])
        .output()
        .expect("output");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "elapsed {:?}",
        started.elapsed()
    );
    assert_failure(&output, "timed out");
    assert_eq!(task_reviews(dir.path())[0].1, None);
}

#[test]
fn remote_check_timeout_returns_while_descendant_holds_stderr() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let _origin = bare_remote(dir.path(), "origin");
    let gate = tempfile::tempdir().expect("gate");
    let pid_dir = tempfile::tempdir().expect("pid");
    let pid_file = pid_dir.path().join("holder.pid");
    let _holder = HolderGuard::watch(&pid_file);
    let started = Instant::now();
    let output = command_with_git_shim(dir.path(), gate.path(), "ls-remote")
        .env("RELAYBOOK_REMOTE_TIMEOUT_SECS", "2")
        .env("RELAYBOOK_GIT_HOLD_STDERR", &pid_file)
        .args([
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
        ])
        .output()
        .expect("output");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "elapsed {:?}",
        started.elapsed()
    );
    assert_failure(&output, "timed out");
    assert_eq!(task_reviews(dir.path())[0].1, None);
    let pid = held_pid(&pid_file);
    assert!(
        pid_is_alive(pid),
        "descendant {pid} was not still holding stderr"
    );
}

#[test]
fn remote_check_bounds_drain_when_descendant_holds_stderr_after_exit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let _origin = bare_remote(dir.path(), "origin");
    let gate = tempfile::tempdir().expect("gate");
    let pid_dir = tempfile::tempdir().expect("pid");
    let pid_file = pid_dir.path().join("holder.pid");
    let _holder = HolderGuard::watch(&pid_file);
    let started = Instant::now();
    let output = command_with_git_shim(dir.path(), gate.path(), "not-a-git-command")
        .env("RELAYBOOK_REMOTE_TIMEOUT_SECS", "2")
        .env("RELAYBOOK_GIT_HOLD_STDERR", &pid_file)
        .args([
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
        ])
        .output()
        .expect("output");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "elapsed {:?}",
        started.elapsed()
    );
    assert!(output.status.success(), "{}", summarize(&output));
    let pid = held_pid(&pid_file);
    assert!(
        pid_is_alive(pid),
        "descendant {pid} was not still holding stderr"
    );
}

#[test]
fn remote_check_passes_no_prompt_env_and_null_stdin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let _origin = bare_remote(dir.path(), "origin");
    let gate = tempfile::tempdir().expect("gate");
    let log_dir = tempfile::tempdir().expect("log");
    let log = log_dir.path().join("git-env.log");
    let output = command_with_git_shim(dir.path(), gate.path(), "not-a-git-command")
        .env("RELAYBOOK_GIT_ENV_LOG", &log)
        .args([
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
        ])
        .output()
        .expect("output");
    assert!(output.status.success(), "{}", summarize(&output));
    let text = fs::read_to_string(&log).expect("env log");
    let stdin_mark = if cfg!(windows) {
        "stdin=not-a-tty"
    } else {
        "stdin=/dev/null"
    };
    assert!(
        text.lines().any(|line| {
            line.contains("ls-remote")
                && line.contains("GIT_TERMINAL_PROMPT=0")
                && line.contains(stdin_mark)
                && !line.contains("stdin=tty")
        }),
        "{text}"
    );
}

#[test]
fn remote_check_ignores_push_url_rewrite_and_reports_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let _origin = bare_remote(dir.path(), "origin");
    git(dir.path(), &["push", "origin", "HEAD:refs/heads/main"]);
    delete_ref_if_present(dir.path(), "refs/remotes/origin/main");
    delete_ref_if_present(dir.path(), "refs/remotes/origin/HEAD");
    git(
        dir.path(),
        &[
            "config",
            "remote.origin.pushurl",
            "/nonexistent/push-disabled",
        ],
    );
    let fetch = git(dir.path(), &["remote", "get-url", "origin"]);
    git(
        dir.path(),
        &["config", "url./nonexistent/rewritten.pushInsteadOf", &fetch],
    );
    assert_failure(
        &record_review(dir.path(), "codex", &head, "approved", false),
        "refs/heads/main exists on origin",
    );
    let status = data_of(dir.path(), &["release", "status", "--json"]);
    assert_eq!(status["remote"]["push_url_differs"], json!(true));
}

#[test]
fn remote_urls_are_redacted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    git(
        dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://user:super-secret-token;a,b'c(d)@127.0.0.1:1/repo.git",
        ],
    );
    let output = relay_output(
        dir.path(),
        &[
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
            "--json",
        ],
    );
    assert!(!output.status.success(), "{}", summarize(&output));
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!shown.contains("super-secret-token"), "{shown}");
    assert!(!shown.contains("user:super-secret-token"), "{shown}");
    let detail = remote_detail(dir.path());
    assert!(!detail.contains("super-secret-token"), "{detail}");
    assert!(detail.contains("***@"), "{detail}");
}

#[test]
fn release_authorize_offline_needs_second_confirmation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = release_reviewed(dir.path(), &fake);
    git(
        dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "/nonexistent/relaybook-r7-offline",
        ],
    );
    let declined = answer_prompts(
        dir.path(),
        &["release", "authorize", "--sha", &head],
        &[PromptAnswer::Phrase, PromptAnswer::Text("n\n")],
    );
    assert!(
        !declined.success,
        "code {} {}",
        declined.code, declined.output
    );
    assert!(
        declined
            .output
            .contains("Proceed without remote verification?"),
        "{}",
        declined.output
    );
    assert!(authorizations(dir.path()).is_empty());
    assert_eq!(latest_remote_override(dir.path()), 0);

    let accepted = answer_prompts(
        dir.path(),
        &["release", "authorize", "--sha", &head],
        &[PromptAnswer::Phrase, PromptAnswer::Phrase],
    );
    assert!(
        accepted.success,
        "code {} {}",
        accepted.code, accepted.output
    );
    assert_eq!(authorizations(dir.path()), vec![head]);
    assert_eq!(latest_remote_override(dir.path()), 1);
    let status = data_of(dir.path(), &["release", "status", "--json"]);
    assert_eq!(status["remote"]["override"], json!(true));
    assert_eq!(status["remote"]["status"], json!("unknown"));
}

#[test]
fn release_authorize_override_stays_on_the_confirmed_check() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = release_reviewed(dir.path(), &fake);
    git(
        dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "/nonexistent/relaybook-r7-override-race",
        ],
    );
    let accepted = authorize_and_insert_during_second_prompt(dir.path(), &head);
    assert!(
        accepted.success,
        "code {} {}",
        accepted.code, accepted.output
    );
    assert_eq!(authorizations(dir.path()), vec![head]);
    let rows = remote_check_rows(dir.path());
    let concurrent = rows
        .iter()
        .find(|row| row.remote.as_deref() == Some("concurrent"))
        .expect("concurrent row");
    assert_eq!(concurrent.override_flag, 0, "{rows:?}");
    let overridden: Vec<_> = rows
        .iter()
        .filter(|row| row.context == "authorize" && row.override_flag == 1)
        .collect();
    assert_eq!(overridden.len(), 1, "{rows:?}");
    assert!(overridden[0].id < concurrent.id, "{rows:?}");
}

#[test]
fn remote_check_local_requires_terminal() {
    let plain = tempfile::tempdir().expect("tempdir");
    run(plain.path(), ["init"]).assert().success();
    let denied = command(plain.path())
        .stdin(Stdio::null())
        .args(["config", "set", "remote_check", "local"])
        .output()
        .expect("output");
    assert_failure(&denied, "remote_check=local requires confirmation at a TTY");
    let shown = run(plain.path(), ["config"]).output().expect("config");
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.contains("remote_check online"), "{shown}");

    let declined = answer_prompts(
        plain.path(),
        &["config", "set", "remote_check", "local"],
        &[PromptAnswer::Text("n\n")],
    );
    assert!(!declined.success, "{}", declined.output);
    assert!(
        declined.output.contains("Set remote_check=local?"),
        "{}",
        declined.output
    );

    let accepted = answer_prompts(
        plain.path(),
        &["config", "set", "remote_check", " local "],
        &[PromptAnswer::Phrase],
    );
    assert!(accepted.success, "{}", accepted.output);
    run(plain.path(), ["config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("remote_check local"));
    command(plain.path())
        .stdin(Stdio::null())
        .args(["config", "set", "remote_check", "local"])
        .assert()
        .success()
        .stdout("remote_check local\n");
    command(plain.path())
        .stdin(Stdio::null())
        .args(["config", "set", "remote_check", "online"])
        .assert()
        .success()
        .stdout("remote_check online\n");
    run(plain.path(), ["config", "set", "remote_check", "sometimes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid remote_check: sometimes"));

    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    git(
        dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "/nonexistent/relaybook-r7-local-only",
        ],
    );
    let switched = answer_prompts(
        dir.path(),
        &["config", "set", "remote_check", "local"],
        &[PromptAnswer::Phrase],
    );
    assert!(switched.success, "{}", switched.output);
    let recorded = data_of(
        dir.path(),
        &[
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
            "--json",
        ],
    );
    assert_eq!(recorded["verdict"], json!("approved"));
    let status = data_of(dir.path(), &["release", "status", "--json"]);
    assert_eq!(status["remote"]["remote_check"], json!("local"));
    assert_eq!(status["remote"]["status"], json!("local"));
}

const RELEASE_REVIEW_TEXT: &str = r#"{"findings":"none","security":"ok","tests_checked":"cargo test","not_checked":"none","prd_covered":true,"gaps":"none"}"#;

#[test]
fn release_review_requires_all_tasks_accepted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    seed_graph_task(dir.path());
    run(dir.path(), ["task", "create", "old", "--title", "Old work"])
        .assert()
        .success();
    let old = finish_task(dir.path(), &fake, "old", "old.txt");
    git(dir.path(), &["checkout", "-b", "feature"]);
    let head = finish_task(dir.path(), &fake, "auth", "work.txt");
    assert_ne!(old, head);
    assert_failure(
        &fake_output(
            dir.path(),
            &fake,
            "graph",
            &["review", "start", "--release"],
        ),
        "not every task in scope is accepted",
    );
    assert!(release_reviews(dir.path()).is_empty());

    run(dir.path(), ["task", "create", "notes", "--title", "Notes"])
        .assert()
        .success();
    assert_failure(
        &fake_output(
            dir.path(),
            &fake,
            "graph",
            &["review", "start", "--release"],
        ),
        "not every task in scope is accepted",
    );
    accept_task(dir.path(), &fake, "auth", &head);

    let started = fake_data(
        dir.path(),
        &fake,
        "affected-large",
        ["review", "start", "--release", "--json"],
    );
    let base = git(dir.path(), &["merge-base", "main", "HEAD"]);
    assert_eq!(base, old);
    assert!(started["slug"].is_null());
    assert_eq!(started["base_sha"], json!(base));
    assert_eq!(started["commit_sha"], json!(head));
    assert_eq!(started["range"], json!(format!("{base}..{head}")));
    assert_eq!(started["graph_status"], json!("built"));
    assert_eq!(started["graph_sha"], json!(head));
    let tasks = started["tasks"].as_array().expect("tasks");
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!(tasks[0]["slug"], json!("auth"));
    assert_eq!(tasks[0]["commit_sha"], json!(head));
    let context = started["graph_context"].as_str().expect("context");
    assert_eq!(context.len(), 4000, "{context}");
    assert!(context.ends_with("\n[truncated]"), "{context}");
    let log = fake.log_text();
    assert!(log.contains("affected work.txt"), "{log}");
    assert!(!log.contains("extract"), "{log}");
    assert_eq!(release_reviews(dir.path()).len(), 1);
    assert_eq!(release_reviews(dir.path())[0].0, None);

    assert_failure(
        &record_release(dir.path(), "codex", &head, "approved", TASK_REVIEW_TEXT),
        "review body is missing prd_covered",
    );
    assert_eq!(release_reviews(dir.path())[0].0, None);
    let recorded = data_of(
        dir.path(),
        &[
            "review",
            "record",
            "--release",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            RELEASE_REVIEW_TEXT,
            "--json",
        ],
    );
    assert_eq!(recorded["scope"], json!("release"));
    assert_eq!(recorded["verdict"], json!("approved"));
    assert_eq!(recorded["commit_sha"], json!(head));
    assert_eq!(recorded["base_sha"], json!(base));
    assert!(recorded["task_id"].is_null());

    let status = data_of(dir.path(), &["release", "status", "--json"]);
    assert_eq!(status["release"], json!("release-reviewed"));
    assert_eq!(status["commit_sha"], json!(head));
    assert_eq!(status["branch"], json!("feature"));
    assert!(status["authorization"].is_null());
    assert_eq!(task_phase(&status["tasks"], "auth"), "accepted");
    assert_eq!(task_phase(&status["tasks"], "old"), "implemented");
    assert_eq!(task_phase(&status["tasks"], "notes"), "planned");

    run(dir.path(), ["task", "create", "extra", "--title", "Extra"])
        .assert()
        .success();
    fake_data(
        dir.path(),
        &fake,
        "graph",
        ["task", "claim", "extra", "--by", "cursor", "--json"],
    );
    assert_failure(
        &fake_output(
            dir.path(),
            &fake,
            "graph",
            &["review", "start", "--release"],
        ),
        "a task is claimed",
    );
    let rows = release_reviews(dir.path());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0.as_deref(), Some("approved"));
}

#[test]
fn release_authorize_refuses_without_tty_or_wrong_sha() {
    let classic = tempfile::tempdir().expect("tempdir");
    run(classic.path(), ["init"]).assert().success();
    assert_failure(
        &relay_output(classic.path(), &["release", "authorize", "--sha", "abc"]),
        "release authorize requires workflow=gated",
    );
    assert_failure(
        &relay_output(classic.path(), &["release", "status"]),
        "release status requires workflow=gated",
    );
    assert_failure(
        &relay_output(classic.path(), &["review", "start", "--release"]),
        "review start requires workflow=gated",
    );
    assert!(authorizations(classic.path()).is_empty());

    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = release_reviewed(dir.path(), &fake);
    fs::write(dir.path().join("scratch.txt"), "x\n").expect("scratch");
    assert_failure(
        &relay_output(dir.path(), &["release", "authorize", "--sha", &head]),
        "working tree is not clean",
    );
    fs::remove_file(dir.path().join("scratch.txt")).expect("remove");
    let other = git(dir.path(), &["rev-parse", "HEAD^"]);
    assert_failure(
        &relay_output(dir.path(), &["release", "authorize", "--sha", &other]),
        "sha does not match HEAD",
    );
    assert_failure(
        &relay_output(dir.path(), &["release", "authorize", "--sha", &head[..12]]),
        "sha does not match HEAD",
    );
    git(
        dir.path(),
        &["update-ref", "refs/remotes/origin/feature", &head],
    );
    assert_failure(
        &relay_output(dir.path(), &["release", "authorize", "--sha", &head]),
        "refs/remotes/origin/feature exists",
    );
    git(
        dir.path(),
        &["update-ref", "-d", "refs/remotes/origin/feature"],
    );
    let output = command(dir.path())
        .stdin(Stdio::null())
        .args(["release", "authorize", "--sha", &head])
        .output()
        .expect("output");
    assert_failure(&output, "release authorize requires confirmation at a TTY");
    assert!(authorizations(dir.path()).is_empty());
}

#[test]
fn commit_after_release_review_invalidates_signoff() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = release_reviewed(dir.path(), &fake);
    let reviewed = data_of(dir.path(), &["release", "status", "--json"]);
    assert_eq!(reviewed["release"], json!("release-reviewed"));
    assert_eq!(reviewed["commit_sha"], json!(head));
    assert!(reviewed["authorization"].is_null());

    let authorized = authorize_at_tty(dir.path(), &fake, None, &head);
    assert!(
        authorized.success,
        "code {} {}",
        authorized.code, authorized.output
    );
    assert!(
        authorized.output.contains(&format!("authorized {head}")),
        "{}",
        authorized.output
    );
    let signed = data_of(dir.path(), &["release", "status", "--json"]);
    assert_eq!(signed["release"], json!("signed-off"));
    assert_eq!(signed["authorization"]["commit_sha"], json!(head));
    assert_eq!(
        signed["authorization"]["authorized_by"],
        json!("maintainer")
    );
    assert_eq!(authorizations(dir.path()), vec![head.clone()]);

    git(
        dir.path(),
        &[
            "-c",
            "user.email=relaybook@example.com",
            "-c",
            "user.name=Relaybook",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "after sign-off",
        ],
    );
    let next = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_ne!(next, head);
    let stale = data_of(dir.path(), &["release", "status", "--json"]);
    assert!(stale["release"].is_null(), "{stale}");
    assert!(stale["authorization"].is_null(), "{stale}");
    assert_eq!(stale["commit_sha"], json!(next));
    assert_eq!(authorizations(dir.path()), vec![head.clone()]);
    assert_failure(
        &relay_output(dir.path(), &["release", "authorize", "--sha", &head]),
        "sha does not match HEAD",
    );
    assert_failure(
        &relay_output(dir.path(), &["release", "authorize", "--sha", &next]),
        "no approved release review",
    );
    assert_eq!(authorizations(dir.path()), vec![head]);
}

#[test]
fn release_authorize_rejects_y_and_accepts_typed_sha() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = release_reviewed(dir.path(), &fake);
    let phrase = format!("authorize {}", &head[..12]);
    let rejected = answer_prompts(
        dir.path(),
        &["release", "authorize", "--sha", &head],
        &[PromptAnswer::Text("y\n")],
    );
    assert!(!rejected.success, "{}", rejected.output);
    assert!(
        rejected.output.contains(&format!("Type {phrase}")),
        "{}",
        rejected.output
    );
    assert!(
        rejected
            .output
            .contains("deliberate typed confirmation at a terminal; not proof of a human"),
        "{}",
        rejected.output
    );
    assert!(
        rejected.output.contains("release was not confirmed"),
        "{}",
        rejected.output
    );
    assert!(authorizations(dir.path()).is_empty());

    let accepted = authorize_at_tty(dir.path(), &fake, None, &head);
    assert!(
        accepted.success,
        "code {} {}",
        accepted.code, accepted.output
    );
    let signed = data_of(dir.path(), &["release", "status", "--json"]);
    assert_eq!(signed["authorization"]["commit_sha"], json!(head));
    assert_eq!(signed["authorization"]["method"], json!("tty-typed"));
    assert!(signed["authorization"]["evidence"].is_null(), "{signed}");
    assert_eq!(authorizations(dir.path()), vec![head]);
}

#[test]
fn release_authorize_rejects_expired_challenge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = release_reviewed(dir.path(), &fake);
    let expired = answer_prompts_in(
        dir.path(),
        &["release", "authorize", "--sha", &head],
        &[PromptAnswer::Phrase],
        &[("RELAYBOOK_SIGNOFF_TIMEOUT_SECS", "0")],
        Duration::from_secs(1),
    );
    assert!(!expired.success, "{}", expired.output);
    assert!(
        expired.output.contains("confirmation challenge expired"),
        "{}",
        expired.output
    );
    assert!(
        !expired.output.contains("release was not confirmed"),
        "{}",
        expired.output
    );
    assert!(authorizations(dir.path()).is_empty());
}

#[test]
fn release_authorize_requires_stdout_tty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = release_reviewed(dir.path(), &fake);
    let shell = posix_shell();
    let probe = pty_run(
        dir.path(),
        &shell,
        &[
            "-c",
            "if [ -t 0 ]; then echo stdin-tty; else echo stdin-not; fi",
        ],
    );
    assert!(
        probe.success && probe.output.contains("stdin-tty"),
        "code {} {}",
        probe.code,
        probe.output
    );

    let logs = tempfile::tempdir().expect("logs");
    let stdout_log = logs.path().join("stdout.txt");
    let stderr_log = logs.path().join("stderr.txt");
    let run = pty_run(
        dir.path(),
        &shell,
        &[
            "-c",
            "\"$1\" release authorize --sha \"$2\" > \"$3\" 2> \"$4\"",
            "sh",
            env!("CARGO_BIN_EXE_relaybook"),
            &head,
            stdout_log.to_str().expect("utf8 stdout log"),
            stderr_log.to_str().expect("utf8 stderr log"),
        ],
    );
    let stderr = fs::read_to_string(&stderr_log).unwrap_or_default();
    assert!(!run.success, "code {} {}\n{stderr}", run.code, run.output);
    assert!(
        stderr.contains("release authorize requires confirmation at a TTY"),
        "pty {}\nstderr {stderr}",
        run.output
    );
    assert!(authorizations(dir.path()).is_empty());
}

#[test]
fn claude_override_and_leaving_gated_use_typed_challenge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = prepare_gated_review(dir.path(), &fake);
    let phrase = format!("claude {}", &head[..12]);
    let rejected = answer_prompts(
        dir.path(),
        &[
            "review",
            "record",
            "auth",
            "--by",
            "claude",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
        ],
        &[PromptAnswer::Text("y\n")],
    );
    assert!(!rejected.success, "{}", rejected.output);
    assert!(
        rejected.output.contains("Record this review as claude?"),
        "{}",
        rejected.output
    );
    assert!(
        rejected.output.contains(&format!("Type {phrase}")),
        "{}",
        rejected.output
    );
    assert!(
        rejected.output.contains("claude review was not confirmed"),
        "{}",
        rejected.output
    );
    assert_eq!(task_reviews(dir.path())[0].1, None);

    let accepted = answer_prompts(
        dir.path(),
        &[
            "review",
            "record",
            "auth",
            "--by",
            "claude",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
        ],
        &[PromptAnswer::Phrase],
    );
    assert!(accepted.success, "{}", accepted.output);
    let reviews = task_reviews(dir.path());
    assert_eq!(reviews[0].0, "claude");
    assert_eq!(reviews[0].1.as_deref(), Some("approved"));
    assert_eq!(reviews[0].2, 1);

    let gated = tempfile::tempdir().expect("tempdir");
    git_repo(gated.path());
    run(gated.path(), ["init"]).assert().success();
    run(gated.path(), ["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    run(gated.path(), ["config", "set", "workflow", "gated"])
        .assert()
        .success();
    let tip = git(gated.path(), &["rev-parse", "HEAD"]);
    let leave = format!("classic {}", &tip[..12]);
    let declined = answer_prompts(
        gated.path(),
        &["config", "set", "workflow", "classic"],
        &[PromptAnswer::Text("y\n")],
    );
    assert!(!declined.success, "{}", declined.output);
    assert!(
        declined.output.contains("Leave gated workflow?"),
        "{}",
        declined.output
    );
    assert!(
        declined.output.contains(&format!("Type {leave}")),
        "{}",
        declined.output
    );
    assert!(
        declined
            .output
            .contains("leaving workflow=gated was not confirmed"),
        "{}",
        declined.output
    );
    assert_eq!(
        json_data(gated.path(), ["config", "--json"])["workflow"],
        json!("gated")
    );
    let switched = answer_prompts(
        gated.path(),
        &["config", "set", "workflow", "classic"],
        &[PromptAnswer::Phrase],
    );
    assert!(switched.success, "{}", switched.output);
    assert_eq!(
        json_data(gated.path(), ["config", "--json"])["workflow"],
        json!("classic")
    );
}

#[test]
fn signed_tag_mode_rejects_missing_unsigned_or_wrong_commit_tag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = release_reviewed(dir.path(), &fake);
    let denied = command(dir.path())
        .stdin(Stdio::null())
        .args(["config", "set", "signoff", "signed-tag"])
        .output()
        .expect("output");
    assert_failure(&denied, "changing signoff requires confirmation at a TTY");
    assert_eq!(
        json_data(dir.path(), ["config", "--json"])["signoff"],
        json!("typed")
    );
    run(dir.path(), ["config", "set", "signoff", "sometimes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid signoff: sometimes"));

    let typed = answer_prompts(
        dir.path(),
        &["config", "set", "signoff", "signed-tag"],
        &[PromptAnswer::Text("y\n")],
    );
    assert!(!typed.success, "{}", typed.output);
    assert!(
        typed.output.contains("signoff change was not confirmed"),
        "{}",
        typed.output
    );
    let phrase = format!("signoff {}", &head[..12]);
    assert!(
        typed.output.contains(&format!("Type {phrase}")),
        "{}",
        typed.output
    );
    let switched = answer_prompts(
        dir.path(),
        &["config", "set", "signoff", "signed-tag"],
        &[PromptAnswer::Phrase],
    );
    assert!(switched.success, "{}", switched.output);
    command(dir.path())
        .stdin(Stdio::null())
        .args(["config", "set", "signoff", "signed-tag"])
        .assert()
        .success()
        .stdout("signoff signed-tag\n");

    let missing = answer_prompts(
        dir.path(),
        &["release", "authorize", "--sha", &head],
        &[PromptAnswer::Phrase],
    );
    assert!(!missing.success, "{}", missing.output);
    assert!(missing.output.contains("is missing"), "{}", missing.output);
    assert!(authorizations(dir.path()).is_empty());

    git(
        dir.path(),
        &["config", "user.email", "relaybook@example.com"],
    );
    git(dir.path(), &["config", "user.name", "Relaybook"]);
    let tag = format!("relaybook-signoff/{}", &head[..12]);
    git(
        dir.path(),
        &[
            "-c",
            "tag.gpgsign=false",
            "tag",
            "-a",
            "-m",
            "unsigned",
            &tag,
        ],
    );
    let unsigned = answer_prompts(
        dir.path(),
        &["release", "authorize", "--sha", &head],
        &[PromptAnswer::Phrase],
    );
    assert!(!unsigned.success, "{}", unsigned.output);
    assert!(
        unsigned.output.contains("is not a valid signature"),
        "{}",
        unsigned.output
    );
    assert!(authorizations(dir.path()).is_empty());

    git(dir.path(), &["tag", "-d", &tag]);
    let parent = git(dir.path(), &["rev-parse", "HEAD^"]);
    git(
        dir.path(),
        &[
            "-c",
            "tag.gpgsign=false",
            "tag",
            "-a",
            "-m",
            "wrong",
            &tag,
            &parent,
        ],
    );
    let wrong = answer_prompts(
        dir.path(),
        &["release", "authorize", "--sha", &head],
        &[PromptAnswer::Phrase],
    );
    assert!(!wrong.success, "{}", wrong.output);
    assert!(
        wrong.output.contains("does not point at HEAD"),
        "{}",
        wrong.output
    );
    assert!(authorizations(dir.path()).is_empty());
}

#[test]
fn signed_tag_mode_accepts_valid_signature() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = release_reviewed(dir.path(), &fake);
    let _keys = install_ssh_signing(dir.path());
    let switched = answer_prompts(
        dir.path(),
        &["config", "set", "signoff", "signed-tag"],
        &[PromptAnswer::Phrase],
    );
    assert!(switched.success, "{}", switched.output);
    let tag = format!("relaybook-signoff/{}", &head[..12]);
    git(dir.path(), &["tag", "-s", "-m", "signoff", &tag]);
    let tags = git(dir.path(), &["tag", "--list"]);
    let authorized = authorize_at_tty(dir.path(), &fake, None, &head);
    assert!(
        authorized.success,
        "code {} {}",
        authorized.code, authorized.output
    );
    assert_eq!(git(dir.path(), &["tag", "--list"]), tags);
    let object = git(dir.path(), &["rev-parse", &format!("refs/tags/{tag}")]);
    let signed = data_of(dir.path(), &["release", "status", "--json"]);
    assert_eq!(
        signed["authorization"]["method"],
        json!("tty-typed+signed-tag")
    );
    let evidence = signed["authorization"]["evidence"]
        .as_str()
        .expect("evidence");
    assert!(evidence.contains(&format!("tag {tag}")), "{evidence}");
    assert!(evidence.contains(&format!("object {object}")), "{evidence}");
    assert!(evidence.contains("verifier "), "{evidence}");
    assert!(evidence.contains("Good"), "{evidence}");
    assert_eq!(authorizations(dir.path()), vec![head]);
}

#[test]
fn signed_tag_replaced_between_commands_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    let head = release_reviewed(dir.path(), &fake);
    let _keys = install_ssh_signing(dir.path());
    let switched = answer_prompts(
        dir.path(),
        &["config", "set", "signoff", "signed-tag"],
        &[PromptAnswer::Phrase],
    );
    assert!(switched.success, "{}", switched.output);
    let tag = format!("relaybook-signoff/{}", &head[..12]);
    git(
        dir.path(),
        &[
            "-c",
            "tag.gpgsign=false",
            "tag",
            "-a",
            "-m",
            "unsigned",
            &tag,
        ],
    );
    let parent = git(dir.path(), &["rev-parse", "HEAD^"]);
    let replaced = authorize_and_replace_signoff_tag(dir.path(), &fake, &head, &tag, &parent);
    assert!(!replaced.success, "{}", replaced.output);
    assert!(
        replaced.output.contains("is not a valid signature"),
        "{}",
        replaced.output
    );
    assert!(authorizations(dir.path()).is_empty());
}

#[test]
fn gated_manual_flow_never_pushes_or_calls_gh() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake = GraphifyFake::install();
    install_gh(&fake);
    let log = fake.outside("agent-log");
    fs::create_dir_all(&log).expect("log");
    seed_graph_task(dir.path());
    let _origin = add_origin(dir.path());
    git(dir.path(), &["checkout", "-b", "feature"]);

    logged_ok(
        dir.path(),
        &fake,
        &log,
        &["graph", "refresh", "--step", "plan"],
    );
    logged_ok(
        dir.path(),
        &fake,
        &log,
        &["task", "claim", "auth", "--by", "cursor"],
    );
    fs::write(dir.path().join("work.txt"), "work\n").expect("work");
    git(dir.path(), &["add", "work.txt"]);
    commit_index(dir.path(), "do the work");
    logged_ok(
        dir.path(),
        &fake,
        &log,
        &["task", "done", "auth", "--changed", "shipped"],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    logged_ok(dir.path(), &fake, &log, &["review", "start", "auth"]);
    logged_ok(
        dir.path(),
        &fake,
        &log,
        &[
            "review",
            "record",
            "auth",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
        ],
    );
    logged_ok(dir.path(), &fake, &log, &["report", "accept", "auth"]);
    logged_ok(dir.path(), &fake, &log, &["review", "start", "--release"]);
    logged_ok(
        dir.path(),
        &fake,
        &log,
        &[
            "review",
            "record",
            "--release",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            RELEASE_REVIEW_TEXT,
        ],
    );
    let authorized = authorize_at_tty(dir.path(), &fake, Some(&log), &head);
    assert!(
        authorized.success,
        "code {} {}",
        authorized.code, authorized.output
    );
    let status = logged_json(dir.path(), &fake, &log, &["release", "status", "--json"]);
    assert_eq!(status["release"], json!("signed-off"));
    assert_eq!(status["authorization"]["commit_sha"], json!(head));
    assert_eq!(status["commit_sha"], json!(head));
    assert!(!log.join("gh.args").exists(), "gh was invoked");
    assert!(
        rev_missing(dir.path(), "refs/remotes/origin/feature"),
        "feature was published"
    );
    let origin_main = git(dir.path(), &["rev-parse", "refs/remotes/origin/main"]);
    assert!(!origin_main.is_empty());
    assert!(!fake.log_text().contains("extract"));
}

#[test]
fn install_is_idempotent_and_keeps_existing_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("CLAUDE.md");
    fs::write(&path, "hello\n").expect("write");

    run(dir.path(), ["install", "--claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated CLAUDE.md"));

    let first = fs::read(&path).expect("read");
    let text = String::from_utf8(first.clone()).expect("utf8");
    assert!(text.starts_with("hello\n"), "{text}");
    assert_eq!(text.matches("<!-- relaybook:start -->").count(), 1);
    assert!(text.contains("## Relaybook coordination (planner)"));

    run(dir.path(), ["install", "--claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("unchanged CLAUDE.md"));
    assert_eq!(fs::read(&path).expect("read"), first);
}

#[test]
fn install_leaves_an_existing_heading_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("CLAUDE.md");
    let original = "notes\n## Relaybook coordination (planner)\ncustom\n";
    fs::write(&path, original).expect("write");

    run(dir.path(), ["install", "--claude"]).assert().success();
    assert_eq!(fs::read_to_string(&path).expect("read"), original);
    assert!(!original.contains("<!-- relaybook:start -->"));
}

fn commit_gated_work(dir: &Path, fake: &GraphifyFake) -> String {
    seed_graph_task(dir);
    fake_data(
        dir,
        fake,
        "graph",
        ["task", "claim", "auth", "--by", "cursor", "--json"],
    );
    fs::write(dir.join("work.txt"), "work\n").expect("work");
    git(dir, &["add", "work.txt"]);
    commit_index(dir, "do the work");
    fake_data(
        dir,
        fake,
        "graph",
        ["task", "done", "auth", "--changed", "shipped", "--json"],
    );
    git(dir, &["rev-parse", "HEAD"])
}

fn prepare_gated_review(dir: &Path, fake: &GraphifyFake) -> String {
    let head = commit_gated_work(dir, fake);
    fake_data(dir, fake, "graph", ["review", "start", "auth", "--json"]);
    head
}

fn record_review(dir: &Path, by: &str, sha: &str, verdict: &str, json: bool) -> Output {
    let mut args = vec![
        "review",
        "record",
        "auth",
        "--by",
        by,
        "--sha",
        sha,
        "--verdict",
        verdict,
        "--text",
        TASK_REVIEW_TEXT,
    ];
    if json {
        args.push("--json");
    }
    relay_output(dir, &args)
}

fn relay_output(dir: &Path, args: &[&str]) -> Output {
    command(dir).args(args).output().expect("output")
}

fn fake_output(dir: &Path, fake: &GraphifyFake, kind: &str, args: &[&str]) -> Output {
    let mut cmd = command(dir);
    fake.apply(&mut cmd, kind);
    cmd.args(args).output().expect("output")
}

fn data_of(dir: &Path, args: &[&str]) -> Value {
    let output = relay_output(dir, args);
    assert!(output.status.success(), "{}", summarize(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert!(envelope["error"].is_null(), "{envelope}");
    envelope["data"].clone()
}

fn assert_failure(output: &Output, message: &str) {
    assert!(!output.status.success(), "{}", summarize(output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(message),
        "missing {message} in {}",
        summarize(output)
    );
}

fn assert_json_failure(output: &Output, message: &str) {
    assert!(!output.status.success(), "{}", summarize(output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: Value = serde_json::from_str(&stdout).expect("json");
    let error = envelope["error"].as_str().unwrap_or("");
    assert!(
        error.contains(message),
        "missing {message} in {error}; {}",
        summarize(output)
    );
    assert_eq!(envelope["ok"], json!(false), "{envelope}");
}

fn task_reviews(dir: &Path) -> Vec<(String, Option<String>, i64, String)> {
    let db = dir.join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(db).expect("open");
    let mut statement = conn
        .prepare(
            "SELECT reviewer, verdict, reviewer_override, commit_sha
             FROM reviews
             WHERE scope = 'task'
             ORDER BY id",
        )
        .expect("prepare");
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .expect("rows");
    rows.map(|row| row.expect("row")).collect()
}

fn task_report_verdict(dir: &Path) -> Option<String> {
    let db = dir.join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.query_row(
        "SELECT verdict FROM reports WHERE kind = 'task' ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get(0),
    )
    .expect("verdict")
}

#[test]
fn codex_model_sets_and_gets_without_terminal_confirmation() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["config", "get", "codex_model"])
        .assert()
        .success()
        .stdout("codex_model gpt-6.1-sol\n");
    run(dir.path(), ["config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("codex_model gpt-6.1-sol"));
    run(
        dir.path(),
        ["config", "set", "codex_model", " gpt-6.2-sol "],
    )
    .assert()
    .success()
    .stdout("codex_model gpt-6.2-sol\n");
    run(dir.path(), ["config", "get", "codex_model"])
        .assert()
        .success()
        .stdout("codex_model gpt-6.2-sol\n");
    run(dir.path(), ["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    run(dir.path(), ["config", "set", "workflow", "gated"])
        .assert()
        .success();
    run(
        dir.path(),
        ["config", "set", "codex_model", "openai:gpt-6.1-sol"],
    )
    .assert()
    .success()
    .stdout("codex_model openai:gpt-6.1-sol\n");
    run(dir.path(), ["config", "set", "codex_model", "-m"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid codex_model"));
    run(
        dir.path(),
        [
            "config",
            "set",
            "codex_model",
            "--dangerously-bypass-approvals-and-sandbox",
        ],
    )
    .assert()
    .failure()
    .stderr(predicate::str::contains("invalid codex_model"));
    run(dir.path(), ["config", "set", "codex_model", "gpt 6"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid codex_model"));
    run(dir.path(), ["config", "get", "codex_model"])
        .assert()
        .success()
        .stdout("codex_model openai:gpt-6.1-sol\n");
    run(dir.path(), ["config", "get", "not_a_setting"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown setting: not_a_setting"));
}

fn run(dir: &Path, args: impl IntoIterator<Item = &'static str>) -> Command {
    let mut cmd = command(dir);
    cmd.args(args);
    cmd
}

fn command(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("relaybook").expect("relaybook binary");
    cmd.current_dir(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    cmd
}

fn spawn_claim(dir: &Path, by: &str) -> std::process::Child {
    command(dir)
        .args(["task", "claim", "auth", "--by", by])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn")
}

fn json_data(dir: &Path, args: impl IntoIterator<Item = &'static str>) -> Value {
    let output = run(dir, args).output().expect("output");
    assert!(output.status.success(), "{}", summarize(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert!(envelope["error"].is_null(), "{envelope}");
    envelope["data"].clone()
}

fn event_rows(dir: &Path) -> Vec<(i64, String, Value)> {
    let data = json_data(dir, ["context", "--all", "--json"]);
    let mut rows: Vec<(i64, String, Value)> = data["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|event| {
            (
                event["id"].as_i64().expect("id"),
                event["kind"].as_str().expect("kind").to_string(),
                event["payload"].clone(),
            )
        })
        .collect();
    rows.sort_by_key(|row| row.0);
    rows
}

fn seed_gated_task(dir: &Path) {
    git_repo(dir);
    run(dir, ["init"]).assert().success();
    run(dir, ["task", "create", "auth", "--title", "Add auth"])
        .assert()
        .success();
    run(dir, ["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    run(dir, ["config", "set", "workflow", "gated"])
        .assert()
        .success();
}

fn commit_worktree(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    commit_index(dir, message);
}

fn commit_index(dir: &Path, message: &str) {
    git(
        dir,
        &[
            "-c",
            "user.email=relaybook@example.com",
            "-c",
            "user.name=Relaybook",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            message,
        ],
    );
}

fn task_status(dir: &Path) -> String {
    let data = json_data(dir, ["context", "auth", "--json"]);
    data["tasks"][0]["status"]
        .as_str()
        .expect("status")
        .to_string()
}

fn report_count(dir: &Path) -> i64 {
    let db = dir.join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.query_row("SELECT COUNT(*) FROM reports", [], |row| row.get(0))
        .expect("count")
}

fn git_stdout(dir: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[cfg(unix)]
fn assert_git_reports_literal(dir: &Path, name: &str) {
    let status = git_stdout(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    );
    let needle = name.as_bytes();
    assert!(
        status.windows(needle.len()).any(|window| window == needle),
        "git status did not report {name:?}: {}",
        String::from_utf8_lossy(&status)
    );
}

/// Pause each `git status` from a gated command until this test can see
/// whether that call holds the ledger write lock. The mutation then happens
/// before `git` reads the worktree, so only a recheck inside the transaction
/// observes it. The helper is portable: it does not read `/proc`.
fn during_write_lock(dir: &Path, args: &[&str], mutate: impl FnOnce()) -> std::process::Output {
    during_git_recheck(dir, args, "status", mutate)
}

/// Same as [`during_write_lock`], pausing `git_arg` instead of `status`.
/// `diff` is the fingerprint read. Pause files live outside the worktree so
/// git does not report them as untracked before the in-lock recheck.
fn during_git_recheck(
    dir: &Path,
    args: &[&str],
    git_arg: &str,
    mutate: impl FnOnce(),
) -> std::process::Output {
    static GATES: AtomicU64 = AtomicU64::new(1);
    let root = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let gate = root.join(format!(
        "relaybook-git-gate-{}-{}",
        std::process::id(),
        GATES.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&gate).expect("gate");
    let db = dir.join(".relaybook").join("ledger.db");
    let mut child = PausedGit::spawn(dir, &gate, git_arg, args);
    let n = child.wait_until_git_holds_lock(&db, &gate, git_arg);
    mutate();
    fs::write(gate.join(format!("release-{n}")), b"1").expect("release");
    child.output()
}

struct PausedGit(Option<Child>);

impl Drop for PausedGit {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl PausedGit {
    fn spawn(dir: &Path, gate: &Path, git_arg: &str, args: &[&str]) -> Self {
        let child = command_with_git_shim(dir, gate, git_arg)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        Self(Some(child))
    }

    fn child(&mut self) -> &mut Child {
        self.0.as_mut().expect("child")
    }

    fn output(&mut self) -> std::process::Output {
        self.0
            .take()
            .expect("child")
            .wait_with_output()
            .expect("wait")
    }

    fn wait_until_git_holds_lock(&mut self, db: &Path, gate: &Path, git_arg: &str) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut n = 1u32;
        loop {
            if !gate.join(format!("entered-{n}")).exists() {
                if let Some(status) = self.child().try_wait().expect("poll") {
                    let output = self.output();
                    panic!(
                        "exited before an in-lock git {git_arg} ({status}): {}",
                        summarize(&output)
                    );
                }
                if Instant::now() >= deadline {
                    panic!(
                        "no git {git_arg} ran inside the ledger write lock (waiting for {git_arg} {n})"
                    );
                }
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            if write_lock_held(db) {
                return n;
            }
            fs::write(gate.join(format!("release-{n}")), b"1").expect("release outside lock");
            n += 1;
            if n > 32 {
                panic!("git {git_arg} kept running outside the ledger write lock");
            }
        }
    }
}

fn write_lock_held(db: &Path) -> bool {
    let conn = rusqlite::Connection::open(db).expect("probe");
    conn.busy_timeout(Duration::ZERO).expect("busy");
    match conn.execute_batch("BEGIN IMMEDIATE") {
        Ok(()) => {
            conn.execute_batch("ROLLBACK").expect("rollback probe");
            false
        }
        Err(rusqlite::Error::SqliteFailure(err, _))
            if err.code == rusqlite::ErrorCode::DatabaseBusy =>
        {
            true
        }
        Err(err) => panic!("lock probe failed: {err}"),
    }
}

fn command_with_git_shim(dir: &Path, gate: &Path, git_arg: &str) -> Command {
    let shim_dir = git_shim_dir();
    let original = std::env::var_os("PATH").expect("PATH");
    let mut path = shim_dir.as_os_str().to_os_string();
    path.push(if cfg!(windows) { ";" } else { ":" });
    path.push(&original);
    let mut cmd = command(dir);
    cmd.env("PATH", path)
        .env("RELAYBOOK_GIT_GATE", gate)
        .env("RELAYBOOK_GIT_GATE_CMD", git_arg)
        .env("RELAYBOOK_REAL_GIT", real_git())
        .env("RELAYBOOK_REAL_PATH", original);
    cmd
}

fn git_shim_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let root = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = root.join(format!("relaybook-git-shim-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("shim dir");
        let binary = dir.join(if cfg!(windows) { "git.exe" } else { "git" });
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/git_shim.rs");
        let output = Command::new("rustc")
            .arg("--edition=2021")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("run rustc");
        assert!(
            output.status.success(),
            "rustc git shim failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        dir
    })
    .clone()
}

fn real_git() -> PathBuf {
    static GIT: OnceLock<PathBuf> = OnceLock::new();
    GIT.get_or_init(find_real_git).clone()
}

fn find_real_git() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    let skip = std::env::var_os("CARGO_TARGET_TMPDIR").map(PathBuf::from);
    let names: &[&str] = if cfg!(windows) {
        &["git.exe"]
    } else {
        &["git"]
    };
    for dir in std::env::split_paths(&path) {
        if skip.as_ref().is_some_and(|root| dir.starts_with(root)) {
            continue;
        }
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    panic!("git is not on PATH");
}

fn git_repo(dir: &Path) {
    git(dir, &["init", "-b", "main"]);
    fs::write(dir.join("README.md"), "seed\n").expect("write");
    git(dir, &["add", "README.md"]);
    git(
        dir,
        &[
            "-c",
            "user.email=relaybook@example.com",
            "-c",
            "user.name=Relaybook",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "seed",
        ],
    );
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

fn report_shas(db: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.query_row(
        "SELECT commit_sha, base_sha, graph_sha FROM reports WHERE kind = 'task'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .expect("report")
}

fn summarize(output: &std::process::Output) -> String {
    format!(
        "status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn seed_graph_task(dir: &Path) {
    git_repo(dir);
    run(dir, ["init"]).assert().success();
    run(dir, ["task", "create", "auth", "--title", "Add auth"])
        .assert()
        .success();
    run(dir, ["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    run(dir, ["config", "set", "workflow", "gated"])
        .assert()
        .success();
    fs::write(dir.join(".gitignore"), ".relaybook/\ngraphify-out/\n").expect("gitignore");
    git(dir, &["add", ".gitignore"]);
    commit_index(dir, "ignore graph output");
}

struct GraphifyFake {
    _root: tempfile::TempDir,
    path: OsString,
    log: PathBuf,
}

impl GraphifyFake {
    fn install() -> Self {
        let root = tempfile::tempdir().expect("fake root");
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).expect("bin");
        let dest = bin.join(if cfg!(windows) {
            "graphify.exe"
        } else {
            "graphify"
        });
        fs::copy(env!("CARGO_BIN_EXE_fake"), &dest).expect("copy fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&dest).expect("meta").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&dest, permissions).expect("chmod");
        }
        let mut path = bin.into_os_string();
        path.push(if cfg!(windows) { ";" } else { ":" });
        if let Some(rest) = std::env::var_os("PATH") {
            path.push(rest);
        }
        let log = root.path().join("graphify.log");
        Self {
            _root: root,
            path,
            log,
        }
    }

    fn apply(&self, cmd: &mut Command, kind: &str) {
        cmd.env("PATH", &self.path);
        cmd.env("GRAPHIFY_LOG", &self.log);
        cmd.env("RELAYBOOK_FAKE_GRAPHIFY", kind);
    }

    fn log_text(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn outside(&self, name: &str) -> PathBuf {
        self._root.path().join(name)
    }
}

fn fake_data_cwd(
    dir: &Path,
    fake: &GraphifyFake,
    kind: &str,
    cwd_log: &Path,
    args: impl IntoIterator<Item = &'static str>,
) -> Value {
    let mut cmd = command(dir);
    fake.apply(&mut cmd, kind);
    cmd.env("GRAPHIFY_CWD_LOG", cwd_log);
    let output = cmd.args(args).output().expect("output");
    assert!(output.status.success(), "{}", summarize(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert!(envelope["error"].is_null(), "{envelope}");
    envelope["data"].clone()
}

fn fake_data(
    dir: &Path,
    fake: &GraphifyFake,
    kind: &str,
    args: impl IntoIterator<Item = &'static str>,
) -> Value {
    let mut cmd = command(dir);
    fake.apply(&mut cmd, kind);
    let output = cmd.args(args).output().expect("output");
    assert!(output.status.success(), "{}", summarize(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert!(envelope["error"].is_null(), "{envelope}");
    envelope["data"].clone()
}

fn json_without_graphify(dir: &Path, args: impl IntoIterator<Item = &'static str>) -> Value {
    let path = std::env::var_os("PATH").unwrap_or_default();
    json_on_path_without_graphify(dir, &path, args)
}

fn json_on_path_without_graphify(
    dir: &Path,
    path: &std::ffi::OsStr,
    args: impl IntoIterator<Item = &'static str>,
) -> Value {
    let output = command(dir)
        .env("PATH", path_without_graphify(path, dir))
        .args(args)
        .output()
        .expect("output");
    assert!(output.status.success(), "{}", summarize(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    envelope["data"].clone()
}

/// Drop every PATH entry for which production `which::which("graphify")` would succeed.
#[cfg(windows)]
fn json_with_path(
    dir: &Path,
    path: &std::ffi::OsStr,
    args: impl IntoIterator<Item = &'static str>,
) -> Value {
    let output = command(dir)
        .env("PATH", path)
        .args(args)
        .output()
        .expect("output");
    assert!(output.status.success(), "{}", summarize(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    envelope["data"].clone()
}

struct PlantedAgent {
    _root: tempfile::TempDir,
    bin: PathBuf,
    path: OsString,
    log: PathBuf,
}

fn classic_auto_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git_repo(dir.path());
    git(
        dir.path(),
        &["config", "user.email", "relaybook@example.com"],
    );
    git(dir.path(), &["config", "user.name", "Relaybook"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["mode", "auto"]).assert().success();
    run(dir.path(), ["config", "set", "gates", "none"])
        .assert()
        .success();
    fs::write(
        dir.path().join("prd.md"),
        "Build a tiny greeting command.\n",
    )
    .expect("prd");
    git(dir.path(), &["add", "prd.md"]);
    commit_index(dir.path(), "add prd");
    dir
}

fn plant_agent(cwd: &Path, names_cursor: bool) -> PlantedAgent {
    let root = tempfile::tempdir().expect("agent bin");
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).expect("bin");
    copy_named_fake(&bin, "claude");
    copy_named_fake(&bin, "gh");
    if names_cursor {
        copy_named_fake(&bin, "agent");
    } else {
        write_other_agent(&bin);
    }
    let mut path = bin.as_os_str().to_os_string();
    let rest = path_hiding(&["cursor-agent", "agent"], cwd);
    if !rest.is_empty() {
        path.push(if cfg!(windows) { ";" } else { ":" });
        path.push(rest);
    }
    let log = root.path().join("log");
    fs::create_dir_all(&log).expect("log");
    PlantedAgent {
        _root: root,
        bin,
        path,
        log,
    }
}

fn copy_named_fake(dir: &Path, name: &str) {
    let dest = dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    fs::copy(env!("CARGO_BIN_EXE_fake"), &dest).expect("copy fake");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&dest).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&dest, permissions).expect("chmod");
    }
}

fn write_other_agent(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dest = dir.join("agent");
        fs::write(&dest, "#!/bin/sh\necho \"grok 1.0\"\nexit 0\n").expect("agent");
        let mut permissions = fs::metadata(&dest).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&dest, permissions).expect("chmod");
    }
    #[cfg(windows)]
    {
        fs::write(
            dir.join("agent.cmd"),
            "@echo off\r\necho grok 1.0\r\nexit /b 0\r\n",
        )
        .expect("agent");
    }
}

fn path_hiding(names: &[&str], cwd: &Path) -> OsString {
    let mut filtered = OsString::new();
    let mut first = true;
    let raw = std::env::var_os("PATH").unwrap_or_default();
    for entry in std::env::split_paths(&raw) {
        if names
            .iter()
            .any(|name| which::which_in(name, Some(entry.as_os_str()), cwd).is_ok())
        {
            continue;
        }
        if !first {
            filtered.push(if cfg!(windows) { ";" } else { ":" });
        }
        first = false;
        filtered.push(&entry);
    }
    filtered
}

fn run_with_agent(dir: &Path, agent: &PlantedAgent) -> Output {
    let mut cmd = command(dir);
    cmd.env("PATH", &agent.path);
    cmd.env("LOG", &agent.log);
    cmd.env("RELAYBOOK_FAKE_KIND", "happy");
    cmd.env("RELAYBOOK", env!("CARGO_BIN_EXE_relaybook"));
    cmd.env_remove("ANTHROPIC_API_KEY");
    cmd.args(["run", "--prd", "prd.md"]);
    cmd.output().expect("run")
}

fn path_without_graphify(path: &std::ffi::OsStr, cwd: &Path) -> OsString {
    let mut filtered = OsString::new();
    let mut first = true;
    for entry in std::env::split_paths(path) {
        if which::which_in("graphify", Some(entry.as_os_str()), cwd).is_ok() {
            continue;
        }
        if !first {
            filtered.push(if cfg!(windows) { ";" } else { ":" });
        }
        first = false;
        filtered.push(&entry);
    }
    filtered
}

fn graph_builds(dir: &Path) -> Vec<(String, String, String)> {
    let db = dir.join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(db).expect("open");
    let mut statement = conn
        .prepare("SELECT step, commit_sha, status FROM graph_builds ORDER BY id")
        .expect("prepare");
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query");
    rows.collect::<Result<Vec<_>, _>>().expect("rows")
}

fn finish_task(dir: &Path, fake: &GraphifyFake, slug: &'static str, file: &'static str) -> String {
    fake_data(
        dir,
        fake,
        "graph",
        ["task", "claim", slug, "--by", "cursor", "--json"],
    );
    fs::write(dir.join(file), "work\n").expect("work");
    git(dir, &["add", file]);
    commit_index(dir, slug);
    let done = fake_data(
        dir,
        fake,
        "graph",
        ["task", "done", slug, "--changed", "shipped", "--json"],
    );
    let head = git(dir, &["rev-parse", "HEAD"]);
    assert_eq!(done["commit_sha"], json!(head));
    head
}

fn accept_task(dir: &Path, fake: &GraphifyFake, slug: &'static str, head: &str) {
    fake_data(dir, fake, "graph", ["review", "start", slug, "--json"]);
    let recorded = data_of(
        dir,
        &[
            "review",
            "record",
            slug,
            "--by",
            "codex",
            "--sha",
            head,
            "--verdict",
            "approved",
            "--text",
            TASK_REVIEW_TEXT,
            "--json",
        ],
    );
    assert_eq!(recorded["verdict"], json!("approved"));
    run(dir, ["report", "accept", slug]).assert().success();
}

fn release_reviewed(dir: &Path, fake: &GraphifyFake) -> String {
    seed_graph_task(dir);
    git(dir, &["checkout", "-b", "feature"]);
    let head = finish_task(dir, fake, "auth", "work.txt");
    accept_task(dir, fake, "auth", &head);
    fake_data(
        dir,
        fake,
        "graph",
        ["review", "start", "--release", "--json"],
    );
    let recorded = data_of(
        dir,
        &[
            "review",
            "record",
            "--release",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            RELEASE_REVIEW_TEXT,
            "--json",
        ],
    );
    assert_eq!(recorded["verdict"], json!("approved"));
    head
}

fn record_release(dir: &Path, by: &str, sha: &str, verdict: &str, text: &str) -> Output {
    relay_output(
        dir,
        &[
            "review",
            "record",
            "--release",
            "--by",
            by,
            "--sha",
            sha,
            "--verdict",
            verdict,
            "--text",
            text,
        ],
    )
}

fn release_reviews(dir: &Path) -> Vec<(Option<String>, String)> {
    let db = dir.join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(db).expect("open");
    let mut statement = conn
        .prepare(
            "SELECT verdict, commit_sha FROM reviews
             WHERE scope = 'release'
             ORDER BY id",
        )
        .expect("prepare");
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("rows");
    rows.map(|row| row.expect("row")).collect()
}

fn authorizations(dir: &Path) -> Vec<String> {
    let db = dir.join(".relaybook").join("ledger.db");
    if !db.is_file() {
        return Vec::new();
    }
    let conn = rusqlite::Connection::open(db).expect("open");
    let mut statement = conn
        .prepare("SELECT commit_sha FROM release_authorizations ORDER BY id")
        .expect("prepare");
    let rows = statement.query_map([], |row| row.get(0)).expect("rows");
    rows.map(|row| row.expect("row")).collect()
}

fn task_phase(tasks: &Value, slug: &str) -> String {
    tasks
        .as_array()
        .expect("tasks")
        .iter()
        .find(|task| task["slug"] == slug)
        .unwrap_or_else(|| panic!("missing {slug} in {tasks}"))["phase"]
        .as_str()
        .expect("phase")
        .to_string()
}

fn add_origin(dir: &Path) -> tempfile::TempDir {
    let origin = tempfile::tempdir().expect("origin");
    let path = origin.path().to_str().expect("utf8 origin");
    git(dir, &["init", "--bare", "-b", "main", path]);
    git(dir, &["remote", "add", "origin", path]);
    git(dir, &["push", "origin", "main"]);
    origin
}

fn bare_remote(dir: &Path, name: &str) -> tempfile::TempDir {
    let origin = tempfile::tempdir().expect("remote");
    let path = origin.path().to_str().expect("utf8 remote");
    git(dir, &["init", "--bare", "-b", "main", path]);
    git(dir, &["remote", "add", name, path]);
    origin
}

fn delete_ref_if_present(dir: &Path, rev: &str) {
    if !rev_missing(dir, rev) {
        git(dir, &["update-ref", "-d", rev]);
    }
}

fn remote_detail(dir: &Path) -> String {
    let db = dir.join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.query_row(
        "SELECT detail FROM remote_checks ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get::<_, Option<String>>(0),
    )
    .expect("detail")
    .unwrap_or_default()
}

fn latest_remote_override(dir: &Path) -> i64 {
    let db = dir.join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.query_row(
        "SELECT override FROM remote_checks ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get(0),
    )
    .expect("override")
}

fn latest_remote_status(dir: &Path) -> String {
    let db = dir.join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.query_row(
        "SELECT status FROM remote_checks ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get(0),
    )
    .expect("status")
}

struct RemoteCheckRow {
    id: i64,
    context: String,
    remote: Option<String>,
    override_flag: i64,
}

impl std::fmt::Debug for RemoteCheckRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteCheckRow")
            .field("id", &self.id)
            .field("context", &self.context)
            .field("remote", &self.remote)
            .field("override", &self.override_flag)
            .finish()
    }
}

fn remote_check_rows(dir: &Path) -> Vec<RemoteCheckRow> {
    let db = dir.join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(db).expect("open");
    let mut statement = conn
        .prepare("SELECT id, context, remote, override FROM remote_checks ORDER BY id")
        .expect("prepare");
    let rows = statement
        .query_map([], |row| {
            Ok(RemoteCheckRow {
                id: row.get(0)?,
                context: row.get(1)?,
                remote: row.get(2)?,
                override_flag: row.get(3)?,
            })
        })
        .expect("rows");
    rows.map(|row| row.expect("row")).collect()
}

fn insert_concurrent_remote_check(dir: &Path) -> i64 {
    let db = dir.join(".relaybook").join("ledger.db");
    let mut conn = rusqlite::Connection::open(db).expect("open");
    conn.busy_timeout(Duration::from_secs(5)).expect("busy");
    let tx = conn.transaction().expect("transaction");
    tx.execute(
        "INSERT INTO remote_checks
            (context, branch, head_sha, remote, status, detail, override, created_at)
         VALUES ('review', 'feature', 'concurrent-sha', 'concurrent', 'absent',
                 'inserted during confirmation', 0, '2099-01-01T00:00:00Z')",
        [],
    )
    .expect("insert concurrent remote check");
    let id = tx.last_insert_rowid();
    tx.commit().expect("commit concurrent remote check");
    id
}

struct HolderGuard {
    pid_file: PathBuf,
    cancel: Arc<AtomicBool>,
    watchdog: Option<thread::JoinHandle<()>>,
}

impl HolderGuard {
    fn watch(pid_file: &Path) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_watch = Arc::clone(&cancel);
        let watched = pid_file.to_path_buf();
        let watchdog = thread::spawn(move || {
            for _ in 0..150 {
                if cancel_watch.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            kill_pid_file(&watched);
        });
        Self {
            pid_file: pid_file.to_path_buf(),
            cancel,
            watchdog: Some(watchdog),
        }
    }
}

impl Drop for HolderGuard {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(handle) = self.watchdog.take() {
            let _ = handle.join();
        }
        kill_pid_file(&self.pid_file);
    }
}

fn held_pid(path: &Path) -> u32 {
    fs::read_to_string(path)
        .expect("holder pid")
        .trim()
        .parse()
        .expect("holder pid number")
}

fn kill_pid_file(path: &Path) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return;
    };
    kill_pid(pid);
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status();
    }
}

fn pid_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(not(unix))]
    {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
    }
}

fn install_gh(fake: &GraphifyFake) {
    let bin = std::env::split_paths(&fake.path).next().expect("fake bin");
    let dest = bin.join(if cfg!(windows) { "gh.exe" } else { "gh" });
    fs::copy(env!("CARGO_BIN_EXE_fake"), &dest).expect("copy gh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&dest).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&dest, permissions).expect("chmod");
    }
}

fn logged(dir: &Path, fake: &GraphifyFake, log: &Path, args: &[&str]) -> Output {
    let mut cmd = command(dir);
    fake.apply(&mut cmd, "graph");
    cmd.env("LOG", log);
    cmd.args(args).output().expect("output")
}

fn logged_ok(dir: &Path, fake: &GraphifyFake, log: &Path, args: &[&str]) {
    let output = logged(dir, fake, log, args);
    assert!(output.status.success(), "{}", summarize(&output));
}

fn logged_json(dir: &Path, fake: &GraphifyFake, log: &Path, args: &[&str]) -> Value {
    let output = logged(dir, fake, log, args);
    assert!(output.status.success(), "{}", summarize(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    envelope["data"].clone()
}

fn rev_missing(dir: &Path, rev: &str) -> bool {
    let output = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options", rev])
        .output()
        .expect("git");
    !output.status.success()
}

struct PtyOutput {
    success: bool,
    code: u32,
    output: String,
}

/// Answer the authorize prompt, then replace the sign-off tag while git is
/// paused on the signature check. The unsigned tag at HEAD is what a pinned
/// object id verifies. A check that looks the name up again sees the signed
/// tag on the parent instead.
fn authorize_and_replace_signoff_tag(
    dir: &Path,
    fake: &GraphifyFake,
    sha: &str,
    tag: &str,
    parent: &str,
) -> PtyOutput {
    use std::io::{Read, Write};
    use std::sync::mpsc;

    use portable_pty::{
        Child as PtyChild, ChildKiller, CommandBuilder, PtySize, native_pty_system,
    };

    static GATES: AtomicU64 = AtomicU64::new(1);
    let root = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let gate = root.join(format!(
        "relaybook-tag-gate-{}-{}",
        std::process::id(),
        GATES.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&gate).expect("gate");
    let mut path = git_shim_dir().into_os_string();
    path.push(if cfg!(windows) { ";" } else { ":" });
    path.push(&fake.path);

    let system = native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_relaybook"));
    cmd.arg("release");
    cmd.arg("authorize");
    cmd.arg("--sha");
    cmd.arg(sha);
    cmd.cwd(dir);
    cmd.env("PATH", path);
    cmd.env("GRAPHIFY_LOG", &fake.log);
    cmd.env("RELAYBOOK_FAKE_GRAPHIFY", "graph");
    cmd.env("RELAYBOOK_GIT_GATE", &gate);
    cmd.env("RELAYBOOK_GIT_GATE_CMD", "tag");
    cmd.env("RELAYBOOK_GIT_GATE_CMD_ALT", "verify-tag");
    cmd.env("RELAYBOOK_REAL_GIT", real_git());
    cmd.env(
        "RELAYBOOK_REAL_PATH",
        std::env::var_os("PATH").expect("PATH"),
    );
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let (send, recv) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        let mut sent = false;
        let mut answered = 0;
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(count) => {
                    buf.extend_from_slice(&tmp[..count]);
                    conpty::answer_cursor_queries(&buf, &mut answered, &mut writer);
                    let phrases = typed_phrases(&buf);
                    if !sent && !phrases.is_empty() {
                        let phrase = format!("{}{ENTER}", phrases[0]);
                        let _ = writer.write_all(phrase.as_bytes());
                        let _ = writer.flush();
                        sent = true;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = send.send(buf);
    });

    let entered = gate.join("entered-1");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if entered.exists() {
            break;
        }
        match PtyChild::try_wait(child.as_mut()) {
            Ok(Some(_)) => {
                drop(pair.master);
                let buf = recv
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap_or_default();
                panic!(
                    "authorize exited before the tag check: {}",
                    String::from_utf8_lossy(&buf)
                );
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = ChildKiller::kill(child.as_mut());
                panic!("sign-off git verify did not start");
            }
            Err(err) => panic!("wait failed: {err}"),
        }
    }
    git(dir, &["tag", "-d", tag]);
    git(dir, &["tag", "-s", "-m", "moved", tag, parent]);
    fs::write(gate.join("release-1"), b"1").expect("release");

    let started = Instant::now();
    let status = loop {
        match PtyChild::try_wait(child.as_mut()) {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(20) => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = ChildKiller::kill(child.as_mut());
                panic!("timed out waiting for release authorize");
            }
            Err(err) => panic!("wait failed: {err}"),
        }
    };
    drop(pair.master);
    let buf = recv
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    PtyOutput {
        success: status.success(),
        code: status.exit_code(),
        output: conpty::strip_vt(&String::from_utf8_lossy(&buf)),
    }
}

fn authorize_at_tty(dir: &Path, fake: &GraphifyFake, log: Option<&Path>, sha: &str) -> PtyOutput {
    use std::io::{Read, Write};
    use std::sync::mpsc;

    use portable_pty::{
        Child as PtyChild, ChildKiller, CommandBuilder, PtySize, native_pty_system,
    };

    let system = native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_relaybook"));
    cmd.arg("release");
    cmd.arg("authorize");
    cmd.arg("--sha");
    cmd.arg(sha);
    cmd.cwd(dir);
    cmd.env("PATH", &fake.path);
    cmd.env("GRAPHIFY_LOG", &fake.log);
    cmd.env("RELAYBOOK_FAKE_GRAPHIFY", "graph");
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    if let Some(log) = log {
        cmd.env("LOG", log);
    }
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let (send, recv) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        let mut sent = false;
        let mut answered = 0;
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(count) => {
                    buf.extend_from_slice(&tmp[..count]);
                    conpty::answer_cursor_queries(&buf, &mut answered, &mut writer);
                    let phrases = typed_phrases(&buf);
                    if !sent && !phrases.is_empty() {
                        let phrase = format!("{}{ENTER}", phrases[0]);
                        let _ = writer.write_all(phrase.as_bytes());
                        let _ = writer.flush();
                        sent = true;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = send.send(buf);
    });
    let started = Instant::now();
    let status = loop {
        match PtyChild::try_wait(child.as_mut()) {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(20) => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = ChildKiller::kill(child.as_mut());
                panic!("timed out waiting for release authorize");
            }
            Err(err) => panic!("wait failed: {err}"),
        }
    };
    drop(pair.master);
    let buf = recv
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    PtyOutput {
        success: status.success(),
        code: status.exit_code(),
        output: conpty::strip_vt(&String::from_utf8_lossy(&buf)),
    }
}

fn typed_phrases(buf: &[u8]) -> Vec<String> {
    let text = conpty::strip_vt(&String::from_utf8_lossy(buf));
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = text.split('\n').collect();
    if !text.ends_with('\n') {
        lines.pop();
    }
    lines
        .into_iter()
        .filter_map(|line| {
            line.strip_prefix("Type ")
                .filter(|phrase| !phrase.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect()
}

#[test]
fn typed_phrases_reads_a_conpty_style_prompt() {
    let buf = b"\x1b]0;relaybook.exe\x07\x1b[?25l\x1b[HAuthorize this release?\r\nType release 0123456789ab\x1b[K\r\n";
    assert_eq!(typed_phrases(buf), vec!["release 0123456789ab".to_string()]);
}

#[test]
fn answer_cursor_queries_replies_once_per_request() {
    let mut answered = 0;
    let mut out = Vec::new();
    conpty::answer_cursor_queries(b"\x1b[6", &mut answered, &mut out);
    assert_eq!(answered, 0);
    assert!(out.is_empty());
    let mut buf = b"\x1b[6".to_vec();
    buf.extend_from_slice(b"n\x1b[6n");
    conpty::answer_cursor_queries(&buf, &mut answered, &mut out);
    assert_eq!(answered, 2);
    assert_eq!(out, b"\x1b[1;1R\x1b[1;1R");
    let len = out.len();
    conpty::answer_cursor_queries(&buf, &mut answered, &mut out);
    assert_eq!(answered, 2);
    assert_eq!(out.len(), len);
}

#[test]
fn strip_vt_leaves_plain_text_unchanged() {
    let text = "Type release 0123456789ab\n";
    assert_eq!(conpty::strip_vt(text), text);
}

fn posix_shell() -> PathBuf {
    if cfg!(windows) {
        let git_bash = PathBuf::from(r"C:\Program Files\Git\bin\sh.exe");
        if git_bash.is_file() {
            return git_bash;
        }
    }
    PathBuf::from("sh")
}

fn pty_run(dir: &Path, program: &Path, args: &[&str]) -> PtyOutput {
    use std::io::Read;
    use std::sync::mpsc;

    use portable_pty::{
        Child as PtyChild, ChildKiller, CommandBuilder, PtySize, native_pty_system,
    };

    let system = native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty");
    let mut cmd = CommandBuilder::new(program);
    for arg in args {
        cmd.arg(*arg);
    }
    cmd.cwd(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let (send, recv) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        let mut answered = 0;
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(count) => {
                    buf.extend_from_slice(&tmp[..count]);
                    conpty::answer_cursor_queries(&buf, &mut answered, &mut writer);
                }
                Err(_) => break,
            }
        }
        let _ = send.send(buf);
    });
    let started = Instant::now();
    let status = loop {
        match PtyChild::try_wait(child.as_mut()) {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(20) => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = ChildKiller::kill(child.as_mut());
                panic!("timed out waiting for pty command");
            }
            Err(err) => panic!("wait failed: {err}"),
        }
    };
    drop(pair.master);
    let buf = recv
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    PtyOutput {
        success: status.success(),
        code: status.exit_code(),
        output: conpty::strip_vt(&String::from_utf8_lossy(&buf)),
    }
}

fn install_ssh_signing(repo: &Path) -> tempfile::TempDir {
    let keys = tempfile::tempdir().expect("keys");
    let private = keys.path().join("id_ed25519");
    let status = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-q", "-f"])
        .arg(&private)
        .status()
        .expect("ssh-keygen");
    assert!(status.success(), "ssh-keygen");
    let public = fs::read_to_string(private.with_extension("pub")).expect("pub");
    let allowed = keys.path().join("allowed_signers");
    fs::write(
        &allowed,
        format!("relaybook@example.com namespaces=\"git\" {}", public.trim()),
    )
    .expect("allowed");
    git(repo, &["config", "user.email", "relaybook@example.com"]);
    git(repo, &["config", "user.name", "Relaybook"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    git(repo, &["config", "tag.gpgsign", "false"]);
    git(repo, &["config", "gpg.format", "ssh"]);
    git(
        repo,
        &[
            "config",
            "user.signingkey",
            private
                .with_extension("pub")
                .to_str()
                .expect("utf8 signing key"),
        ],
    );
    git(
        repo,
        &[
            "config",
            "gpg.ssh.allowedSignersFile",
            allowed.to_str().expect("utf8 allowed signers"),
        ],
    );
    keys
}

#[derive(Clone, Copy)]
enum PromptAnswer {
    Phrase,
    Text(&'static str),
}

fn answer_prompts(dir: &Path, args: &[&str], answers: &[PromptAnswer]) -> PtyOutput {
    answer_prompts_in(dir, args, answers, &[], Duration::ZERO)
}

fn answer_prompts_in(
    dir: &Path,
    args: &[&str],
    answers: &[PromptAnswer],
    env: &[(&str, &str)],
    delay: Duration,
) -> PtyOutput {
    use std::io::{Read, Write};
    use std::sync::mpsc;

    use portable_pty::{
        Child as PtyChild, ChildKiller, CommandBuilder, PtySize, native_pty_system,
    };

    let system = native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_relaybook"));
    for arg in args {
        cmd.arg(*arg);
    }
    cmd.cwd(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    for (key, value) in env {
        cmd.env(*key, *value);
    }
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let (send, recv) = mpsc::channel();
    let answers = answers.to_vec();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        let mut sent = 0usize;
        let mut answered = 0;
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(count) => {
                    buf.extend_from_slice(&tmp[..count]);
                    conpty::answer_cursor_queries(&buf, &mut answered, &mut writer);
                    let phrases = typed_phrases(&buf);
                    while sent < answers.len() && phrases.len() > sent {
                        if !delay.is_zero() {
                            thread::sleep(delay);
                        }
                        let line = match answers[sent] {
                            PromptAnswer::Phrase => format!("{}{ENTER}", phrases[sent]),
                            PromptAnswer::Text(text) => text.strip_suffix('\n').map_or_else(
                                || text.to_string(),
                                |prefix| format!("{prefix}{ENTER}"),
                            ),
                        };
                        let _ = writer.write_all(line.as_bytes());
                        let _ = writer.flush();
                        sent += 1;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = send.send(buf);
    });
    let started = Instant::now();
    let status = loop {
        match PtyChild::try_wait(child.as_mut()) {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(20) => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = ChildKiller::kill(child.as_mut());
                panic!("timed out waiting for prompts");
            }
            Err(err) => panic!("wait failed: {err}"),
        }
    };
    drop(pair.master);
    let buf = recv
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    PtyOutput {
        success: status.success(),
        code: status.exit_code(),
        output: conpty::strip_vt(&String::from_utf8_lossy(&buf)),
    }
}

fn authorize_and_insert_during_second_prompt(dir: &Path, sha: &str) -> PtyOutput {
    use std::io::{Read, Write};
    use std::sync::mpsc;

    use portable_pty::{
        Child as PtyChild, ChildKiller, CommandBuilder, PtySize, native_pty_system,
    };

    let system = native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_relaybook"));
    cmd.arg("release");
    cmd.arg("authorize");
    cmd.arg("--sha");
    cmd.arg(sha);
    cmd.cwd(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let (send, recv) = mpsc::channel();
    let (inserted_tx, inserted_rx) = mpsc::channel();
    let ledger_dir = dir.to_path_buf();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        let mut stage = 0usize;
        let mut answered = 0;
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(count) => {
                    buf.extend_from_slice(&tmp[..count]);
                    conpty::answer_cursor_queries(&buf, &mut answered, &mut writer);
                    let text = conpty::strip_vt(&String::from_utf8_lossy(&buf));
                    let phrases = typed_phrases(&buf);
                    if stage == 0 && text.contains("Authorize this release?") && !phrases.is_empty()
                    {
                        let line = format!("{}{ENTER}", phrases[0]);
                        let _ = writer.write_all(line.as_bytes());
                        let _ = writer.flush();
                        stage = 1;
                    } else if stage == 1
                        && text.contains("Proceed without remote verification?")
                        && phrases.len() >= 2
                    {
                        let id = insert_concurrent_remote_check(&ledger_dir);
                        let _ = inserted_tx.send(id);
                        let line = format!("{}{ENTER}", phrases[1]);
                        let _ = writer.write_all(line.as_bytes());
                        let _ = writer.flush();
                        stage = 2;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = send.send(buf);
    });
    let started = Instant::now();
    let status = loop {
        match PtyChild::try_wait(child.as_mut()) {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(20) => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = ChildKiller::kill(child.as_mut());
                panic!("timed out waiting for release authorize");
            }
            Err(err) => panic!("wait failed: {err}"),
        }
    };
    drop(pair.master);
    let buf = recv
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    inserted_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("concurrent remote check was not inserted during confirmation");
    PtyOutput {
        success: status.success(),
        code: status.exit_code(),
        output: conpty::strip_vt(&String::from_utf8_lossy(&buf)),
    }
}
