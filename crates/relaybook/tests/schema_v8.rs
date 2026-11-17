use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::{Value, json};

const GUARD: &str = "workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)";

fn command(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("relaybook").expect("relaybook binary");
    cmd.current_dir(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    cmd
}

fn run<'a>(dir: &Path, args: impl IntoIterator<Item = &'a str>) -> Command {
    let mut cmd = command(dir);
    cmd.args(args);
    cmd
}

fn git(dir: &Path, args: &[&str]) {
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
}

fn json_output(dir: &Path, args: &[&str]) -> std::process::Output {
    run(dir, args.iter().copied()).output().expect("output")
}

#[test]
fn raise_message_reaches_the_user() {
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

    let db = dir.path().join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(&db).expect("open");
    let task_id: i64 = conn
        .query_row("SELECT id FROM tasks WHERE slug = 'auth'", [], |row| {
            row.get(0)
        })
        .expect("id");
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM reports", [], |row| row.get(0))
        .expect("count");
    let err = conn
        .execute(
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
        .expect_err("guard");
    let shown = format!("sqlite: {err}");
    assert_eq!(
        shown,
        format!("sqlite: {GUARD}"),
        "CLI prints sqlite errors as sqlite: <message>"
    );
    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM reports", [], |row| row.get(0))
        .expect("count");
    assert_eq!(after, before);
    drop(conn);

    fs_write_seed(dir.path());
    git(dir.path(), &["add", "README.md"]);
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
            "-m",
            "seed",
        ],
    );
    let accept = run(dir.path(), ["report", "accept", "auth"])
        .output()
        .expect("accept");
    assert!(!accept.status.success());
    let stderr = String::from_utf8_lossy(&accept.stderr);
    assert!(stderr.contains("no approved review at HEAD"), "{stderr}");
    assert!(!stderr.contains(GUARD), "{stderr}");

    let conn = rusqlite::Connection::open(&db).expect("open");
    conn.execute("DROP TRIGGER relaybook_guard_task_accept", [])
        .expect("drop");
    drop(conn);
    let listed = run(dir.path(), ["task", "list"]).output().expect("list");
    assert!(listed.status.success());
    let human = String::from_utf8_lossy(&listed.stdout);
    assert!(!human.contains("ledger_guard"), "{human}");

    let conn = rusqlite::Connection::open(&db).expect("open");
    conn.execute("DROP TRIGGER relaybook_guard_task_accept", [])
        .expect("drop");
    drop(conn);
    let output = json_output(dir.path(), &["task", "list", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], json!(true));
    assert_eq!(envelope["data"]["ledger_guard"], json!("restored"));

    let again = json_output(dir.path(), &["task", "list", "--json"]);
    let envelope: Value = serde_json::from_slice(&again.stdout).expect("json");
    assert!(envelope["data"].get("ledger_guard").is_none(), "{envelope}");

    let conn = rusqlite::Connection::open(&db).expect("open");
    conn.execute("DROP TRIGGER relaybook_guard_run_update", [])
        .expect("drop");
    drop(conn);
    let failed = json_output(dir.path(), &["report", "accept", "missing", "--json"]);
    assert!(!failed.status.success());
    let envelope: Value = serde_json::from_slice(&failed.stdout).expect("json");
    assert_eq!(envelope["ok"], json!(false));
    assert_eq!(envelope["data"]["ledger_guard"], json!("restored"));
    let error = envelope["error"].as_str().unwrap_or("");
    assert!(!error.contains(GUARD), "{envelope}");

    let conn = rusqlite::Connection::open(&db).expect("open");
    conn.pragma_update(None, "user_version", 9)
        .expect("version");
    drop(conn);
    let status = run(dir.path(), ["status"]).output().expect("status");
    assert!(!status.status.success());
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(
        stderr.contains("this ledger needs a newer relaybook"),
        "{stderr}"
    );
}

fn fs_write_seed(dir: &Path) {
    std::fs::write(dir.join("README.md"), "seed\n").expect("readme");
}
