use std::io::{BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::live::Session;
use crate::stream;

const PLANNER: &str = include_str!("../templates/auto-planner.md");
const REVIEWER: &str = include_str!("../templates/auto-reviewer.md");
const EXECUTOR: &str = include_str!("../templates/auto-executor.md");

const CLAUDE_TOOLS: &str =
    "Bash(relaybook:*),Bash(graphify:*),Bash(git log:*),Bash(git diff:*),Read,Grep,Glob";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Planner,
    Reviewer,
    Executor,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Reviewer => "reviewer",
            Self::Executor => "executor",
        }
    }

    fn template(self) -> &'static str {
        match self {
            Self::Planner => PLANNER,
            Self::Reviewer => REVIEWER,
            Self::Executor => EXECUTOR,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AgentResult {
    Finished {
        session_id: Option<String>,
        est_cost_usd: f64,
        output: String,
    },
    UsageLimit {
        cli: String,
        reset_at: Option<String>,
        output: String,
    },
    Failed {
        output: String,
    },
    /// The CLI cannot be started. The run stops immediately, with no failure streak.
    Unavailable {
        cli: String,
    },
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccountKind {
    ApiKey,
    Subscription,
    Plan,
}

impl AccountKind {
    fn label(self) -> &'static str {
        match self {
            Self::ApiKey => "api key",
            Self::Subscription => "subscription account",
            Self::Plan => "plan account",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Accounts {
    pub claude: AccountKind,
    pub cursor: AccountKind,
}

pub(crate) fn preflight() -> Result<Accounts, String> {
    if crate::exe::Program::resolve("claude").is_none() {
        return Err("claude: not on PATH; install Claude Code".to_string());
    }
    if crate::exe::resolve_cursor().is_none() {
        return Err(
            "cursor: not on PATH; install Cursor CLI (`cursor-agent` or `agent`)".to_string(),
        );
    }
    let claude = claude_account()?;
    let cursor = cursor_account()?;
    Ok(Accounts { claude, cursor })
}

/// Classic preflight, plus `codex` on PATH and `codex login status` exiting 0.
/// Any non-zero status is not signed in. A usage-limit exit code is not special.
pub(crate) fn preflight_gated() -> Result<Accounts, String> {
    let accounts = preflight()?;
    require_codex()?;
    Ok(accounts)
}

pub(crate) fn require_codex() -> Result<(), String> {
    let Some(program) = crate::exe::Program::resolve("codex") else {
        return Err("codex: not on PATH".to_string());
    };
    let output = program
        .command()
        .args(["login", "status"])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                "codex: not on PATH".to_string()
            } else {
                format!("codex: {err}")
            }
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err("codex: not signed in; run codex login".to_string())
    }
}

pub(crate) fn account_line(accounts: &Accounts) -> String {
    format!(
        "claude: {}, cursor: {}",
        accounts.claude.label(),
        accounts.cursor.label()
    )
}

pub(crate) fn call_claude(
    cwd: &Path,
    role: Role,
    prompt: &str,
    budget: Option<&str>,
    run_id: i64,
    live: &mut Session,
) -> AgentResult {
    let Some(program) = crate::exe::Program::resolve("claude") else {
        return AgentResult::Failed {
            output: "claude: not on PATH; install Claude Code".to_string(),
        };
    };
    let mut command = program.command();
    command
        .current_dir(cwd)
        .arg("-p")
        .arg(prompt)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--append-system-prompt")
        .arg(role.template())
        .arg("--allowedTools")
        .arg(CLAUDE_TOOLS)
        .arg("--permission-mode")
        .arg("default")
        .arg("--disallowedTools")
        .arg("Edit,Write,NotebookEdit")
        .env("RELAYBOOK_RUN_ID", run_id.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(budget) = budget {
        command.arg("--max-budget-usd").arg(budget);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return AgentResult::Failed {
                output: err.to_string(),
            };
        }
    };
    drive(child, None, "claude", live)
}

pub(crate) fn call_cursor(
    cwd: &Path,
    prompt: &str,
    timeout: Duration,
    run_id: i64,
    live: &mut Session,
) -> AgentResult {
    let Some(program) = crate::exe::resolve_cursor() else {
        return AgentResult::Failed {
            output: "cursor: not on PATH; install Cursor CLI (`cursor-agent` or `agent`)"
                .to_string(),
        };
    };
    let child = match program
        .command()
        .current_dir(cwd)
        .arg("-p")
        .arg("--force")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--stream-partial-output")
        .arg(prompt)
        .env("RELAYBOOK_RUN_ID", run_id.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            return AgentResult::Failed {
                output: err.to_string(),
            };
        }
    };
    drive(child, Some(timeout), "cursor", live)
}

/// Bytes of the review packet (guide, handoff, and diff). Inside Codex's cap.
pub(crate) const REVIEW_PACKET_LIMIT: usize = 64 * 1024;

/// Prompt plus stdin, in characters. Verified on codex-cli 0.169.0.
pub(crate) const CODEX_INPUT_CHAR_LIMIT: usize = 1_048_576;

const TASK_CODEX_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["verdict", "body"],
  "properties": {
    "verdict": { "type": "string", "enum": ["approved", "changes-requested"] },
    "body": {
      "type": "object",
      "additionalProperties": true,
      "required": ["findings", "security", "tests_checked", "not_checked"],
      "properties": {
        "findings": {},
        "security": {},
        "tests_checked": {},
        "not_checked": {}
      }
    }
  }
}
"#;

const RELEASE_CODEX_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["verdict", "body"],
  "properties": {
    "verdict": { "type": "string", "enum": ["approved", "changes-requested"] },
    "body": {
      "type": "object",
      "additionalProperties": true,
      "required": ["findings", "security", "tests_checked", "not_checked", "prd_covered", "gaps"],
      "properties": {
        "findings": {},
        "security": {},
        "tests_checked": {},
        "not_checked": {},
        "prd_covered": {},
        "gaps": {}
      }
    }
  }
}
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexSandbox {
    ReadOnly,
    Isolated,
}

pub(crate) fn review_input_too_large(prompt: &str, packet: &str) -> bool {
    packet.len() > REVIEW_PACKET_LIMIT
        || prompt
            .chars()
            .count()
            .saturating_add(packet.chars().count())
            > CODEX_INPUT_CHAR_LIMIT
}

pub(crate) fn codex_args(
    root: &Path,
    schema: &Path,
    out: &Path,
    prompt: &str,
    sandbox: CodexSandbox,
) -> Vec<String> {
    let mut args = Vec::with_capacity(12);
    args.push("exec".to_string());
    if sandbox == CodexSandbox::ReadOnly {
        args.push("-s".to_string());
        args.push("read-only".to_string());
    }
    args.push("--ephemeral".to_string());
    args.push("--color".to_string());
    args.push("never".to_string());
    args.push("-C".to_string());
    args.push(root.display().to_string());
    args.push("--output-schema".to_string());
    args.push(schema.display().to_string());
    args.push("-o".to_string());
    args.push(out.display().to_string());
    args.push(prompt.to_string());
    args
}

/// One JSON object `{"verdict":"approved"|"changes-requested","body":{...}}`.
/// Trailing values are rejected. The body text is what `review_body` checks.
pub(crate) fn parse_codex_verdict(text: &str) -> Result<(String, String), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("codex output is empty".to_string());
    }
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| "codex output is not one JSON object".to_string())?;
    let Some(object) = value.as_object() else {
        return Err("codex output is not one JSON object".to_string());
    };
    let verdict = object
        .get("verdict")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "codex output is missing verdict".to_string())?;
    if verdict != "approved" && verdict != "changes-requested" {
        return Err("codex output has an invalid verdict".to_string());
    }
    let body = object
        .get("body")
        .filter(|item| item.is_object())
        .ok_or_else(|| "codex output is missing body".to_string())?;
    let body =
        serde_json::to_string(body).map_err(|_| "codex output body is not JSON".to_string())?;
    Ok((verdict.to_string(), body))
}

/// Run Codex and return the `-o` file only when the process exits 0 and that
/// file is non-empty. A non-zero exit, or a missing or empty file, is
/// [`AgentResult::Failed`]. A missing binary is [`AgentResult::Unavailable`].
#[allow(clippy::too_many_arguments)] // cwd, prompt, packet, paths, run id, and the sandbox are distinct
pub(crate) fn call_codex(
    cwd: &Path,
    prompt: &str,
    packet: &str,
    out: &Path,
    run_id: i64,
    live: &mut Session,
    sandbox: CodexSandbox,
    release: bool,
) -> AgentResult {
    let Some(program) = crate::exe::Program::resolve("codex") else {
        return AgentResult::Unavailable {
            cli: "codex".to_string(),
        };
    };
    let root = crate::gitinfo::toplevel(cwd).unwrap_or_else(|| cwd.to_path_buf());
    if let Some(parent) = out.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return AgentResult::Failed {
            output: err.to_string(),
        };
    }
    let schema = out.with_file_name("codex-schema.json");
    let schema_text = if release {
        RELEASE_CODEX_SCHEMA
    } else {
        TASK_CODEX_SCHEMA
    };
    if let Err(err) = std::fs::write(&schema, schema_text) {
        return AgentResult::Failed {
            output: err.to_string(),
        };
    }
    let _ = std::fs::remove_file(out);
    let args = codex_args(&root, &schema, out, prompt, sandbox);
    if let Some(flag) = forbidden_codex_arg(&args) {
        return AgentResult::Failed {
            output: format!("refusing codex flag {flag}"),
        };
    }
    let mut command = program.command();
    command
        .current_dir(cwd)
        .args(&args)
        .env("RELAYBOOK_RUN_ID", run_id.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return AgentResult::Unavailable {
                cli: "codex".to_string(),
            };
        }
        Err(err) => {
            return AgentResult::Failed {
                output: err.to_string(),
            };
        }
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let packet = packet.to_string();
    let stdin_thread = std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(packet.as_bytes());
        }
    });
    let stdout_thread = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut stdout) = stdout {
            let _ = stdout.read_to_string(&mut text);
        }
        text
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut text);
        }
        text
    });
    let status = match child.wait() {
        Ok(status) => status,
        Err(err) => {
            return AgentResult::Failed {
                output: err.to_string(),
            };
        }
    };
    let _ = stdin_thread.join();
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    if let Some(line) = stdout.lines().find(|line| !line.trim().is_empty()) {
        live.observe(&crate::stream::Piece::Text(line.to_string()));
    }
    if !status.success() {
        return AgentResult::Failed {
            output: format!(
                "codex exited {}\n{stdout}\n{stderr}",
                status.code().unwrap_or(-1)
            ),
        };
    }
    match std::fs::read_to_string(out) {
        Ok(text) if !text.trim().is_empty() => AgentResult::Finished {
            session_id: None,
            est_cost_usd: 0.0,
            output: text,
        },
        Ok(_) => AgentResult::Failed {
            output: format!("codex output is empty\n{stderr}"),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => AgentResult::Failed {
            output: format!("codex output is missing\n{stderr}"),
        },
        Err(err) => AgentResult::Failed {
            output: err.to_string(),
        },
    }
}

fn forbidden_codex_arg(args: &[String]) -> Option<&str> {
    for (index, arg) in args.iter().enumerate() {
        if arg == "--add-dir"
            || arg == "--approve-for-me"
            || arg == "--trust"
            || arg == "--worktree"
            || arg == "apply"
            || arg == "danger-full-access"
            || arg == "workspace-write"
            || arg.starts_with("--dangerously")
        {
            return Some(arg.as_str());
        }
        if arg == "-s" && args.get(index + 1).map(String::as_str) != Some("read-only") {
            return Some(arg.as_str());
        }
    }
    None
}

fn drive(
    mut child: std::process::Child,
    timeout: Option<Duration>,
    cli: &str,
    live: &mut Session,
) -> AgentResult {
    let Some(stdout) = child.stdout.take() else {
        return AgentResult::Failed {
            output: "missing stdout".to_string(),
        };
    };
    let Some(stderr) = child.stderr.take() else {
        return AgentResult::Failed {
            output: "missing stderr".to_string(),
        };
    };
    let (tx, rx) = mpsc::channel();
    let stdout_thread = std::thread::spawn(move || {
        stream::read_lines(BufReader::new(stdout), |line| {
            let _ = tx.send(line.to_string());
        });
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut text = String::new();
        stream::read_lines(BufReader::new(stderr), |line| {
            text.push_str(line);
            text.push('\n');
        });
        text
    });
    let started = Instant::now();
    let mut raw = String::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => {
                raw.push_str(&line);
                raw.push('\n');
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                    for piece in stream::pieces(&value) {
                        live.observe(&piece);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if timeout.is_some_and(|limit| started.elapsed() >= limit) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_thread.join();
                    let _ = stderr_thread.join();
                    return AgentResult::TimedOut;
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        while let Ok(line) = rx.try_recv() {
                            raw.push_str(&line);
                            raw.push('\n');
                            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                                for piece in stream::pieces(&value) {
                                    live.observe(&piece);
                                }
                            }
                        }
                        let _ = stdout_thread.join();
                        let stderr = stderr_thread.join().unwrap_or_default();
                        return classify(cli, status.success(), &raw, &stderr);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let _ = child.kill();
                        return AgentResult::Failed {
                            output: err.to_string(),
                        };
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let status = child.wait();
                let _ = stdout_thread.join();
                let stderr = stderr_thread.join().unwrap_or_default();
                return match status {
                    Ok(status) => classify(cli, status.success(), &raw, &stderr),
                    Err(err) => AgentResult::Failed {
                        output: err.to_string(),
                    },
                };
            }
        }
    }
}

pub(crate) fn write_transcript(
    root: &Path,
    run_id: i64,
    index: u32,
    role: Role,
    body: &str,
) -> std::io::Result<PathBuf> {
    let dir = root
        .join(".relaybook")
        .join("runs")
        .join(run_id.to_string());
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{index}-{}.json", role.as_str()));
    let mut file = std::fs::File::create(&path)?;
    file.write_all(body.as_bytes())?;
    Ok(path)
}

pub(crate) fn executor_template() -> &'static str {
    Role::Executor.template()
}

pub(crate) fn cursor_timeout(minutes: i64) -> Duration {
    if let Ok(seconds) = std::env::var("RELAYBOOK_CURSOR_TIMEOUT_SECS")
        && let Ok(seconds) = seconds.parse::<u64>()
    {
        return Duration::from_secs(seconds);
    }
    Duration::from_secs(u64::try_from(minutes.max(1)).unwrap_or(60) * 60)
}

pub(crate) fn classify(cli: &str, ok: bool, stdout: &str, stderr: &str) -> AgentResult {
    let combined = format!("{stdout}\n{stderr}");
    if let Some(reset_at) = usage_limit(&combined) {
        return AgentResult::UsageLimit {
            cli: cli.to_string(),
            reset_at,
            output: combined,
        };
    }
    if !ok {
        return AgentResult::Failed { output: combined };
    }
    let value = stdout.lines().rev().find_map(|line| {
        let line = line.trim();
        if line.starts_with('{') {
            serde_json::from_str::<serde_json::Value>(line).ok()
        } else {
            None
        }
    });
    let session_id = value
        .as_ref()
        .and_then(|item| item.get("session_id"))
        .and_then(|item| item.as_str())
        .map(str::to_string);
    let est_cost_usd = value.as_ref().and_then(cost_field).unwrap_or(0.0);
    AgentResult::Finished {
        session_id,
        est_cost_usd,
        output: combined,
    }
}

fn cost_field(value: &serde_json::Value) -> Option<f64> {
    for key in ["total_cost_usd", "cost_usd"] {
        if let Some(number) = value.get(key).and_then(serde_json::Value::as_f64) {
            return Some(number);
        }
    }
    None
}

fn usage_limit(text: &str) -> Option<Option<String>> {
    let lower = text.to_ascii_lowercase();
    let limited = lower.contains("usage limit")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("hit your limit")
        || lower.contains("too many requests");
    if !limited {
        return None;
    }
    Some(reset_time(text))
}

fn reset_time(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(index) = rest.find("20") {
        let candidate = &rest[index..];
        let token: String = candidate
            .chars()
            .take_while(|ch| ch.is_ascii_digit() || "-:TZ.".contains(*ch))
            .collect();
        if token.len() >= 20 && token.contains('T') {
            return Some(token);
        }
        rest = &rest[index + 2..];
    }
    None
}

fn claude_account() -> Result<AccountKind, String> {
    if std::env::var_os("ANTHROPIC_API_KEY").is_some_and(|value| !value.is_empty()) {
        return Ok(AccountKind::ApiKey);
    }
    let output = Command::new("claude").args(["auth", "status"]).output();
    let text = output
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            format!(
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .unwrap_or_default()
        .to_ascii_lowercase();
    if text.contains("logged") || text.contains("subscription") || text.contains("org") {
        Ok(AccountKind::Subscription)
    } else {
        Err("claude: not signed in; run claude /login".to_string())
    }
}

fn cursor_account() -> Result<AccountKind, String> {
    let Some(program) = crate::exe::resolve_cursor() else {
        return Err(
            "cursor: not on PATH; install Cursor CLI (`cursor-agent` or `agent`)".to_string(),
        );
    };
    for args in [["status"], ["about"]] {
        let output = program.command().args(args).output();
        let Some(output) = output.ok().filter(|item| item.status.success()) else {
            continue;
        };
        let text = format!(
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .to_ascii_lowercase();
        if text.contains("logged in") || text.contains('@') {
            return Ok(AccountKind::Plan);
        }
    }
    Err("cursor: not signed in; run cursor-agent login".to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        AccountKind, Accounts, AgentResult, CodexSandbox, account_line, classify, claude_account,
        codex_args, forbidden_codex_arg, parse_codex_verdict, review_input_too_large,
    };

    #[test]
    fn usage_limit_is_not_a_failure_and_keeps_the_reset_time() {
        let result = classify(
            "claude",
            false,
            "usage limit reached. resets at 2026-11-10T18:00:00Z",
            "",
        );
        assert_eq!(
            result,
            AgentResult::UsageLimit {
                cli: "claude".to_string(),
                reset_at: Some("2026-11-10T18:00:00Z".to_string()),
                output: "usage limit reached. resets at 2026-11-10T18:00:00Z\n".to_string(),
            }
        );
    }

    #[test]
    fn json_cost_is_an_estimate() {
        let result = classify(
            "claude",
            true,
            r#"{"session_id":"s1","total_cost_usd":0.25}"#,
            "",
        );
        match result {
            AgentResult::Finished {
                session_id,
                est_cost_usd,
                ..
            } => {
                assert_eq!(session_id.as_deref(), Some("s1"));
                assert_eq!(est_cost_usd, 0.25);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn account_line_never_prints_the_api_key_or_an_address() {
        let sentinel = "sentinel-key-value";
        let previous = std::env::var_os("ANTHROPIC_API_KEY");
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", sentinel) };
        let kind = claude_account().expect("api key");
        let line = account_line(&Accounts {
            claude: kind,
            cursor: AccountKind::Plan,
        });
        unsafe {
            match previous {
                Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
                None => std::env::remove_var("ANTHROPIC_API_KEY"),
            }
        }
        assert_eq!(kind, AccountKind::ApiKey);
        assert_eq!(line, "claude: api key, cursor: plan account");
        assert!(!line.contains(sentinel));
        assert!(!line.contains('@'));
    }

    #[test]
    fn codex_read_only_argv_uses_only_verified_flags() {
        let args = codex_args(
            Path::new("/repo"),
            Path::new("/tmp/codex-schema.json"),
            Path::new("/tmp/codex-last.md"),
            "Review task work. Use only the packet on stdin.",
            CodexSandbox::ReadOnly,
        );
        let joined = args.join(" ");
        assert!(joined.starts_with("exec -s read-only --ephemeral --color never -C /repo "));
        assert!(joined.contains("--output-schema /tmp/codex-schema.json"));
        assert!(joined.contains("-o /tmp/codex-last.md"));
        assert!(forbidden_codex_arg(&args).is_none(), "{joined}");
        for flag in [
            "--add-dir",
            "--dangerously",
            "--approve-for-me",
            "--trust",
            "--worktree",
            "apply",
            "danger-full-access",
        ] {
            assert!(!joined.contains(flag), "{joined}");
        }
    }

    #[test]
    fn isolated_codex_argv_omits_the_sandbox_flag() {
        let args = codex_args(
            Path::new("/repo"),
            Path::new("/tmp/codex-schema.json"),
            Path::new("/tmp/codex-last.md"),
            "Release review. Use only the packet on stdin.",
            CodexSandbox::Isolated,
        );
        assert!(!args.iter().any(|arg| arg == "-s"));
        assert!(!args.iter().any(|arg| arg == "read-only"));
        assert!(args.contains(&"--ephemeral".to_string()));
        assert!(args.contains(&"--output-schema".to_string()));
        assert!(forbidden_codex_arg(&args).is_none());
    }

    #[test]
    fn codex_verdict_must_be_one_json_object() {
        let (verdict, body) = parse_codex_verdict(
            "{\"verdict\":\"approved\",\"body\":{\"findings\":[],\"security\":[]}}\n",
        )
        .expect("object");
        assert_eq!(verdict, "approved");
        assert!(body.contains("findings"));
        assert!(parse_codex_verdict("").is_err());
        assert!(parse_codex_verdict("   ").is_err());
        assert!(parse_codex_verdict("not json").is_err());
        assert!(parse_codex_verdict("[]").is_err());
        assert!(
            parse_codex_verdict("{\"verdict\":\"approved\"}{\"verdict\":\"approved\"}").is_err()
        );
        assert!(parse_codex_verdict("{\"verdict\":\"approved\",\"body\":[]}").is_err());
        assert!(parse_codex_verdict("{\"verdict\":\"yes\",\"body\":{}}").is_err());
    }

    #[test]
    fn review_packet_limit_fails_closed_before_the_cli_cap() {
        let prompt = "Review task work.";
        assert!(!review_input_too_large(prompt, &"a".repeat(64 * 1024)));
        assert!(review_input_too_large(prompt, &"a".repeat(64 * 1024 + 1)));
        let packet = "a".repeat(1_048_576);
        assert!(review_input_too_large(prompt, &packet));
    }
}
