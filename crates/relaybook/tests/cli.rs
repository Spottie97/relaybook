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
    run(dir.path(), ["task", "done", "auth"]).assert().success();

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
    run(dir.path(), ["task", "done", "ship"]).assert().success();
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

    run(dir.path(), ["task", "done", "base"]).assert().success();
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

    run(dir.path(), ["task", "done", "auth"]).assert().success();
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
    run(dir.path(), ["task", "done", "old"]).assert().success();
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
    run(dir.path(), ["task", "done", "old"]).assert().success();
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
