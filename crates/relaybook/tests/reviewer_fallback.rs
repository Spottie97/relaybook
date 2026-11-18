//! Reviewer fallback to Claude Sonnet 5.5. Executable paths use the fake CLIs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

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

fn origin_repo() -> Repo {
    let dir = tempfile::tempdir().expect("temp");
    let origin = tempfile::tempdir().expect("origin");
    git(dir.path(), &["init", "-b", "main"]);
    git(
        dir.path(),
        &["config", "user.email", "relaybook@example.com"],
    );
    git(dir.path(), &["config", "user.name", "Relaybook"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
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
    Repo {
        work: dir,
        _origin: origin,
    }
}

fn gated_repo() -> Repo {
    let repo = origin_repo();
    let dir = repo.path();
    relaybook(dir)
        .args(["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    relaybook(dir)
        .args(["config", "set", "workflow", "gated"])
        .assert()
        .success();
    relaybook(dir)
        .args(["config", "set", "gates", "plan,review"])
        .assert()
        .success();
    relaybook(dir)
        .args(["config", "set", "max_cycles", "9"])
        .assert()
        .success();
    repo
}

struct Probes {
    _root: tempfile::TempDir,
    log: PathBuf,
    bin: PathBuf,
}

fn install_probes() -> Probes {
    let root = tempfile::tempdir().expect("probes");
    let log = root.path().join("log");
    let bin = root.path().join("bin");
    fs::create_dir_all(&log).expect("log");
    fs::create_dir_all(&bin).expect("bin");
    for name in ["claude", "cursor-agent", "gh", "codex"] {
        copy_fake(&bin, name);
    }
    Probes {
        _root: root,
        log,
        bin,
    }
}

fn copy_fake(dir: &Path, name: &str) {
    let src = PathBuf::from(env!("CARGO_BIN_EXE_fake"));
    let dest = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    fs::copy(src, &dest).expect("copy fake");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&dest).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&dest, permissions).expect("chmod");
    }
}

fn apply_probes(cmd: &mut Command, probes: &Probes) {
    let mut path = std::ffi::OsString::from(&probes.bin);
    path.push(if cfg!(windows) { ";" } else { ":" });
    if let Some(rest) = std::env::var_os("PATH") {
        path.push(rest);
    }
    cmd.env("PATH", path);
    cmd.env("LOG", &probes.log);
    cmd.env("RELAYBOOK_FAKE_KIND", "happy");
    cmd.env_remove("ANTHROPIC_API_KEY");
    cmd.env_remove("RELAYBOOK_FAKE_CODEX_KIND");
    cmd.env_remove("RELAYBOOK_FAKE_CODEX_VERSION");
    cmd.env_remove("RELAYBOOK_FAKE_CLAUDE_REVIEW");
    cmd.env_remove("RELAYBOOK_CLAUDE_REVIEW_HOLD");
    cmd.env_remove("RELAYBOOK_CODEX_TIMEOUT_SECS");
}

fn run_with(dir: &Path, probes: &Probes, args: &[&str], codex_kind: &str) -> std::process::Output {
    let mut cmd = relaybook(dir);
    apply_probes(&mut cmd, probes);
    cmd.env("RELAYBOOK_FAKE_CODEX_KIND", codex_kind);
    cmd.args(args);
    cmd.output().expect("run")
}

fn db(dir: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("ledger")
}

fn set_setting(dir: &Path, key: &str, value: &str) {
    db(dir)
        .execute(
            "INSERT INTO settings (key, value, updated_at, updated_by)
             VALUES (?1, ?2, '2026-01-01T00:00:00.000Z', 'test')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
        .expect("setting");
}

fn text_of(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn event_count(dir: &Path) -> i64 {
    db(dir)
        .query_row("SELECT COUNT(*) FROM reviewer_events", [], |row| row.get(0))
        .expect("events")
}

fn claude_args(probes: &Probes) -> String {
    fs::read_to_string(probes.log.join("claude.args")).unwrap_or_default()
}

fn codex_args(probes: &Probes) -> String {
    fs::read_to_string(probes.log.join("codex.args")).unwrap_or_default()
}

/// codex-verification-v2.md row 4. The sentence has no `ERROR:` prefix.
#[test]
fn oversized_input_fixture_matches_v2_row_4() {
    let dir = tempfile::tempdir().expect("temp");
    let dest = dir
        .path()
        .join(format!("codex{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_fake"), &dest).expect("copy");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&dest).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&dest, permissions).expect("chmod");
    }
    let out = dir.path().join("out.md");
    let output = Command::new(&dest)
        .args(["exec", "-o", out.to_str().expect("utf8")])
        .env("RELAYBOOK_FAKE_CODEX_KIND", "too-large")
        .output()
        .expect("fake");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Input exceeds the maximum length of 1048576 characters (input_error_code input_too_large, max_chars 1048576, actual_chars 4194123)\n"
    );
    assert_eq!(fs::read_to_string(&out).unwrap_or_default(), "");
}

#[test]
fn ordinary_codex_failures_never_fall_back() {
    for kind in [
        "auth401",
        "too-large",
        "plain429",
        "garbage",
        "limit-stdout",
    ] {
        let repo = gated_repo();
        let probes = install_probes();
        let output = run_with(
            repo.path(),
            &probes,
            &["run", "--prd", "prd.md", "--gates", "none"],
            kind,
        );
        let text = text_of(&output);
        assert!(!output.status.success(), "{kind}: {text}");
        assert!(
            text.contains("two consecutive agent failures"),
            "{kind}: {text}"
        );
        assert_eq!(event_count(repo.path()), 0, "{kind}");
        assert!(
            !claude_args(&probes).contains("--tools"),
            "{kind}: {}",
            claude_args(&probes)
        );
    }
}

#[test]
fn timeout_and_vanished_codex_do_not_fall_back() {
    let repo = gated_repo();
    let probes = install_probes();
    let mut cmd = relaybook(repo.path());
    apply_probes(&mut cmd, &probes);
    cmd.env("RELAYBOOK_FAKE_CODEX_KIND", "sleep");
    cmd.env("RELAYBOOK_CODEX_TIMEOUT_SECS", "1");
    cmd.args(["run", "--prd", "prd.md", "--gates", "none"]);
    let output = cmd.output().expect("timeout");
    let text = text_of(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("timeout"), "{text}");
    assert_eq!(event_count(repo.path()), 0);
    assert!(!claude_args(&probes).contains("--tools"));

    let repo = gated_repo();
    let probes = install_probes();
    let mut cmd = relaybook(repo.path());
    apply_probes(&mut cmd, &probes);
    cmd.env("RELAYBOOK_FAKE_CODEX_VANISH", "1");
    cmd.env("PATH", path_without_codex(&probes.bin));
    cmd.args(["run", "--prd", "prd.md", "--gates", "none"]);
    let output = cmd.output().expect("vanished");
    let text = text_of(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("codex unavailable"), "{text}");
    assert_eq!(event_count(repo.path()), 0);
    assert!(!claude_args(&probes).contains("--tools"));
}

fn path_without_codex(bin: &Path) -> std::ffi::OsString {
    let mut path = std::ffi::OsString::from(bin);
    if let Some(rest) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&rest) {
            let name = format!("codex{}", std::env::consts::EXE_SUFFIX);
            if dir.join(&name).is_file() {
                continue;
            }
            path.push(if cfg!(windows) { ";" } else { ":" });
            path.push(&dir);
        }
    }
    path
}

#[test]
fn unverified_codex_version_never_falls_back() {
    let repo = gated_repo();
    let probes = install_probes();
    let mut cmd = relaybook(repo.path());
    apply_probes(&mut cmd, &probes);
    cmd.env("RELAYBOOK_FAKE_CODEX_KIND", "limit");
    cmd.env("RELAYBOOK_FAKE_CODEX_VERSION", "codex-cli 0.0.1");
    cmd.args(["run", "--prd", "prd.md", "--gates", "none"]);
    let output = cmd.output().expect("run");
    let text = text_of(&output);
    assert!(text.contains("two consecutive agent failures"), "{text}");
    assert_eq!(event_count(repo.path()), 0);
    assert!(!claude_args(&probes).contains("--tools"));
}

#[test]
fn limit_with_fallback_off_pauses_and_calls_no_other_reviewer() {
    let repo = gated_repo();
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    let text = text_of(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("usage limit reached on codex"), "{text}");
    let conn = db(repo.path());
    let (status, streak, reason): (String, i64, String) = conn
        .query_row(
            "SELECT status, failure_streak, stop_reason FROM runs ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("run");
    assert_eq!(status, "paused");
    assert_eq!(streak, 0);
    assert!(reason.starts_with("codex|"), "{reason}");
    assert!(!reason.contains("6:39"), "{reason}");
    let (source, after): (String, String) = conn
        .query_row(
            "SELECT retry_source, retry_after FROM reviewer_events WHERE kind = 'codex_limited'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("event");
    assert_eq!(source, "backoff");
    assert!(!after.contains("6:39"), "{after}");
    assert!(!claude_args(&probes).contains("--tools"));
    assert!(!probes.log.join("gh.args").exists());
    let resumed = run_with(repo.path(), &probes, &["run", "--resume"], "limit");
    let resumed_text = text_of(&resumed);
    assert!(!resumed.status.success(), "{resumed_text}");
    assert!(
        resumed_text.contains("codex limited until"),
        "{resumed_text}"
    );
    assert!(
        !codex_args(&probes).contains("Probe."),
        "{}",
        codex_args(&probes)
    );
}

#[test]
fn limit_falls_back_to_read_only_sonnet() {
    let repo = gated_repo();
    set_setting(repo.path(), "review_fallback", "sonnet");
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    let text = text_of(&output);
    assert!(
        text.contains("claude-sonnet-5-5 — fallback: codex usage limit until"),
        "{text}"
    );
    let args = claude_args(&probes);
    let reviewer = args
        .lines()
        .find(|line| line.contains(" --tools "))
        .unwrap_or_else(|| panic!("no reviewer argv\n{args}"));
    for flag in [
        "--model claude-sonnet-5-5",
        "--permission-mode dontAsk",
        "--tools Read,Grep,Glob",
        "--allowedTools Read,Grep,Glob",
        "--disallowedTools Edit,Write,NotebookEdit,Bash,WebFetch,WebSearch",
        "--no-session-persistence",
        "--output-format json",
        "--strict-mcp-config",
        "--restricted",
    ] {
        assert!(reviewer.contains(flag), "{flag} missing in {reviewer}");
    }
    assert!(!reviewer.split_whitespace().any(|arg| arg == "--mcp-config"));
    assert!(!reviewer.contains("--dangerously"));
    let conn = db(repo.path());
    let (reviewer_name, model, reason): (String, String, String) = conn
        .query_row(
            "SELECT reviewer, reviewer_model, fallback_reason FROM reviews
             WHERE reviewer = 'claude' AND verdict = 'approved'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("sonnet review");
    assert_eq!(reviewer_name, "claude");
    assert_eq!(model, "claude-sonnet-5-5");
    assert!(reason.starts_with("codex usage limit until "), "{reason}");
    let kinds: Vec<String> = conn
        .prepare("SELECT kind FROM reviewer_events ORDER BY id")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("map")
        .map(|row| row.expect("kind"))
        .collect();
    assert!(kinds.contains(&"codex_limited".to_string()), "{kinds:?}");
    assert!(
        kinds.contains(&"switched_to_fallback".to_string()),
        "{kinds:?}"
    );
}

#[test]
fn no_probe_before_backoff_and_probe_ok_switches_back() {
    let repo = gated_repo();
    set_setting(repo.path(), "review_fallback", "sonnet");
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit-then-ok",
    );
    let text = text_of(&output);
    assert!(text.contains("waiting for Codex re-review"), "{text}");
    assert!(
        !codex_args(&probes).contains("Probe."),
        "probed before the window:\n{}",
        codex_args(&probes)
    );
    db(repo.path())
        .execute(
            "UPDATE reviewer_events SET retry_after = strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 minutes')",
            [],
        )
        .expect("backdate");
    let _ = fs::remove_file(probes.log.join("codex.args"));
    let resumed = run_with(repo.path(), &probes, &["run", "--resume"], "limit-then-ok");
    let resumed_text = text_of(&resumed);
    assert!(resumed_text.contains("Codex probe ok"), "{resumed_text}");
    assert!(
        resumed_text.contains("Switched back to Codex"),
        "{resumed_text}"
    );
    assert!(
        codex_args(&probes).contains("Probe."),
        "{}",
        codex_args(&probes)
    );
    let kinds: Vec<String> = db(repo.path())
        .prepare("SELECT kind FROM reviewer_events ORDER BY id")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("map")
        .map(|row| row.expect("kind"))
        .collect();
    assert!(kinds.contains(&"probe_ok".to_string()), "{kinds:?}");
    assert!(
        kinds.contains(&"switched_to_codex".to_string()),
        "{kinds:?}"
    );
}

#[test]
fn probe_still_limited_extends_backoff_and_probe_ordinary_failure_halts() {
    let repo = gated_repo();
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    assert!(output.status.success(), "{}", text_of(&output));
    let first: String = db(repo.path())
        .query_row(
            "SELECT retry_after FROM reviewer_events WHERE kind = 'codex_limited'",
            [],
            |row| row.get(0),
        )
        .expect("first");
    db(repo.path())
        .execute(
            "UPDATE reviewer_events SET retry_after = strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 minutes')",
            [],
        )
        .expect("backdate");
    let resumed = run_with(repo.path(), &probes, &["run", "--resume"], "limit");
    let text = text_of(&resumed);
    assert!(text.contains("Codex still limited"), "{text}");
    let second: String = db(repo.path())
        .query_row(
            "SELECT retry_after FROM reviewer_events WHERE kind = 'probe_limited' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("second");
    assert!(second > first, "{second} vs {first}");

    let repo = gated_repo();
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    assert!(output.status.success(), "{}", text_of(&output));
    db(repo.path())
        .execute(
            "UPDATE reviewer_events SET retry_after = strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 minutes')",
            [],
        )
        .expect("backdate");
    let resumed = run_with(repo.path(), &probes, &["run", "--resume"], "plain429");
    let text = text_of(&resumed);
    assert!(
        text.contains("codex probe failed (not a usage limit)"),
        "{text}"
    );
    assert!(
        !claude_args(&probes).contains("--tools"),
        "{}",
        claude_args(&probes)
    );
}

#[test]
fn sonnet_approval_blocks_release_until_codex_rereview() {
    let repo = gated_repo();
    set_setting(repo.path(), "review_fallback", "sonnet");
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    let text = text_of(&output);
    assert!(text.contains("waiting for Codex re-review"), "{text}");
    let release_prompts = claude_args(&probes)
        .lines()
        .filter(|line| line.contains("Release review"))
        .count();
    assert_eq!(release_prompts, 0, "{}", claude_args(&probes));
    let status = relaybook(repo.path())
        .arg("status")
        .output()
        .expect("status");
    let status_text = text_of(&status);
    assert!(status_text.contains("pending"), "{status_text}");
    let head = head_sha(repo.path());
    let authorize = relaybook(repo.path())
        .args(["release", "authorize", "--sha", &head])
        .output()
        .expect("authorize");
    let authorize_text = text_of(&authorize);
    assert!(
        authorize_text.contains("codex re-review pending"),
        "{authorize_text}"
    );
    let approve = relaybook(repo.path())
        .arg("approve")
        .output()
        .expect("approve");
    let approve_text = text_of(&approve);
    assert!(
        approve_text.contains("codex re-review pending"),
        "{approve_text}"
    );
}

#[test]
fn codex_rereview_changes_requested_stops_run_and_off_does_not_block() {
    let repo = gated_repo();
    set_setting(repo.path(), "review_fallback", "sonnet");
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    assert!(
        text_of(&output).contains("waiting for Codex re-review"),
        "{}",
        text_of(&output)
    );
    db(repo.path())
        .execute(
            "UPDATE reviewer_events SET retry_after = strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 minutes')",
            [],
        )
        .expect("backdate");
    let resumed = run_with(
        repo.path(),
        &probes,
        &["run", "--resume"],
        "probe-ok-changes",
    );
    let text = text_of(&resumed);
    assert!(
        text.contains("codex re-review requested changes on work"),
        "{text}"
    );
    let status: String = db(repo.path())
        .query_row("SELECT status FROM tasks WHERE slug = 'work'", [], |row| {
            row.get(0)
        })
        .expect("task");
    assert_eq!(status, "done", "a re-review must not reopen the task");

    let repo = gated_repo();
    set_setting(repo.path(), "review_fallback", "sonnet");
    set_setting(repo.path(), "fallback_rereview", "off");
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    let text = text_of(&output);
    assert!(
        !text.contains("codex re-review pending") && !text.contains("waiting for Codex re-review"),
        "{text}"
    );
    assert!(
        claude_args(&probes).contains("Release review"),
        "{}",
        claude_args(&probes)
    );
    let head = head_sha(repo.path());
    let authorize = relaybook(repo.path())
        .args(["release", "authorize", "--sha", &head])
        .output()
        .expect("authorize");
    assert!(
        !text_of(&authorize).contains("codex re-review pending"),
        "{}",
        text_of(&authorize)
    );
}

#[test]
fn loosening_fallback_settings_needs_terminal_and_no_live_run() {
    let repo = gated_repo();
    let output = relaybook(repo.path())
        .args(["config", "set", "review_fallback", "sonnet"])
        .output()
        .expect("set");
    let text = text_of(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("confirmation at a TTY"), "{text}");
    let probes = install_probes();
    let paused = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    assert!(paused.status.success(), "{}", text_of(&paused));
    let live = relaybook(repo.path())
        .args(["config", "set", "review_fallback", "sonnet"])
        .output()
        .expect("live");
    assert!(
        text_of(&live).contains("a gated run is already live"),
        "{}",
        text_of(&live)
    );
}

#[cfg(unix)]
#[test]
fn loosening_fallback_accepts_the_typed_phrase_at_a_tty() {
    let repo = gated_repo();
    let head = head_sha(repo.path());
    let token = &head[..12];
    let phrase = format!("sonnet {token}\n");
    let output = pty_answer(
        repo.path(),
        &["config", "set", "review_fallback", "sonnet"],
        &phrase,
    );
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("review_fallback sonnet"), "{text}");
}

#[test]
fn classic_run_never_probes_or_falls_back() {
    let repo = origin_repo();
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    let text = text_of(&output);
    assert!(output.status.success(), "{text}");
    assert!(!codex_args(&probes).contains("Probe."));
    assert!(!probes.log.join("codex.args").exists() || codex_args(&probes).is_empty());
    assert_eq!(event_count(repo.path()), 0);
}

#[test]
fn stale_sonnet_invocation_cannot_record() {
    let repo = gated_repo();
    set_setting(repo.path(), "review_fallback", "sonnet");
    let probes = install_probes();
    let hold = probes.log.join("hold");
    fs::create_dir_all(&hold).expect("hold");
    let mut first = spawn_sonnet(
        repo.path(),
        &probes,
        &hold,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "first",
    );
    assert!(
        wait_for(&hold.join("started-1"), Duration::from_secs(45)),
        "sonnet did not start\n{}",
        held_output(&probes, "first")
    );
    let _ = relaybook(repo.path()).args(["run", "stop"]).output();
    let decision = relaybook(repo.path())
        .args([
            "decision",
            "log",
            "work",
            "--why",
            "replacement",
            "--by",
            "cursor",
        ])
        .output()
        .expect("decision");
    assert!(decision.status.success(), "{}", text_of(&decision));
    let mut second = spawn_sonnet(repo.path(), &probes, &hold, &["run", "--resume"], "second");
    assert!(
        wait_for(&hold.join("started-2"), Duration::from_secs(45)),
        "resume did not start a replacement review\n{}",
        held_output(&probes, "second")
    );
    fs::write(hold.join("release-1"), "go\n").expect("release");
    let finished = wait_child(&mut first, Duration::from_secs(30));
    let _ = second.kill();
    let _ = second.wait();
    let text = format!("{}\n{}", held_output(&probes, "first"), finished);
    assert!(text.contains("stale review invocation"), "{text}");
    let approved: i64 = db(repo.path())
        .query_row(
            "SELECT COUNT(*) FROM reviews WHERE reviewer = 'claude' AND verdict = 'approved'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert_eq!(approved, 0, "{text}");
}

#[test]
fn report_replaced_during_sonnet_review_is_not_accepted() {
    let repo = gated_repo();
    set_setting(repo.path(), "review_fallback", "sonnet");
    let probes = install_probes();
    let hold = probes.log.join("hold");
    fs::create_dir_all(&hold).expect("hold");
    let mut child = spawn_sonnet(
        repo.path(),
        &probes,
        &hold,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "held",
    );
    assert!(
        wait_for(&hold.join("started-1"), Duration::from_secs(45)),
        "sonnet did not start\n{}",
        held_output(&probes, "held")
    );
    let changed = db(repo.path())
        .execute(
            "UPDATE tasks SET updated_at = '2099-01-01T00:00:00.000Z' WHERE slug = 'work'",
            [],
        )
        .expect("replace");
    assert_eq!(changed, 1);
    fs::write(hold.join("release-1"), "go\n").expect("release");
    let _ = wait_child(&mut child, Duration::from_secs(30));
    let text = held_output(&probes, "held");
    assert!(
        text.contains("ledger changed during review") || text.contains("stale review invocation"),
        "{text}"
    );
    let approved: i64 = db(repo.path())
        .query_row(
            "SELECT COUNT(*) FROM reviews WHERE reviewer = 'claude' AND verdict = 'approved'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert_eq!(approved, 0, "{text}");
}

fn run_state(dir: &Path) -> (i64, String, String, String) {
    db(dir)
        .query_row(
            "SELECT id, branch, status, stage FROM runs ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("run")
}

fn origin_tip(dir: &Path, branch: &str) -> Option<String> {
    let output = Command::new("git")
        .current_dir(dir)
        .args([
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{branch}"),
        ])
        .output()
        .expect("rev-parse");
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn remote_checks(dir: &Path) -> i64 {
    db(dir)
        .query_row("SELECT COUNT(*) FROM remote_checks", [], |row| row.get(0))
        .expect("checks")
}

/// Sonnet approves the release while re-review is off. Tightening the policy
/// and stopping the run still blocks authorization, including a null run id.
#[test]
fn stopped_sonnet_release_blocks_later_authorization() {
    let repo = gated_repo();
    set_setting(repo.path(), "review_fallback", "sonnet");
    set_setting(repo.path(), "fallback_rereview", "off");
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    let text = text_of(&output);
    let approved: i64 = db(repo.path())
        .query_row(
            "SELECT COUNT(*) FROM reviews
             WHERE scope = 'release' AND reviewer = 'claude' AND verdict = 'approved'",
            [],
            |row| row.get(0),
        )
        .expect("reviews");
    assert_eq!(approved, 1, "{text}");
    set_setting(repo.path(), "fallback_rereview", "before-release");
    let _ = relaybook(repo.path()).args(["run", "stop"]).output();
    let (_run_id, _branch, status, _stage) = run_state(repo.path());
    assert_eq!(status, "stopped", "{text}");
    let head = head_sha(repo.path());
    let (review_id, review_sha): (i64, String) = db(repo.path())
        .query_row(
            "SELECT id, commit_sha FROM reviews
             WHERE scope = 'release' AND reviewer = 'claude' AND verdict = 'approved'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("review id");
    assert_eq!(review_sha, head, "{text}");
    let authorize = relaybook(repo.path())
        .args(["release", "authorize", "--sha", &head])
        .output()
        .expect("authorize");
    assert!(
        text_of(&authorize).contains("codex re-review pending"),
        "{}",
        text_of(&authorize)
    );
    let err = db(repo.path())
        .execute(
            "INSERT INTO release_authorizations (
                run_id, branch, commit_sha, review_id, authorized_by, created_at
             ) VALUES (NULL, 'feature', ?1, ?2, 'maintainer', '2026-01-01T00:00:00.000Z')",
            rusqlite::params![head, review_id],
        )
        .expect_err("null run id");
    assert!(err.to_string().contains("codex re-review pending"), "{err}");
}

#[test]
fn tightened_policy_blocks_resume_and_publish() {
    let repo = gated_repo();
    set_setting(repo.path(), "review_fallback", "sonnet");
    set_setting(repo.path(), "fallback_rereview", "off");
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    let text = text_of(&output);
    let (run_id, branch, status, stage) = run_state(repo.path());
    assert_eq!(status, "waiting", "{text}");
    assert_eq!(stage, "gate_release", "{text}");
    let head = head_sha(repo.path());
    let review_id: i64 = db(repo.path())
        .query_row(
            "SELECT id FROM reviews
             WHERE scope = 'release' AND reviewer = 'claude' AND verdict = 'approved'",
            [],
            |row| row.get(0),
        )
        .expect("review");
    db(repo.path())
        .execute(
            "INSERT INTO release_authorizations (
                run_id, branch, commit_sha, review_id, authorized_by, created_at
             ) VALUES (?1, ?2, ?3, ?4, 'maintainer', '2026-01-01T00:00:00.000Z')",
            rusqlite::params![run_id, branch, head, review_id],
        )
        .expect("authorize while off");
    set_setting(repo.path(), "fallback_rereview", "before-release");
    let before = origin_tip(repo.path(), &branch);
    let checks = remote_checks(repo.path());
    let resumed = run_with(repo.path(), &probes, &["run", "--resume"], "limit");
    let resumed_text = text_of(&resumed);
    assert!(
        resumed_text.contains("codex re-review pending"),
        "{resumed_text}"
    );
    let (_id, _branch, status, stage) = run_state(repo.path());
    assert_eq!(stage, "gate_release", "{resumed_text}");
    assert_eq!(status, "waiting", "{resumed_text}");
    assert!(!probes.log.join("gh.args").exists(), "{resumed_text}");
    assert_eq!(origin_tip(repo.path(), &branch), before, "{resumed_text}");

    db(repo.path())
        .execute(
            "UPDATE runs SET stage = 'publish', status = 'waiting', write_stamp = write_stamp + 1
             WHERE id = ?1",
            [run_id],
        )
        .expect("stage");
    let published = run_with(repo.path(), &probes, &["run", "--resume"], "limit");
    let published_text = text_of(&published);
    assert!(
        published_text.contains("codex re-review pending"),
        "{published_text}"
    );
    assert!(!probes.log.join("gh.args").exists(), "{published_text}");
    assert_eq!(origin_tip(repo.path(), &branch), before, "{published_text}");
    assert_eq!(remote_checks(repo.path()), checks, "{published_text}");
    let (_id, _branch, _status, stage) = run_state(repo.path());
    assert_ne!(stage, "publishing", "{published_text}");
}

#[test]
fn pull_request_body_names_completed_and_pending_coverage() {
    let repo = gated_repo();
    set_setting(repo.path(), "review_fallback", "sonnet");
    set_setting(repo.path(), "fallback_rereview", "off");
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    let text = text_of(&output);
    let (run_id, branch, status, stage) = run_state(repo.path());
    assert_eq!(status, "waiting", "{text}");
    assert_eq!(stage, "gate_release", "{text}");
    let head = head_sha(repo.path());
    let (task_review, task_id, release_review): (i64, i64, i64) = db(repo.path())
        .query_row(
            "SELECT task.id, task.task_id, release.id
             FROM reviews AS task
             JOIN reviews AS release ON release.run_id = task.run_id
             WHERE task.run_id = ?1
               AND task.scope = 'task'
               AND task.reviewer = 'claude'
               AND task.verdict = 'approved'
               AND release.scope = 'release'
               AND release.reviewer = 'claude'
               AND release.verdict = 'approved'",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("reviews");
    db(repo.path())
        .execute(
            "INSERT INTO reviews (
                scope, task_id, run_id, branch, reviewer, commit_sha, ledger_mark,
                verdict, started_at, rereview_of
             ) VALUES (
                'task', ?1, ?2, ?3, 'codex', ?4, 'mark',
                'approved', '2026-01-01T00:00:00.000Z', ?5
             )",
            rusqlite::params![task_id, run_id, branch, head, task_review],
        )
        .expect("rereview");
    db(repo.path())
        .execute(
            "INSERT INTO release_authorizations (
                run_id, branch, commit_sha, review_id, authorized_by, created_at
             ) VALUES (?1, ?2, ?3, ?4, 'maintainer', '2026-01-01T00:00:00.000Z')",
            rusqlite::params![run_id, branch, head, release_review],
        )
        .expect("authorize");
    let published = run_with(repo.path(), &probes, &["run", "--resume"], "limit");
    let published_text = text_of(&published);
    let body = fs::read_to_string(probes.log.join("gh.args")).unwrap_or_default();
    assert!(
        body.contains("Codex re-reviewed: approved"),
        "{published_text}\n{body}"
    );
    assert!(
        body.contains("awaiting Codex re-review"),
        "{published_text}\n{body}"
    );
    assert!(
        body.contains(&format!("re-review of {task_review}")),
        "{published_text}\n{body}"
    );
}

#[test]
fn probe_stops_when_the_ledger_changes_during_the_probe() {
    let repo = gated_repo();
    set_setting(repo.path(), "review_fallback", "sonnet");
    let probes = install_probes();
    let output = run_with(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "limit",
    );
    assert!(
        text_of(&output).contains("waiting for Codex re-review"),
        "{}",
        text_of(&output)
    );
    db(repo.path())
        .execute(
            "UPDATE reviewer_events SET retry_after = strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 minutes')",
            [],
        )
        .expect("backdate");
    let hold = probes.log.join("hold");
    fs::create_dir_all(&hold).expect("hold");
    let mut child = spawn_probe(repo.path(), &probes, &hold);
    assert!(
        wait_for(&hold.join("started-1"), Duration::from_secs(45)),
        "probe did not start\n{}",
        held_output(&probes, "probe")
    );
    let changed = db(repo.path())
        .execute(
            "UPDATE tasks SET updated_at = '2099-01-01T00:00:00.000Z'",
            [],
        )
        .expect("mutate");
    assert!(changed >= 1, "no task row to mutate");
    fs::write(hold.join("release-1"), "{\"ok\":true}\n").expect("release");
    let _ = wait_child(&mut child, Duration::from_secs(30));
    let text = held_output(&probes, "probe");
    assert!(text.contains("ledger changed during review"), "{text}");
    assert!(!text.contains("Codex probe ok"), "{text}");
    assert!(!text.contains("Switched back to Codex"), "{text}");
    let kinds: Vec<String> = db(repo.path())
        .prepare("SELECT kind FROM reviewer_events ORDER BY id")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("map")
        .map(|row| row.expect("kind"))
        .collect();
    assert!(kinds.contains(&"probe_failed".to_string()), "{kinds:?}");
    assert!(!kinds.contains(&"probe_ok".to_string()), "{kinds:?}");
    assert!(
        !kinds.contains(&"switched_to_codex".to_string()),
        "{kinds:?}"
    );
}

fn spawn_probe(dir: &Path, probes: &Probes, hold: &Path) -> std::process::Child {
    let mut cmd = relaybook(dir);
    apply_probes(&mut cmd, probes);
    cmd.env("RELAYBOOK_FAKE_CODEX_KIND", "hold");
    cmd.env("RELAYBOOK_CODEX_HOLD", hold);
    cmd.args(["run", "--resume"]);
    cmd.stdout(std::process::Stdio::from(
        fs::File::create(probes.log.join("probe.out")).expect("stdout"),
    ));
    cmd.stderr(std::process::Stdio::from(
        fs::File::create(probes.log.join("probe.err")).expect("stderr"),
    ));
    cmd.spawn().expect("spawn")
}

fn head_sha(dir: &Path) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("rev-parse");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn spawn_sonnet(
    dir: &Path,
    probes: &Probes,
    hold: &Path,
    args: &[&str],
    name: &str,
) -> std::process::Child {
    let mut cmd = relaybook(dir);
    apply_probes(&mut cmd, probes);
    cmd.env("RELAYBOOK_FAKE_CODEX_KIND", "limit");
    cmd.env("RELAYBOOK_CLAUDE_REVIEW_HOLD", hold);
    cmd.args(args);
    cmd.stdout(std::process::Stdio::from(
        fs::File::create(probes.log.join(format!("{name}.out"))).expect("stdout"),
    ));
    cmd.stderr(std::process::Stdio::from(
        fs::File::create(probes.log.join(format!("{name}.err"))).expect("stderr"),
    ));
    cmd.spawn().expect("spawn")
}

fn held_output(probes: &Probes, name: &str) -> String {
    format!(
        "{}{}",
        fs::read_to_string(probes.log.join(format!("{name}.out"))).unwrap_or_default(),
        fs::read_to_string(probes.log.join(format!("{name}.err"))).unwrap_or_default()
    )
}

fn wait_for(path: &Path, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if path.is_file() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn wait_child(child: &mut std::process::Child, limit: Duration) -> String {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return format!("exit {status}"),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                return "timed out".to_string();
            }
            Err(err) => return err.to_string(),
        }
    }
}

#[cfg(unix)]
fn pty_answer(dir: &Path, args: &[&str], phrase: &str) -> Vec<u8> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};

    let system = native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_relaybook"));
    for arg in args {
        cmd.arg(*arg);
    }
    cmd.cwd(dir);
    cmd.env("RELAYBOOK", env!("CARGO_BIN_EXE_relaybook"));
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    writer.write_all(phrase.as_bytes()).expect("write");
    let _ = writer.flush();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    buf
}
