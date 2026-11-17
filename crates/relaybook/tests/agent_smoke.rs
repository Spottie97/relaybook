#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    assert!(clear_at < graph_at && graph_at < work_at);
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
