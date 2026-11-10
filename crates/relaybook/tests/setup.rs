use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use assert_cmd::prelude::*;
use serde_json::Value;

#[test]
fn fresh_setup_is_safe_to_rerun() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = fake_graphify(dir.path(), false);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup"]).assert().success();

    let claude = fs::read_to_string(dir.path().join("CLAUDE.md")).expect("claude");
    assert_eq!(claude.matches("<!-- relaybook:start -->").count(), 1);
    assert!(dir.path().join(".relaybook").join("ledger.db").is_file());
    assert!(dir.path().join("AGENTS.md").is_file());

    setup(dir.path(), &bin, &log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("init: skipped"))
        .stdout(predicates::str::contains("install: skipped"));
    let again = fs::read_to_string(dir.path().join("CLAUDE.md")).expect("claude");
    assert_eq!(again, claude);
}

#[test]
fn missing_graphify_skips_without_failing() {
    let dir = tempfile::tempdir().expect("tempdir");
    relaybook(dir.path())
        .args(["setup"])
        .env("PATH", "/usr/bin:/bin")
        .assert()
        .success()
        .stdout(predicates::str::contains("uv tool install graphifyy"));
    assert!(dir.path().join("CLAUDE.md").is_file());
    assert!(dir.path().join(".relaybook").join("ledger.db").is_file());
}

#[test]
fn no_graphify_does_not_invoke_graphify() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = fake_graphify(dir.path(), false);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--no-graphify"));
    assert!(!log.exists());
}

#[test]
fn failing_graphify_still_finishes_relaybook_steps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = fake_graphify(dir.path(), true);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("failed"));
    assert!(dir.path().join("CLAUDE.md").is_file());
    assert!(dir.path().join(".relaybook").join("ledger.db").is_file());

    let output = setup(dir.path(), &bin, &log, &["setup", "--json"])
        .output()
        .expect("output");
    assert_eq!(output.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], false);
    let steps = envelope["data"]["steps"].as_array().expect("steps");
    assert!(steps.iter().any(|step| {
        step["name"] == "init" && (step["status"] == "done" || step["status"] == "skipped")
    }));
    assert!(steps.iter().any(|step| step["status"] == "failed"));
}

#[test]
fn merge_and_rebase_refresh_the_graph_and_keep_existing_hooks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git_init(root);
    let bin = fake_graphify(root, false);
    let log = root.join("graphify.log");
    let marker = root.join("hook-marker.log");
    write_hook(
        &root.join(".git").join("hooks").join("post-merge"),
        &format!("#!/bin/sh\necho merge >> '{}'\n", marker.display()),
    );
    write_hook(
        &root.join(".git").join("hooks").join("post-rewrite"),
        &format!("#!/bin/sh\necho rewrite >> '{}'\n", marker.display()),
    );

    setup(root, &bin, &log, &["setup"]).assert().success();
    setup(root, &bin, &log, &["setup"]).assert().success();
    let merged =
        fs::read_to_string(root.join(".git").join("hooks").join("post-merge")).expect("hook");
    assert_eq!(merged.matches("# relaybook:hooks:start").count(), 1);
    assert!(merged.contains("echo merge >>"));

    let before = count_log(&log, "update .");
    commit(root, "base.txt", "base");
    git(root, &["checkout", "-b", "feature"]);
    commit(root, "feature.txt", "feature");
    git(root, &["checkout", "main"]);
    commit(root, "main.txt", "main");
    let merge = git_env(root, &bin, &log, &["merge", "--no-edit", "feature"]);
    assert!(
        merge.status.success(),
        "{}",
        String::from_utf8_lossy(&merge.stderr)
    );
    assert!(
        wait_for(&log, "update .", before + 1),
        "graphify log: {}",
        read_lossy(&log)
    );
    assert!(
        read_lossy(&marker).contains("merge"),
        "marker: {}",
        read_lossy(&marker)
    );

    let origin = tempfile::tempdir().expect("origin");
    git_init(origin.path());
    commit(origin.path(), "shared.txt", "origin");
    let work = tempfile::tempdir().expect("work");
    git(
        root,
        &[
            "clone",
            origin.path().to_str().expect("path"),
            work.path().to_str().expect("path"),
        ],
    );
    configure(work.path());
    let work_bin = fake_graphify(work.path(), false);
    let work_log = work.path().join("graphify.log");
    let work_marker = work.path().join("hook-marker.log");
    write_hook(
        &work.path().join(".git").join("hooks").join("post-rewrite"),
        &format!("#!/bin/sh\necho rewrite >> '{}'\n", work_marker.display()),
    );
    setup(work.path(), &work_bin, &work_log, &["setup"])
        .assert()
        .success();
    commit(origin.path(), "remote.txt", "remote");
    commit(work.path(), "local.txt", "local");
    let before_rebase = count_log(&work_log, "update .");
    let pull = git_env(
        work.path(),
        &work_bin,
        &work_log,
        &["pull", "--rebase", "origin", "main"],
    );
    assert!(
        pull.status.success(),
        "{}",
        String::from_utf8_lossy(&pull.stderr)
    );
    assert!(
        wait_for(&work_log, "update .", before_rebase + 1),
        "rebase log: {}",
        read_lossy(&work_log)
    );
    assert!(
        read_lossy(&work_marker).contains("rewrite"),
        "marker: {}",
        read_lossy(&work_marker)
    );
}

#[test]
fn custom_hook_managers_are_left_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    git(dir.path(), &["config", "core.hooksPath", ".githooks"]);
    let bin = fake_graphify(dir.path(), false);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("core.hooksPath"));
    assert!(
        !dir.path()
            .join(".git")
            .join("hooks")
            .join("post-merge")
            .exists()
    );
    assert!(!dir.path().join(".githooks").join("post-merge").exists());

    let husky = tempfile::tempdir().expect("husky");
    git_init(husky.path());
    fs::create_dir(husky.path().join(".husky")).expect("husky");
    let husky_bin = fake_graphify(husky.path(), false);
    let husky_log = husky.path().join("graphify.log");
    setup(husky.path(), &husky_bin, &husky_log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Husky"));
    assert!(
        !husky
            .path()
            .join(".git")
            .join("hooks")
            .join("post-merge")
            .exists()
    );

    let left = tempfile::tempdir().expect("lefthook");
    git_init(left.path());
    fs::write(left.path().join("lefthook.yml"), "pre-commit:\n").expect("lefthook");
    let left_bin = fake_graphify(left.path(), false);
    let left_log = left.path().join("graphify.log");
    setup(left.path(), &left_bin, &left_log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Lefthook"));
    assert!(
        !left
            .path()
            .join(".git")
            .join("hooks")
            .join("post-merge")
            .exists()
    );
}

#[test]
fn no_hooks_skips_hook_installation() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let bin = fake_graphify(dir.path(), false);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup", "--no-hooks"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--no-hooks"));
    assert!(
        !dir.path()
            .join(".git")
            .join("hooks")
            .join("post-merge")
            .exists()
    );
    let text = fs::read_to_string(&log).expect("log");
    assert!(!text.contains("hook install"));
    assert!(text.contains("update ."));
}

#[test]
fn ci_workflow_is_written_once_and_never_overwrites() {
    let dir = tempfile::tempdir().expect("tempdir");
    setup_plain(dir.path(), &["setup", "--ci", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains("GRAPHIFY_PR_TOKEN"))
        .stdout(predicates::str::contains(
            "Allow GitHub Actions to create and approve pull requests",
        ));
    let path = dir
        .path()
        .join(".github")
        .join("workflows")
        .join("graphify-update.yml");
    let text = fs::read_to_string(&path).expect("workflow");
    assert!(text.contains("paths-ignore:"));
    assert!(text.contains("graphify-out/**"));
    assert!(text.contains("graphify extract ."));
    assert!(text.contains("graphify update ."));
    assert!(text.contains("peter-evans/create-pull-request"));
    assert!(text.contains("secrets.GRAPHIFY_PR_TOKEN || github.token"));
    let attributes = fs::read_to_string(dir.path().join(".gitattributes")).expect("attributes");
    assert!(attributes.contains("graphify-out/graph.json merge=graphify"));

    setup_plain(dir.path(), &["setup", "--ci", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ci workflow: skipped"));
    assert_eq!(fs::read_to_string(&path).expect("workflow"), text);

    let existing = tempfile::tempdir().expect("existing");
    let existing_path = existing
        .path()
        .join(".github")
        .join("workflows")
        .join("graphify-update.yml");
    fs::create_dir_all(existing_path.parent().expect("parent")).expect("mkdir");
    fs::write(&existing_path, "custom: true\n").expect("write");
    setup_plain(existing.path(), &["setup", "--ci", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ci workflow: skipped"));
    assert_eq!(
        fs::read_to_string(&existing_path).expect("read"),
        "custom: true\n"
    );
}

#[test]
fn legacy_hook_markers_are_replaced_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let hook = dir.path().join(".git").join("hooks").join("post-merge");
    write_hook(
        &hook,
        "#!/bin/sh\n# agentmesh:hooks:start\necho old\n# agentmesh:hooks:end\necho kept\n",
    );
    let bin = fake_graphify(dir.path(), false);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup"]).assert().success();
    let text = fs::read_to_string(&hook).expect("hook");
    assert_eq!(text.matches("# relaybook:hooks:start").count(), 1);
    assert!(!text.contains("agentmesh"));
    assert!(text.contains("echo kept"));
    let again = text.clone();
    setup(dir.path(), &bin, &log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("post-merge: already installed"));
    assert_eq!(fs::read_to_string(&hook).expect("hook"), again);
}

#[test]
fn shell_hook_with_exit_still_refreshes_the_graph() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git_init(root);
    let bin = fake_graphify(root, false);
    let log = root.join("graphify.log");
    let marker = root.join("hook-marker.log");
    write_hook(
        &root.join(".git").join("hooks").join("post-merge"),
        &format!("#!/bin/sh\necho merge >> '{}'\nexit 0\n", marker.display()),
    );
    setup(root, &bin, &log, &["setup"]).assert().success();
    let hook =
        fs::read_to_string(root.join(".git").join("hooks").join("post-merge")).expect("hook");
    let block = hook.find("# relaybook:hooks:start").expect("block");
    let exit_at = hook.find("exit 0").expect("exit");
    assert!(block < exit_at, "{hook}");
    commit(root, "base.txt", "base");
    git(root, &["checkout", "-b", "feature"]);
    commit(root, "feature.txt", "feature");
    git(root, &["checkout", "main"]);
    commit(root, "main.txt", "main");
    let before = count_log(&log, "update .");
    let merge = git_env(root, &bin, &log, &["merge", "--no-edit", "feature"]);
    assert!(
        merge.status.success(),
        "{}",
        String::from_utf8_lossy(&merge.stderr)
    );
    assert!(
        wait_for(&log, "update .", before + 1),
        "{}",
        read_lossy(&log)
    );
    assert!(read_lossy(&marker).contains("merge"));

    let again =
        fs::read_to_string(root.join(".git").join("hooks").join("post-merge")).expect("hook");
    setup(root, &bin, &log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("post-merge: already installed"));
    assert_eq!(
        fs::read_to_string(root.join(".git").join("hooks").join("post-merge")).expect("hook"),
        again
    );
}

#[test]
fn python_hook_is_left_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git_init(root);
    let body = "#!/usr/bin/env python3\nprint('hook')\n";
    let merge = root.join(".git").join("hooks").join("post-merge");
    let rewrite = root.join(".git").join("hooks").join("post-rewrite");
    write_hook(&merge, body);
    write_hook(&rewrite, body);
    let bin = fake_graphify(root, false);
    let log = root.join("graphify.log");
    setup(root, &bin, &log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "post-merge: skipped (python3 hook; add 'graphify update . &' yourself)",
        ))
        .stdout(predicates::str::contains(
            "post-rewrite: skipped (python3 hook; add 'graphify update . &' yourself)",
        ));
    assert_eq!(fs::read_to_string(&merge).expect("merge"), body);
    assert_eq!(fs::read_to_string(&rewrite).expect("rewrite"), body);
}

fn setup(dir: &Path, bin: &Path, log: &Path, args: &[&str]) -> Command {
    let mut cmd = relaybook(dir);
    cmd.args(args);
    with_graphify(&mut cmd, bin, log);
    cmd
}

fn setup_plain(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = relaybook(dir);
    cmd.args(args);
    cmd.env("PATH", "/usr/bin:/bin");
    cmd
}

fn relaybook(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("relaybook").expect("relaybook binary");
    cmd.current_dir(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    cmd
}

fn fake_graphify(dir: &Path, fail: bool) -> PathBuf {
    let bin = dir.join("fake-bin");
    fs::create_dir_all(&bin).expect("mkdir");
    let script = bin.join("graphify");
    let exit = if fail { "exit 1" } else { "exit 0" };
    fs::write(
        &script,
        format!("#!/bin/sh\necho \"$*\" >> \"$GRAPHIFY_LOG\"\necho boom >&2\n{exit}\n"),
    )
    .expect("write");
    let mut permissions = fs::metadata(&script).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");
    bin
}

fn with_graphify(cmd: &mut Command, bin: &Path, log: &Path) {
    let path = std::env::var("PATH").unwrap_or_default();
    cmd.env("PATH", format!("{}:{path}", bin.display()));
    cmd.env("GRAPHIFY_LOG", log);
}

fn git_init(dir: &Path) {
    git(dir, &["init", "-b", "main"]);
    configure(dir);
}

fn configure(dir: &Path) {
    git(dir, &["config", "user.name", "Relaybook"]);
    git(dir, &["config", "user.email", "relaybook@example.com"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn commit(dir: &Path, file: &str, message: &str) {
    fs::write(dir.join(file), message).expect("write");
    git(dir, &["add", file]);
    git(dir, &["commit", "-m", message]);
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "{args:?} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git_env(dir: &Path, bin: &Path, log: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    with_graphify(&mut cmd, bin, log);
    cmd.output().expect("git")
}

fn write_hook(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, body).expect("write");
    let mut permissions = fs::metadata(path).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

fn count_log(path: &Path, needle: &str) -> usize {
    if !path.is_file() {
        return 0;
    }
    fs::read_to_string(path)
        .expect("read")
        .lines()
        .filter(|line| line.contains(needle))
        .count()
}

fn wait_for(path: &Path, needle: &str, at_least: usize) -> bool {
    for _ in 0..40 {
        if count_log(path, needle) >= at_least {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn read_lossy(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}
