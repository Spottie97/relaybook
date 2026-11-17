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
        .args(["config", "set", "reviewer", "codex"])
        .assert()
        .success();
    relaybook(dir.path())
        .args(["config", "set", "workflow", "gated"])
        .assert()
        .success();
    relaybook(dir.path())
        .args(["config", "set", "gates", "plan,review"])
        .assert()
        .success();
    relaybook(dir.path())
        .args(["config", "set", "max_cycles", "9"])
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

fn assert_refused(dir: &Path, bare: &Path, args: &[&str]) {
    let before = head_sha(dir);
    assert_eq!(head_branch(dir), "main");
    let probes = install_probes();
    let output = run_probed(dir, &probes, args);
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{args:?}\n{text}{err}");
    assert!(
        text.contains(GATED_RUN_REFUSAL),
        "{args:?}\nstdout={text}\nstderr={err}"
    );
    assert_agents_not_called(&probes.log);
    assert_eq!(head_branch(dir), "main");
    assert_eq!(head_sha(dir), before);
    assert!(ref_missing(dir, "refs/heads/relaybook/prd"));
    assert!(ref_missing(bare, "refs/heads/relaybook/prd"));
    assert_eq!(setting(dir, "max_cycles"), json!(9));
    assert_eq!(setting(dir, "gates"), json!("plan,review"));
    assert_eq!(setting(dir, "workflow"), json!("gated"));
}

#[test]
fn gated_run_refuses_before_preflight_git_or_agents() {
    let repo = gated_repo();
    assert_refused(
        repo.path(),
        &repo.bare,
        &[
            "run",
            "--prd",
            "prd.md",
            "--max-cycles",
            "3",
            "--gates",
            "none",
        ],
    );
}

#[test]
fn gated_approve_and_resume_refuse() {
    let repo = gated_repo();
    assert_refused(repo.path(), &repo.bare, &["approve"]);
    assert_refused(
        repo.path(),
        &repo.bare,
        &["run", "--resume", "--max-cycles", "3", "--gates", "none"],
    );
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

    let declined = answer_at_tty(dir.path(), &["config", "set", "workflow", "classic"], "n\n");
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

    let accepted = answer_at_tty(dir.path(), &["config", "set", "workflow", "classic"], "y\n");
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
        "y\n",
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

struct PtyOutput {
    success: bool,
    code: u32,
    output: String,
}

fn answer_at_tty(dir: &Path, args: &[&str], answer: &str) -> PtyOutput {
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
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let (send, recv) = mpsc::channel();
    let answer = answer.to_string();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        let mut sent = false;
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(count) => {
                    buf.extend_from_slice(&tmp[..count]);
                    if !sent && buf.windows(5).any(|window| window == b"[y/N]") {
                        let _ = writer.write_all(answer.as_bytes());
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
