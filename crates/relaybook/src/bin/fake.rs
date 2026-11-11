use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

fn main() {
    let role = role_name();
    let args: Vec<String> = env::args().skip(1).collect();
    match role.as_str() {
        "graphify" => graphify(&args),
        "rtk" => rtk(&args),
        "gh" => gh(&args),
        "claude" => claude(&args),
        "cursor-agent" | "agent" => cursor(&args),
        other => {
            eprintln!("unknown fake role {other}");
            std::process::exit(1);
        }
    }
}

fn role_name() -> String {
    env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "fake".to_string())
}

fn kind() -> String {
    env::var("RELAYBOOK_FAKE_KIND").unwrap_or_default()
}

fn graphify(args: &[String]) {
    if env::var("RELAYBOOK_FAKE_GRAPHIFY").ok().as_deref() == Some("claude-settings")
        && args.first().map(String::as_str) == Some("claude")
    {
        let _ = fs::create_dir_all(".claude");
        let _ = fs::write(
            ".claude/settings.json",
            "{\"hooks\":{\"PreToolUse\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"/opt/graphify hook-guard search\",\"timeout\":10}]}]}}\n",
        );
        std::process::exit(0);
    }
    if let Ok(log) = env::var("GRAPHIFY_LOG") {
        append(&log, &args.join(" "));
    }
    eprintln!("boom");
    if sibling_fail() {
        std::process::exit(1);
    }
}

fn rtk(args: &[String]) {
    if args.first().map(String::as_str) == Some("verify") {
        std::process::exit(0);
    }
    std::process::exit(1);
}

fn gh(args: &[String]) {
    if let Ok(log) = env::var("LOG") {
        append(
            &PathBuf::from(log).join("gh.args").display().to_string(),
            &args.join(" "),
        );
    }
    println!("https://example.test/pr/1");
}

fn claude(args: &[String]) {
    if args.first().map(String::as_str) == Some("auth") {
        println!("logged in");
        return;
    }
    if let Ok(log) = env::var("LOG") {
        append(
            &PathBuf::from(log).join("claude.args").display().to_string(),
            &args.join(" "),
        );
    }
    match kind().as_str() {
        "edit" => {
            let _ = fs::write("dirty.txt", "dirty\n");
        }
        "fail" => std::process::exit(1),
        "limit" if !PathBuf::from(".git/limited").is_file() => {
            let _ = fs::create_dir_all(".git");
            let _ = fs::write(".git/limited", "limited\n");
            println!("usage limit reached. resets at 2026-11-10T18:00:00Z");
            std::process::exit(1);
        }
        _ => {}
    }
    let joined = args.join(" ");
    if kind() == "stop" && joined.contains("PRD:") {
        relay(&["run", "stop"]);
        println!(r#"{{"session_id":"sess-1","total_cost_usd":0.20}}"#);
        return;
    }
    if kind() == "depend" && joined.contains("PRD:") {
        relay(&[
            "task",
            "create",
            "uploads",
            "--title",
            "Upload",
            "--depends-on",
            "login",
        ]);
        println!(r#"{{"session_id":"sess-1","total_cost_usd":0.20}}"#);
        return;
    }
    if joined.contains("PRD:") {
        relay(&["task", "create", "work", "--title", "Do the work"]);
        relay(&["step", "add", "work", "ship it"]);
        if kind() == "two" {
            relay(&[
                "task",
                "create",
                "more",
                "--title",
                "Do more",
                "--depends-on",
                "work",
            ]);
        }
    } else if kind() == "reopen" {
        relay(&["task", "reopen", "work", "--reason", "try again"]);
    } else if kind() == "editdirty" {
        let _ = append_file("extra.txt", "more\n");
        relay(&["report", "accept", "work"]);
        relay(&["run", "complete"]);
    } else {
        relay(&["report", "accept", "work"]);
        relay(&["run", "complete"]);
    }
    println!(r#"{{"session_id":"sess-1","total_cost_usd":0.20}}"#);
}

fn cursor(args: &[String]) {
    if args.first().map(String::as_str) == Some("--version") {
        println!("Cursor Agent 0.0.0-test");
        return;
    }
    if args
        .first()
        .map(String::as_str)
        .is_some_and(|arg| arg == "status" || arg == "about")
    {
        println!("logged in");
        return;
    }
    if let Ok(log) = env::var("LOG") {
        append(
            &PathBuf::from(log).join("cursor.args").display().to_string(),
            &args.join(" "),
        );
    }
    match kind().as_str() {
        "fail" => std::process::exit(1),
        "limit-cursor" => {
            println!("rate limit");
            std::process::exit(1);
        }
        "block" => {
            relay(&["task", "block", "work", "--reason", "stuck"]);
            return;
        }
        "tag" => {
            git(&["tag", "v9"]);
        }
        "extra" => {
            let _ = fs::write("extra.txt", "leftover\n");
        }
        "editdirty" => {
            let _ = fs::write("extra.txt", "before\n");
        }
        "switch" => {
            git(&["checkout", "main"]);
            return;
        }
        "manual" => {
            relay(&["mode", "manual"]);
        }
        "sleep" => thread::sleep(Duration::from_secs(30)),
        "depend" => {
            let slug = task_slug(args).unwrap_or_else(|| "uploads".to_string());
            finish_slug(&slug);
            println!(r#"{{"ok":true}}"#);
            return;
        }
        _ => {}
    }
    relay(&["task", "claim", "work", "--by", "cursor"]);
    let _ = append_file("work.txt", "hello\n");
    git(&["add", "work.txt"]);
    git(&["commit", "-m", "do the work"]);
    relay(&[
        "task",
        "done",
        "work",
        "--changed",
        "added work",
        "--verified",
        "git show",
        "--not-verified",
        "live",
        "--review",
        "diff",
    ]);
    if kind() == "pushmain" {
        git(&["push", "origin", "HEAD:main"]);
    }
    println!(r#"{{"ok":true}}"#);
}

fn task_slug(args: &[String]) -> Option<String> {
    let joined = args.join(" ");
    let marker = "Task slug: ";
    let rest = joined.split(marker).nth(1)?;
    let slug = rest.split_whitespace().next()?;
    Some(slug.to_string())
}

fn finish_slug(slug: &str) {
    relay(&["task", "claim", slug, "--by", "cursor"]);
    let _ = append_file(&format!("{slug}.txt"), "hello\n");
    git(&["add", &format!("{slug}.txt")]);
    git(&["commit", "-m", &format!("do {slug}")]);
    relay(&[
        "task",
        "done",
        slug,
        "--changed",
        "added work",
        "--verified",
        "git show",
        "--not-verified",
        "live",
        "--review",
        "diff",
    ]);
}

fn sibling_fail() -> bool {
    let Ok(exe) = env::current_exe() else {
        return false;
    };
    let mut marker = exe;
    marker.set_extension("fail");
    marker.is_file()
}

fn append(path: &str, line: &str) {
    if let Some(parent) = PathBuf::from(path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn append_file(path: &str, text: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(text.as_bytes())
}

fn relay(args: &[&str]) {
    let Some(bin) = env::var_os("RELAYBOOK") else {
        return;
    };
    let _ = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn git(args: &[&str]) {
    let _ = Command::new("git").args(args).status();
}
