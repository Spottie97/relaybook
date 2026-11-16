use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use serde_json::{Value, json};

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
        .stdout("2 new Cursor reports: relaybook inbox\n")
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
        .success()
        .stdout("workflow classic\n");
    run(dir.path(), ["config", "set", "reviewer", "claude"])
        .assert()
        .success()
        .stdout("reviewer claude\n");
    let restored = json_data(dir.path(), ["config", "--json"]);
    assert_eq!(restored["reviewer"], json!("claude"));
    assert_eq!(restored["workflow"], json!("classic"));
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
    let allowed = tempfile::tempdir().expect("tempdir");
    let head = prepare_gated_review(allowed.path(), &fake);
    git(
        allowed.path(),
        &["update-ref", "refs/remotes/origin/other", &head],
    );
    let recorded = data_of(
        allowed.path(),
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
    let mut path = OsString::new();
    let mut first = true;
    if let Some(current) = std::env::var_os("PATH") {
        for entry in std::env::split_paths(&current) {
            let has_graphify = ["graphify", "graphify.exe"]
                .iter()
                .any(|name| entry.join(name).is_file());
            if has_graphify {
                continue;
            }
            if !first {
                path.push(if cfg!(windows) { ";" } else { ":" });
            }
            first = false;
            path.push(entry);
        }
    }
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
