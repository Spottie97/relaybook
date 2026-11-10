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
    assert_eq!(text.matches("<!-- agentmesh:start -->").count(), 1);
    assert!(text.contains("## AgentMesh coordination (planner)"));

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
    let original = "notes\n## AgentMesh coordination (planner)\ncustom\n";
    fs::write(&path, original).expect("write");

    run(dir.path(), ["install", "--claude"]).assert().success();
    assert_eq!(fs::read_to_string(&path).expect("read"), original);
    assert!(!original.contains("<!-- agentmesh:start -->"));
}

fn run(dir: &Path, args: impl IntoIterator<Item = &'static str>) -> Command {
    let mut cmd = command(dir);
    cmd.args(args);
    cmd
}

fn command(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("agentmesh").expect("agentmesh binary");
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
    let data = json_data(dir, ["context", "--json"]);
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
