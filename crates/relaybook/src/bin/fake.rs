use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
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
        "codex" => codex(&args),
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

/// Planner and executor skip their `edit` and `tag` side effects. Codex still
/// performs the kind. Classic tests leave this unset.
fn reviewer_only() -> bool {
    env::var("RELAYBOOK_FAKE_REVIEWER_ONLY").ok().as_deref() == Some("1")
}

fn graphify(args: &[String]) {
    if let Ok(path) = env::var("GRAPHIFY_CWD_LOG") {
        let cwd = env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        append(&path, &cwd);
    }
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
    match env::var("RELAYBOOK_FAKE_GRAPHIFY").ok().as_deref() {
        Some("graph") => {
            if args.first().map(String::as_str) == Some("update") {
                write_graph();
            }
            return;
        }
        Some("affected-large") => {
            if args.first().map(String::as_str) == Some("update") {
                write_graph();
            }
            if args.first().map(String::as_str) == Some("affected") {
                print!("{}", "a".repeat(5000));
            }
            return;
        }
        Some("fail") => {
            eprintln!("boom");
            std::process::exit(1);
        }
        Some("hold") => {
            if args.first().map(String::as_str) == Some("update") && !wait_for_graphify_hold() {
                eprintln!("graphify hold timed out");
                std::process::exit(98);
            }
            if args.first().map(String::as_str) == Some("update") {
                write_graph();
            }
            return;
        }
        _ => {}
    }
    eprintln!("boom");
    if sibling_fail() {
        std::process::exit(1);
    }
}

fn write_graph() {
    let _ = fs::create_dir_all("graphify-out");
    let _ = fs::write("graphify-out/graph.json", "{}\n");
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
        "edit" if !reviewer_only() => {
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
        emit_stream(
            r#"{"type":"result","session_id":"sess-1","total_cost_usd":0.20,"result":"stopped"}"#,
        );
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
        emit_stream(
            r#"{"type":"result","session_id":"sess-1","total_cost_usd":0.20,"result":"planned"}"#,
        );
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
    emit_stream(
        r#"{"type":"result","session_id":"sess-1","total_cost_usd":0.20,"result":"reviewed"}"#,
    );
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
        "tag" if !reviewer_only() => {
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
            emit_stream(r#"{"type":"result","subtype":"success","ok":true}"#);
            return;
        }
        _ => {}
    }
    relay(&["task", "claim", "work", "--by", "cursor"]);
    let _ = append_file("work.txt", "hello\n");
    git(&["add", "work.txt"]);
    if kind() == "huge" {
        let _ = fs::write("huge.txt", "x".repeat(80_000));
        git(&["add", "huge.txt"]);
    }
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
    emit_stream(r#"{"type":"result","subtype":"success","ok":true}"#);
}

fn codex(args: &[String]) {
    if args.first().map(String::as_str) == Some("login") {
        if env::var("RELAYBOOK_FAKE_CODEX_AUTH").ok().as_deref() == Some("missing") {
            eprintln!("Not logged in");
            std::process::exit(1);
        }
        println!("Logged in using ChatGPT");
        return;
    }
    if let Ok(log) = env::var("LOG") {
        append(
            &PathBuf::from(log).join("codex.args").display().to_string(),
            &args.join(" "),
        );
    }
    let mut packet = String::new();
    let _ = std::io::stdin().read_to_string(&mut packet);
    if let Ok(log) = env::var("LOG") {
        append(
            &PathBuf::from(log).join("codex.stdin").display().to_string(),
            &packet,
        );
    }
    let out = flag_value(args, "-o").or_else(|| flag_value(args, "--output-last-message"));
    match kind().as_str() {
        "edit" => {
            let _ = fs::write("reviewer-edit.txt", "edited\n");
        }
        "branch" => {
            git(&["commit", "--allow-empty", "-m", "reviewer moved HEAD"]);
        }
        "tag" => {
            git(&["tag", "reviewer-tag"]);
        }
        "config" => {
            git(&["config", "reviewer.probe", "1"]);
        }
        "hook" => {
            let _ = fs::create_dir_all(".git/hooks");
            let _ = fs::write(".git/hooks/pre-commit", "#!/bin/sh\n");
        }
        "hook-target" => {
            // Edit the file a hook symlink points at. Leave the link unchanged.
            let _ = fs::write(".git/shared-pre-commit", "#!/bin/sh\necho changed\n");
        }
        "ledger" => {
            relay(&[
                "decision",
                "log",
                "work",
                "--why",
                "reviewer wrote",
                "--by",
                "codex",
            ]);
        }
        "ledger-fail" => {
            relay(&[
                "decision",
                "log",
                "work",
                "--why",
                "reviewer wrote",
                "--by",
                "codex",
            ]);
            write_out(out.as_deref(), "");
            std::process::exit(1);
        }
        "ledger-garbage" => {
            relay(&[
                "decision",
                "log",
                "work",
                "--why",
                "reviewer wrote",
                "--by",
                "codex",
            ]);
            write_out(out.as_deref(), "not json\n");
            return;
        }
        "garbage" => {
            write_out(out.as_deref(), "not json\n");
            return;
        }
        "schema" => {
            let text = verdict_json(args, "approved");
            let text = text.trim_end();
            let Some(text) = text.strip_suffix('}') else {
                write_out(out.as_deref(), "not json\n");
                return;
            };
            write_out(out.as_deref(), &format!("{text},\"injected\":true}}\n"));
            return;
        }
        "hold" => {
            let Some(dir) = env::var("RELAYBOOK_CODEX_HOLD").ok() else {
                eprintln!("RELAYBOOK_CODEX_HOLD is unset");
                std::process::exit(99);
            };
            let seq = claim_hold_slot(&dir);
            if seq == 0 {
                eprintln!("codex hold slot failed");
                std::process::exit(98);
            }
            let root = PathBuf::from(&dir);
            let _ = fs::write(
                root.join(format!("started-{seq}")),
                out.clone().unwrap_or_default(),
            );
            let release = root.join(format!("release-{seq}"));
            for _ in 0..1600 {
                if release.is_file() {
                    let text = fs::read_to_string(&release).unwrap_or_default();
                    if let Some(rest) = text.strip_prefix("relaybook-hold-exit ") {
                        let mut lines = rest.splitn(2, '\n');
                        let code = lines
                            .next()
                            .unwrap_or("1")
                            .trim()
                            .parse::<i32>()
                            .unwrap_or(1);
                        write_out(out.as_deref(), lines.next().unwrap_or(""));
                        std::process::exit(code);
                    }
                    write_out(out.as_deref(), &text);
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            eprintln!("codex hold timed out");
            std::process::exit(98);
        }
        "empty" | "fail" => {
            write_out(out.as_deref(), "");
            std::process::exit(1);
        }
        "changes" => {
            let marker = PathBuf::from(".git/codex-asked-changes");
            if marker.is_file() {
                write_out(out.as_deref(), &verdict_json(args, "approved"));
            } else {
                let _ = fs::create_dir_all(".git");
                let _ = fs::write(&marker, "1\n");
                write_out(out.as_deref(), &verdict_json(args, "changes-requested"));
            }
            return;
        }
        _ => {}
    }
    write_out(out.as_deref(), &verdict_json(args, "approved"));
}

fn wait_for_graphify_hold() -> bool {
    let Some(dir) = env::var("RELAYBOOK_GRAPHIFY_HOLD").ok() else {
        eprintln!("RELAYBOOK_GRAPHIFY_HOLD is unset");
        std::process::exit(99);
    };
    let from = env::var("RELAYBOOK_GRAPHIFY_HOLD_FROM")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(1);
    let seq = claim_hold_slot(&dir);
    if seq == 0 {
        eprintln!("graphify hold slot failed");
        std::process::exit(98);
    }
    let root = PathBuf::from(&dir);
    let _ = fs::write(root.join(format!("started-{seq}")), "update");
    if seq < from {
        return true;
    }
    let release = root.join(format!("release-{seq}"));
    for _ in 0..1600 {
        if release.is_file() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn claim_hold_slot(dir: &str) -> i64 {
    let root = PathBuf::from(dir);
    let _ = fs::create_dir_all(&root);
    for n in 1..100 {
        let path = root.join(format!("claim-{n}"));
        if OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .is_ok()
        {
            return n;
        }
    }
    0
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn verdict_json(args: &[String], verdict: &str) -> String {
    let release = args.iter().any(|arg| arg.contains("Release review"));
    if release {
        format!(
            "{{\"verdict\":\"{verdict}\",\"body\":{{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[],\"prd_covered\":[],\"gaps\":[]}}}}\n"
        )
    } else {
        format!(
            "{{\"verdict\":\"{verdict}\",\"body\":{{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[]}}}}\n"
        )
    }
}

fn write_out(path: Option<&str>, text: &str) {
    let Some(path) = path else {
        return;
    };
    if let Some(parent) = PathBuf::from(path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, text);
}

fn emit_stream(result: &str) {
    println!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Planning the next step."}}]}}}}"#
    );
    println!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Read","input":{{"file_path":"src/x.rs"}}}}]}}}}"#
    );
    println!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Edit","input":{{"file_path":"src/y.rs"}}}}]}}}}"#
    );
    println!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Bash","input":{{"command":"cargo test"}}}}]}}}}"#
    );
    println!("{result}");
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
