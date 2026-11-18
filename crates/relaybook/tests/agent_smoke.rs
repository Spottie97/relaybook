#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/agent-smoke.sh")
}

fn home_gitconfig() -> Vec<u8> {
    let home = std::env::var("HOME").expect("HOME");
    fs::read(Path::new(&home).join(".gitconfig")).unwrap_or_default()
}

fn git(dir: &Path, global: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .expect("git")
}

fn git_ok(dir: &Path, global: &Path, args: &[&str]) {
    let output = git(dir, global, args);
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn head(dir: &Path, global: &Path) -> String {
    let output = git(dir, global, &["rev-parse", "HEAD"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

fn is_git_command(code: &str) -> bool {
    code.split(['(', ';', '|', '&', '`']).any(|part| {
        let part = part.trim();
        part == "git" || part.starts_with("git ")
    })
}

fn is_repo_op(code: &str) -> bool {
    is_git_command(code)
        || code.contains("init_throwaway")
        || code.contains("run_in_repo")
        || code.contains("run_graphify")
}

#[test]
fn graphify_smoke_init_ignores_inherited_git_directory() {
    let script = script_path();
    let text = fs::read_to_string(&script).expect("agent-smoke.sh");
    let marker = text
        .find("RELAYBOOK_SMOKE_LIB:-}")
        .expect("smoke lib guard");
    let body = &text[marker..];
    let clear_at = body
        .find("\nclear_git_repo_env\n")
        .expect("clear before throwaway repos");
    let graph_at = body
        .find("\ninit_throwaway \"$graph\" \"$graph_home\"\n")
        .expect("graph init");
    let work_at = body
        .find("\ninit_throwaway \"$work\" \"\"\n")
        .expect("agent repo init keeps the caller HOME");
    let schema_at = body.find("\nsmoke_schema_v8 ").expect("schema v8 smoke");
    let remote_at = body.find("\nsmoke_ls_remote ").expect("ls-remote smoke");
    let codex_at = body.find("\nsmoke_codex\n").expect("codex smoke");
    let claude_at = body.find("\nclaude -p ").expect("agent prompts");
    assert!(clear_at < graph_at && graph_at < work_at);
    assert!(
        graph_at < schema_at
            && schema_at < remote_at
            && remote_at < codex_at
            && codex_at < work_at
            && work_at < claude_at
    );
    assert!(text.contains("git ls-remote"));
    assert!(text.contains("--exit-code"));
    assert!(text.contains("pushInsteadOf"));
    assert!(text.contains("push_url_differs=true"));
    assert!(text.contains("codex exec"));
    assert!(text.contains("-s read-only"));
    assert!(text.contains("--ephemeral"));
    assert!(text.contains("--color never"));
    assert!(text.contains("--output-schema"));
    assert!(text.contains("codex-wrote.txt"));
    assert!(!text.contains("--add-dir"));
    assert!(!text.contains("--dangerously"));
    assert!(!text.contains("--approve-for-me"));
    assert!(!text.contains("--trust"));
    assert!(!text.contains("--worktree"));
    let clear_line = body[..clear_at].lines().count();
    for line in body.lines().take(clear_line) {
        let code = line.split('#').next().unwrap_or("").trim();
        assert!(
            !is_repo_op(code),
            "git command before clear_git_repo_env: {line}"
        );
    }
    assert!(text.contains("unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE"));
    for status in ["sh", "bash"] {
        let output = Command::new(status)
            .arg("-n")
            .arg(&script)
            .output()
            .expect(status);
        assert!(
            output.status.success(),
            "{status} -n failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let root = tempfile::tempdir().expect("tempdir");
    let global = root.path().join("global.gitconfig");
    fs::write(&global, "").expect("global gitconfig");
    let canary = root.path().join("canary");
    let graph = root.path().join("graph");
    let graph_home = root.path().join("graph-home");
    let work = root.path().join("work");
    let bare = root.path().join("bare");
    let caller_home = root.path().join("caller-home");
    fs::create_dir(&canary).expect("canary");
    fs::create_dir(&graph).expect("graph");
    fs::create_dir(&graph_home).expect("graph home");
    fs::create_dir(&work).expect("work");
    fs::create_dir(&caller_home).expect("caller home");
    fs::write(caller_home.join("sentinel"), "untouched").expect("sentinel");
    let real_before = home_gitconfig();
    let system_before = fs::read("/etc/gitconfig").ok();

    git_ok(&canary, &global, &["init", "-b", "main"]);
    fs::write(canary.join("a.txt"), "hi\n").expect("canary file");
    git_ok(&canary, &global, &["add", "a.txt"]);
    git_ok(
        &canary,
        &global,
        &[
            "-c",
            "user.name=Relaybook",
            "-c",
            "user.email=relaybook@example.com",
            "commit",
            "-q",
            "-m",
            "canary",
        ],
    );
    let head_before = head(&canary, &global);
    let config_before = fs::read(canary.join(".git/config")).expect("canary config");
    let index_before = fs::read(canary.join(".git/index")).expect("canary index");

    let output = Command::new("sh")
        .env("RELAYBOOK_SMOKE_LIB", "1")
        .env("GIT_DIR", canary.join(".git"))
        .env("GIT_WORK_TREE", &canary)
        .env("GIT_INDEX_FILE", canary.join(".git/index"))
        .env("HOME", &caller_home)
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("SCRIPT", &script)
        .env("GRAPH", &graph)
        .env("GRAPH_HOME", &graph_home)
        .env("WORK", &work)
        .env("BARE", &bare)
        .arg("-c")
        .arg(
            r#"
set -eu
. "$SCRIPT"
graph=$GRAPH
graph_home=$GRAPH_HOME
work=$WORK
die() { printf '%s\n' "$1" >&2; exit 1; }
init_throwaway "$graph" "$graph_home"
mkdir -p "$graph/src"
printf 'fn main() {}\n' > "$graph/src/main.rs"
run_in_repo "$graph" "$graph_home" git add src/main.rs
run_in_repo "$graph" "$graph_home" git commit -q -m "tiny app"
seen=$(run_in_repo "$graph" "$graph_home" sh -c 'printf %s "$HOME"')
[ "$seen" = "$graph_home" ] || die "graph phase HOME was $seen"
gitdir=$(run_in_repo "$graph" "$graph_home" git rev-parse --git-dir)
[ "$gitdir" = ".git" ] || die "graph git dir was $gitdir"
subject=$(run_in_repo "$graph" "$graph_home" git log -1 --format=%s)
[ "$subject" = "tiny app" ] || die "graph commit was $subject"
init_throwaway "$work" ""
seen_work=$(run_in_repo "$work" "" sh -c 'printf %s "$HOME"')
[ "$seen_work" = "$HOME" ] || die "agent phase HOME was $seen_work"
workdir=$(run_in_repo "$work" "" git rev-parse --git-dir)
[ "$workdir" = ".git" ] || die "work git dir was $workdir"
mkdir -p "$BARE"
clear_git_repo_env
(
  cd "$BARE" || exit 1
  git init -q -b main
)
[ -d "$BARE/.git" ] || die "bare init missed $BARE"
"#,
        )
        .output()
        .expect("sh");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(head(&canary, &global), head_before);
    assert_eq!(
        fs::read(canary.join(".git/config")).expect("canary config"),
        config_before
    );
    assert_eq!(
        fs::read(canary.join(".git/index")).expect("canary index"),
        index_before
    );
    assert_eq!(
        fs::read(caller_home.join("sentinel")).expect("sentinel"),
        b"untouched"
    );
    assert!(!caller_home.join(".gitconfig").exists());
    assert!(
        home_gitconfig() == real_before,
        "real ~/.gitconfig was modified"
    );
    assert!(
        fs::read("/etc/gitconfig").ok() == system_before,
        "system gitconfig was modified"
    );
}

const FAKE_CODEX: &str = r#"#!/bin/sh
mode=${SMOKE_CODEX_MODE:-ok}
log=${SMOKE_CODEX_LOG:-}
rc=${SMOKE_CODEX_RC:-1}
err=${SMOKE_CODEX_ERR:-codex failed}
verdict=${SMOKE_CODEX_VERDICT:-}
valid='{"verdict":"approved","body":{"findings":[],"security":[],"tests_checked":[],"not_checked":[]}}'

if [ "${1:-}" = "login" ]; then
  exit 0
fi
if [ "${1:-}" = "--version" ]; then
  printf '%s\n' "codex-cli 0.169.0"
  exit 0
fi
if [ "${1:-}" = "exec" ] && [ "${2:-}" = "--help" ]; then
  printf '%s\n' "--ephemeral --output-schema --color --sandbox read-only -m"
  exit 0
fi

if [ -n "$log" ]; then
  printf '%s\n' "$*" >> "$log"
fi
cat >/dev/null || true

out=
repo=
prev=
for arg in "$@"; do
  if [ "$prev" = "-o" ]; then
    out=$arg
  fi
  if [ "$prev" = "-C" ]; then
    repo=$arg
  fi
  prev=$arg
done
has_schema=0
for arg in "$@"; do
  if [ "$arg" = "--output-schema" ]; then
    has_schema=1
  fi
done

if [ "$has_schema" -eq 1 ]; then
  case "$mode" in
    missing)
      exit 0
      ;;
    payload)
      printf '%s' "$verdict" > "$out"
      exit 0
      ;;
    fail)
      if [ "${SMOKE_CODEX_WRITE_VALID:-}" = 1 ]; then
        printf '%s\n' "$valid" > "$out"
      fi
      printf '%s\n' "$err" >&2
      exit "$rc"
      ;;
    review_write)
      printf '%s\n' "$valid" > "$out"
      printf '%s\n' written > "$repo/codex-wrote.txt"
      exit 0
      ;;
    *)
      if [ -n "$verdict" ]; then
        printf '%s' "$verdict" > "$out"
      else
        printf '%s\n' "$valid" > "$out"
      fi
      exit 0
      ;;
  esac
fi

case "$mode" in
  probe_write)
    printf '%s\n' written > "$repo/codex-wrote.txt"
    exit 0
    ;;
  probe_dirty)
    printf '%s\n' dirty >> "$repo/src-main.rs"
    exit 0
    ;;
  probe_fail)
    printf '%s\n' "sandbox probe crashed" >&2
    exit "$rc"
    ;;
  fail)
    printf '%s\n' "$err" >&2
    exit "$rc"
    ;;
  *)
    exit 0
    ;;
esac
"#;

fn run_codex_smoke(mode: &str, extra: &[(&str, &str)]) -> (Output, String) {
    let root = tempfile::tempdir().expect("tempdir");
    let bin = root.path().join("bin");
    fs::create_dir(&bin).expect("bin");
    let codex = bin.join("codex");
    fs::write(&codex, FAKE_CODEX).expect("fake codex");
    let mut perms = fs::metadata(&codex).expect("meta").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&codex, perms).expect("chmod");
    let log = root.path().join("codex.log");
    let path = std::env::var("PATH").unwrap_or_default();
    let mut command = Command::new("sh");
    command
        .env("RELAYBOOK_SMOKE_LIB", "1")
        .env("SCRIPT", script_path())
        .env("SMOKE_CODEX_MODE", mode)
        .env("SMOKE_CODEX_LOG", &log)
        .env_remove("SMOKE_CODEX_RC")
        .env_remove("SMOKE_CODEX_ERR")
        .env_remove("SMOKE_CODEX_VERDICT")
        .env_remove("SMOKE_CODEX_WRITE_VALID")
        .env("PATH", format!("{}:{path}", bin.display()))
        .arg("-c")
        .arg("set -eu\n. \"$SCRIPT\"\nsmoke_codex\n");
    for (key, value) in extra {
        command.env(key, value);
    }
    let output = command.output().expect("sh");
    let logged = fs::read_to_string(&log).unwrap_or_default();
    (output, logged)
}

fn assert_codex_smoke_failed(mode: &str, output: &Output, needle: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "{mode}: smoke passed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("schema-valid verdict"),
        "{mode}: reported success\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains(needle),
        "{mode}: missing {needle}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn codex_smoke_fails_on_nonzero_exec() {
    for (mode, extra, needle) in [
        (
            "fail",
            vec![
                ("SMOKE_CODEX_RC", "1"),
                ("SMOKE_CODEX_ERR", "ERROR: usage limit reached"),
            ],
            "codex review exec failed (rc 1)",
        ),
        (
            "fail",
            vec![
                ("SMOKE_CODEX_RC", "1"),
                ("SMOKE_CODEX_ERR", "connection refused"),
                ("SMOKE_CODEX_WRITE_VALID", "1"),
            ],
            "codex review exec failed (rc 1)",
        ),
        (
            "fail",
            vec![
                ("SMOKE_CODEX_RC", "139"),
                ("SMOKE_CODEX_ERR", "codex crashed"),
            ],
            "codex review exec failed (rc 139)",
        ),
    ] {
        let (output, _) = run_codex_smoke(mode, &extra);
        assert_codex_smoke_failed(mode, &output, needle);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let err = extra
            .iter()
            .find(|(key, _)| *key == "SMOKE_CODEX_ERR")
            .map(|(_, value)| *value)
            .unwrap_or("");
        assert!(
            stderr.contains(err),
            "{mode}: dropped codex diagnostics\nstderr:\n{stderr}"
        );
    }
}

#[test]
fn codex_smoke_fails_when_verdict_missing() {
    let (output, _) = run_codex_smoke("missing", &[]);
    assert_codex_smoke_failed("missing", &output, "codex review verdict is missing");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("verdict file missing"),
        "missing: dropped missing-verdict evidence\nstderr:\n{stderr}"
    );
}

#[test]
fn codex_smoke_fails_when_verdict_not_schema_valid() {
    let payloads = [
        "",
        "   \n",
        "not-json",
        "{\"verdict\":\"approved\"}",
        "{\"verdict\":\"approved\",\"body\":{\"findings\":\"none\"}}",
        "{\"verdict\":\"yes\",\"body\":{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[]}}",
        "{\"verdict\":\"approved\",\"body\":{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[]},\"extra\":true}",
        "{\"verdict\":\"approved\",\"body\":{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[]}}{\"verdict\":\"approved\"}",
        "{\"verdict\":\"approved\",\"body\":{\"findings\":\"none\",\"security\":\"none\",\"tests_checked\":\"none\",\"not_checked\":\"none\"}}",
        "{\"verdict\":\"approved\",\"body\":{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[],\"token\":\"kestrel\"}}",
        "{\"verdict\":\"approved\",\"body\":{\"findings\":[1],\"security\":[],\"tests_checked\":[],\"not_checked\":[]}}",
        "[]",
        "{\"verdict\":\"approved\",\"body\":[]}",
    ];
    for payload in payloads {
        let (output, _) = run_codex_smoke("payload", &[("SMOKE_CODEX_VERDICT", payload)]);
        assert_codex_smoke_failed(
            "payload",
            &output,
            "codex review verdict is not schema-valid",
        );
    }
}

#[test]
fn codex_smoke_fails_when_sandbox_probe_does_not_hold() {
    for (mode, needle) in [
        ("probe_write", "codex read-only wrote codex-wrote.txt"),
        ("probe_dirty", "codex read-only changed the worktree"),
        ("probe_fail", "codex sandbox probe failed"),
        ("review_write", "codex read-only wrote codex-wrote.txt"),
    ] {
        let (output, _) = run_codex_smoke(mode, &[]);
        assert_codex_smoke_failed(mode, &output, needle);
    }
}

#[test]
fn codex_smoke_accepts_valid_verdict_and_separate_denial() {
    let verdicts = [
        "",
        r#"{"verdict":"approved","body":{"findings":[],"security":[],"tests_checked":[],"not_checked":[]}}"#,
        r#"{"verdict":"changes-requested","body":{"findings":["one"],"security":[],"tests_checked":["cargo test"],"not_checked":[]}}"#,
    ];
    for verdict in verdicts {
        let (output, log) = run_codex_smoke("ok", &[("SMOKE_CODEX_VERDICT", verdict)]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "verdict {verdict}: smoke failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stdout.contains("schema-valid verdict"),
            "verdict {verdict}: missing success line\nstdout:\n{stdout}"
        );
        let execs: Vec<_> = log
            .lines()
            .filter(|line| line.starts_with("exec "))
            .collect();
        assert_eq!(
            execs.len(),
            2,
            "verdict {verdict}: review and denial were not separate\nlog:\n{log}"
        );
        assert!(
            execs[0].contains("--output-schema"),
            "review argv lost --output-schema: {}",
            execs[0]
        );
        assert!(
            execs[0].contains("-m gpt-6.1-sol"),
            "review argv lost -m: {}",
            execs[0]
        );
        assert!(
            !execs[0].contains("codex-wrote.txt"),
            "review prompt still asks for a write: {}",
            execs[0]
        );
        assert!(
            execs[1].contains("codex-wrote.txt"),
            "denial probe did not ask for the write: {}",
            execs[1]
        );
        assert!(
            execs[1].contains("-m gpt-6.1-sol"),
            "denial probe lost -m: {}",
            execs[1]
        );
        assert!(
            !execs[1].contains("--output-schema"),
            "denial probe was schema-gated: {}",
            execs[1]
        );
    }
}

#[test]
fn codex_smoke_schema_matches_production_task_schema() {
    let script = fs::read_to_string(script_path()).expect("script");
    let marker = "<<'CODEX_TASK_SCHEMA'\n";
    let start = script.find(marker).expect("schema heredoc") + marker.len();
    let rest = &script[start..];
    let end = rest.find("\nCODEX_TASK_SCHEMA\n").expect("schema end");
    let smoke: serde_json::Value = serde_json::from_str(&rest[..end]).expect("smoke schema");

    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agents.rs"))
        .expect("agents.rs");
    let const_marker = "const TASK_CODEX_SCHEMA: &str = r#\"";
    let start = source.find(const_marker).expect("const") + const_marker.len();
    let end = source[start..].find("\"#").expect("const end");
    let production: serde_json::Value =
        serde_json::from_str(&source[start..start + end]).expect("production schema");
    assert_eq!(smoke, production);

    let model = relaybook_core::DEFAULT_CODEX_MODEL;
    assert!(script.contains(&format!("codex_model={model}")));
    assert!(
        script.matches("-m \"$codex_model\"").count() >= 2,
        "review and probe must both pass -m"
    );
}
