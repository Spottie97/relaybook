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

fn install_fakes(kind: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().expect("fakes");
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).expect("mkdir");
    let claude = r#"#!/bin/sh
if [ "$1" = auth ]; then echo logged in; exit 0; fi
printf '%s\n' "$*" >> "$LOG/claude.args"
case __KIND__ in
  edit)
    echo dirty > dirty.txt
    ;;
  fail)
    exit 1
    ;;
  limit)
    if [ ! -f .git/limited ]; then
      echo limited > .git/limited
      echo "usage limit reached. resets at 2026-11-10T18:00:00Z"
      exit 1
    fi
    ;;
esac
if printf '%s' "$*" | grep -q 'PRD:'; then
  "$RELAYBOOK" task create work --title "Do the work"
  "$RELAYBOOK" step add work "ship it"
  if [ __KIND__ = two ]; then
    "$RELAYBOOK" task create more --title "Do more" --depends-on work
  fi
else
  if [ __KIND__ = reopen ]; then
    "$RELAYBOOK" task reopen work --reason "try again"
  elif [ __KIND__ = editdirty ]; then
    echo more >> extra.txt
    "$RELAYBOOK" report accept work
    "$RELAYBOOK" run complete
  else
    "$RELAYBOOK" report accept work
    "$RELAYBOOK" run complete
  fi
fi
printf '%s\n' '{"session_id":"sess-1","total_cost_usd":0.20}'
"#;
    let cursor = r#"#!/bin/sh
if [ "$1" = status ] || [ "$1" = about ]; then echo logged in; exit 0; fi
printf '%s\n' "$*" >> "$LOG/cursor.args"
case __KIND__ in
  fail|limit-cursor)
    if [ "$KIND" = limit-cursor ]; then
      echo "rate limit"
      exit 1
    fi
    exit 1
    ;;
  block)
    "$RELAYBOOK" task block work --reason "stuck"
    exit 0
    ;;
  tag)
    git tag v9
    ;;
  extra)
    echo leftover > extra.txt
    ;;
  editdirty)
    echo before > extra.txt
    ;;
  switch)
    git checkout main
    exit 0
    ;;
  manual)
    "$RELAYBOOK" mode manual
    ;;
  sleep)
    sleep 30
    ;;
esac
"$RELAYBOOK" task claim work --by cursor
echo hello >> work.txt
git add work.txt
git commit -m "do the work"
"$RELAYBOOK" task done work --changed "added work" --verified "git show" --not-verified "live" --review "diff"
if [ __KIND__ = pushmain ]; then
  git push origin HEAD:main
fi
printf '%s\n' '{"ok":true}'
"#;
    write_exe(&bin.join("claude"), &claude.replace("__KIND__", kind));
    write_exe(&bin.join("cursor-agent"), &cursor.replace("__KIND__", kind));
    write_exe(
        &bin.join("gh"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$LOG/gh.args\"\necho https://example.test/pr/1\n",
    );
    (root, bin)
}

fn write_exe(path: &Path, body: &str) {
    fs::write(path, body).expect("script");
    let mut permissions = fs::metadata(path).expect("meta").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
    }
    fs::set_permissions(path, permissions).expect("chmod");
}

fn with_fakes(cmd: &mut Command, log: &Path, bin: &Path) {
    fs::create_dir_all(log).expect("log");
    let path = std::env::var("PATH").unwrap_or_default();
    cmd.env("PATH", format!("{}:{path}", bin.display()));
    cmd.env("LOG", log);
    cmd.env_remove("ANTHROPIC_API_KEY");
}

fn run(dir: &Path, kind: &str, args: &[&str]) -> std::process::Output {
    let (root, bin) = install_fakes(kind);
    let log = root.path().join("log");
    let mut cmd = relaybook(dir);
    with_fakes(&mut cmd, &log, &bin);
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
    let (root, bin) = install_fakes("sleep");
    let mut cmd = relaybook(dir.path());
    with_fakes(&mut cmd, &root.path().join("log"), &bin);
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
