use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;

fn relaybook(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("relaybook").expect("relaybook binary");
    cmd.current_dir(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    cmd.env("PATH", "/usr/bin:/bin");
    cmd
}

fn git_init(dir: &Path) {
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Relaybook"]);
    git(dir, &["config", "user.email", "relaybook@example.com"]);
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<(String, Vec<u8>)>) {
    for entry in fs::read_dir(dir).expect("read") {
        let entry = entry.expect("entry");
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name == ".git" || name == ".relaybook" {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, found);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("prefix")
            .to_string_lossy()
            .replace('\\', "/");
        found.push((relative, fs::read(&path).expect("read")));
    }
}

#[test]
fn setup_then_uninstall_restores_a_fresh_repo() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let before = files(dir.path());
    relaybook(dir.path())
        .args(["setup", "--no-graphify"])
        .assert()
        .success();
    assert!(dir.path().join("CLAUDE.md").is_file());
    let report = relaybook(dir.path())
        .args(["uninstall", "--yes"])
        .output()
        .expect("uninstall");
    assert!(report.status.success());
    let text = String::from_utf8(report.stdout).expect("utf8");
    assert!(text.contains("kept .relaybook/"), "{text}");
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("CLAUDE.md"))
            .count(),
        1
    );
    assert!(
        text.lines().any(|line| line == "removed CLAUDE.md"),
        "{text}"
    );
    assert!(text.lines().any(|line| line == "removed .cursor"), "{text}");
    assert!(text.lines().any(|line| line == "removed .claude"), "{text}");
    assert_eq!(files(dir.path()), before);
    assert!(!dir.path().join(".cursor").exists());
    assert!(!dir.path().join(".claude").exists());
    assert!(dir.path().join(".relaybook").join("ledger.db").is_file());
    relaybook(dir.path())
        .args(["uninstall", "--yes"])
        .assert()
        .success();
}

#[test]
fn uninstall_keeps_preexisting_project_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    fs::write(dir.path().join("CLAUDE.md"), "keep the intro\n").expect("write");
    fs::write(dir.path().join(".gitignore"), "keep-me\n").expect("write");
    fs::create_dir_all(dir.path().join(".claude")).expect("mkdir");
    fs::write(
        dir.path().join(".claude").join("settings.json"),
        "{\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"echo stay\"\n          }\n        ]\n      }\n    ]\n  }\n}\n",
    )
    .expect("write");
    let hooks = dir.path().join(".git").join("hooks");
    fs::create_dir_all(&hooks).expect("mkdir");
    fs::write(hooks.join("post-merge"), "#!/bin/sh\necho keep\n").expect("write");
    let before_claude = fs::read(dir.path().join("CLAUDE.md")).expect("read");
    let before_ignore = fs::read(dir.path().join(".gitignore")).expect("read");
    let before_settings = fs::read(dir.path().join(".claude").join("settings.json")).expect("read");
    let before_hook = fs::read(hooks.join("post-merge")).expect("read");

    relaybook(dir.path())
        .args(["setup", "--no-graphify"])
        .assert()
        .success();
    let report = relaybook(dir.path())
        .args(["uninstall", "--yes"])
        .output()
        .expect("uninstall");
    assert!(report.status.success());
    let text = String::from_utf8(report.stdout).expect("utf8");
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("CLAUDE.md"))
            .count(),
        1
    );
    assert!(
        text.lines().any(|line| line == "restored CLAUDE.md"),
        "{text}"
    );
    assert!(dir.path().join(".claude").is_dir());
    assert!(!dir.path().join(".cursor").exists());

    assert_eq!(
        fs::read(dir.path().join("CLAUDE.md")).expect("read"),
        before_claude
    );
    assert_eq!(
        fs::read(dir.path().join(".gitignore")).expect("read"),
        before_ignore
    );
    assert_eq!(
        fs::read(dir.path().join(".claude").join("settings.json")).expect("read"),
        before_settings
    );
    assert_eq!(
        fs::read(hooks.join("post-merge")).expect("read"),
        before_hook
    );
}

#[test]
fn dry_run_and_purge_and_an_active_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    relaybook(dir.path())
        .args(["setup", "--no-graphify"])
        .assert()
        .success();
    relaybook(dir.path())
        .args(["uninstall", "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains("would remove CLAUDE.md"))
        .stdout(predicates::str::contains("would keep .relaybook/"));
    assert!(dir.path().join("CLAUDE.md").is_file());

    let db = dir.path().join(".relaybook").join("ledger.db");
    let connection = rusqlite::Connection::open(&db).expect("open");
    connection
        .execute(
            "INSERT INTO runs (prd_path, prd_sha256, branch, stage, cycle, agent_calls, est_cost_usd, status, started_at)
             VALUES ('prd.md', 'abc', 'relaybook/prd', 'plan', 1, 0, 0, 'active', '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("insert");
    drop(connection);
    relaybook(dir.path())
        .args(["uninstall", "--yes"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("relaybook run stop"));
    assert!(dir.path().join("CLAUDE.md").is_file());

    connection_clear(&db);
    relaybook(dir.path())
        .args(["uninstall", "--yes", "--purge"])
        .assert()
        .success();
    assert!(!dir.path().join(".relaybook").exists());
    assert!(!dir.path().join("CLAUDE.md").exists());
}

fn connection_clear(db: &Path) {
    let connection = rusqlite::Connection::open(db).expect("open");
    connection.execute("DELETE FROM runs", []).expect("delete");
}

#[test]
fn edited_command_is_kept_and_graphify_uninstall_is_optional() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    relaybook(dir.path())
        .args(["setup", "--no-graphify"])
        .assert()
        .success();
    let command = dir
        .path()
        .join(".cursor")
        .join("commands")
        .join("relaybook.md");
    fs::write(&command, "custom cursor command\n").expect("write");
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).expect("mkdir");
    copy_fake(&bin, "graphify");
    let log = dir.path().join("graphify.log");
    relaybook(dir.path())
        .args(["uninstall", "--yes", "--graphify"])
        .env("PATH", prepend_path(&bin))
        .env("GRAPHIFY_LOG", &log)
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "skipped .cursor/commands/relaybook.md (no relaybook:managed marker)",
        ));
    assert_eq!(
        fs::read_to_string(&command).expect("command"),
        "custom cursor command\n"
    );
    let recorded = fs::read_to_string(log).expect("log");
    assert!(recorded.contains("claude uninstall"), "{recorded}");
    assert!(recorded.contains("cursor uninstall"), "{recorded}");
    assert!(recorded.contains("hook uninstall"), "{recorded}");
}

#[test]
fn ci_round_trip_removes_the_workflow_and_merge_driver() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    relaybook(dir.path())
        .args(["setup", "--ci", "--no-graphify"])
        .assert()
        .success();
    assert!(
        dir.path()
            .join(".github")
            .join("workflows")
            .join("graphify-update.yml")
            .is_file()
    );
    relaybook(dir.path())
        .args(["uninstall", "--yes"])
        .assert()
        .success();
    assert!(
        !dir.path()
            .join(".github")
            .join("workflows")
            .join("graphify-update.yml")
            .exists()
    );
    assert!(!dir.path().join(".gitattributes").exists());
    assert!(!dir.path().join(".github").exists());
    let name = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["config", "--get", "merge.graphify.name"])
        .output()
        .expect("config");
    assert!(!name.status.success());
}

#[test]
fn uninstall_removes_every_prompt_hook_form() {
    let forms = [
        (
            "command -v relaybook >/dev/null 2>&1 && relaybook hook prompt || true",
            None,
        ),
        ("relaybook hook prompt", None),
        (
            "if (Get-Command relaybook -ErrorAction SilentlyContinue) { relaybook hook prompt }",
            Some("powershell"),
        ),
    ];
    for (command, shell) in forms {
        let dir = tempfile::tempdir().expect("tempdir");
        git_init(dir.path());
        let path = dir.path().join(".claude").join("settings.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let mut hook = serde_json::json!({
            "type": "command",
            "command": command
        });
        if let Some(shell) = shell {
            hook["shell"] = serde_json::json!(shell);
        }
        let body = serde_json::json!({
            "keep": true,
            "hooks": {
                "UserPromptSubmit": [{ "hooks": [hook] }],
                "Stop": [{ "hooks": [{ "type": "command", "command": "echo stay" }] }]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&body).expect("json")).expect("write");
        relaybook(dir.path())
            .args(["uninstall", "--yes"])
            .assert()
            .success()
            .stdout(predicates::str::contains(
                "removed .claude/settings.json (UserPromptSubmit hook)",
            ));
        let text = fs::read_to_string(&path).expect("settings");
        assert!(!text.contains("UserPromptSubmit"), "{text}");
        assert!(!text.contains(command), "{text}");
        assert!(text.contains("echo stay"), "{text}");
        assert!(
            text.contains("\"keep\": true") || text.contains("\"keep\":true"),
            "{text}"
        );
    }
}

fn copy_fake(dir: &Path, name: &str) {
    fs::create_dir_all(dir).expect("mkdir");
    let src = PathBuf::from(env!("CARGO_BIN_EXE_fake"));
    let dest = if cfg!(windows) {
        dir.join(format!("{name}.exe"))
    } else {
        dir.join(name)
    };
    fs::copy(src, &dest).expect("copy fake");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&dest).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&dest, permissions).expect("chmod");
    }
}

fn prepend_path(bin: &Path) -> std::ffi::OsString {
    let mut path = std::ffi::OsString::from(bin);
    if let Some(rest) = std::env::var_os("PATH") {
        path.push(if cfg!(windows) { ";" } else { ":" });
        path.push(rest);
    }
    path
}
