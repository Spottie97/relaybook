use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::{Value, json};

const GATED_RUN_REFUSAL: &str = "relaybook run is not available with workflow=gated in this build";

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
    bare: PathBuf,
}

impl Repo {
    fn path(&self) -> &Path {
        self.work.path()
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
        bare,
        _origin: origin,
    }
}

fn enter_gated(dir: &Path) {
    relaybook(dir).arg("init").assert().success();
    relaybook(dir)
        .args(["config", "set", "workflow", "classic"])
        .assert()
        .success()
        .stdout("workflow classic\n");
    relaybook(dir)
        .args(["config", "set", "reviewer", "codex"])
        .assert()
        .success()
        .stdout("reviewer codex\n");
    relaybook(dir)
        .args(["config", "set", "workflow", "gated"])
        .assert()
        .success()
        .stdout("workflow gated\n");
}

struct Probes {
    _root: tempfile::TempDir,
    log: PathBuf,
    bin: PathBuf,
}

fn install_probes() -> Probes {
    let root = tempfile::tempdir().expect("probes");
    let log = root.path().join("log");
    let fakes = root.path().join("fakes");
    let bin = root.path().join("bin");
    fs::create_dir_all(&log).expect("log");
    fs::create_dir_all(&fakes).expect("fakes");
    fs::create_dir_all(&bin).expect("bin");
    for name in ["claude", "cursor-agent", "gh"] {
        copy_fake(&fakes, name);
    }
    #[cfg(unix)]
    {
        let git_bin = program_on_path("git");
        write_trace_wrapper(&bin.join("git"), "git", &git_bin);
        for name in ["claude", "cursor-agent", "gh"] {
            write_trace_wrapper(&bin.join(name), name, &fakes.join(name));
        }
    }
    #[cfg(not(unix))]
    {
        for name in ["claude", "cursor-agent", "gh"] {
            copy_fake(&bin, name);
        }
    }
    Probes {
        _root: root,
        log,
        bin,
    }
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

#[cfg(unix)]
fn program_on_path(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("missing {name} on PATH");
}

#[cfg(unix)]
fn write_trace_wrapper(path: &Path, name: &str, target: &Path) {
    let target = target.display();
    let script = format!(
        "#!/bin/sh\nif [ -z \"$RELAYBOOK_TRACE_LOG\" ]; then echo \"RELAYBOOK_TRACE_LOG is unset\" >&2; exit 99; fi\nprintf '%s\\n' \"{name} $*\" >> \"$RELAYBOOK_TRACE_LOG\"\nexec '{target}' \"$@\"\n"
    );
    fs::write(path, script).expect("wrapper");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(unix)]
fn write_idle_cursor(path: &Path, fake: &Path) {
    let fake = fake.display();
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ] || [ \"$1\" = \"status\" ] || [ \"$1\" = \"about\" ]; then\n  exec '{fake}' \"$@\"\nfi\nif [ -z \"$RELAYBOOK_EXECUTOR_HELD\" ] || [ -z \"$RELAYBOOK_EXECUTOR_RELEASE\" ]; then\n  echo \"executor hold is unset\" >&2\n  exit 99\nfi\n: > \"$RELAYBOOK_EXECUTOR_HELD\"\ni=0\nwhile [ ! -f \"$RELAYBOOK_EXECUTOR_RELEASE\" ]; do\n  i=$((i + 1))\n  if [ \"$i\" -gt 2000 ]; then echo \"executor hold timed out\" >&2; exit 98; fi\n  sleep 0.05\ndone\nexit 0\n"
    );
    fs::write(path, script).expect("wrapper");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(unix)]
fn write_hold_cursor(path: &Path, fake: &Path) {
    let fake = fake.display();
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ] || [ \"$1\" = \"status\" ] || [ \"$1\" = \"about\" ]; then\n  exec '{fake}' \"$@\"\nfi\nif [ -z \"$RELAYBOOK_EXECUTOR_HELD\" ] || [ -z \"$RELAYBOOK_EXECUTOR_RELEASE\" ]; then\n  echo \"executor hold is unset\" >&2\n  exit 99\nfi\n: > \"$RELAYBOOK_EXECUTOR_HELD\"\ni=0\nwhile [ ! -f \"$RELAYBOOK_EXECUTOR_RELEASE\" ]; do\n  i=$((i + 1))\n  if [ \"$i\" -gt 400 ]; then echo \"executor hold timed out\" >&2; exit 98; fi\n  sleep 0.05\ndone\ngit checkout main || exit 97\nexit 0\n"
    );
    fs::write(path, script).expect("wrapper");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(unix)]
fn write_push_gate_wrapper(path: &Path, target: &Path) {
    let target = target.display();
    let script = format!(
        "#!/bin/sh\nif [ -z \"$RELAYBOOK_TRACE_LOG\" ]; then echo \"RELAYBOOK_TRACE_LOG is unset\" >&2; exit 99; fi\nprintf '%s\\n' \"git $*\" >> \"$RELAYBOOK_TRACE_LOG\"\nif [ -n \"$RELAYBOOK_PUSH_GATE\" ]; then\n  for arg in \"$@\"; do\n    if [ \"$arg\" = \"push\" ]; then\n      i=0\n      while [ ! -f \"$RELAYBOOK_PUSH_GATE\" ]; do\n        i=$((i + 1))\n        if [ \"$i\" -gt 400 ]; then echo \"push gate timed out\" >&2; exit 98; fi\n        sleep 0.05\n      done\n      break\n    fi\n  done\nfi\nexec '{target}' \"$@\"\n"
    );
    fs::write(path, script).expect("wrapper");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

fn apply_probes(cmd: &mut Command, probes: &Probes) {
    let mut path = std::ffi::OsString::from(&probes.bin);
    if let Some(rest) = std::env::var_os("PATH") {
        path.push(if cfg!(windows) { ";" } else { ":" });
        path.push(rest);
    }
    cmd.env("PATH", path);
    cmd.env("LOG", &probes.log);
    cmd.env("RELAYBOOK_FAKE_KIND", "happy");
    cmd.env("RELAYBOOK_TRACE_LOG", probes.log.join("invocations"));
    cmd.env_remove("ANTHROPIC_API_KEY");
}

fn run_probed(dir: &Path, probes: &Probes, args: &[&str]) -> std::process::Output {
    let mut cmd = relaybook(dir);
    apply_probes(&mut cmd, probes);
    cmd.args(args);
    cmd.output().expect("run")
}

fn assert_agents_not_called(log: &Path) {
    for name in ["claude.args", "cursor.args", "gh.args"] {
        assert!(!log.join(name).exists(), "{name} was written");
    }
    #[cfg(unix)]
    {
        let text = fs::read_to_string(log.join("invocations")).unwrap_or_default();
        assert!(
            text.is_empty(),
            "git or agents ran before the refusal:\n{text}"
        );
    }
}

fn ref_missing(dir: &Path, rev: &str) -> bool {
    let output = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options", rev])
        .output()
        .expect("git");
    !output.status.success()
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

#[derive(Debug)]
struct RunRow {
    workflow: Option<String>,
    stage: String,
    status: String,
    stop_reason: Option<String>,
}

fn run_row(dir: &Path) -> RunRow {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    conn.query_row(
        "SELECT workflow, stage, status, stop_reason FROM runs ORDER BY id DESC LIMIT 1",
        [],
        |row| {
            Ok(RunRow {
                workflow: row.get(0)?,
                stage: row.get(1)?,
                status: row.get(2)?,
                stop_reason: row.get(3)?,
            })
        },
    )
    .expect("run row")
}

fn seed_run(dir: &Path, branch: &str, stage: &str, status: &str) -> i64 {
    let mut ledger =
        relaybook_core::Ledger::open(&dir.join(".relaybook").join("ledger.db")).expect("ledger");
    let mut run = ledger
        .start_run("prd.md", "abc", branch, Some("gated"))
        .expect("start run");
    run.stage = stage.to_string();
    run.status = status.to_string();
    ledger.save_run(&mut run).expect("save run");
    run.id
}

fn seed_stopped(dir: &Path, branch: &str, workflow: Option<&str>) -> i64 {
    let mut ledger =
        relaybook_core::Ledger::open(&dir.join(".relaybook").join("ledger.db")).expect("ledger");
    let mut run = ledger
        .start_run("prd.md", "abc", branch, workflow)
        .expect("start run");
    run.status = "stopped".to_string();
    ledger.save_run(&mut run).expect("save run");
    run.id
}

fn assign_run(dir: &Path, slug: &str, run_id: i64) {
    let mut ledger =
        relaybook_core::Ledger::open(&dir.join(".relaybook").join("ledger.db")).expect("ledger");
    assert!(
        ledger.assign_run(slug, run_id).expect("assign"),
        "{slug} was already linked"
    );
}

fn set_run_stage(dir: &Path, stage: &str) {
    let mut ledger =
        relaybook_core::Ledger::open(&dir.join(".relaybook").join("ledger.db")).expect("ledger");
    let mut run = ledger.active_run().expect("active").expect("run");
    run.stage = stage.to_string();
    ledger.save_run(&mut run).expect("save");
}

fn authorization_count(dir: &Path) -> i64 {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    conn.query_row("SELECT COUNT(*) FROM release_authorizations", [], |row| {
        row.get(0)
    })
    .expect("count")
}

fn run_status(dir: &Path, id: i64) -> (String, String) {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    conn.query_row(
        "SELECT status, stage FROM runs WHERE id = ?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .expect("run")
}

fn authorization_run_id(dir: &Path) -> Option<i64> {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    conn.query_row(
        "SELECT run_id FROM release_authorizations ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get(0),
    )
    .expect("authorization")
}

fn reviewed_release(repo: &Repo) -> (String, i64) {
    let dir = repo.path();
    git(dir, &["checkout", "-b", "relaybook/prd"]);
    fs::write(dir.join("ship.txt"), "ship\n").expect("ship");
    git(dir, &["add", "ship.txt"]);
    git(dir, &["commit", "-m", "ship"]);
    let head = head_sha(dir);
    relaybook(dir)
        .args(["review", "start", "--release"])
        .assert()
        .success();
    relaybook(dir)
        .args([
            "review",
            "record",
            "--release",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            RELEASE_REVIEW,
        ])
        .assert()
        .success();
    let run_id = seed_run(dir, "relaybook/prd", "gate_release", "waiting");
    (head, run_id)
}

fn bare_rev(bare: &Path, rev: &str) -> Option<String> {
    let output = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().expect("utf8"),
            "rev-parse",
            "--verify",
            "--end-of-options",
            rev,
        ])
        .output()
        .expect("rev-parse");
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn bare_relaybook_refs(bare: &Path) -> String {
    let output = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().expect("utf8"),
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/relaybook",
        ])
        .output()
        .expect("for-each-ref");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn head_branch(dir: &Path) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .expect("branch");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn setting(dir: &Path, key: &str) -> Value {
    let output = relaybook(dir)
        .args(["config", "--json"])
        .output()
        .expect("config");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    envelope["data"][key].clone()
}

fn assert_review_halt_never_pushes(dir: &Path, bare: &Path) {
    let probes = install_probes();
    let output = run_probed(dir, &probes, &["run", "--prd", "prd.md", "--gates", "none"]);
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}{err}");
    assert!(
        text.contains("codex routing not built"),
        "stdout={text}\nstderr={err}"
    );
    assert!(
        !text.contains(GATED_RUN_REFUSAL),
        "the skeleton must run, not refuse up front:\n{text}"
    );
    assert!(!probes.log.join("gh.args").exists(), "gh was invoked");
    assert!(ref_missing(bare, "refs/heads/relaybook/prd"));
    assert!(
        bare_relaybook_refs(bare).is_empty(),
        "a run branch was pushed"
    );
    let row = run_row(dir);
    assert_eq!(row.workflow.as_deref(), Some("gated"));
    assert_eq!(row.stage, "review");
    assert_eq!(row.status, "stopped");
    assert!(
        row.stop_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("codex routing not built")),
        "{row:?}"
    );
}

#[test]
fn gated_run_refuses_before_preflight_git_or_agents() {
    // R9 replaces the pre-preflight refusal. The no-push property stays.
    let repo = gated_repo();
    assert_review_halt_never_pushes(repo.path(), &repo.bare);
}

#[test]
fn gated_halt_never_pushes() {
    let repo = gated_repo();
    assert_review_halt_never_pushes(repo.path(), &repo.bare);
}

#[test]
fn gated_approve_and_resume_refuse() {
    let repo = gated_repo();
    for args in [&["approve"][..], &["run", "--resume"][..]] {
        let before = head_sha(repo.path());
        let probes = install_probes();
        let output = run_probed(repo.path(), &probes, args);
        let text = String::from_utf8_lossy(&output.stdout);
        let err = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{args:?}\n{text}{err}");
        assert!(
            text.contains("no run to resume"),
            "{args:?}\nstdout={text}\nstderr={err}"
        );
        assert!(!text.contains(GATED_RUN_REFUSAL), "{text}");
        assert!(!probes.log.join("gh.args").exists(), "{args:?} invoked gh");
        assert_eq!(head_branch(repo.path()), "main");
        assert_eq!(head_sha(repo.path()), before);
        assert!(ref_missing(repo.path(), "refs/heads/relaybook/prd"));
        assert!(ref_missing(&repo.bare, "refs/heads/relaybook/prd"));
        assert_eq!(setting(repo.path(), "workflow"), json!("gated"));
    }
}

#[test]
fn gated_stop_complete_watch_and_add_still_run() {
    let repo = gated_repo();
    let before = head_sha(repo.path());
    let cases = [
        (vec!["run", "stop"], "no active run"),
        (vec!["run", "complete"], "no active run"),
        (vec!["run", "watch"], "no run to watch"),
        (vec!["run", "add", "login"], "no active run"),
    ];
    for (args, message) in cases {
        let probes = install_probes();
        let output = run_probed(repo.path(), &probes, &args);
        let text = String::from_utf8_lossy(&output.stdout);
        let err = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{args:?}\n{text}{err}");
        assert!(text.contains(message), "{args:?}\n{text}{err}");
        assert!(
            !text.contains(GATED_RUN_REFUSAL),
            "{args:?} was refused:\n{text}"
        );
        assert_agents_not_called(&probes.log);
    }
    assert_eq!(head_sha(repo.path()), before);
    assert!(ref_missing(repo.path(), "refs/heads/relaybook/prd"));
    assert!(ref_missing(&repo.bare, "refs/heads/relaybook/prd"));
}

#[test]
fn leaving_gated_requires_terminal_confirmation() {
    let dir = tempfile::tempdir().expect("temp");
    enter_gated(dir.path());

    relaybook(dir.path())
        .args(["config", "set", "workflow", "classic"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains(
            "leaving workflow=gated requires confirmation at a TTY",
        ));
    assert_eq!(setting(dir.path(), "workflow"), json!("gated"));
    assert_eq!(setting(dir.path(), "reviewer"), json!("codex"));

    let declined = answer_at_tty(dir.path(), &["config", "set", "workflow", "classic"], false);
    assert!(!declined.success, "{}", declined.output);
    assert_eq!(declined.code, 1, "{}", declined.output);
    assert!(
        declined.output.contains("Leave gated workflow?"),
        "{}",
        declined.output
    );
    assert!(
        declined
            .output
            .contains("leaving workflow=gated was not confirmed"),
        "{}",
        declined.output
    );
    assert_eq!(setting(dir.path(), "workflow"), json!("gated"));

    let accepted = answer_at_tty(dir.path(), &["config", "set", "workflow", "classic"], true);
    assert!(accepted.success, "{}", accepted.output);
    assert!(
        accepted.output.contains("workflow classic"),
        "{}",
        accepted.output
    );
    assert_eq!(setting(dir.path(), "workflow"), json!("classic"));

    relaybook(dir.path())
        .args(["config", "set", "workflow", "classic"])
        .assert()
        .success()
        .stdout("workflow classic\n");
    relaybook(dir.path())
        .args(["config", "set", "reviewer", "claude"])
        .assert()
        .success()
        .stdout("reviewer claude\n");
    assert_eq!(setting(dir.path(), "reviewer"), json!("claude"));
    assert_eq!(setting(dir.path(), "workflow"), json!("classic"));
}

#[test]
fn leaving_gated_rejects_padded_classic_without_a_tty() {
    let dir = tempfile::tempdir().expect("temp");
    enter_gated(dir.path());

    for value in [" classic", "classic ", " classic ", "\tclassic\n"] {
        relaybook(dir.path())
            .args(["config", "set", "workflow", value])
            .assert()
            .failure()
            .code(1)
            .stderr(predicates::str::contains(
                "leaving workflow=gated requires confirmation at a TTY",
            ));
        assert_eq!(setting(dir.path(), "workflow"), json!("gated"));
        assert_eq!(setting(dir.path(), "reviewer"), json!("codex"));
    }

    let accepted = answer_at_tty(
        dir.path(),
        &["config", "set", "workflow", " classic "],
        true,
    );
    assert!(accepted.success, "{}", accepted.output);
    assert!(
        accepted.output.contains("Leave gated workflow?"),
        "{}",
        accepted.output
    );
    assert!(
        accepted.output.contains("workflow classic"),
        "{}",
        accepted.output
    );
    assert_eq!(setting(dir.path(), "workflow"), json!("classic"));

    relaybook(dir.path())
        .args(["config", "set", "workflow", " classic "])
        .assert()
        .success()
        .stdout("workflow classic\n");
    assert_eq!(setting(dir.path(), "workflow"), json!("classic"));
}

#[test]
fn entering_gated_needs_no_terminal() {
    let dir = tempfile::tempdir().expect("temp");
    enter_gated(dir.path());
    assert_eq!(setting(dir.path(), "workflow"), json!("gated"));
    assert_eq!(setting(dir.path(), "reviewer"), json!("codex"));

    relaybook(dir.path())
        .args(["config", "set", "workflow", "gated"])
        .assert()
        .success()
        .stdout("workflow gated\n");
    relaybook(dir.path())
        .args(["mode", "auto"])
        .assert()
        .success()
        .stdout("mode auto\n");
    relaybook(dir.path())
        .args(["mode", "manual"])
        .assert()
        .success()
        .stdout("mode manual\n");
    assert_eq!(setting(dir.path(), "workflow"), json!("gated"));
    assert_eq!(setting(dir.path(), "mode"), json!("manual"));
}

const OWNED: &str = "a gated run owns this; use relaybook approve / run --resume";
const RELEASE_REVIEW: &str = r#"{"findings":"none","security":"ok","tests_checked":"cargo test","not_checked":"none","prd_covered":true,"gaps":"none"}"#;

#[test]
fn gated_run_freezes_workflow() {
    let repo = gated_repo();
    let probes = install_probes();
    let output = run_probed(repo.path(), &probes, &["run", "--prd", "prd.md"]);
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    assert!(
        text.contains("waiting for approval: relaybook approve"),
        "{text}"
    );
    assert!(!probes.log.join("gh.args").exists());
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
    assert_eq!(run_row(repo.path()).workflow.as_deref(), Some("gated"));

    relaybook(repo.path())
        .args(["config", "set", "workflow", "classic"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains(OWNED));
    assert_eq!(setting(repo.path(), "workflow"), json!("gated"));

    let stopped = run_probed(repo.path(), &probes, &["run", "stop"]);
    let stopped_text = String::from_utf8_lossy(&stopped.stdout);
    assert_eq!(stopped.status.code(), Some(1), "{stopped_text}");
    assert!(
        stopped_text.contains("stopped by request"),
        "{stopped_text}"
    );
    assert_eq!(run_row(repo.path()).status, "stopped");

    let left = answer_at_tty(repo.path(), &["config", "set", "workflow", "classic"], true);
    assert!(left.success, "{}", left.output);
    assert_eq!(setting(repo.path(), "workflow"), json!("classic"));
    assert_eq!(run_row(repo.path()).workflow.as_deref(), Some("gated"));

    let resume = install_probes();
    let output = run_probed(repo.path(), &resume, &["run", "--resume"]);
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}{err}");
    assert!(
        text.contains("workflow changed during run"),
        "stdout={text}\nstderr={err}"
    );
    assert!(!resume.log.join("gh.args").exists());
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
}

#[test]
fn approve_at_gate_release_requires_authorization_at_head() {
    let repo = gated_repo();
    let (head, run_id) = reviewed_release(&repo);
    let probes = install_probes();
    let output = run_probed(repo.path(), &probes, &["approve"]);
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}{err}");
    assert!(
        text.contains(&format!(
            "waiting for sign-off: relaybook release authorize --sha {head}"
        )),
        "stdout={text}\nstderr={err}"
    );
    assert!(!probes.log.join("gh.args").exists());
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
    assert_eq!(run_row(repo.path()).status, "waiting");

    let authorized = answer_at_tty(repo.path(), &["release", "authorize", "--sha", &head], true);
    assert!(authorized.success, "{}", authorized.output);
    assert_eq!(authorization_run_id(repo.path()), Some(run_id));

    let probes = install_probes();
    let output = run_probed(repo.path(), &probes, &["approve"]);
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    assert!(text.contains("opened https://example.test/pr/1"), "{text}");
    let gh = fs::read_to_string(probes.log.join("gh.args")).expect("gh.args");
    assert_eq!(
        gh.lines().filter(|line| line.contains("pr create")).count(),
        1,
        "{gh}"
    );
    assert_eq!(
        bare_rev(&repo.bare, "refs/heads/relaybook/prd").as_deref(),
        Some(head.as_str())
    );
}

#[test]
fn publish_refuses_when_head_moved_after_authorization() {
    let repo = gated_repo();
    let (head, _) = reviewed_release(&repo);
    let authorized = answer_at_tty(repo.path(), &["release", "authorize", "--sha", &head], true);
    assert!(authorized.success, "{}", authorized.output);
    set_run_stage(repo.path(), "publish");
    fs::write(repo.path().join("later.txt"), "later\n").expect("later");
    git(repo.path(), &["add", "later.txt"]);
    git(repo.path(), &["commit", "-m", "move HEAD"]);
    let moved = head_sha(repo.path());
    assert_ne!(moved, head);

    let probes = install_probes();
    let output = run_probed(repo.path(), &probes, &["approve"]);
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}{err}");
    assert!(
        text.contains("HEAD moved after authorization"),
        "stdout={text}\nstderr={err}"
    );
    assert!(!probes.log.join("gh.args").exists());
    assert!(bare_rev(&repo.bare, "refs/heads/relaybook/prd").is_none());
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
}

#[test]
fn manual_review_and_accept_refuse_on_live_gated_run_tasks() {
    let repo = gated_repo();
    relaybook(repo.path())
        .args(["task", "create", "work", "--title", "Work"])
        .assert()
        .success();
    relaybook(repo.path())
        .args(["task", "create", "other", "--title", "Other"])
        .assert()
        .success();
    let run_id = seed_run(repo.path(), "main", "execute", "active");
    assign_run(repo.path(), "work", run_id);

    for args in [
        &["review", "start", "work"][..],
        &[
            "review",
            "record",
            "work",
            "--by",
            "codex",
            "--sha",
            "abc",
            "--verdict",
            "approved",
            "--text",
            "{}",
        ][..],
        &["report", "accept", "work"][..],
        &["review", "start", "--release"][..],
        &[
            "review",
            "record",
            "--release",
            "--by",
            "codex",
            "--sha",
            "abc",
            "--verdict",
            "approved",
            "--text",
            RELEASE_REVIEW,
        ][..],
        &["config", "set", "workflow", "classic"][..],
    ] {
        relaybook(repo.path())
            .args(args)
            .assert()
            .failure()
            .code(1)
            .stderr(predicates::str::contains(OWNED));
    }
    assert_eq!(setting(repo.path(), "workflow"), json!("gated"));
    assert_eq!(run_row(repo.path()).status, "active");
    relaybook(repo.path())
        .args(["release", "status"])
        .assert()
        .success();
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
}

fn unpublished_branch(dir: &Path) {
    git(dir, &["checkout", "-b", "relaybook/prd"]);
    fs::write(dir.join("ship.txt"), "ship\n").expect("ship");
    git(dir, &["add", "ship.txt"]);
    git(dir, &["commit", "-m", "ship"]);
}

#[test]
fn resume_stops_classic_or_unset_workflow_without_pushing() {
    for workflow in [Some("classic"), None] {
        let repo = origin_repo();
        unpublished_branch(repo.path());
        seed_stopped(repo.path(), "relaybook/prd", workflow);
        relaybook(repo.path())
            .args(["config", "set", "reviewer", "codex"])
            .assert()
            .success();
        relaybook(repo.path())
            .args(["config", "set", "workflow", "gated"])
            .assert()
            .success();
        let probes = install_probes();
        let output = run_probed(repo.path(), &probes, &["run", "--resume"]);
        let text = String::from_utf8_lossy(&output.stdout);
        let err = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{workflow:?}\n{text}{err}");
        assert!(
            text.contains("workflow changed during run"),
            "{workflow:?}\nstdout={text}\nstderr={err}"
        );
        assert!(!probes.log.join("gh.args").exists(), "{workflow:?}");
        assert!(
            bare_rev(&repo.bare, "refs/heads/relaybook/prd").is_none(),
            "{workflow:?} pushed the run branch"
        );
        let row = run_row(repo.path());
        assert_eq!(row.status, "stopped");
        assert_eq!(row.workflow.as_deref(), workflow);
        assert!(
            row.stop_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("workflow changed during run")),
            "{row:?}"
        );
        #[cfg(unix)]
        {
            let trace = fs::read_to_string(probes.log.join("invocations")).unwrap_or_default();
            assert!(
                !trace.lines().any(|line| line.starts_with("git push ")),
                "{workflow:?}\n{trace}"
            );
        }
    }
}

#[test]
fn gated_publish_does_not_follow_annotated_tags() {
    let repo = gated_repo();
    let (head, _) = reviewed_release(&repo);
    git(repo.path(), &["tag", "-a", "signoff", "-m", "signoff"]);
    git(repo.path(), &["config", "push.followTags", "true"]);
    let authorized = answer_at_tty(repo.path(), &["release", "authorize", "--sha", &head], true);
    assert!(authorized.success, "{}", authorized.output);
    let probes = install_probes();
    let output = run_probed(repo.path(), &probes, &["approve"]);
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    assert_eq!(
        bare_rev(&repo.bare, "refs/heads/relaybook/prd").as_deref(),
        Some(head.as_str())
    );
    assert!(
        bare_rev(&repo.bare, "refs/tags/signoff").is_none(),
        "annotated tag was published"
    );
    let tags = Command::new("git")
        .args([
            "--git-dir",
            repo.bare.to_str().expect("utf8"),
            "for-each-ref",
            "--format=%(refname)",
            "refs/tags",
        ])
        .output()
        .expect("for-each-ref");
    assert!(tags.status.success());
    assert!(
        String::from_utf8_lossy(&tags.stdout).trim().is_empty(),
        "tags on bare: {}",
        String::from_utf8_lossy(&tags.stdout)
    );
    #[cfg(unix)]
    {
        let trace = fs::read_to_string(probes.log.join("invocations")).expect("trace");
        assert!(
            trace
                .lines()
                .any(|line| line.starts_with("git push ") && line.contains("--no-follow-tags")),
            "{trace}"
        );
    }
}

#[cfg(unix)]
#[test]
fn concurrent_approve_pushes_once_and_keeps_complete() {
    use std::process::Stdio;
    use std::thread;
    use std::time::{Duration, Instant};

    let repo = gated_repo();
    let (head, _) = reviewed_release(&repo);
    let authorized = answer_at_tty(repo.path(), &["release", "authorize", "--sha", &head], true);
    assert!(authorized.success, "{}", authorized.output);
    let probes = install_probes();
    write_push_gate_wrapper(&probes.bin.join("git"), &program_on_path("git"));
    let gate = probes.log.join("allow-push");
    struct ReleasePush(PathBuf);
    impl Drop for ReleasePush {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, "go\n");
        }
    }
    let _release = ReleasePush(gate.clone());

    let spawn = |dir: &Path, probes: &Probes, gate: &Path| {
        let mut cmd = relaybook(dir);
        apply_probes(&mut cmd, probes);
        cmd.env("RELAYBOOK_PUSH_GATE", gate);
        cmd.arg("approve");
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.spawn().expect("spawn approve")
    };
    let mut children = vec![
        spawn(repo.path(), &probes, &gate),
        spawn(repo.path(), &probes, &gate),
    ];
    let trace_path = probes.log.join("invocations");
    let pushes = |text: &str| {
        text.lines()
            .filter(|line| line.starts_with("git push "))
            .count()
    };
    let started = Instant::now();
    loop {
        let text = fs::read_to_string(&trace_path).unwrap_or_default();
        let seen = pushes(&text);
        let mut exited = 0;
        for child in &mut children {
            if matches!(child.try_wait(), Ok(Some(_))) {
                exited += 1;
            }
        }
        if seen >= 2 || (seen == 1 && exited >= 1) || started.elapsed() > Duration::from_secs(20) {
            thread::sleep(Duration::from_millis(300));
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let blocked = fs::read_to_string(&trace_path).unwrap_or_default();
    assert_eq!(
        pushes(&blocked),
        1,
        "both approves reached push before either finished:\n{blocked}"
    );
    fs::write(&gate, "go\n").expect("release push");
    let mut outputs = Vec::new();
    for child in children {
        let output = child.wait_with_output().expect("wait");
        outputs.push(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let trace = fs::read_to_string(&trace_path).unwrap_or_default();
    assert_eq!(pushes(&trace), 1, "{trace}");
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.starts_with("gh pr create"))
            .count(),
        1,
        "{trace}"
    );
    assert!(
        outputs
            .iter()
            .any(|text| text.contains("run changed concurrently")),
        "{outputs:?}"
    );
    assert!(
        outputs
            .iter()
            .any(|text| text.contains("opened https://example.test/pr/1")),
        "{outputs:?}"
    );
    let row = run_row(repo.path());
    assert_eq!(row.status, "complete", "{row:?}");
    assert_eq!(row.stage, "done", "{row:?}");
    assert_eq!(
        bare_rev(&repo.bare, "refs/heads/relaybook/prd").as_deref(),
        Some(head.as_str())
    );
}

#[cfg(unix)]
#[test]
fn guard_after_switching_to_gated_during_execute_does_not_push() {
    use std::process::Stdio;
    use std::thread;
    use std::time::{Duration, Instant};

    let repo = origin_repo();
    relaybook(repo.path())
        .args(["config", "set", "gates", "none"])
        .assert()
        .success();
    let probes = install_probes();
    let role = probes.log.join("cursor-agent");
    fs::copy(env!("CARGO_BIN_EXE_fake"), &role).expect("copy cursor");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&role).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&role, permissions).expect("chmod");
    }
    write_hold_cursor(&probes.bin.join("cursor-agent"), &role);
    let held = probes.log.join("executor-held");
    let release = probes.log.join("executor-release");
    let mut cmd = relaybook(repo.path());
    apply_probes(&mut cmd, &probes);
    cmd.env("RELAYBOOK_EXECUTOR_HELD", &held);
    cmd.env("RELAYBOOK_EXECUTOR_RELEASE", &release);
    cmd.args(["run", "--prd", "prd.md"]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn run");
    let started = Instant::now();
    while !held.exists() {
        if started.elapsed() > Duration::from_secs(15) || child.try_wait().ok().flatten().is_some()
        {
            let _ = child.kill();
            let output = child.wait_with_output().expect("wait");
            panic!(
                "executor was not held\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(30));
    }
    relaybook(repo.path())
        .args(["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    relaybook(repo.path())
        .args(["config", "set", "workflow", "gated"])
        .assert()
        .success();
    fs::write(&release, "go\n").expect("release executor");
    let output = child.wait_with_output().expect("wait run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("workflow changed during run"), "{text}");
    assert!(
        bare_rev(&repo.bare, "refs/heads/relaybook/prd").is_none(),
        "halt pushed the run branch"
    );
    assert!(!probes.log.join("gh.args").exists(), "{text}");
    let row = run_row(repo.path());
    assert_eq!(row.status, "stopped", "{row:?}");
    assert_eq!(row.workflow.as_deref(), Some("classic"), "{row:?}");
    assert!(
        row.stop_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("workflow changed during run")),
        "{row:?}"
    );
    assert_eq!(
        head_branch(repo.path()),
        "main",
        "executor did not switch branches, so the guard halt was not reached"
    );
    #[cfg(unix)]
    {
        let trace = fs::read_to_string(probes.log.join("invocations")).unwrap_or_default();
        assert!(
            !trace.lines().any(|line| line.starts_with("git push ")),
            "{trace}"
        );
    }
}

#[cfg(unix)]
#[test]
fn resume_during_publish_does_not_push() {
    use std::process::Stdio;
    use std::thread;
    use std::time::{Duration, Instant};

    let repo = gated_repo();
    let older = seed_stopped(repo.path(), "relaybook/prd", Some("gated"));
    let (head, newer) = reviewed_release(&repo);
    assert!(older < newer);
    let authorized = answer_at_tty(repo.path(), &["release", "authorize", "--sha", &head], true);
    assert!(authorized.success, "{}", authorized.output);
    let probes = install_probes();
    write_push_gate_wrapper(&probes.bin.join("git"), &program_on_path("git"));
    let gate = probes.log.join("allow-push");
    struct ReleasePush(PathBuf);
    impl Drop for ReleasePush {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, "go\n");
        }
    }
    let _release = ReleasePush(gate.clone());

    let mut approve = relaybook(repo.path());
    apply_probes(&mut approve, &probes);
    approve.env("RELAYBOOK_PUSH_GATE", &gate);
    approve.arg("approve");
    approve.stdout(Stdio::piped());
    approve.stderr(Stdio::piped());
    let mut approve = approve.spawn().expect("spawn approve");
    let trace_path = probes.log.join("invocations");
    let pushes = |text: &str| {
        text.lines()
            .filter(|line| line.starts_with("git push "))
            .count()
    };
    let started = Instant::now();
    loop {
        let text = fs::read_to_string(&trace_path).unwrap_or_default();
        if pushes(&text) >= 1 || started.elapsed() > Duration::from_secs(20) {
            break;
        }
        if matches!(approve.try_wait(), Ok(Some(_))) {
            break;
        }
        thread::sleep(Duration::from_millis(30));
    }
    let held = fs::read_to_string(&trace_path).unwrap_or_default();
    assert_eq!(pushes(&held), 1, "approve did not reach push:\n{held}");

    let mut resume = relaybook(repo.path());
    apply_probes(&mut resume, &probes);
    resume.env("RELAYBOOK_PUSH_GATE", &gate);
    resume.args(["run", "--resume"]);
    resume.stdout(Stdio::piped());
    resume.stderr(Stdio::piped());
    let mut resume = resume.spawn().expect("spawn resume");
    let started = Instant::now();
    let resume = loop {
        let text = fs::read_to_string(&trace_path).unwrap_or_default();
        let exited = matches!(resume.try_wait(), Ok(Some(_)));
        if exited || pushes(&text) >= 2 || started.elapsed() > Duration::from_secs(15) {
            break resume;
        }
        thread::sleep(Duration::from_millis(30));
    };
    fs::write(&gate, "go\n").expect("release push");
    let resume_output = resume.wait_with_output().expect("wait resume");
    let approve_output = approve.wait_with_output().expect("wait approve");
    let resume_text = format!(
        "{}{}",
        String::from_utf8_lossy(&resume_output.stdout),
        String::from_utf8_lossy(&resume_output.stderr)
    );
    let approve_text = format!(
        "{}{}",
        String::from_utf8_lossy(&approve_output.stdout),
        String::from_utf8_lossy(&approve_output.stderr)
    );
    let trace = fs::read_to_string(&trace_path).unwrap_or_default();
    assert_eq!(
        pushes(&trace),
        1,
        "{trace}\nresume={resume_text}\napprove={approve_text}"
    );
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.starts_with("gh pr create"))
            .count(),
        1,
        "{trace}"
    );
    assert!(
        resume_text.contains("a run is already publishing"),
        "{resume_text}"
    );
    assert!(
        approve_text.contains("opened https://example.test/pr/1"),
        "{approve_text}"
    );
    let (older_status, _) = run_status(repo.path(), older);
    assert_eq!(older_status, "stopped", "{resume_text}");
    let (newer_status, newer_stage) = run_status(repo.path(), newer);
    assert_eq!(newer_status, "complete", "{approve_text}");
    assert_eq!(newer_stage, "done");
    assert_eq!(
        bare_rev(&repo.bare, "refs/heads/relaybook/prd").as_deref(),
        Some(head.as_str())
    );
}

#[cfg(unix)]
#[test]
fn stop_then_resume_during_publish_pushes_once() {
    use std::process::Stdio;
    use std::thread;
    use std::time::{Duration, Instant};

    let repo = gated_repo();
    let older = seed_stopped(repo.path(), "relaybook/prd", Some("gated"));
    let (head, newer) = reviewed_release(&repo);
    assert!(older < newer);
    let authorized = answer_at_tty(repo.path(), &["release", "authorize", "--sha", &head], true);
    assert!(authorized.success, "{}", authorized.output);
    let probes = install_probes();
    write_push_gate_wrapper(&probes.bin.join("git"), &program_on_path("git"));
    let gate = probes.log.join("allow-push");
    struct ReleasePush(PathBuf);
    impl Drop for ReleasePush {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, "go\n");
        }
    }
    let _release = ReleasePush(gate.clone());

    let mut approve = relaybook(repo.path());
    apply_probes(&mut approve, &probes);
    approve.env("RELAYBOOK_PUSH_GATE", &gate);
    approve.arg("approve");
    approve.stdout(Stdio::piped());
    approve.stderr(Stdio::piped());
    let mut approve = approve.spawn().expect("spawn approve");
    let trace_path = probes.log.join("invocations");
    let pushes = |text: &str| {
        text.lines()
            .filter(|line| line.starts_with("git push "))
            .count()
    };
    let started = Instant::now();
    loop {
        let text = fs::read_to_string(&trace_path).unwrap_or_default();
        if pushes(&text) >= 1 || started.elapsed() > Duration::from_secs(20) {
            break;
        }
        if matches!(approve.try_wait(), Ok(Some(_))) {
            break;
        }
        thread::sleep(Duration::from_millis(30));
    }
    let held = fs::read_to_string(&trace_path).unwrap_or_default();
    assert_eq!(pushes(&held), 1, "approve did not reach push:\n{held}");

    let stop = relaybook(repo.path())
        .args(["run", "stop"])
        .output()
        .expect("stop");
    let stop_text = format!(
        "{}{}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr)
    );
    assert_eq!(stop.status.code(), Some(1), "{stop_text}");
    assert!(
        stop_text.contains("a publish claim is in progress; stop does not release it"),
        "{stop_text}"
    );
    let (held_status, held_stage) = run_status(repo.path(), newer);
    assert_eq!(held_status, "active", "{stop_text}");
    assert_eq!(held_stage, "publishing", "{stop_text}");

    let mut resume = relaybook(repo.path());
    apply_probes(&mut resume, &probes);
    resume.env("RELAYBOOK_PUSH_GATE", &gate);
    resume.args(["run", "--resume"]);
    resume.stdout(Stdio::piped());
    resume.stderr(Stdio::piped());
    let mut resume = resume.spawn().expect("spawn resume");
    let started = Instant::now();
    let resume = loop {
        let text = fs::read_to_string(&trace_path).unwrap_or_default();
        let exited = matches!(resume.try_wait(), Ok(Some(_)));
        if exited || pushes(&text) >= 2 || started.elapsed() > Duration::from_secs(15) {
            break resume;
        }
        thread::sleep(Duration::from_millis(30));
    };
    fs::write(&gate, "go\n").expect("release push");
    let resume_output = resume.wait_with_output().expect("wait resume");
    let approve_output = approve.wait_with_output().expect("wait approve");
    let resume_text = format!(
        "{}{}",
        String::from_utf8_lossy(&resume_output.stdout),
        String::from_utf8_lossy(&resume_output.stderr)
    );
    let approve_text = format!(
        "{}{}",
        String::from_utf8_lossy(&approve_output.stdout),
        String::from_utf8_lossy(&approve_output.stderr)
    );
    let trace = fs::read_to_string(&trace_path).unwrap_or_default();
    assert_eq!(
        pushes(&trace),
        1,
        "{trace}\nresume={resume_text}\napprove={approve_text}"
    );
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.starts_with("gh pr create"))
            .count(),
        1,
        "{trace}"
    );
    assert!(
        resume_text.contains("a run is already publishing"),
        "{resume_text}"
    );
    assert!(
        approve_text.contains("opened https://example.test/pr/1"),
        "{approve_text}"
    );
    let (older_status, _) = run_status(repo.path(), older);
    assert_eq!(older_status, "stopped", "{resume_text}");
    let (newer_status, newer_stage) = run_status(repo.path(), newer);
    assert_eq!(newer_status, "complete", "{approve_text}");
    assert_eq!(newer_stage, "done");
    assert_eq!(
        bare_rev(&repo.bare, "refs/heads/relaybook/prd").as_deref(),
        Some(head.as_str())
    );
}

#[test]
fn older_gated_run_cannot_resume_while_a_newer_one_is_live() {
    let repo = gated_repo();
    let older = seed_stopped(repo.path(), "relaybook/prd", Some("gated"));
    let newer = seed_run(repo.path(), "relaybook/prd", "execute", "active");
    assert!(older < newer);
    let probes = install_probes();
    let output = run_probed(repo.path(), &probes, &["run", "--resume"]);
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}{err}");
    assert!(
        text.contains("a gated run is already live"),
        "stdout={text}\nstderr={err}"
    );
    assert!(!probes.log.join("gh.args").exists(), "{text}{err}");
    let (older_status, _) = run_status(repo.path(), older);
    assert_eq!(older_status, "stopped", "{text}{err}");
    let (newer_status, newer_stage) = run_status(repo.path(), newer);
    assert_eq!(newer_status, "active", "{text}{err}");
    assert_eq!(newer_stage, "execute");
    assert!(bare_rev(&repo.bare, "refs/heads/relaybook/prd").is_none());
    #[cfg(unix)]
    {
        let trace = fs::read_to_string(probes.log.join("invocations")).unwrap_or_default();
        assert!(
            !trace.lines().any(|line| line.starts_with("git push ")),
            "{trace}"
        );
    }
}

#[cfg(unix)]
#[test]
fn release_authorize_refuses_a_run_resumed_during_confirmation() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::thread;
    use std::time::{Duration, Instant};

    let repo = gated_repo();
    let dir = repo.path();
    git(dir, &["checkout", "-b", "relaybook/prd"]);
    fs::write(dir.join("ship.txt"), "ship\n").expect("ship");
    git(dir, &["add", "ship.txt"]);
    git(dir, &["commit", "-m", "ship"]);
    let head = head_sha(dir);
    relaybook(dir)
        .args(["review", "start", "--release"])
        .assert()
        .success();
    relaybook(dir)
        .args([
            "review",
            "record",
            "--release",
            "--by",
            "codex",
            "--sha",
            &head,
            "--verdict",
            "approved",
            "--text",
            RELEASE_REVIEW,
        ])
        .assert()
        .success();
    relaybook(dir)
        .args(["task", "create", "work", "--title", "Work"])
        .assert()
        .success();
    let run_id = seed_stopped(dir, "relaybook/prd", Some("gated"));
    assign_run(dir, "work", run_id);
    let mut ledger =
        relaybook_core::Ledger::open(&dir.join(".relaybook").join("ledger.db")).expect("ledger");
    let mut run = ledger.run_by_id(run_id).expect("run");
    run.stage = "execute".to_string();
    ledger.save_run(&mut run).expect("stage");
    drop(ledger);
    let probes = install_probes();
    let role = probes.log.join("cursor-agent");
    fs::copy(env!("CARGO_BIN_EXE_fake"), &role).expect("copy cursor");
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&role).expect("meta").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&role, permissions).expect("chmod");
    }
    write_idle_cursor(&probes.bin.join("cursor-agent"), &role);
    let held = probes.log.join("executor-held");
    let release = probes.log.join("executor-release");
    let child = std::sync::Mutex::new(None);
    let work = dir.to_path_buf();
    let output = authorize_while_prompted(dir, &head, || {
        let mut cmd = relaybook(&work);
        apply_probes(&mut cmd, &probes);
        cmd.env("RELAYBOOK_EXECUTOR_HELD", &held);
        cmd.env("RELAYBOOK_EXECUTOR_RELEASE", &release);
        cmd.env("RELAYBOOK_CURSOR_TIMEOUT_SECS", "90");
        cmd.args(["run", "--resume"]);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.process_group(0);
        let mut resume = cmd.spawn().expect("spawn resume");
        let started = Instant::now();
        while !held.exists() {
            let exited = matches!(resume.try_wait(), Ok(Some(_)));
            if exited || started.elapsed() > Duration::from_secs(15) {
                let _ = Command::new("kill")
                    .args(["-KILL", "--", &format!("-{}", resume.id())])
                    .status();
                let output = resume.wait_with_output().expect("wait resume");
                panic!(
                    "executor was not held\n{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(run_row(&work).status, "active");
        *child.lock().expect("child") = Some(resume);
    });
    if let Some(resume) = child.lock().expect("child").take() {
        let killed = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", resume.id())])
            .status()
            .expect("kill");
        assert!(
            killed.success(),
            "process group {} was not killed",
            resume.id()
        );
        let _ = resume.wait_with_output();
    }
    assert!(!output.success, "{}", output.output);
    assert!(output.output.contains(OWNED), "{}", output.output);
    assert_eq!(authorization_count(repo.path()), 0);
    assert_eq!(run_row(repo.path()).status, "active");
    assert_eq!(head_sha(repo.path()), head);
}

struct PtyOutput {
    success: bool,
    code: u32,
    output: String,
}

fn typed_phrases(buf: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(buf);
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = text.split('\n').collect();
    if !text.ends_with('\n') {
        lines.pop();
    }
    lines
        .into_iter()
        .filter_map(|line| {
            line.strip_prefix("Type ")
                .filter(|phrase| !phrase.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn authorize_while_prompted(dir: &Path, sha: &str, during: impl FnOnce()) -> PtyOutput {
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use portable_pty::{
        Child as PtyChild, ChildKiller, CommandBuilder, PtySize, native_pty_system,
    };

    let system = native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_relaybook"));
    cmd.arg("release");
    cmd.arg("authorize");
    cmd.arg("--sha");
    cmd.arg(sha);
    cmd.cwd(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let (seen_tx, seen_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (send, recv) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        let mut sent = false;
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(count) => {
                    buf.extend_from_slice(&tmp[..count]);
                    let text = String::from_utf8_lossy(&buf);
                    let phrases = typed_phrases(&buf);
                    if !sent && text.contains("Authorize this release?") && !phrases.is_empty() {
                        let _ = seen_tx.send(phrases[0].clone());
                        if go_rx.recv_timeout(Duration::from_secs(30)).is_ok() {
                            let line = format!("{}\n", phrases[0]);
                            let _ = writer.write_all(line.as_bytes());
                            let _ = writer.flush();
                        }
                        sent = true;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = send.send(buf);
    });
    seen_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("authorization prompt");
    during();
    let _ = go_tx.send(());
    let started = Instant::now();
    let status = loop {
        match PtyChild::try_wait(child.as_mut()) {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(20) => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = ChildKiller::kill(child.as_mut());
                panic!("timed out waiting for release authorize");
            }
            Err(err) => panic!("wait failed: {err}"),
        }
    };
    let buf = recv
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    PtyOutput {
        success: status.success(),
        code: status.exit_code(),
        output: String::from_utf8_lossy(&buf).into_owned(),
    }
}

fn answer_at_tty(dir: &Path, args: &[&str], accept: bool) -> PtyOutput {
    answer_at_tty_with(dir, args, accept, &[])
}

fn answer_at_tty_with(
    dir: &Path,
    args: &[&str],
    accept: bool,
    env: &[(&str, &std::ffi::OsStr)],
) -> PtyOutput {
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use portable_pty::{
        Child as PtyChild, ChildKiller, CommandBuilder, PtySize, native_pty_system,
    };

    let system = native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_relaybook"));
    for arg in args {
        cmd.arg(*arg);
    }
    cmd.cwd(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    for (key, value) in env {
        cmd.env(*key, value);
    }
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let (send, recv) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        let mut sent = false;
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(count) => {
                    buf.extend_from_slice(&tmp[..count]);
                    let phrases = typed_phrases(&buf);
                    if !sent && !phrases.is_empty() {
                        let line = if accept {
                            format!("{}\n", phrases[0])
                        } else {
                            "n\n".to_string()
                        };
                        let _ = writer.write_all(line.as_bytes());
                        let _ = writer.flush();
                        sent = true;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = send.send(buf);
    });
    let started = Instant::now();
    let status = loop {
        match PtyChild::try_wait(child.as_mut()) {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(20) => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = ChildKiller::kill(child.as_mut());
                panic!("timed out waiting for config set");
            }
            Err(err) => panic!("wait failed: {err}"),
        }
    };
    let buf = recv
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    PtyOutput {
        success: status.success(),
        code: status.exit_code(),
        output: String::from_utf8_lossy(&buf).into_owned(),
    }
}

#[cfg(unix)]
fn write_push_result_wrapper(path: &Path, target: &Path) {
    let target = target.display();
    let script = format!(
        "#!/bin/sh\nif [ -z \"$RELAYBOOK_TRACE_LOG\" ]; then echo \"RELAYBOOK_TRACE_LOG is unset\" >&2; exit 99; fi\nprintf '%s\\n' \"git $*\" >> \"$RELAYBOOK_TRACE_LOG\"\nif [ -n \"$RELAYBOOK_PUSH_GATE\" ]; then\n  for arg in \"$@\"; do\n    if [ \"$arg\" = \"push\" ]; then\n      i=0\n      while [ ! -f \"$RELAYBOOK_PUSH_GATE\" ]; do\n        i=$((i + 1))\n        if [ \"$i\" -gt 400 ]; then echo \"push gate timed out\" >&2; exit 98; fi\n        sleep 0.05\n      done\n      decision=$(head -n 1 \"$RELAYBOOK_PUSH_GATE\")\n      if [ \"$decision\" = \"fail\" ]; then echo \"push refused\" >&2; exit 1; fi\n      break\n    fi\n  done\nfi\nexec '{target}' \"$@\"\n"
    );
    fs::write(path, script).expect("wrapper");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(unix)]
fn write_ls_remote_fail_wrapper(path: &Path, target: &Path) {
    let target = target.display();
    let script = format!(
        "#!/bin/sh\nif [ -z \"$RELAYBOOK_TRACE_LOG\" ]; then echo \"RELAYBOOK_TRACE_LOG is unset\" >&2; exit 99; fi\nprintf '%s\\n' \"git $*\" >> \"$RELAYBOOK_TRACE_LOG\"\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"ls-remote\" ]; then echo \"ls-remote failed\" >&2; exit 1; fi\ndone\nexec '{target}' \"$@\"\n"
    );
    fs::write(path, script).expect("wrapper");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(unix)]
fn write_stamp(dir: &Path, id: i64) -> i64 {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    conn.query_row("SELECT write_stamp FROM runs WHERE id = ?1", [id], |row| {
        row.get(0)
    })
    .expect("stamp")
}

#[cfg(unix)]
fn command_text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(unix)]
fn task_run_id(dir: &Path, slug: &str) -> Option<i64> {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    conn.query_row("SELECT run_id FROM tasks WHERE slug = ?1", [slug], |row| {
        row.get(0)
    })
    .expect("task")
}

/// Holds `git push` after the publish claim is committed. `decision` is `go` or `fail`.
#[cfg(unix)]
fn during_held_publish(
    decision: &str,
    during: impl FnOnce(&Path, i64),
) -> (String, (String, String)) {
    use std::process::Stdio;
    use std::thread;
    use std::time::{Duration, Instant};

    let repo = gated_repo();
    let _older = seed_stopped(repo.path(), "relaybook/prd", Some("gated"));
    let (head, run_id) = reviewed_release(&repo);
    let authorized = answer_at_tty(repo.path(), &["release", "authorize", "--sha", &head], true);
    assert!(authorized.success, "{}", authorized.output);
    let probes = install_probes();
    write_push_result_wrapper(&probes.bin.join("git"), &program_on_path("git"));
    let gate = probes.log.join("allow-push");
    struct ReleasePush(PathBuf, String);
    impl Drop for ReleasePush {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, &self.1);
        }
    }
    let _release = ReleasePush(gate.clone(), format!("{decision}\n"));

    let mut approve = relaybook(repo.path());
    apply_probes(&mut approve, &probes);
    approve.env("RELAYBOOK_PUSH_GATE", &gate);
    approve.arg("approve");
    approve.stdout(Stdio::piped());
    approve.stderr(Stdio::piped());
    let mut approve = approve.spawn().expect("spawn approve");
    let trace_path = probes.log.join("invocations");
    let started = Instant::now();
    loop {
        let text = fs::read_to_string(&trace_path).unwrap_or_default();
        let held = text.lines().any(|line| line.starts_with("git push "));
        if held
            || started.elapsed() > Duration::from_secs(20)
            || matches!(approve.try_wait(), Ok(Some(_)))
        {
            break;
        }
        thread::sleep(Duration::from_millis(30));
    }
    let held = fs::read_to_string(&trace_path).unwrap_or_default();
    assert!(
        held.lines().any(|line| line.starts_with("git push ")),
        "approve did not reach push:\n{held}"
    );
    during(repo.path(), run_id);
    fs::write(&gate, format!("{decision}\n")).expect("release push");
    let output = approve.wait_with_output().expect("wait approve");
    let text = command_text(&output);
    let row = run_status(repo.path(), run_id);
    (text, row)
}

#[cfg(unix)]
#[test]
fn complete_and_add_during_publish_are_refused_and_the_publisher_finishes() {
    let (text, (status, stage)) = during_held_publish("go", |dir, run_id| {
        let stamp = write_stamp(dir, run_id);
        let complete = relaybook(dir)
            .args(["run", "complete"])
            .output()
            .expect("complete");
        let complete_text = command_text(&complete);
        assert!(!complete.status.success(), "{complete_text}");
        assert!(
            complete_text.contains("a publish claim is in progress; complete does not change it"),
            "{complete_text}"
        );
        assert_eq!(write_stamp(dir, run_id), stamp);
        relaybook(dir)
            .args(["task", "create", "greet", "--title", "Say hello"])
            .assert()
            .success();
        let add = relaybook(dir)
            .args(["run", "add", "greet"])
            .output()
            .expect("add");
        let add_text = command_text(&add);
        assert!(!add.status.success(), "{add_text}");
        assert!(
            add_text.contains("a publish claim is in progress"),
            "{add_text}"
        );
        assert_eq!(task_run_id(dir, "greet"), None);
        assert_eq!(write_stamp(dir, run_id), stamp);
        let (status, stage) = run_status(dir, run_id);
        assert_eq!(status, "active");
        assert_eq!(stage, "publishing");
    });
    assert!(text.contains("opened https://example.test/pr/1"), "{text}");
    assert_eq!(status, "complete", "{text}");
    assert_eq!(stage, "done", "{text}");
}

#[cfg(unix)]
#[test]
fn complete_and_stop_during_a_failed_publish_let_the_failure_save_land() {
    let (text, (status, stage)) = during_held_publish("fail", |dir, run_id| {
        let stamp = write_stamp(dir, run_id);
        let complete = relaybook(dir)
            .args(["run", "complete"])
            .output()
            .expect("complete");
        let complete_text = command_text(&complete);
        assert!(!complete.status.success(), "{complete_text}");
        assert!(
            complete_text.contains("a publish claim is in progress; complete does not change it"),
            "{complete_text}"
        );
        let stop = relaybook(dir).args(["run", "stop"]).output().expect("stop");
        let stop_text = command_text(&stop);
        assert!(!stop.status.success(), "{stop_text}");
        assert!(
            stop_text.contains("a publish claim is in progress; stop does not release it"),
            "{stop_text}"
        );
        assert_eq!(write_stamp(dir, run_id), stamp);
        let (status, stage) = run_status(dir, run_id);
        assert_eq!(status, "active", "{stop_text}");
        assert_eq!(stage, "publishing");
    });
    assert!(text.contains("push refused"), "{text}");
    assert!(!text.contains("run changed concurrently"), "{text}");
    assert_eq!(status, "stopped", "{text}");
    assert_eq!(stage, "publish", "{text}");
}

#[cfg(unix)]
fn publishing_claim(dir: &Path, run_id: i64) {
    let mut ledger =
        relaybook_core::Ledger::open(&dir.join(".relaybook").join("ledger.db")).expect("ledger");
    let mut run = ledger.run_by_id(run_id).expect("run");
    run.stage = "publishing".to_string();
    run.status = "active".to_string();
    ledger.save_run(&mut run).expect("publishing");
}

#[cfg(unix)]
fn exited_pid() -> i64 {
    let mut child = std::process::Command::new("true").spawn().expect("true");
    let pid = i64::from(child.id());
    child.wait().expect("exit");
    pid
}

#[cfg(unix)]
#[test]
fn recover_publish_refuses_a_live_publisher_a_bad_phrase_and_no_tty() {
    let repo = gated_repo();
    let (head, run_id) = reviewed_release(&repo);
    let authorized = answer_at_tty(repo.path(), &["release", "authorize", "--sha", &head], true);
    assert!(authorized.success, "{}", authorized.output);
    publishing_claim(repo.path(), run_id);
    let id = run_id.to_string();
    let missing = relaybook(repo.path())
        .args(["run", "recover-publish", &id])
        .output()
        .expect("no tty");
    let missing_text = command_text(&missing);
    assert!(!missing.status.success(), "{missing_text}");
    assert!(
        missing_text.contains("recover-publish requires confirmation at a TTY"),
        "{missing_text}"
    );
    assert_eq!(run_status(repo.path(), run_id).1, "publishing");

    let wrong = answer_at_tty(repo.path(), &["run", "recover-publish", &id], false);
    assert!(!wrong.success, "{}", wrong.output);
    assert!(
        wrong.output.contains("publish recovery was not confirmed"),
        "{}",
        wrong.output
    );
    assert_eq!(run_status(repo.path(), run_id).1, "publishing");

    let alive = answer_at_tty(repo.path(), &["run", "recover-publish", &id], true);
    assert!(!alive.success, "{}", alive.output);
    assert!(
        alive.output.contains("publisher is still running"),
        "{}",
        alive.output
    );
    assert_eq!(run_status(repo.path(), run_id).1, "publishing");
}

#[cfg(unix)]
#[test]
fn recover_publish_frees_a_dead_publisher_and_a_later_publish_rechecks() {
    let repo = gated_repo();
    let (head, run_id) = reviewed_release(&repo);
    let authorized = answer_at_tty(repo.path(), &["release", "authorize", "--sha", &head], true);
    assert!(authorized.success, "{}", authorized.output);
    publishing_claim(repo.path(), run_id);
    let dead = exited_pid();
    let db = repo.path().join(".relaybook").join("ledger.db");
    rusqlite::Connection::open(&db)
        .expect("open")
        .execute(
            "UPDATE publish_claims SET pid = ?1 WHERE run_id = ?2",
            [dead, run_id],
        )
        .expect("pid");
    let id = run_id.to_string();
    let recovered = answer_at_tty(repo.path(), &["run", "recover-publish", &id], true);
    assert!(recovered.success, "{}", recovered.output);
    assert!(
        recovered.output.contains("push result unknown"),
        "{}",
        recovered.output
    );
    let (status, stage) = run_status(repo.path(), run_id);
    assert_eq!(status, "stopped");
    assert_eq!(stage, "publish");
    let conn = rusqlite::Connection::open(&db).expect("open");
    let check: (String, String, i64) = conn
        .query_row(
            "SELECT context, status, override FROM remote_checks WHERE context = 'recover-publish'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("check");
    assert_eq!(check.1, "unknown");
    assert_eq!(check.2, 0);
    let payload: String = conn
        .query_row(
            "SELECT payload FROM events WHERE kind = 'run_linked' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("event");
    assert!(payload.contains("recover-publish"), "{payload}");
    assert!(payload.contains(&dead.to_string()), "{payload}");
    assert!(payload.contains("\"push_result\":\"unknown\""), "{payload}");
    drop(conn);

    let mut ledger = relaybook_core::Ledger::open(&db).expect("ledger");
    ledger
        .set_setting_with("remote_check", "local", true)
        .expect("local");
    drop(ledger);

    let probes = install_probes();
    write_ls_remote_fail_wrapper(&probes.bin.join("git"), &program_on_path("git"));
    let mut resume = relaybook(repo.path());
    apply_probes(&mut resume, &probes);
    resume.args(["run", "--resume"]);
    let output = resume.output().expect("resume");
    let text = command_text(&output);
    assert!(!output.status.success(), "{text}");
    let trace = fs::read_to_string(probes.log.join("invocations")).unwrap_or_default();
    let ls = trace.lines().position(|line| line.contains(" ls-remote "));
    let push = trace.lines().position(|line| {
        line.split_whitespace().nth(1) == Some("push")
            || line.split_whitespace().any(|arg| arg == "push")
    });
    assert!(ls.is_some(), "remote check did not run:\n{trace}\n{text}");
    assert!(
        push.is_none() || ls.is_some_and(|ls| push.is_some_and(|push| ls < push)),
        "publish ran before the remote check:\n{trace}"
    );
    assert!(push.is_none(), "publish ran:\n{trace}");
    let again = rusqlite::Connection::open(&db).expect("open");
    let publish_check: String = again
        .query_row(
            "SELECT status FROM remote_checks WHERE context = 'publish' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("publish check");
    assert_eq!(publish_check, "unknown");
}

#[cfg(unix)]
#[test]
fn later_publish_after_recovery_accepts_the_offline_override() {
    let repo = gated_repo();
    let (head, run_id) = reviewed_release(&repo);
    let authorized = answer_at_tty(repo.path(), &["release", "authorize", "--sha", &head], true);
    assert!(authorized.success, "{}", authorized.output);
    publishing_claim(repo.path(), run_id);
    let dead = exited_pid();
    let db = repo.path().join(".relaybook").join("ledger.db");
    rusqlite::Connection::open(&db)
        .expect("open")
        .execute(
            "UPDATE publish_claims SET pid = ?1 WHERE run_id = ?2",
            [dead, run_id],
        )
        .expect("pid");
    let mut ledger = relaybook_core::Ledger::open(&db).expect("ledger");
    ledger.recover_publishing(run_id).expect("recover");
    ledger
        .set_setting_with("remote_check", "local", true)
        .expect("local");
    drop(ledger);

    let probes = install_probes();
    write_ls_remote_fail_wrapper(&probes.bin.join("git"), &program_on_path("git"));
    let mut path = std::ffi::OsString::from(&probes.bin);
    path.push(":");
    path.push(std::env::var_os("PATH").expect("PATH"));
    let trace = probes.log.join("invocations");
    let kind = std::ffi::OsString::from("happy");
    let env = [
        ("PATH", path.as_os_str()),
        ("RELAYBOOK_FAKE_KIND", kind.as_os_str()),
        ("RELAYBOOK_TRACE_LOG", trace.as_os_str()),
        ("LOG", probes.log.as_os_str()),
    ];
    let resumed = answer_at_tty_with(repo.path(), &["run", "--resume"], true, &env);
    assert!(
        resumed.output.contains("offline"),
        "override was not offered:\n{}",
        resumed.output
    );
    assert!(
        resumed.success,
        "override did not let publish continue:\n{}",
        resumed.output
    );
    assert!(
        resumed.output.contains("opened https://example.test/pr/1"),
        "{}",
        resumed.output
    );
    let text = fs::read_to_string(&trace).unwrap_or_default();
    let ls = text.lines().position(|line| line.contains(" ls-remote "));
    let push = text
        .lines()
        .position(|line| line.split_whitespace().any(|arg| arg == "push"));
    assert!(ls.is_some(), "remote check did not run:\n{text}");
    assert!(
        ls.is_some_and(|ls| push.is_some_and(|push| ls < push)),
        "push did not follow the remote check:\n{text}"
    );
}

#[cfg(unix)]
#[test]
fn recover_publish_refuses_a_valid_foreign_lease_and_accepts_an_expired_one() {
    let repo = gated_repo();
    let (_head, run_id) = reviewed_release(&repo);
    publishing_claim(repo.path(), run_id);
    let db = repo.path().join(".relaybook").join("ledger.db");
    rusqlite::Connection::open(&db)
        .expect("open")
        .execute(
            "UPDATE publish_claims SET host = 'other-host' WHERE run_id = ?1",
            [run_id],
        )
        .expect("host");
    let id = run_id.to_string();
    let fresh = answer_at_tty(repo.path(), &["run", "recover-publish", &id], true);
    assert!(!fresh.success, "{}", fresh.output);
    assert!(
        fresh.output.contains("publish lease is still valid"),
        "{}",
        fresh.output
    );
    assert_eq!(run_status(repo.path(), run_id).1, "publishing");

    rusqlite::Connection::open(&db)
        .expect("open")
        .execute(
            "UPDATE publish_claims SET heartbeat_at = '2000-01-01T00:00:00.000Z' WHERE run_id = ?1",
            [run_id],
        )
        .expect("expire");
    let expired = answer_at_tty(repo.path(), &["run", "recover-publish", &id], true);
    assert!(expired.success, "{}", expired.output);
    assert!(
        expired.output.contains("push result unknown"),
        "{}",
        expired.output
    );
    let (status, stage) = run_status(repo.path(), run_id);
    assert_eq!(status, "stopped");
    assert_eq!(stage, "publish");
    let claims: i64 = rusqlite::Connection::open(&db)
        .expect("open")
        .query_row(
            "SELECT COUNT(*) FROM publish_claims WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .expect("claims");
    assert_eq!(claims, 0);
}
