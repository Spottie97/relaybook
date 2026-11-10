use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Accounts {
    pub claude: String,
    pub cursor: String,
}

pub(crate) fn preflight() -> Result<Accounts, String> {
    if !on_path("claude") {
        return Err("claude: not on PATH; install Claude Code".to_string());
    }
    if !on_path("cursor-agent") {
        return Err("cursor: not on PATH; install Cursor CLI".to_string());
    }
    let claude = claude_account()?;
    let cursor = cursor_account()?;
    Ok(Accounts { claude, cursor })
}

pub(crate) fn account_line(accounts: &Accounts) -> String {
    format!("claude: {}, cursor: {}", accounts.claude, accounts.cursor)
}

pub(crate) fn call_claude(
    cwd: &Path,
    role: Role,
    prompt: &str,
    budget: Option<&str>,
    run_id: i64,
) -> AgentResult {
    let mut command = Command::new("claude");
    command
        .current_dir(cwd)
        .arg("-p")
        .arg(prompt)
        .arg("--output-format")
        .arg("json")
        .arg("--append-system-prompt")
        .arg(role.template())
        .arg("--allowedTools")
        .arg(CLAUDE_TOOLS)
        .arg("--permission-mode")
        .arg("default")
        .arg("--disallowedTools")
        .arg("Edit,Write,NotebookEdit")
        .env("RELAYBOOK_RUN_ID", run_id.to_string());
    if let Some(budget) = budget {
        command.arg("--max-budget-usd").arg(budget);
    }
    let output = command.output();
    finish("claude", output)
}

pub(crate) fn call_cursor(cwd: &Path, prompt: &str, timeout: Duration, run_id: i64) -> AgentResult {
    let mut child = match Command::new("cursor-agent")
        .current_dir(cwd)
        .arg("-p")
        .arg("--force")
        .arg("--output-format")
        .arg("json")
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
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = std::io::Read::read_to_string(&mut pipe, &mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
                }
                return classify("cursor", status.success(), &stdout, &stderr);
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return AgentResult::TimedOut;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(err) => {
                return AgentResult::Failed {
                    output: err.to_string(),
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

fn finish(cli: &str, output: std::io::Result<std::process::Output>) -> AgentResult {
    match output {
        Ok(output) => classify(
            cli,
            output.status.success(),
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        ),
        Err(err) => AgentResult::Failed {
            output: err.to_string(),
        },
    }
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

fn claude_account() -> Result<String, String> {
    if std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok("api key".to_string());
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
        Ok("subscription account".to_string())
    } else {
        Err("claude: not signed in; run claude /login".to_string())
    }
}

fn cursor_account() -> Result<String, String> {
    for args in [["status"], ["about"]] {
        let output = Command::new("cursor-agent").args(args).output();
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
            return Ok("plan account".to_string());
        }
    }
    Err("cursor: not signed in; run cursor-agent login".to_string())
}

fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

#[cfg(test)]
mod tests {
    use super::{AgentResult, classify};

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
}
