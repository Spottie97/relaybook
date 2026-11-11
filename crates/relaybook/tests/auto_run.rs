use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;

fn relaybook(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("relaybook").expect("bin");
    cmd.current_dir(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    cmd.env("RELAYBOOK", env!("CARGO_BIN_EXE_relaybook"));
    cmd
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Repo {
    work: tempfile::TempDir,
    _origin: tempfile::TempDir,
}

impl Repo {
    fn path(&self) -> &Path {
        self.work.path()
    }
}

fn repo() -> Repo {
    let dir = tempfile::tempdir().expect("temp");
    let origin = tempfile::tempdir().expect("origin");
    git(dir.path(), &["init", "-b", "main"]);
    git(
        dir.path(),
        &["config", "user.email", "relaybook@example.com"],
    );
    git(dir.path(), &["config", "user.name", "Relaybook"]);
    fs::write(dir.path().join("README.md"), "seed\n").expect("write");
    git(dir.path(), &["add", "README.md"]);
    git(dir.path(), &["commit", "-m", "seed"]);
    let bare = origin.path().join("origin.git");
    git(
        dir.path(),
        &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    git(
        dir.path(),
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    git(dir.path(), &["push", "-u", "origin", "main"]);
    fs::write(
        dir.path().join("prd.md"),
        "Build a tiny greeting command.\n",
    )
    .expect("prd");
    git(dir.path(), &["add", "prd.md"]);
    git(dir.path(), &["commit", "-m", "add prd"]);
    git(dir.path(), &["push", "origin", "main"]);
    relaybook(dir.path()).arg("init").assert().success();
    relaybook(dir.path())
        .args(["mode", "auto"])
        .assert()
        .success();
    relaybook(dir.path())
        .args(["config", "set", "gates", "none"])
        .assert()
        .success();
    Repo {
        work: dir,
        _origin: origin,
    }
}

fn install_fakes() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().expect("fakes");
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).expect("mkdir");
    for name in ["claude", "cursor-agent", "gh"] {
        copy_fake(&bin, name);
    }
    (root, bin)
}

fn copy_fake(dir: &Path, name: &str) {
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

fn with_fakes(cmd: &mut Command, log: &Path, bin: &Path, kind: &str) {
    fs::create_dir_all(log).expect("log");
    let mut path = std::ffi::OsString::from(bin);
    if let Some(rest) = std::env::var_os("PATH") {
        path.push(if cfg!(windows) { ";" } else { ":" });
        path.push(rest);
    }
    cmd.env("PATH", path);
    cmd.env("LOG", log);
    cmd.env("RELAYBOOK_FAKE_KIND", kind);
    cmd.env_remove("ANTHROPIC_API_KEY");
}

fn run(dir: &Path, kind: &str, args: &[&str]) -> std::process::Output {
    let (root, bin) = install_fakes();
    let log = root.path().join("log");
    let mut cmd = relaybook(dir);
    with_fakes(&mut cmd, &log, &bin, kind);
    cmd.args(args);
    let output = cmd.output().expect("run");
    let saved = std::env::temp_dir().join(format!(
        "relaybook-log-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = fs::create_dir_all(&saved);
    if log.is_dir() {
        for entry in fs::read_dir(&log).expect("read log") {
            let entry = entry.expect("entry");
            let _ = fs::copy(entry.path(), saved.join(entry.file_name()));
        }
    }
    let _ = fs::write(
        dir.join(".relaybook").join("log-path"),
        saved.display().to_string(),
    );
    output
}

#[test]
fn hands_off_run_opens_a_pr_and_leaves_main_untouched() {
    let dir = repo();
    let main_before = rev(dir.path(), "origin/main");
    let output = run(dir.path(), "happy", &["run", "--prd", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("claude: subscription account, cursor: plan account"),
        "{text}"
    );
    assert!(text.contains("opened https://example.test/pr/1"), "{text}");
    assert!(
        text.contains("--allowedTools")
            || args_contain(dir.path(), "claude.args", "--allowedTools")
    );
    assert!(args_contain(dir.path(), "claude.args", "Bash(relaybook:*)"));
    assert!(args_contain(dir.path(), "claude.args", "--permission-mode"));
    assert!(args_contain(dir.path(), "claude.args", "--disallowedTools"));
    assert!(args_contain(
        dir.path(),
        "claude.args",
        "Edit,Write,NotebookEdit"
    ));
    assert!(!args_contain(dir.path(), "claude.args", "--max-budget-usd"));
    assert!(args_contain(dir.path(), "cursor.args", "--force"));
    assert!(args_contain(dir.path(), "gh.args", "pr create"));
    assert_eq!(rev(dir.path(), "origin/main"), main_before);
    assert!(rev(dir.path(), "origin/relaybook/prd").len() > 10);
    let transcript = dir.path().join(".relaybook").join("runs");
    assert!(transcript.is_dir());
    let context = relaybook(dir.path())
        .arg("context")
        .output()
        .expect("context");
    let context = String::from_utf8_lossy(&context.stdout);
    assert!(!context.contains("sess-1"), "{context}");
}

#[test]
fn positional_prd_starts_a_run() {
    let dir = repo();
    let output = run(dir.path(), "happy", &["run", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("opened https://example.test/pr/1"), "{text}");
}

#[test]
fn plan_gate_waits_and_approve_finishes() {
    let dir = repo();
    relaybook(dir.path())
        .args(["config", "set", "gates", "plan"])
        .assert()
        .success();
    let output = run(dir.path(), "happy", &["run", "--prd", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("waiting for approval: relaybook approve"),
        "{text}"
    );
    let output = run(dir.path(), "happy", &["approve"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("opened https://example.test/pr/1"), "{text}");
}

#[test]
fn review_gate_waits_after_each_review() {
    let dir = repo();
    relaybook(dir.path())
        .args(["config", "set", "gates", "plan,review"])
        .assert()
        .success();
    let first = run(dir.path(), "happy", &["run", "--prd", "prd.md"]);
    assert!(String::from_utf8_lossy(&first.stdout).contains("waiting for approval"));
    let second = run(dir.path(), "happy", &["approve"]);
    let text = String::from_utf8_lossy(&second.stdout);
    assert!(second.status.success(), "{text}");
    assert!(
        text.contains("waiting for approval: relaybook approve"),
        "{text}"
    );
    let third = run(dir.path(), "happy", &["approve"]);
    let text = String::from_utf8_lossy(&third.stdout);
    assert!(
        third.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&third.stderr)
    );
    assert!(text.contains("opened https://example.test/pr/1"), "{text}");
}

#[test]
fn reopen_sends_the_task_back_until_the_cap() {
    let dir = repo();
    relaybook(dir.path())
        .args(["config", "set", "max_reopens_per_task", "1"])
        .assert()
        .success();
    let output = run(dir.path(), "reopen", &["run", "--prd", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("reopened more than 1 times"), "{text}");
}

#[test]
fn stop_conditions_cover_cycles_calls_cost_block_failure_timeout_and_manual() {
    let dir = repo();
    relaybook(dir.path())
        .args(["config", "set", "max_cycles", "1"])
        .assert()
        .success();
    let output = run(dir.path(), "two", &["run", "--prd", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("stopped: max_cycles"),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dir = repo();
    relaybook(dir.path())
        .args(["config", "set", "max_agent_calls", "1"])
        .assert()
        .success();
    let output = run(dir.path(), "happy", &["run", "--prd", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("stopped: max_agent_calls"), "{text}");
    relaybook(dir.path())
        .args(["config", "set", "max_agent_calls", "20"])
        .assert()
        .success();
    let output = run(dir.path(), "happy", &["run", "--resume"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dir = repo();
    relaybook(dir.path())
        .args(["config", "set", "est_cost_cap_usd", "0.01"])
        .assert()
        .success();
    let output = run(dir.path(), "happy", &["run", "--prd", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("stopped: est_cost_cap_usd"), "{text}");

    let dir = repo();
    let output = run(dir.path(), "block", &["run", "--prd", "prd.md"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("stopped: blocked: work"));

    let dir = repo();
    let output = run(dir.path(), "fail", &["run", "--prd", "prd.md"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("two consecutive agent failures"));

    let dir = repo();
    let (root, bin) = install_fakes();
    let mut cmd = relaybook(dir.path());
    with_fakes(&mut cmd, &root.path().join("log"), &bin, "sleep");
    cmd.env("RELAYBOOK_CURSOR_TIMEOUT_SECS", "1");
    cmd.args(["run", "--prd", "prd.md"]);
    let output = cmd.output().expect("timeout");
    assert!(String::from_utf8_lossy(&output.stdout).contains("stopped: timeout"));

    let dir = repo();
    let output = run(dir.path(), "manual", &["run", "--prd", "prd.md"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("stopped: mode is manual"));
}

#[test]
fn usage_limit_pauses_and_resume_retries() {
    let dir = repo();
    let output = run(dir.path(), "limit", &["run", "--prd", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains(
            "usage limit reached on claude; relaybook run --resume after 2026-11-10T18:00:00Z"
        ),
        "{text}"
    );
    let output = run(dir.path(), "limit", &["run", "--resume"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("opened https://example.test/pr/1"), "{text}");
}

#[test]
fn claude_review_ignores_files_cursor_already_left_untracked() {
    let dir = repo();
    let output = run(dir.path(), "extra", &["run", "--prd", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("opened https://example.test/pr/1"), "{text}");
}

#[test]
fn review_that_edits_an_already_dirty_file_is_stopped() {
    let dir = repo();
    let output = run(dir.path(), "editdirty", &["run", "--prd", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("claude edited the worktree"),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success());
}

#[test]
fn guardrails_stop_a_misbehaving_agent() {
    for (kind, needle) in [
        ("edit", "claude edited the worktree"),
        ("tag", "created a tag"),
        ("pushmain", "pushed main"),
        ("switch", "switched branch"),
    ] {
        let dir = repo();
        let output = run(dir.path(), kind, &["run", "--prd", "prd.md"]);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains(needle),
            "{kind}: {text} {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if kind != "pushmain" {
            assert_eq!(rev(dir.path(), "origin/main"), rev(dir.path(), "main"));
        }
    }
}

#[test]
fn manual_mode_refuses_to_start_and_preflight_names_the_fix() {
    let dir = repo();
    relaybook(dir.path())
        .args(["mode", "manual"])
        .assert()
        .success();
    let output = run(dir.path(), "happy", &["run", "--prd", "prd.md"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("mode is manual"));

    let dir = tempfile::tempdir().expect("temp");
    relaybook(dir.path()).arg("init").assert().success();
    relaybook(dir.path())
        .args(["mode", "auto"])
        .assert()
        .success();
    let output = relaybook(dir.path())
        .args(["run", "--prd", "prd.md"])
        .env("PATH", "/usr/bin:/bin")
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("run");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("claude: not on PATH")
            || String::from_utf8_lossy(&output.stderr).contains("claude: not on PATH")
    );
}

#[test]
fn existing_tasks_are_adopted_and_new_only_leaves_them() {
    let dir = repo();
    relaybook(dir.path())
        .args(["task", "create", "work", "--title", "Already planned"])
        .assert()
        .success();
    let output = run(dir.path(), "happy", &["run", "--prd", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("adopted 1 existing tasks: work"), "{text}");
    assert!(task_list(dir.path()).contains("\"status\":\"done\""),);

    let dir = repo();
    relaybook(dir.path())
        .args(["task", "create", "other", "--title", "Leave me"])
        .assert()
        .success();
    let output = run(
        dir.path(),
        "happy",
        &["run", "--new-only", "--prd", "prd.md"],
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!text.contains("adopted "), "{text}");
    let listed = task_list(dir.path());
    assert!(listed.contains("\"slug\":\"other\""), "{listed}");
    assert!(listed.contains("\"status\":\"open\""), "{listed}");
}

#[test]
fn a_dependency_outside_the_run_is_pulled_in_or_named() {
    let dir = repo();
    relaybook(dir.path())
        .args(["task", "create", "login", "--title", "Sign in"])
        .assert()
        .success();
    let output = run(
        dir.path(),
        "depend",
        &["run", "--new-only", "--prd", "prd.md"],
    );
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("adopted 1 existing tasks: login"), "{text}");
    let listed = task_list(dir.path());
    assert!(listed.contains("\"slug\":\"login\""), "{listed}");
    assert!(listed.contains("\"slug\":\"uploads\""), "{listed}");

    let dir = repo();
    relaybook(dir.path())
        .args(["task", "create", "login", "--title", "Sign in"])
        .assert()
        .success();
    let first = run(dir.path(), "stop", &["run", "--prd", "prd.md"]);
    assert!(
        !first.status.success(),
        "stop run should end when the planner stops it"
    );
    let output = run(
        dir.path(),
        "depend",
        &["run", "--new-only", "--prd", "prd.md"],
    );
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains(
            "uploads waits on login, which is not in this run; link it with relaybook run add login"
        ),
        "{text}"
    );
}

#[test]
fn run_add_links_unfinished_tasks_and_rejects_the_rest() {
    let dir = repo();
    relaybook(dir.path())
        .args(["config", "set", "gates", "plan"])
        .assert()
        .success();
    relaybook(dir.path())
        .args(["task", "create", "login", "--title", "Sign in"])
        .assert()
        .success();
    let output = run(
        dir.path(),
        "happy",
        &["run", "--new-only", "--prd", "prd.md"],
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("waiting for approval"), "{text}");
    relaybook(dir.path())
        .args(["run", "add", "login"])
        .assert()
        .success()
        .stdout(predicates::str::contains("linked login"));
    relaybook(dir.path())
        .args(["run", "add", "login"])
        .assert()
        .success()
        .stdout(predicates::str::contains("login already in this run"));
    let events = relaybook(dir.path())
        .args(["context", "login", "--json"])
        .output()
        .expect("context");
    let events = String::from_utf8_lossy(&events.stdout);
    assert!(events.contains("run_linked"), "{events}");
    relaybook(dir.path())
        .args(["task", "create", "gone", "--title", "Finished"])
        .assert()
        .success();
    relaybook(dir.path())
        .args(["task", "claim", "gone", "--by", "cursor"])
        .assert()
        .success();
    relaybook(dir.path())
        .args([
            "task",
            "done",
            "gone",
            "--changed",
            "done",
            "--verified",
            "seen",
            "--not-verified",
            "rest",
            "--review",
            "ok",
        ])
        .assert()
        .success();
    relaybook(dir.path())
        .args(["run", "add", "gone"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("gone is done"));
    relaybook(dir.path())
        .args(["run", "stop"])
        .assert()
        .failure();
    relaybook(dir.path())
        .args(["run", "add", "login"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("no active run"));
}

fn task_list(dir: &Path) -> String {
    let output = relaybook(dir)
        .args(["task", "list", "--all", "--json"])
        .output()
        .expect("list");
    String::from_utf8(output.stdout).expect("utf8")
}

fn args_contain(dir: &Path, name: &str, needle: &str) -> bool {
    let path = fs::read_to_string(dir.join(".relaybook").join("log-path")).unwrap_or_default();
    fs::read_to_string(Path::new(path.trim()).join(name))
        .unwrap_or_default()
        .contains(needle)
}

fn rev(dir: &Path, name: &str) -> String {
    let output = Command::new("git")
        .args(["rev-parse", name])
        .current_dir(dir)
        .output()
        .expect("rev");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
