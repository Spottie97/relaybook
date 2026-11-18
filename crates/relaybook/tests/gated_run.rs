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
    for name in ["claude", "cursor-agent", "gh", "codex"] {
        copy_fake(&fakes, name);
    }
    #[cfg(unix)]
    {
        let git_bin = program_on_path("git");
        write_trace_wrapper(&bin.join("git"), "git", &git_bin);
        for name in ["claude", "cursor-agent", "gh", "codex"] {
            write_trace_wrapper(&bin.join(name), name, &fakes.join(name));
        }
    }
    #[cfg(not(unix))]
    {
        for name in ["claude", "cursor-agent", "gh", "codex"] {
            copy_fake(&bin, name);
        }
    }
    Probes {
        _root: root,
        log,
        bin,
    }
}

/// Path of a copied fake: `name` plus `std::env::consts::EXE_SUFFIX`.
fn fake_executable(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

fn copy_fake(dir: &Path, name: &str) {
    let src = PathBuf::from(env!("CARGO_BIN_EXE_fake"));
    let dest = fake_executable(dir, name);
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
    cmd.env_remove("RELAYBOOK_ABORT_REVIEW_TRANSITION");
    cmd.env_remove("RELAYBOOK_CODEX_USER");
}

fn run_probed(dir: &Path, probes: &Probes, args: &[&str]) -> std::process::Output {
    run_probed_kind(dir, probes, args, "happy")
}

fn run_probed_kind(dir: &Path, probes: &Probes, args: &[&str], kind: &str) -> std::process::Output {
    let mut cmd = relaybook(dir);
    apply_probes(&mut cmd, probes);
    cmd.env("RELAYBOOK_FAKE_KIND", kind);
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
    // R10 replaces the "codex routing not built" halt. The run reviews with
    // Codex and waits at gate_release. It still does not push or open a PR.
    let probes = install_probes();
    let output = run_probed(dir, &probes, &["run", "--prd", "prd.md", "--gates", "none"]);
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    assert!(
        text.contains("waiting for sign-off: relaybook release authorize --sha "),
        "stdout={text}\nstderr={err}"
    );
    assert!(
        !text.contains("codex routing not built"),
        "stdout={text}\nstderr={err}"
    );
    assert!(
        !text.contains(GATED_RUN_REFUSAL),
        "the runner must review, not refuse up front:\n{text}"
    );
    assert!(!probes.log.join("gh.args").exists(), "gh was invoked");
    assert!(ref_missing(bare, "refs/heads/relaybook/prd"));
    assert!(
        bare_relaybook_refs(bare).is_empty(),
        "a run branch was pushed"
    );
    let row = run_row(dir);
    assert_eq!(row.workflow.as_deref(), Some("gated"));
    assert_eq!(row.stage, "gate_release");
    assert_eq!(row.status, "waiting");
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
fn write_vanishing_codex(path: &Path, fake: &Path) {
    let fake = fake.display();
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"login\" ]; then\n  '{fake}' \"$@\"\n  status=$?\n  rm -f \"$0\"\n  exit \"$status\"\nfi\nexec '{fake}' \"$@\"\n"
    );
    fs::write(path, script).expect("wrapper");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

fn codex_lines(log: &Path) -> String {
    fs::read_to_string(log.join("codex.args")).unwrap_or_default()
}

#[derive(Debug)]
struct StoredReview {
    scope: String,
    reviewer: String,
    run_id: Option<i64>,
    commit_sha: String,
    verdict: Option<String>,
}

fn stored_reviews(dir: &Path) -> Vec<StoredReview> {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    let mut stmt = conn
        .prepare("SELECT scope, reviewer, run_id, commit_sha, verdict FROM reviews ORDER BY id")
        .expect("prepare");
    stmt.query_map([], |row| {
        Ok(StoredReview {
            scope: row.get(0)?,
            reviewer: row.get(1)?,
            run_id: row.get(2)?,
            commit_sha: row.get(3)?,
            verdict: row.get(4)?,
        })
    })
    .expect("query")
    .collect::<Result<Vec<_>, _>>()
    .expect("reviews")
}

fn report_verdicts(dir: &Path) -> Vec<Option<String>> {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    let mut stmt = conn
        .prepare("SELECT verdict FROM reports ORDER BY id")
        .expect("prepare");
    stmt.query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("reports")
}

fn assert_codex_argv(line: &str, read_only: bool) {
    let args: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(args.first().copied(), Some("exec"), "{line}");
    assert!(args.contains(&"--ephemeral"), "{line}");
    assert!(args.contains(&"--color"), "{line}");
    assert!(args.contains(&"never"), "{line}");
    assert!(args.contains(&"-C"), "{line}");
    assert!(args.contains(&"--output-schema"), "{line}");
    assert!(args.contains(&"-o"), "{line}");
    let sandbox = args.windows(2).any(|pair| pair == ["-s", "read-only"]);
    assert_eq!(sandbox, read_only, "{line}");
    if !read_only {
        assert!(!args.contains(&"-s"), "{line}");
    }
    for flag in [
        "--add-dir",
        "--approve-for-me",
        "--trust",
        "--worktree",
        "apply",
        "danger-full-access",
        "workspace-write",
    ] {
        assert!(!args.contains(&flag), "{line}");
    }
    assert!(
        args.iter().all(|arg| !arg.starts_with("--dangerously")),
        "{line}"
    );
}

#[test]
fn missing_codex_fails_preflight() {
    let repo = gated_repo();
    let probes = install_probes();
    fs::remove_file(fake_executable(&probes.bin, "codex")).expect("remove codex");
    let mut cmd = relaybook(repo.path());
    apply_probes(&mut cmd, &probes);
    cmd.env("PATH", &probes.bin);
    cmd.args(["run", "--prd", "prd.md", "--gates", "none"]);
    let output = cmd.output().expect("run");
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}{err}");
    assert!(text.contains("codex: not on PATH"), "{text}{err}");
    let trace = fs::read_to_string(probes.log.join("invocations")).unwrap_or_default();
    assert!(
        trace.lines().all(|line| !line.starts_with("git ")),
        "git ran before preflight failed:\n{trace}"
    );
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
}

#[cfg(unix)]
fn path_without_real_codex(bin: &Path) -> std::ffi::OsString {
    let mut path = std::ffi::OsString::from(bin);
    if let Some(rest) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&rest) {
            if fake_executable(&dir, "codex").is_file() || dir.join("codex.exe").is_file() {
                continue;
            }
            path.push(":");
            path.push(&dir);
        }
    }
    path
}

#[cfg(unix)]
#[test]
fn codex_vanishing_mid_run_halts_without_retry() {
    let repo = gated_repo();
    let probes = install_probes();
    write_vanishing_codex(
        &probes.bin.join("codex"),
        &probes._root.path().join("fakes").join("codex"),
    );
    let mut cmd = relaybook(repo.path());
    apply_probes(&mut cmd, &probes);
    cmd.env("PATH", path_without_real_codex(&probes.bin));
    cmd.args(["run", "--prd", "prd.md", "--gates", "none"]);
    let output = cmd.output().expect("run");
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}{err}");
    assert!(text.contains("unavailable"), "{text}{err}");
    assert!(!text.contains("two consecutive"), "{text}{err}");
    assert!(
        codex_lines(&probes.log).is_empty(),
        "{}",
        codex_lines(&probes.log)
    );
    let claude = fs::read_to_string(probes.log.join("claude.args")).unwrap_or_default();
    assert!(!claude.contains("Use relaybook inbox"), "{claude}");
    assert!(!probes.log.join("gh.args").exists());
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
    let row = run_row(repo.path());
    assert_eq!(row.stage, "review");
    assert_eq!(row.status, "stopped");
}

#[test]
fn codex_argv_is_read_only() {
    let repo = gated_repo();
    let probes = install_probes();
    let output = run_probed(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    let args = codex_lines(&probes.log);
    assert!(!args.is_empty(), "codex was not invoked");
    for line in args.lines().filter(|line| !line.is_empty()) {
        assert_codex_argv(line, true);
    }
}

#[test]
fn codex_isolated_argv_omits_the_sandbox_flag() {
    let repo = gated_repo();
    relaybook(repo.path())
        .args(["config", "set", "codex_sandbox", "isolated"])
        .assert()
        .success()
        .stdout("codex_sandbox isolated\n");
    let probes = install_probes();
    let output = run_probed(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    let args = codex_lines(&probes.log);
    assert!(!args.is_empty(), "codex was not invoked");
    for line in args.lines().filter(|line| !line.is_empty()) {
        assert_codex_argv(line, false);
    }
}

#[test]
fn codex_verdict_is_recorded_by_relaybook() {
    let repo = gated_repo();
    let probes = install_probes();
    let output = run_probed(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    let head = head_sha(repo.path());
    let reviews = stored_reviews(repo.path());
    let task = reviews
        .iter()
        .find(|review| review.scope == "task")
        .expect("task review");
    assert_eq!(task.reviewer, "codex");
    assert!(task.run_id.is_some(), "{:?}", task.run_id);
    assert_eq!(task.commit_sha, head);
    assert_eq!(task.verdict.as_deref(), Some("approved"));
    let release = reviews
        .iter()
        .find(|review| review.scope == "release")
        .expect("release review");
    assert_eq!(release.reviewer, "codex");
    assert_eq!(release.run_id, task.run_id);
    assert_eq!(release.verdict.as_deref(), Some("approved"));
}

#[test]
fn codex_changes_requested_reopens_and_cursor_fixes() {
    let repo = gated_repo();
    let probes = install_probes();
    let output = run_probed_kind(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "changes",
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    let cursor = fs::read_to_string(probes.log.join("cursor.args")).unwrap_or_default();
    assert!(
        cursor.lines().filter(|line| !line.is_empty()).count() >= 2,
        "{cursor}"
    );
    let reviews = stored_reviews(repo.path());
    assert!(
        reviews
            .iter()
            .any(|review| review.verdict.as_deref() == Some("changes_requested")),
        "{reviews:?}"
    );
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
    assert!(!probes.log.join("gh.args").exists());
    let row = run_row(repo.path());
    assert_eq!(row.stage, "gate_release");
    assert_eq!(row.status, "waiting");
}

#[test]
fn codex_unparsable_output_never_accepts() {
    let repo = gated_repo();
    let probes = install_probes();
    let output = run_probed_kind(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "garbage",
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}{err}");
    assert!(
        text.contains("two consecutive agent failures"),
        "{text}{err}"
    );
    assert!(
        report_verdicts(repo.path())
            .iter()
            .all(|verdict| verdict.is_none()),
        "{:?}",
        report_verdicts(repo.path())
    );
    assert!(!probes.log.join("gh.args").exists());
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
}

#[test]
fn codex_oversized_packet_halts_before_exec() {
    let repo = gated_repo();
    let probes = install_probes();
    let output = run_probed_kind(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "huge",
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}{err}");
    assert!(
        text.contains("diff too large for auto review; review manually"),
        "{text}{err}"
    );
    assert!(
        codex_lines(&probes.log).is_empty(),
        "codex ran before the size halt:\n{}",
        codex_lines(&probes.log)
    );
    assert!(!probes.log.join("gh.args").exists());
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
    let row = run_row(repo.path());
    assert_eq!(row.stage, "review");
    assert_eq!(row.status, "stopped");
}

#[test]
fn gated_run_waits_at_release_and_never_pushes_or_opens_pr_first() {
    let repo = gated_repo();
    assert_review_halt_never_pushes(repo.path(), &repo.bare);
}

#[test]
fn classic_run_never_calls_codex() {
    let repo = origin_repo();
    let probes = install_probes();
    let output = run_probed(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    assert!(text.contains("opened https://example.test/pr/1"), "{text}");
    let trace = fs::read_to_string(probes.log.join("invocations")).unwrap_or_default();
    assert!(
        trace.lines().all(|line| !line.starts_with("codex")),
        "{trace}"
    );
    assert!(!probes.log.join("codex.args").exists());
}

#[test]
fn stale_codex_result_does_not_approve_the_replacement_review() {
    let repo = gated_repo();
    let probes = install_probes();
    let hold = probes.log.join("hold");
    fs::create_dir_all(&hold).expect("hold");
    let mut guard = HoldGuard {
        hold: hold.clone(),
        children: Vec::new(),
    };
    guard.children.push(spawn_held(
        repo.path(),
        &probes,
        &hold,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "first",
    ));
    assert!(
        wait_for_file(&hold.join("started-1"), std::time::Duration::from_secs(45)),
        "codex did not start the first review\n{}",
        held_output(&probes, "first")
    );
    let head = head_sha(repo.path());
    let stop = relaybook(repo.path())
        .args(["run", "stop"])
        .output()
        .expect("stop");
    let stop_text = format!(
        "{}{}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr)
    );
    assert!(
        !stop.status.success() && stop_text.contains("stopped"),
        "{stop_text}"
    );
    let decision = relaybook(repo.path())
        .args([
            "decision",
            "log",
            "work",
            "--why",
            "replacement handoff",
            "--by",
            "cursor",
        ])
        .output()
        .expect("decision");
    assert!(
        decision.status.success(),
        "{}{}",
        String::from_utf8_lossy(&decision.stdout),
        String::from_utf8_lossy(&decision.stderr)
    );
    let conn = rusqlite::Connection::open(repo.path().join(".relaybook").join("ledger.db"))
        .expect("ledger");
    let changed = conn
        .execute(
            "UPDATE reports SET body = ?1 WHERE kind = 'task'",
            [r#"{"changed":["replacement-handoff"],"verified":[],"not_verified":[],"deviations":[],"review":[]}"#],
        )
        .expect("update report");
    assert_eq!(changed, 1);
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("checkpoint");
    drop(conn);
    assert_eq!(head_sha(repo.path()), head, "the handoff change moved HEAD");

    guard.children.push(spawn_held(
        repo.path(),
        &probes,
        &hold,
        &["run", "--resume"],
        "second",
    ));
    assert!(
        wait_for_file(&hold.join("started-2"), std::time::Duration::from_secs(45)),
        "resume did not start a replacement review\n{}",
        held_output(&probes, "second")
    );
    let first_out = fs::read_to_string(hold.join("started-1")).expect("started-1");
    let second_out = fs::read_to_string(hold.join("started-2")).expect("started-2");
    assert_ne!(
        first_out.trim(),
        second_out.trim(),
        "invocations share an output file"
    );
    assert!(
        output_is_outside(repo.path(), &first_out),
        "first output is inside the repository: {first_out}"
    );
    assert!(
        output_is_outside(repo.path(), &second_out),
        "replacement output is inside the repository: {second_out}"
    );
    assert_private_invocation(&first_out);
    assert_private_invocation(&second_out);
    fs::write(
        hold.join("release-1"),
        r#"{"verdict":"approved","body":{"findings":[],"security":[],"tests_checked":[],"not_checked":[],"token":"kestrel-stale"}}"#,
    )
    .expect("release");
    assert!(
        child_exited(&mut guard.children[0], std::time::Duration::from_secs(20)),
        "first review did not finish\n{}",
        held_output(&probes, "first")
    );
    let finished = held_output(&probes, "first");
    assert!(finished.contains("stale review invocation"), "{finished}");
    let reviews = review_text(repo.path());
    assert!(
        reviews.iter().any(|(verdict, _)| verdict.is_none()),
        "{reviews:?}"
    );
    assert!(
        reviews.iter().all(|(verdict, body)| {
            verdict.is_none() && !body.as_deref().unwrap_or("").contains("kestrel-stale")
        }),
        "{reviews:?}"
    );
    assert!(
        report_verdicts(repo.path())
            .iter()
            .all(|verdict| verdict.is_none()),
        "{:?}",
        report_verdicts(repo.path())
    );
    assert_eq!(head_sha(repo.path()), head);
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
    assert!(!probes.log.join("gh.args").exists());
    let stdin = fs::read_to_string(probes.log.join("codex.stdin")).unwrap_or_default();
    assert!(
        stdin.contains("replacement-handoff"),
        "replacement review did not see the changed handoff:\n{stdin}"
    );
    let row = run_row(repo.path());
    assert_eq!(row.status, "active", "{} {}", row.stage, row.status);
    assert_eq!(row.stage, "review");
    assert_eq!(failure_streak(repo.path()), 0);
}

#[test]
fn stale_codex_failure_does_not_change_the_replacement_review() {
    assert_stale_codex_result_is_ignored("relaybook-hold-exit 1\n", false);
}

#[test]
fn stale_codex_malformed_output_does_not_change_the_replacement_review() {
    assert_stale_codex_result_is_ignored("not json\n", false);
}

#[test]
fn stale_codex_failure_does_not_drive_a_waiting_checkpoint() {
    assert_stale_codex_result_is_ignored("relaybook-hold-exit 1\n", true);
}

#[test]
fn stale_codex_malformed_output_does_not_drive_a_waiting_checkpoint() {
    assert_stale_codex_result_is_ignored("not json\n", true);
}

#[cfg(debug_assertions)]
#[test]
fn review_transition_failure_does_not_leave_an_accepted_report() {
    let repo = gated_repo();
    let probes = install_probes();
    let mut cmd = relaybook(repo.path());
    apply_probes(&mut cmd, &probes);
    cmd.env("RELAYBOOK_ABORT_REVIEW_TRANSITION", "1");
    cmd.args(["run", "--prd", "prd.md", "--gates", "none"]);
    let output = cmd.output().expect("run");
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}{err}");
    assert!(
        text.contains("two consecutive agent failures"),
        "{text}{err}"
    );
    assert!(
        report_verdicts(repo.path())
            .iter()
            .all(|verdict| verdict.is_none()),
        "{:?}",
        report_verdicts(repo.path())
    );
    assert!(
        stored_reviews(repo.path())
            .iter()
            .all(|review| review.verdict.is_none()),
        "{:?}",
        stored_reviews(repo.path())
    );
    assert!(!probes.log.join("gh.args").exists());
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
}

#[test]
fn resume_after_committed_accept_continues_without_a_second_review() {
    let repo = gated_repo();
    let probes = install_probes();
    let output = run_probed(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    let before = codex_exec_count(&probes.log);
    assert!(
        before >= 2,
        "task and release reviews did not both run: {before}"
    );
    let conn = rusqlite::Connection::open(repo.path().join(".relaybook").join("ledger.db"))
        .expect("ledger");
    let changed = conn
        .execute(
            "UPDATE runs
             SET stage = 'review', status = 'stopped', current_slug = 'work',
                 failure_streak = 0, write_stamp = write_stamp + 1
             WHERE id = ?1",
            [latest_run_id(repo.path())],
        )
        .expect("rewind");
    assert_eq!(changed, 1);
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("checkpoint");
    drop(conn);

    let output = run_probed(repo.path(), &probes, &["run", "--resume"]);
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    assert!(
        !text.contains("two consecutive agent failures"),
        "{text}{err}"
    );
    assert!(!text.contains("no unreviewed report"), "{text}{err}");
    assert_eq!(
        codex_exec_count(&probes.log),
        before + 1,
        "resume started another task review\n{}",
        codex_lines(&probes.log)
    );
    assert!(
        report_verdicts(repo.path())
            .iter()
            .any(|verdict| verdict.as_deref() == Some("accepted")),
        "{:?}",
        report_verdicts(repo.path())
    );
    let row = run_row(repo.path());
    assert_eq!(row.stage, "gate_release", "{} {}", row.stage, row.status);
    assert_eq!(row.status, "waiting");
    assert_eq!(failure_streak(repo.path()), 0);
    assert!(!probes.log.join("gh.args").exists());
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
}

#[cfg(unix)]
#[test]
fn isolated_codex_writes_outside_a_read_only_repository() {
    // User namespaces and bind mounts return EPERM here. Clearing the
    // repository write bits for the Codex process is the same constraint:
    // an `-o` path inside the worktree cannot be created.
    let repo = gated_repo();
    relaybook(repo.path())
        .args(["config", "set", "codex_sandbox", "isolated"])
        .assert()
        .success();
    let probes = install_probes();
    write_readonly_codex(
        &probes.bin.join("codex"),
        &probes._root.path().join("fakes").join("codex"),
    );
    let output = run_probed(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    assert!(
        !repo.path().join("codex-ro-probe").exists(),
        "the repository was writable during review"
    );
    let reviews = stored_reviews(repo.path());
    assert!(
        reviews
            .iter()
            .any(|review| review.scope == "task" && review.verdict.as_deref() == Some("approved")),
        "{reviews:?}"
    );
    let lines = codex_lines(&probes.log);
    let mut outputs = Vec::new();
    for line in lines.lines().filter(|line| line.starts_with("exec ")) {
        assert_codex_argv(line, false);
        outputs.push(output_flag(line));
    }
    assert!(outputs.len() >= 2, "{lines}");
    for path in &outputs {
        assert!(
            output_is_outside(repo.path(), path),
            "{path} is inside the repository"
        );
        let file = PathBuf::from(path);
        let parent = file.parent().expect("output directory");
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = fs::symlink_metadata(parent)
            .expect("dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "{parent:?}");
        let schema = parent.join("codex-schema.json");
        let schema_mode = fs::symlink_metadata(&schema)
            .expect("schema")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(schema_mode, 0o600, "{schema:?}");
        assert!(file.is_file(), "codex did not create {path}");
    }
}

#[test]
fn codex_extra_property_never_approves_or_accepts() {
    let repo = gated_repo();
    let probes = install_probes();
    let output = run_probed_kind(
        repo.path(),
        &probes,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "schema",
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{text}{err}");
    assert!(
        text.contains("two consecutive agent failures"),
        "{text}{err}"
    );
    assert!(
        report_verdicts(repo.path())
            .iter()
            .all(|verdict| verdict.is_none()),
        "{:?}",
        report_verdicts(repo.path())
    );
    let reviews = stored_reviews(repo.path());
    assert!(
        reviews.iter().all(|review| review.verdict.is_none()),
        "{reviews:?}"
    );
    assert!(!probes.log.join("gh.args").exists());
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
}

#[test]
fn gated_auto_review_records_task_and_release_graph_refresh() {
    let repo = gated_repo();
    commit_graphify_ignore(repo.path());
    let probes = install_probes();
    copy_fake(&probes.bin, "graphify");
    let log = probes.log.join("graphify.log");
    let output = run_with_graphify(repo.path(), &probes, &log, "graph");
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    let head = head_sha(repo.path());
    let run_id = latest_run_id(repo.path());
    let builds = graph_builds(repo.path());
    assert!(builds.iter().all(|row| row.step != "plan"), "{builds:?}");
    for step in ["review", "release-review"] {
        let row = builds
            .iter()
            .find(|row| row.step == step)
            .unwrap_or_else(|| panic!("missing {step}: {builds:?}"));
        assert_eq!(row.status, "built", "{builds:?}");
        assert_eq!(row.commit_sha, head, "{builds:?}");
        assert_eq!(row.run_id, Some(run_id), "{builds:?}");
    }
    let reviews = review_graphs(repo.path());
    for scope in ["task", "release"] {
        let row = reviews
            .iter()
            .find(|row| row.0 == scope && row.2.as_deref() == Some("approved"))
            .unwrap_or_else(|| panic!("missing approved {scope}: {reviews:?}"));
        assert_eq!(row.1.as_deref(), Some(head.as_str()), "{reviews:?}");
    }
    let stdin = fs::read_to_string(probes.log.join("codex.stdin")).unwrap_or_default();
    assert!(stdin.contains(&head), "{stdin}");
    assert!(stdin.contains("\"graph_status\": \"built\""), "{stdin}");
    let trace = fs::read_to_string(&log).unwrap_or_default();
    assert!(trace.matches("update .").count() >= 2, "{trace}");
    assert!(!trace.contains("extract"), "{trace}");
}

#[test]
fn gated_auto_review_continues_when_graph_refresh_fails() {
    let repo = gated_repo();
    commit_graphify_ignore(repo.path());
    let probes = install_probes();
    copy_fake(&probes.bin, "graphify");
    let log = probes.log.join("graphify.log");
    let output = run_with_graphify(repo.path(), &probes, &log, "fail");
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    let run_id = latest_run_id(repo.path());
    let builds = graph_builds(repo.path());
    for step in ["review", "release-review"] {
        let row = builds
            .iter()
            .find(|row| row.step == step)
            .unwrap_or_else(|| panic!("missing {step}: {builds:?}"));
        assert_eq!(row.status, "failed", "{builds:?}");
        assert_eq!(row.run_id, Some(run_id), "{builds:?}");
    }
    let reviews = review_graphs(repo.path());
    assert!(
        reviews.iter().all(|(_, graph_sha, _)| graph_sha.is_none()),
        "{reviews:?}"
    );
    assert!(
        reviews
            .iter()
            .any(|row| row.2.as_deref() == Some("approved")),
        "{reviews:?}"
    );
    let stdin = fs::read_to_string(probes.log.join("codex.stdin")).unwrap_or_default();
    assert!(stdin.contains("\"graph_status\": \"failed\""), "{stdin}");
    assert!(stdin.contains("no graph"), "{stdin}");
    let trace = fs::read_to_string(&log).unwrap_or_default();
    assert!(trace.contains("update ."), "{trace}");
    let row = run_row(repo.path());
    assert_eq!(row.stage, "gate_release", "{}", row.status);
    assert_eq!(row.status, "waiting");
    assert!(
        report_verdicts(repo.path())
            .iter()
            .any(|verdict| verdict.as_deref() == Some("accepted")),
        "{:?}",
        report_verdicts(repo.path())
    );
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
    assert!(!probes.log.join("gh.args").exists());
}

struct HoldGuard {
    hold: PathBuf,
    children: Vec<std::process::Child>,
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        for n in 1..=8 {
            let _ = fs::write(self.hold.join(format!("release-{n}")), "{}\n");
        }
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_held(
    dir: &Path,
    probes: &Probes,
    hold: &Path,
    args: &[&str],
    name: &str,
) -> std::process::Child {
    let mut cmd = relaybook(dir);
    apply_probes(&mut cmd, probes);
    cmd.env("RELAYBOOK_FAKE_KIND", "hold");
    cmd.env("RELAYBOOK_CODEX_HOLD", hold);
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
    let out = fs::read_to_string(probes.log.join(format!("{name}.out"))).unwrap_or_default();
    let err = fs::read_to_string(probes.log.join(format!("{name}.err"))).unwrap_or_default();
    format!("{out}{err}")
}

fn wait_for_file(path: &Path, limit: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < limit {
        if path.is_file() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    path.is_file()
}

fn child_exited(child: &mut std::process::Child, limit: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if start.elapsed() < limit => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => return false,
        }
    }
}

fn output_is_outside(root: &Path, raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() {
        return false;
    }
    let path = Path::new(raw);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    let Ok(path) = std::path::absolute(&path) else {
        return false;
    };
    !path.starts_with(root)
}

fn output_flag(line: &str) -> String {
    let args: Vec<&str> = line.split_whitespace().collect();
    let index = args
        .iter()
        .position(|arg| *arg == "-o")
        .unwrap_or_else(|| panic!("no -o in {line}"));
    args.get(index + 1)
        .unwrap_or_else(|| panic!("no -o path in {line}"))
        .to_string()
}

fn review_text(dir: &Path) -> Vec<(Option<String>, Option<String>)> {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    let mut stmt = conn
        .prepare("SELECT verdict, body FROM reviews ORDER BY id")
        .expect("prepare");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("reviews")
}

fn commit_graphify_ignore(dir: &Path) {
    let path = dir.join(".gitignore");
    let mut text = fs::read_to_string(&path).unwrap_or_default();
    if text.contains("graphify-out/") {
        return;
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("graphify-out/\n");
    fs::write(&path, text).expect("gitignore");
    git(dir, &["add", ".gitignore"]);
    git(dir, &["commit", "-m", "ignore graphify-out"]);
}

fn run_with_graphify(dir: &Path, probes: &Probes, log: &Path, kind: &str) -> std::process::Output {
    let mut cmd = relaybook(dir);
    apply_probes(&mut cmd, probes);
    cmd.env("GRAPHIFY_LOG", log);
    cmd.env("RELAYBOOK_FAKE_GRAPHIFY", kind);
    cmd.args(["run", "--prd", "prd.md", "--gates", "none"]);
    cmd.output().expect("run")
}

#[derive(Debug)]
struct GraphBuildRow {
    step: String,
    status: String,
    commit_sha: String,
    run_id: Option<i64>,
}

fn graph_builds(dir: &Path) -> Vec<GraphBuildRow> {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    let mut stmt = conn
        .prepare("SELECT step, status, commit_sha, run_id FROM graph_builds ORDER BY id")
        .expect("prepare");
    stmt.query_map([], |row| {
        Ok(GraphBuildRow {
            step: row.get(0)?,
            status: row.get(1)?,
            commit_sha: row.get(2)?,
            run_id: row.get(3)?,
        })
    })
    .expect("query")
    .collect::<Result<Vec<_>, _>>()
    .expect("builds")
}

fn assert_private_invocation(raw: &str) {
    let path = Path::new(raw.trim());
    let parent = path
        .parent()
        .unwrap_or_else(|| panic!("no directory in {raw}"));
    let meta = fs::symlink_metadata(parent).unwrap_or_else(|err| panic!("{parent:?}: {err}"));
    assert!(
        meta.file_type().is_dir() && !meta.file_type().is_symlink(),
        "{raw}"
    );
    let schema = parent.join("codex-schema.json");
    let schema_meta =
        fs::symlink_metadata(&schema).unwrap_or_else(|err| panic!("{schema:?}: {err}"));
    assert!(
        schema_meta.is_file() && !schema_meta.file_type().is_symlink(),
        "{schema:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{parent:?} mode {mode:o}");
        let schema_mode = schema_meta.permissions().mode();
        assert_eq!(
            schema_mode & 0o777,
            0o600,
            "{schema:?} mode {schema_mode:o}"
        );
    }
}

fn failure_streak(dir: &Path) -> i64 {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    conn.query_row(
        "SELECT failure_streak FROM runs ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get(0),
    )
    .expect("streak")
}

fn nonempty_lines(path: &Path) -> usize {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .count()
}

fn codex_exec_count(log: &Path) -> usize {
    codex_lines(log)
        .lines()
        .filter(|line| line.starts_with("exec "))
        .count()
}

fn assert_stale_codex_result_is_ignored(release: &str, checkpoint: bool) {
    let repo = gated_repo();
    let probes = install_probes();
    let hold = probes.log.join("hold");
    fs::create_dir_all(&hold).expect("hold");
    let mut guard = HoldGuard {
        hold: hold.clone(),
        children: Vec::new(),
    };
    guard.children.push(spawn_held(
        repo.path(),
        &probes,
        &hold,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "first",
    ));
    assert!(
        wait_for_file(&hold.join("started-1"), std::time::Duration::from_secs(45)),
        "codex did not start the first review\n{}",
        held_output(&probes, "first")
    );
    let head = head_sha(repo.path());
    let stop = relaybook(repo.path())
        .args(["run", "stop"])
        .output()
        .expect("stop");
    let stop_text = format!(
        "{}{}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr)
    );
    assert!(
        !stop.status.success() && stop_text.contains("stopped"),
        "{stop_text}"
    );
    let decision = relaybook(repo.path())
        .args([
            "decision",
            "log",
            "work",
            "--why",
            "replacement handoff",
            "--by",
            "cursor",
        ])
        .output()
        .expect("decision");
    assert!(
        decision.status.success(),
        "{}{}",
        String::from_utf8_lossy(&decision.stdout),
        String::from_utf8_lossy(&decision.stderr)
    );
    let conn = rusqlite::Connection::open(repo.path().join(".relaybook").join("ledger.db"))
        .expect("ledger");
    let changed = conn
        .execute(
            "UPDATE reports SET body = ?1 WHERE kind = 'task'",
            [r#"{"changed":["replacement-handoff"],"verified":[],"not_verified":[],"deviations":[],"review":[]}"#],
        )
        .expect("update report");
    assert_eq!(changed, 1);
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("checkpoint");
    drop(conn);
    assert_eq!(head_sha(repo.path()), head, "the handoff change moved HEAD");

    guard.children.push(spawn_held(
        repo.path(),
        &probes,
        &hold,
        &["run", "--resume"],
        "second",
    ));
    assert!(
        wait_for_file(&hold.join("started-2"), std::time::Duration::from_secs(45)),
        "resume did not start a replacement review\n{}",
        held_output(&probes, "second")
    );
    let first_out = fs::read_to_string(hold.join("started-1")).expect("started-1");
    let second_out = fs::read_to_string(hold.join("started-2")).expect("started-2");
    assert_ne!(
        first_out.trim(),
        second_out.trim(),
        "invocations share an output file"
    );
    assert!(output_is_outside(repo.path(), &first_out), "{first_out}");
    assert!(output_is_outside(repo.path(), &second_out), "{second_out}");
    assert_private_invocation(&first_out);
    assert_private_invocation(&second_out);
    if checkpoint {
        let conn = rusqlite::Connection::open(repo.path().join(".relaybook").join("ledger.db"))
            .expect("ledger");
        let changed = conn
            .execute(
                "UPDATE runs
                 SET stage = 'gate_review:accepted', status = 'waiting', failure_streak = 0,
                     write_stamp = write_stamp + 1
                 WHERE id = ?1",
                [latest_run_id(repo.path())],
            )
            .expect("park");
        assert_eq!(changed, 1);
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .expect("checkpoint");
        drop(conn);
    }
    let cursor_before = nonempty_lines(&probes.log.join("cursor.args"));
    fs::write(hold.join("release-1"), release).expect("release");
    assert!(
        child_exited(&mut guard.children[0], std::time::Duration::from_secs(20)),
        "first review did not finish\n{}",
        held_output(&probes, "first")
    );
    let finished = held_output(&probes, "first");
    assert!(finished.contains("stale review invocation"), "{finished}");
    assert!(
        !finished.contains("two consecutive agent failures"),
        "{finished}"
    );
    assert_eq!(
        nonempty_lines(&probes.log.join("cursor.args")),
        cursor_before,
        "{finished}"
    );
    assert!(
        report_verdicts(repo.path())
            .iter()
            .all(|verdict| verdict.is_none()),
        "{:?}",
        report_verdicts(repo.path())
    );
    let reviews = review_text(repo.path());
    assert!(
        reviews.iter().all(|(verdict, _)| verdict.is_none()),
        "{reviews:?}"
    );
    assert_eq!(failure_streak(repo.path()), 0, "{finished}");
    assert_eq!(head_sha(repo.path()), head);
    assert!(bare_relaybook_refs(&repo.bare).is_empty());
    assert!(!probes.log.join("gh.args").exists());
    let row = run_row(repo.path());
    if checkpoint {
        assert_eq!(row.status, "waiting", "{} {}", row.stage, row.status);
        assert_eq!(row.stage, "gate_review:accepted");
    } else {
        assert_eq!(row.status, "active", "{} {}", row.stage, row.status);
        assert_eq!(row.stage, "review");
    }
    let stdin = fs::read_to_string(probes.log.join("codex.stdin")).unwrap_or_default();
    assert!(stdin.contains("replacement-handoff"), "{stdin}");
}

fn latest_run_id(dir: &Path) -> i64 {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    conn.query_row("SELECT id FROM runs ORDER BY id DESC LIMIT 1", [], |row| {
        row.get(0)
    })
    .expect("run id")
}

fn review_graphs(dir: &Path) -> Vec<(String, Option<String>, Option<String>)> {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    let mut stmt = conn
        .prepare("SELECT scope, graph_sha, verdict FROM reviews ORDER BY id")
        .expect("prepare");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("reviews")
}

#[cfg(unix)]
fn write_readonly_codex(path: &Path, fake: &Path) {
    let fake = fake.display();
    let script = format!(
        "#!/bin/sh\nif [ -z \"$RELAYBOOK_TRACE_LOG\" ]; then echo \"RELAYBOOK_TRACE_LOG is unset\" >&2; exit 99; fi\nprintf '%s\\n' \"codex $*\" >> \"$RELAYBOOK_TRACE_LOG\"\nif [ \"$1\" = \"login\" ]; then\n  exec '{fake}' \"$@\"\nfi\nrepo=$(pwd)\nrestore() {{ chmod -R u+w \"$repo\" 2>/dev/null || true; }}\ntrap restore EXIT\nchmod -R a-w \"$repo\" || exit 93\nif touch \"$repo/codex-ro-probe\" 2>/dev/null; then\n  echo \"repository was writable during review\" >&2\n  exit 92\nfi\n'{fake}' \"$@\"\nstatus=$?\nrestore\ntrap - EXIT\nexit \"$status\"\n"
    );
    fs::write(path, script).expect("wrapper");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

/// Claim refresh, task-done refresh, then the review refresh. Later updates
/// belong to a replacement invocation.
const REVIEW_GRAPHIFY_HOLD_FROM: &str = "3";

fn spawn_graph_review(
    dir: &Path,
    probes: &Probes,
    graph_hold: &Path,
    codex_hold: Option<&Path>,
    args: &[&str],
    name: &str,
) -> std::process::Child {
    let mut cmd = relaybook(dir);
    apply_probes(&mut cmd, probes);
    cmd.env("RELAYBOOK_FAKE_GRAPHIFY", "hold");
    cmd.env("RELAYBOOK_GRAPHIFY_HOLD", graph_hold);
    cmd.env("RELAYBOOK_GRAPHIFY_HOLD_FROM", REVIEW_GRAPHIFY_HOLD_FROM);
    cmd.env("GRAPHIFY_LOG", probes.log.join("graphify.log"));
    if let Some(hold) = codex_hold {
        cmd.env("RELAYBOOK_FAKE_KIND", "hold");
        cmd.env("RELAYBOOK_CODEX_HOLD", hold);
    }
    cmd.args(args);
    cmd.stdout(std::process::Stdio::from(
        fs::File::create(probes.log.join(format!("{name}.out"))).expect("stdout"),
    ));
    cmd.stderr(std::process::Stdio::from(
        fs::File::create(probes.log.join(format!("{name}.err"))).expect("stderr"),
    ));
    cmd.spawn().expect("spawn")
}

fn release_later_graph_holds(hold: &Path, from: i64) {
    for seq in from..=8 {
        let started = hold.join(format!("started-{seq}"));
        let release = hold.join(format!("release-{seq}"));
        if started.is_file() && !release.is_file() {
            fs::write(&release, "go\n").expect("release graphify");
        }
    }
}

fn wait_for_child_releasing_graph(
    child: &mut std::process::Child,
    hold: &Path,
    probes: &Probes,
    name: &str,
) {
    let start = std::time::Instant::now();
    loop {
        release_later_graph_holds(hold, 4);
        if child_exited(child, std::time::Duration::from_millis(50)) {
            return;
        }
        if start.elapsed() > std::time::Duration::from_secs(60) {
            panic!(
                "{name} did not finish\n{}\ngraphify:\n{}",
                held_output(probes, name),
                fs::read_to_string(probes.log.join("graphify.log")).unwrap_or_default()
            );
        }
    }
}

fn report_rows(dir: &Path) -> Vec<(String, Option<String>)> {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    let mut stmt = conn
        .prepare("SELECT body, verdict FROM reports ORDER BY id")
        .expect("prepare");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("reports")
}

fn replace_done_report(dir: &Path, slug: &str, changed: &str) {
    let head = head_sha(dir);
    let mut ledger =
        relaybook_core::Ledger::open(&dir.join(".relaybook").join("ledger.db")).expect("ledger");
    ledger
        .reopen_task(slug, "replaced during refresh")
        .expect("reopen");
    let claimed_head = head.clone();
    ledger
        .claim_task_anchored(slug, "cursor", None, move || Ok(Some(claimed_head)))
        .expect("claim");
    ledger
        .finish_task(
            slug,
            Some(relaybook_core::Sections {
                changed: vec![changed.to_string()],
                verified: Vec::new(),
                not_verified: Vec::new(),
                deviations: Vec::new(),
                review: Vec::new(),
            }),
            Some(&head),
        )
        .expect("finish");
    drop(ledger);
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("checkpoint");
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingReviewIdentity {
    id: i64,
    report_id: Option<i64>,
    started_at: String,
    ledger_mark: String,
    invocation_token: Option<String>,
}

fn pending_reviews(dir: &Path) -> Vec<PendingReviewIdentity> {
    let conn = rusqlite::Connection::open(dir.join(".relaybook").join("ledger.db")).expect("open");
    let mut stmt = conn
        .prepare(
            "SELECT id, report_id, started_at, ledger_mark, invocation_token
             FROM reviews WHERE verdict IS NULL ORDER BY id",
        )
        .expect("prepare");
    stmt.query_map([], |row| {
        Ok(PendingReviewIdentity {
            id: row.get(0)?,
            report_id: row.get(1)?,
            started_at: row.get(2)?,
            ledger_mark: row.get(3)?,
            invocation_token: row.get(4)?,
        })
    })
    .expect("query")
    .collect::<Result<Vec<_>, _>>()
    .expect("reviews")
}

fn retarget_mark(mark: &str, report_id: i64) -> String {
    let mut value: serde_json::Value = serde_json::from_str(mark).expect("mark");
    value["reports"] = serde_json::json!(report_id);
    serde_json::to_string(&value).expect("mark")
}

struct ReusedReview {
    id: i64,
    task_id: i64,
    run_id: Option<i64>,
    branch: String,
    reviewer: String,
    reviewer_override: i64,
    base_sha: Option<String>,
    commit_sha: String,
    graph_sha: Option<String>,
    mark: String,
    started_at: String,
}

/// Delete the pending review and insert another row with the same id, a new
/// report, and a ledger mark that still matches the ledger.
fn reuse_review_id(dir: &Path, changed: &str) -> i64 {
    let path = dir.join(".relaybook").join("ledger.db");
    let conn = rusqlite::Connection::open(&path).expect("open");
    conn.pragma_update(None, "foreign_keys", "ON").expect("fk");
    let row = conn
        .query_row(
            "SELECT id, task_id, run_id, branch, reviewer, reviewer_override, base_sha,
                    commit_sha, graph_sha, ledger_mark, started_at
             FROM reviews WHERE verdict IS NULL ORDER BY id DESC LIMIT 1",
            [],
            |row| {
                Ok(ReusedReview {
                    id: row.get(0)?,
                    task_id: row.get::<_, Option<i64>>(1)?.expect("task review"),
                    run_id: row.get(2)?,
                    branch: row.get(3)?,
                    reviewer: row.get(4)?,
                    reviewer_override: row.get(5)?,
                    base_sha: row.get(6)?,
                    commit_sha: row.get(7)?,
                    graph_sha: row.get(8)?,
                    mark: row.get(9)?,
                    started_at: row.get(10)?,
                })
            },
        )
        .expect("pending review");
    let ReusedReview {
        id,
        task_id,
        run_id,
        branch,
        reviewer,
        reviewer_override,
        base_sha,
        commit_sha,
        graph_sha,
        mark,
        started_at,
    } = row;
    let body = format!(
        "{{\"changed\":[\"{changed}\"],\"verified\":[],\"not_verified\":[],\"deviations\":[],\"review\":[]}}"
    );
    conn.execute(
        "INSERT INTO reports (
            task_id, kind, author, body, bytes, created_at, commit_sha
         ) VALUES (?1, 'task', 'cursor', ?2, ?3, '2026-11-17T00:00:00.000Z', ?4)",
        rusqlite::params![task_id, body, body.len() as i64, commit_sha],
    )
    .expect("replacement report");
    let report_id = conn.last_insert_rowid();
    let mark = retarget_mark(&mark, report_id);
    let deleted = conn
        .execute("DELETE FROM reviews WHERE id = ?1", [id])
        .expect("delete review");
    assert_eq!(deleted, 1);
    conn.execute(
        "INSERT INTO reviews (
            id, scope, task_id, report_id, run_id, branch, reviewer, reviewer_override,
            base_sha, commit_sha, graph_sha, ledger_mark, started_at
         ) VALUES (?1, 'task', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            id,
            task_id,
            report_id,
            run_id,
            branch,
            reviewer,
            reviewer_override,
            base_sha,
            commit_sha,
            graph_sha,
            mark,
            started_at,
        ],
    )
    .expect("reuse review id");
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("checkpoint");
    report_id
}

#[test]
fn report_replaced_during_graph_refresh_is_not_accepted() {
    let repo = gated_repo();
    commit_graphify_ignore(repo.path());
    let probes = install_probes();
    copy_fake(&probes.bin, "graphify");
    let hold = probes.log.join("graphify-hold");
    fs::create_dir_all(&hold).expect("hold");
    let mut guard = HoldGuard {
        hold: hold.clone(),
        children: Vec::new(),
    };
    guard.children.push(spawn_graph_review(
        repo.path(),
        &probes,
        &hold,
        None,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "refresh",
    ));
    assert!(
        wait_for_file(&hold.join("started-3"), std::time::Duration::from_secs(45)),
        "review refresh did not block\n{}\n{}",
        held_output(&probes, "refresh"),
        fs::read_to_string(probes.log.join("graphify.log")).unwrap_or_default()
    );
    assert!(
        pending_reviews(repo.path()).is_empty(),
        "the review row existed before refresh returned: {:?}",
        pending_reviews(repo.path())
    );
    replace_done_report(repo.path(), "work", "replacement-during-refresh");
    fs::write(hold.join("release-3"), "go\n").expect("release");
    wait_for_child_releasing_graph(&mut guard.children[0], &hold, &probes, "refresh");
    let text = held_output(&probes, "refresh");
    let rows = report_rows(repo.path());
    assert!(
        rows.iter().any(|(body, verdict)| {
            body.contains("replacement-during-refresh") && verdict.is_none()
        }),
        "the replacement report was accepted\n{rows:?}\n{text}"
    );
    assert!(
        rows.iter()
            .all(|(_, verdict)| verdict.as_deref() != Some("accepted")),
        "a report was accepted for the wrong handoff\n{rows:?}\n{text}"
    );
    assert!(
        text.contains("report changed during graph refresh"),
        "{text}"
    );
    assert!(bare_relaybook_refs(&repo.bare).is_empty(), "{text}");
    assert!(!probes.log.join("gh.args").exists(), "{text}");
}

#[test]
fn stale_review_preparer_cannot_replace_a_newer_pending_review() {
    let repo = gated_repo();
    commit_graphify_ignore(repo.path());
    let probes = install_probes();
    copy_fake(&probes.bin, "graphify");
    let graph_hold = probes.log.join("graphify-hold");
    let codex_hold = probes.log.join("codex-hold");
    fs::create_dir_all(&graph_hold).expect("graph hold");
    fs::create_dir_all(&codex_hold).expect("codex hold");
    let mut guard = HoldGuard {
        hold: graph_hold.clone(),
        children: Vec::new(),
    };
    guard.children.push(spawn_graph_review(
        repo.path(),
        &probes,
        &graph_hold,
        Some(&codex_hold),
        &["run", "--prd", "prd.md", "--gates", "none"],
        "older",
    ));
    assert!(
        wait_for_file(
            &graph_hold.join("started-3"),
            std::time::Duration::from_secs(45)
        ),
        "older review refresh did not block\n{}\n{}",
        held_output(&probes, "older"),
        fs::read_to_string(probes.log.join("graphify.log")).unwrap_or_default()
    );
    assert!(
        pending_reviews(repo.path()).is_empty(),
        "a review existed before the older preparer created one"
    );
    let stop = relaybook(repo.path())
        .args(["run", "stop"])
        .output()
        .expect("stop");
    let stop_text = format!(
        "{}{}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr)
    );
    assert!(
        !stop.status.success() && stop_text.contains("stopped"),
        "{stop_text}"
    );
    guard.children.push(spawn_graph_review(
        repo.path(),
        &probes,
        &graph_hold,
        Some(&codex_hold),
        &["run", "--resume"],
        "newer",
    ));
    assert!(
        wait_for_file(
            &graph_hold.join("started-4"),
            std::time::Duration::from_secs(45)
        ),
        "newer review refresh did not block\n{}\n{}",
        held_output(&probes, "newer"),
        fs::read_to_string(probes.log.join("graphify.log")).unwrap_or_default()
    );
    fs::write(graph_hold.join("release-4"), "go\n").expect("release newer");
    assert!(
        wait_for_file(
            &codex_hold.join("started-1"),
            std::time::Duration::from_secs(45)
        ),
        "newer invocation did not reach codex\n{}",
        held_output(&probes, "newer")
    );
    let pending = pending_reviews(repo.path());
    assert_eq!(pending.len(), 1, "{pending:?}");
    let newer = pending[0].clone();
    fs::write(graph_hold.join("release-3"), "go\n").expect("release older");
    assert!(
        child_exited(&mut guard.children[0], std::time::Duration::from_secs(20)),
        "older preparer did not finish\n{}",
        held_output(&probes, "older")
    );
    let older_text = held_output(&probes, "older");
    assert_eq!(
        pending_reviews(repo.path()),
        vec![newer],
        "the older preparer replaced the newer review\n{older_text}"
    );
    assert!(
        older_text.contains("stale review invocation"),
        "{older_text}"
    );
    assert!(
        report_rows(repo.path())
            .iter()
            .all(|(_, verdict)| verdict.as_deref() != Some("accepted")),
        "{:?}",
        report_rows(repo.path())
    );
}

#[test]
fn stale_task_preparer_paused_before_reload_keeps_the_newer_review() {
    assert_stale_preparer_paused_before_reload(false);
}

#[test]
fn stale_release_preparer_paused_before_reload_keeps_the_newer_review() {
    assert_stale_preparer_paused_before_reload(true);
}

/// Pause the older preparer on the review-start reload, stop/resume, and let
/// the replacement reach Codex. `release_review` holds the second pause, after
/// the older process has finished its task review.
fn assert_stale_preparer_paused_before_reload(release_review: bool) {
    let repo = gated_repo();
    let probes = install_probes();
    copy_fake(&probes.bin, "graphify");
    let prepare = probes.log.join("prepare-hold");
    let codex = probes.log.join("codex-hold");
    fs::create_dir_all(&prepare).expect("prepare hold");
    fs::create_dir_all(&codex).expect("codex hold");
    let mut guard = ProcessGuard {
        releases: vec![prepare.clone(), codex.clone()],
        children: Vec::new(),
    };
    let slot = if release_review { 2 } else { 1 };
    guard.children.push(spawn_prepare_pause(
        repo.path(),
        &probes,
        &prepare,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "older",
    ));
    if release_review {
        assert!(
            wait_for_file(
                &prepare.join("started-1"),
                std::time::Duration::from_secs(45)
            ),
            "task prepare did not block\n{}",
            held_output(&probes, "older")
        );
        fs::write(prepare.join("release-1"), "go\n").expect("release task prepare");
    }
    assert!(
        wait_for_file(
            &prepare.join(format!("started-{slot}")),
            std::time::Duration::from_secs(45)
        ),
        "prepare did not block before the run reload\n{}",
        held_output(&probes, "older")
    );
    assert!(
        pending_reviews(repo.path()).is_empty(),
        "a pending review existed before this starter reloaded the run: {:?}",
        pending_reviews(repo.path())
    );
    let stop = relaybook(repo.path())
        .args(["run", "stop"])
        .output()
        .expect("stop");
    let stop_text = format!(
        "{}{}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr)
    );
    assert!(
        !stop.status.success() && stop_text.contains("stopped"),
        "{stop_text}"
    );
    guard.children.push(spawn_codex_pause(
        repo.path(),
        &probes,
        &codex,
        &["run", "--resume"],
        "newer",
    ));
    assert!(
        wait_for_file(&codex.join("started-1"), std::time::Duration::from_secs(45)),
        "replacement invocation did not reach codex\n{}",
        held_output(&probes, "newer")
    );
    let pending = pending_reviews(repo.path());
    assert_eq!(pending.len(), 1, "{pending:?}");
    let newer = pending[0].clone();
    assert!(
        newer
            .invocation_token
            .as_deref()
            .is_some_and(|token| !token.is_empty()),
        "replacement review has no invocation token: {newer:?}"
    );
    fs::write(prepare.join(format!("release-{slot}")), "go\n").expect("release stale prepare");
    let started = std::time::Instant::now();
    loop {
        let kept = pending_reviews(repo.path());
        if kept != vec![newer.clone()] {
            let older_text = held_output(&probes, "older");
            assert_eq!(
                kept,
                vec![newer.clone()],
                "the stale preparer replaced the newer review\n{older_text}"
            );
        }
        if child_exited(&mut guard.children[0], std::time::Duration::from_millis(50)) {
            break;
        }
        if started.elapsed() > std::time::Duration::from_secs(20) {
            panic!(
                "stale preparer did not finish\n{}",
                held_output(&probes, "older")
            );
        }
    }
    let older_text = held_output(&probes, "older");
    let kept = pending_reviews(repo.path());
    assert_eq!(
        kept,
        vec![newer.clone()],
        "the stale preparer replaced the newer review\n{older_text}"
    );
    assert_eq!(kept[0].id, newer.id);
    assert_eq!(kept[0].invocation_token, newer.invocation_token);
    assert_eq!(kept[0].started_at, newer.started_at);
    assert!(
        older_text.contains("stale review invocation"),
        "{older_text}"
    );
    let row = run_row(repo.path());
    assert_eq!(row.status, "active", "{} {}", row.stage, row.status);
    assert!(
        row.stop_reason.is_none(),
        "stale preparer stopped the live run: {} {:?}",
        row.status,
        row.stop_reason
    );
    if release_review {
        assert_eq!(row.stage, "release_review", "{}", row.status);
    } else {
        assert_eq!(row.stage, "review", "{}", row.status);
    }
    assert!(
        guard.children[1].try_wait().expect("newer").is_none(),
        "replacement invocation exited\n{}",
        held_output(&probes, "newer")
    );
}

struct ProcessGuard {
    releases: Vec<PathBuf>,
    children: Vec<std::process::Child>,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        for dir in &self.releases {
            for n in 1..=8 {
                let _ = fs::write(dir.join(format!("release-{n}")), "go\n");
            }
        }
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_prepare_pause(
    dir: &Path,
    probes: &Probes,
    prepare: &Path,
    args: &[&str],
    name: &str,
) -> std::process::Child {
    let mut cmd = relaybook(dir);
    apply_probes(&mut cmd, probes);
    cmd.env("RELAYBOOK_REVIEW_PREPARE_HOLD", prepare);
    cmd.args(args);
    cmd.stdout(std::process::Stdio::from(
        fs::File::create(probes.log.join(format!("{name}.out"))).expect("stdout"),
    ));
    cmd.stderr(std::process::Stdio::from(
        fs::File::create(probes.log.join(format!("{name}.err"))).expect("stderr"),
    ));
    cmd.spawn().expect("spawn")
}

fn spawn_codex_pause(
    dir: &Path,
    probes: &Probes,
    codex: &Path,
    args: &[&str],
    name: &str,
) -> std::process::Child {
    let mut cmd = relaybook(dir);
    apply_probes(&mut cmd, probes);
    cmd.env("RELAYBOOK_FAKE_KIND", "hold");
    cmd.env("RELAYBOOK_CODEX_HOLD", codex);
    cmd.args(args);
    cmd.stdout(std::process::Stdio::from(
        fs::File::create(probes.log.join(format!("{name}.out"))).expect("stdout"),
    ));
    cmd.stderr(std::process::Stdio::from(
        fs::File::create(probes.log.join(format!("{name}.err"))).expect("stderr"),
    ));
    cmd.spawn().expect("spawn")
}

#[test]
fn reused_review_id_cannot_approve_a_different_report() {
    let repo = gated_repo();
    let probes = install_probes();
    let hold = probes.log.join("codex-hold");
    fs::create_dir_all(&hold).expect("hold");
    let mut guard = HoldGuard {
        hold: hold.clone(),
        children: Vec::new(),
    };
    guard.children.push(spawn_held(
        repo.path(),
        &probes,
        &hold,
        &["run", "--prd", "prd.md", "--gates", "none"],
        "review",
    ));
    assert!(
        wait_for_file(&hold.join("started-1"), std::time::Duration::from_secs(45)),
        "codex did not start\n{}",
        held_output(&probes, "review")
    );
    let before = pending_reviews(repo.path());
    assert_eq!(before.len(), 1, "{before:?}");
    reuse_review_id(repo.path(), "reused-review-report");
    fs::write(
        hold.join("release-1"),
        "{\"verdict\":\"approved\",\"body\":{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[]}}\n",
    )
    .expect("approve");
    let started = std::time::Instant::now();
    loop {
        for seq in 2..=4 {
            let slot = hold.join(format!("started-{seq}"));
            let release = hold.join(format!("release-{seq}"));
            if slot.is_file() && !release.is_file() {
                fs::write(
                    &release,
                    "{\"verdict\":\"approved\",\"body\":{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[],\"prd_covered\":true,\"gaps\":[]}}\n",
                )
                .expect("release later codex");
            }
        }
        if child_exited(&mut guard.children[0], std::time::Duration::from_millis(50)) {
            break;
        }
        if started.elapsed() > std::time::Duration::from_secs(30) {
            panic!("review did not finish\n{}", held_output(&probes, "review"));
        }
    }
    let text = held_output(&probes, "review");
    let rows = report_rows(repo.path());
    assert!(
        rows.iter()
            .any(|(body, verdict)| body.contains("reused-review-report") && verdict.is_none()),
        "a reused review id accepted a different report\n{rows:?}\n{text}"
    );
    assert!(
        rows.iter()
            .all(|(_, verdict)| verdict.as_deref() != Some("accepted")),
        "{rows:?}\n{text}"
    );
    assert!(text.contains("stale review invocation"), "{text}");
    assert_eq!(failure_streak(repo.path()), 0, "{text}");
}
