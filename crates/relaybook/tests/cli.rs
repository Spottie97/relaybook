use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

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
fn task_list_hides_done_tasks_until_asked() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(dir.path(), ["task", "create", "old", "--title", "Old"])
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
fn task_done_stores_a_report_and_rejects_a_long_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    run(dir.path(), ["init"]).assert().success();
    run(
        dir.path(),
        ["task", "create", "auth", "--title", "Add auth"],
    )
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
    command(dir.path())
        .args(["task", "done", "big", "--changed", &huge])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("limit 1500"));
    let still = json_data(dir.path(), ["context", "big", "--json"]);
    assert_eq!(still["tasks"][0]["status"], "open");

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
        .stdout(predicate::str::contains("gates plan"));
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

fn summarize(output: &std::process::Output) -> String {
    format!(
        "status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
