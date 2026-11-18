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
/// `codex login status` does not accept `-m` (codex-cli 0.169.0 `--help`).
pub(crate) fn preflight_gated() -> Result<Accounts, String> {
    let accounts = preflight()?;
    require_codex()?;
    Ok(accounts)
}

pub(crate) fn codex_login_status_args() -> [&'static str; 2] {
    // codex-cli 0.169.0 `codex login status --help` lists `-c`, `--enable`,
    // and `--disable`. It does not accept `-m`.
    ["login", "status"]
}

pub(crate) fn require_codex() -> Result<(), String> {
    let Some(program) = crate::exe::Program::resolve("codex") else {
        return Err("codex: not on PATH".to_string());
    };
    let output = program
        .command()
        .args(codex_login_status_args())
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
      "additionalProperties": false,
      "required": ["findings", "security", "tests_checked", "not_checked"],
      "properties": {
        "findings": { "type": "array", "items": { "type": "string" } },
        "security": { "type": "array", "items": { "type": "string" } },
        "tests_checked": { "type": "array", "items": { "type": "string" } },
        "not_checked": { "type": "array", "items": { "type": "string" } }
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
      "additionalProperties": false,
      "required": ["findings", "security", "tests_checked", "not_checked", "prd_covered", "gaps"],
      "properties": {
        "findings": { "type": "array", "items": { "type": "string" } },
        "security": { "type": "array", "items": { "type": "string" } },
        "tests_checked": { "type": "array", "items": { "type": "string" } },
        "not_checked": { "type": "array", "items": { "type": "string" } },
        "prd_covered": { "type": "array", "items": { "type": "string" } },
        "gaps": { "type": "array", "items": { "type": "string" } }
      }
    }
  }
}
"#;

pub(crate) fn task_codex_schema() -> &'static str {
    TASK_CODEX_SCHEMA
}

pub(crate) fn release_codex_schema() -> &'static str {
    RELEASE_CODEX_SCHEMA
}

/// Probe output. Strict object with one boolean. Success is exactly `{"ok":true}`.
const PROBE_CODEX_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["ok"],
  "properties": {
    "ok": { "type": "boolean" }
  }
}
"#;

/// Stderr phrases captured from codex-cli 0.169.0 in codex-verification-v3 E2.
/// The apostrophe in the first phrase is U+2019. Nothing else is a limit.
const CODEX_LIMIT_SIGNATURES: &[(&str, &str)] = &[
    ("usage_limit", "You\u{2019}ve hit your usage limit"),
    (
        "quota_exceeded",
        "Quota exceeded. Check your plan and billing details.",
    ),
];

/// `codex --version` lines this build will treat as a usage limit.
const CODEX_VERIFIED_VERSIONS: &[&str] = &["codex-cli 0.169.0"];

/// Stderr phrases that stay on the ordinary failure path even if a limit phrase is also present.
const CODEX_ORDINARY_SIGNATURES: &[&str] = &["401", "input_too_large", "Not logged in"];

/// Read-only tools the Sonnet reviewer is allowed to name.
const SONNET_REVIEW_TOOLS: &str = "Read,Grep,Glob";

const SONNET_DISALLOWED_TOOLS: &str = "Edit,Write,NotebookEdit,Bash,WebFetch,WebSearch";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexExit {
    Ok,
    Limited { signature: String },
    Ordinary,
}

/// A classified Codex usage limit. `retry_after` is always `None`: the E2
/// text is a zone-less local clock ("try again at 6:39 AM"), so the driver
/// uses the configured backoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexLimit {
    pub signature: String,
    pub version: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexSandbox {
    ReadOnly,
    Isolated,
}

const TASK_BODY_KEYS: &[&str] = &["findings", "security", "tests_checked", "not_checked"];
const RELEASE_BODY_KEYS: &[&str] = &[
    "findings",
    "security",
    "tests_checked",
    "not_checked",
    "prd_covered",
    "gaps",
];

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
    model: &str,
) -> Result<Vec<String>, String> {
    let model = relaybook_core::parse_codex_model(model).map_err(|err| err.to_string())?;
    let mut args = Vec::with_capacity(14);
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
    args.push("-m".to_string());
    args.push(model);
    args.push("--output-schema".to_string());
    args.push(schema.display().to_string());
    args.push("-o".to_string());
    args.push(out.display().to_string());
    args.push(prompt.to_string());
    Ok(args)
}

/// `-o` path for one review. The directory is outside `root` so an isolated
/// reviewer can create the file when the repository is read-only. The directory
/// itself is private; see [`prepare_codex_output`].
pub(crate) fn codex_output_file(
    root: &Path,
    run_id: i64,
    review_id: i64,
    generation: i64,
) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let name = format!(
        "relaybook-codex-{run_id}-{review_id}-{generation}-{}-{nanos}",
        std::process::id()
    );
    let mut base = std::env::temp_dir();
    if path_is_inside(root, &base.join(&name)) {
        base = PathBuf::from("/tmp");
    }
    base.join(name).join("codex-last.md")
}

fn path_is_inside(root: &Path, path: &Path) -> bool {
    let Ok(root) = std::path::absolute(root) else {
        return false;
    };
    let Ok(path) = std::path::absolute(path) else {
        return false;
    };
    path.starts_with(root)
}

/// One JSON object matching the task or release schema. Extra properties are
/// rejected. Each body field is an array of strings. The body text is what
/// `review_body` checks again.
pub(crate) fn parse_review_verdict(text: &str, release: bool) -> Result<(String, String), String> {
    parse_review_verdict_inner(text, release)
}

pub(crate) fn parse_codex_verdict(text: &str, release: bool) -> Result<(String, String), String> {
    parse_review_verdict_inner(text, release)
}

fn parse_review_verdict_inner(text: &str, release: bool) -> Result<(String, String), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("codex output is empty".to_string());
    }
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| "codex output is not one JSON object".to_string())?;
    let Some(object) = value.as_object() else {
        return Err("codex output is not one JSON object".to_string());
    };
    if object.len() != 2 || !object.contains_key("verdict") || !object.contains_key("body") {
        return Err("codex output does not match the schema".to_string());
    }
    let verdict = object
        .get("verdict")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "codex output has an invalid verdict".to_string())?;
    if verdict != "approved" && verdict != "changes-requested" {
        return Err("codex output has an invalid verdict".to_string());
    }
    let body = object
        .get("body")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "codex output is missing body".to_string())?;
    let keys = if release {
        RELEASE_BODY_KEYS
    } else {
        TASK_BODY_KEYS
    };
    for key in keys {
        if !body.contains_key(*key) {
            return Err(format!("codex output is missing {key}"));
        }
    }
    if body.len() != keys.len() {
        return Err("codex output does not match the schema".to_string());
    }
    for key in keys {
        let value = body.get(*key).expect("key");
        let items = value
            .as_array()
            .ok_or_else(|| format!("codex output {key} must be an array of strings"))?;
        if items.iter().any(|item| !item.is_string()) {
            return Err(format!("codex output {key} must be an array of strings"));
        }
    }
    let body = serde_json::to_string(object.get("body").expect("body"))
        .map_err(|_| "codex output body is not JSON".to_string())?;
    Ok((verdict.to_string(), body))
}

/// Create the invocation directory and schema.
///
/// The directory is mode `0700` and is created with `mkdir`, which fails if
/// the path already exists, including as a symlink. `share_user` is set only
/// for the isolated sandbox. When it names another account, that account alone
/// is granted access. A missing account or a failed grant does not widen the
/// directory.
fn prepare_codex_output(
    out: &Path,
    schema: &Path,
    schema_text: &str,
    share_user: Option<&str>,
) -> Result<(), String> {
    let Some(parent) = out.parent() else {
        return Err("codex output has no directory".to_string());
    };
    if parent == std::env::temp_dir() {
        return Err("codex output directory is the temp root".to_string());
    }
    match std::fs::symlink_metadata(parent) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err("codex output directory is a symlink".to_string());
        }
        Ok(_) => return Err("codex output directory already exists".to_string()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.to_string()),
    }
    if std::fs::symlink_metadata(out).is_ok() || std::fs::symlink_metadata(schema).is_ok() {
        return Err("codex output path already exists".to_string());
    }
    create_private_dir(parent)?;
    write_private_schema(schema, schema_text)?;
    if let Some(user) = share_user.map(str::trim).filter(|user| !user.is_empty())
        && let Err(err) = grant_isolated_user(parent, schema, user)
    {
        let _ = std::fs::remove_file(schema);
        let _ = std::fs::remove_dir(parent);
        return Err(err);
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|err| err.to_string())?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|err| err.to_string())?;
        let meta = std::fs::symlink_metadata(path).map_err(|err| err.to_string())?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err("codex output directory is not a real directory".to_string());
        }
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("codex output directory is not private".to_string());
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(path).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn write_private_schema(schema: &Path, schema_text: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(o_nofollow())
            .open(schema)
            .map_err(|err| err.to_string())?;
        file.write_all(schema_text.as_bytes())
            .map_err(|err| err.to_string())?;
        std::fs::set_permissions(schema, std::fs::Permissions::from_mode(0o600))
            .map_err(|err| err.to_string())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(schema, schema_text).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn read_codex_output(out: &Path, share_user: Option<&str>) -> Result<String, String> {
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let Some(parent) = out.parent() else {
            return Err("codex output has no directory".to_string());
        };
        let parent_meta = std::fs::symlink_metadata(parent).map_err(|err| err.to_string())?;
        if parent_meta.file_type().is_symlink() || !parent_meta.is_dir() {
            return Err("codex output directory is not a real directory".to_string());
        }
        if parent_meta.permissions().mode() & 0o007 != 0 {
            return Err("codex output directory is world-accessible".to_string());
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(o_nofollow())
            .open(out)
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    "codex output is missing".to_string()
                } else {
                    "codex output is not a regular file".to_string()
                }
            })?;
        let meta = file.metadata().map_err(|err| err.to_string())?;
        if !meta.is_file() {
            return Err("codex output is not a regular file".to_string());
        }
        if !output_uid_allowed(meta.uid(), parent_meta.uid(), share_user) {
            return Err("codex output is not owned by the review invocation".to_string());
        }
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|err| err.to_string())?;
        if text.trim().is_empty() {
            return Err("codex output is empty".to_string());
        }
        Ok(text)
    }
    #[cfg(not(unix))]
    {
        let _ = share_user;
        let text = std::fs::read_to_string(out).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                "codex output is missing".to_string()
            } else {
                err.to_string()
            }
        })?;
        if text.trim().is_empty() {
            return Err("codex output is empty".to_string());
        }
        Ok(text)
    }
}

#[cfg(unix)]
fn o_nofollow() -> i32 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        0x20000
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        0x100
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )))]
    {
        0
    }
}

#[cfg(unix)]
fn output_uid_allowed(file_uid: u32, dir_uid: u32, share_user: Option<&str>) -> bool {
    if file_uid == dir_uid {
        return true;
    }
    let Some(user) = share_user.map(str::trim).filter(|user| !user.is_empty()) else {
        return false;
    };
    account_uid(user).ok() == Some(file_uid)
}

fn grant_isolated_user(dir: &Path, schema: &Path, user: &str) -> Result<(), String> {
    if !username_ok(user) {
        return Err("codex user is invalid".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir_uid = std::fs::symlink_metadata(dir)
            .map_err(|err| err.to_string())?
            .uid();
        let user_uid = account_uid(user)?;
        if user_uid == dir_uid {
            return Ok(());
        }
        run_setfacl(&["-m", &format!("u:{user}:rwx"), &dir.display().to_string()])?;
        run_setfacl(&["-m", &format!("u:{user}:r"), &schema.display().to_string()])?;
        let mode = std::fs::symlink_metadata(dir)
            .map_err(|err| err.to_string())?
            .permissions()
            .mode();
        if mode & 0o007 != 0 {
            return Err("codex output directory is world-accessible".to_string());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, schema);
        Err(format!(
            "cannot share the codex output directory with {user}"
        ))
    }
}

fn username_ok(user: &str) -> bool {
    let mut chars = user.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && user.len() <= 32
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

#[cfg(unix)]
fn account_uid(user: &str) -> Result<u32, String> {
    if !username_ok(user) {
        return Err("codex user is invalid".to_string());
    }
    let output = Command::new("getent")
        .args(["passwd", user])
        .output()
        .map_err(|err| format!("getent: {err}"))?;
    if !output.status.success() {
        return Err(format!("codex user {user} was not found"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let uid = text
        .split(':')
        .nth(2)
        .ok_or_else(|| format!("codex user {user} was not found"))?;
    uid.parse::<u32>()
        .map_err(|_| format!("codex user {user} was not found"))
}

#[cfg(unix)]
fn run_setfacl(args: &[&str]) -> Result<(), String> {
    let output = Command::new("setfacl")
        .args(args)
        .output()
        .map_err(|err| format!("setfacl: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    if detail.is_empty() {
        Err("could not share the codex output directory".to_string())
    } else {
        Err(format!(
            "could not share the codex output directory: {detail}"
        ))
    }
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
    schema_text: &str,
    model: &str,
) -> AgentResult {
    let Some(program) = crate::exe::Program::resolve("codex") else {
        return AgentResult::Unavailable {
            cli: "codex".to_string(),
        };
    };
    let root = crate::gitinfo::toplevel(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let schema = out.with_file_name("codex-schema.json");
    let share_user = if sandbox == CodexSandbox::Isolated {
        std::env::var("RELAYBOOK_CODEX_USER")
            .ok()
            .filter(|user| !user.trim().is_empty())
    } else {
        None
    };
    if let Err(err) = prepare_codex_output(out, &schema, schema_text, share_user.as_deref()) {
        return AgentResult::Failed { output: err };
    }
    let args = match codex_args(&root, &schema, out, prompt, sandbox, model) {
        Ok(args) => args,
        Err(err) => return AgentResult::Failed { output: err },
    };
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
    let status = match wait_codex(&mut child) {
        Ok(status) => status,
        Err(result) => {
            let _ = stdin_thread.join();
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return result;
        }
    };
    let _ = stdin_thread.join();
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    if let Some(line) = stdout.lines().find(|line| !line.trim().is_empty()) {
        live.observe(&crate::stream::Piece::Text(line.to_string()));
    }
    if !status.success() {
        return classify_codex_failure(status.code(), &stdout, &stderr, out);
    }
    match read_codex_output(out, share_user.as_deref()) {
        Ok(text) => AgentResult::Finished {
            session_id: None,
            est_cost_usd: 0.0,
            output: text,
        },
        Err(err) => AgentResult::Failed {
            output: format!("{err}\n{stderr}"),
        },
    }
}

/// `child.wait()`, unless `RELAYBOOK_CODEX_TIMEOUT_SECS` is set. That variable
/// is a test seam. Production waits until Codex exits.
fn wait_codex(child: &mut std::process::Child) -> Result<std::process::ExitStatus, AgentResult> {
    let Some(limit) = std::env::var("RELAYBOOK_CODEX_TIMEOUT_SECS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
    else {
        return child.wait().map_err(|err| AgentResult::Failed {
            output: err.to_string(),
        });
    };
    let deadline = Instant::now() + Duration::from_secs(limit);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AgentResult::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => {
                return Err(AgentResult::Failed {
                    output: err.to_string(),
                });
            }
        }
    }
}

fn classify_codex_failure(
    code: Option<i32>,
    stdout: &str,
    stderr: &str,
    out: &Path,
) -> AgentResult {
    let file = match std::fs::read_to_string(out) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => "unreadable".to_string(),
    };
    let version = codex_version();
    match classify_codex_exit(code, stderr, stdout, &file, &version) {
        CodexExit::Limited { signature } => AgentResult::UsageLimit {
            cli: "codex".to_string(),
            reset_at: None,
            output: format!("{signature}\n{version}\n{stderr}"),
        },
        CodexExit::Ok | CodexExit::Ordinary => AgentResult::Failed {
            output: format!("codex exited {}\n{stdout}\n{stderr}", code.unwrap_or(-1)),
        },
    }
}

pub(crate) fn codex_limit_detail(result: &AgentResult) -> Option<CodexLimit> {
    let AgentResult::UsageLimit { cli, output, .. } = result else {
        return None;
    };
    if cli != "codex" {
        return None;
    }
    let mut lines = output.splitn(3, '\n');
    let signature = lines.next()?.to_string();
    let version = lines.next()?.to_string();
    let stderr = lines.next().unwrap_or("").to_string();
    if !CODEX_LIMIT_SIGNATURES
        .iter()
        .any(|(id, _)| *id == signature)
    {
        return None;
    }
    Some(CodexLimit {
        signature,
        version,
        stderr,
    })
}

/// `codex --version`, trimmed. Empty when the binary cannot be run.
/// The classifier uses this string and does not look at `-m`.
pub(crate) fn codex_version() -> String {
    let Some(program) = crate::exe::Program::resolve("codex") else {
        return String::new();
    };
    let Ok(output) = program.command().arg("--version").output() else {
        return String::new();
    };
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A usage limit only when every signal is present. Stdout is ignored.
/// The `-o` text is `out`. A phrase in stdout or in `out` is not a limit.
pub(crate) fn classify_codex_exit(
    code: Option<i32>,
    stderr: &str,
    stdout: &str,
    out: &str,
    version: &str,
) -> CodexExit {
    let _ = stdout;
    if code == Some(0) && !out.trim().is_empty() {
        return CodexExit::Ok;
    }
    let version_ok = CODEX_VERIFIED_VERSIONS
        .iter()
        .any(|item| version.trim() == *item);
    let signature = CODEX_LIMIT_SIGNATURES
        .iter()
        .find(|(_, phrase)| stderr.contains(phrase));
    let ordinary = CODEX_ORDINARY_SIGNATURES
        .iter()
        .any(|phrase| stderr.contains(phrase));
    if code == Some(1)
        && out.trim().is_empty()
        && version_ok
        && !ordinary
        && let Some((id, _)) = signature
    {
        return CodexExit::Limited {
            signature: (*id).to_string(),
        };
    }
    CodexExit::Ordinary
}

/// E2 prints only a zone-less local clock. That text is not a timestamp.
/// Returns `None` for every captured line. Callers use the configured backoff.
pub(crate) fn parse_codex_retry_after(stderr: &str) -> Option<String> {
    let _ = stderr;
    None
}

/// Tiny Codex exec used to see whether the limit window has ended.
/// Empty stdin. The prompt starts with `Probe.` so a test double can tell it
/// from a review. The sandbox is the configured one, and `-m` is `model`.
pub(crate) fn call_codex_probe(
    cwd: &Path,
    out: &Path,
    run_id: i64,
    live: &mut Session,
    sandbox: CodexSandbox,
    model: &str,
) -> AgentResult {
    let result = call_codex(
        cwd,
        "Probe. Return {\"ok\":true}.",
        "",
        out,
        run_id,
        live,
        sandbox,
        PROBE_CODEX_SCHEMA,
        model,
    );
    match result {
        AgentResult::Finished { output, .. } => match parse_probe_output(&output) {
            Ok(()) => AgentResult::Finished {
                session_id: None,
                est_cost_usd: 0.0,
                output,
            },
            Err(err) => AgentResult::Failed { output: err },
        },
        other => other,
    }
}

fn parse_probe_output(text: &str) -> Result<(), String> {
    let text = text.trim();
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| "probe output is not one JSON object".to_string())?;
    let Some(object) = value.as_object() else {
        return Err("probe output is not one JSON object".to_string());
    };
    if object.len() != 1 || object.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err("probe output is not {\"ok\":true}".to_string());
    }
    Ok(())
}

/// Sonnet review argv. Flags were checked against `claude --help` (2.1.325):
/// `-p`, `--model`, `--permission-mode dontAsk`, `--tools`, `--allowedTools`,
/// `--disallowedTools`, `--no-session-persistence`, `--output-format json`,
/// `--strict-mcp-config`, and `--restricted`.
pub(crate) fn sonnet_review_args(prompt: &str) -> Vec<String> {
    vec![
        "-p".to_string(),
        prompt.to_string(),
        "--model".to_string(),
        relaybook_core::SONNET_REVIEW_MODEL.to_string(),
        "--permission-mode".to_string(),
        "dontAsk".to_string(),
        "--tools".to_string(),
        SONNET_REVIEW_TOOLS.to_string(),
        "--allowedTools".to_string(),
        SONNET_REVIEW_TOOLS.to_string(),
        "--disallowedTools".to_string(),
        SONNET_DISALLOWED_TOOLS.to_string(),
        "--no-session-persistence".to_string(),
        "--output-format".to_string(),
        "json".to_string(),
        "--strict-mcp-config".to_string(),
        "--restricted".to_string(),
    ]
}

/// Refuse before spawn when the argv could edit, run a shell, or use the web.
/// `--strict-mcp-config` is not `--mcp-config`.
pub(crate) fn forbidden_claude_review_arg(args: &[String]) -> Option<String> {
    let mut saw_dont_ask = false;
    let mut saw_tools = false;
    let mut saw_allowed = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg.starts_with("--dangerously")
            || arg == "--add-dir"
            || arg == "--resume"
            || arg == "--continue"
            || arg == "-c"
            || arg == "--fallback-model"
            || arg == "--mcp-config"
        {
            return Some(arg.to_string());
        }
        if arg == "--permission-mode" {
            let mode = args.get(index + 1).map(String::as_str);
            if mode != Some("dontAsk") {
                return Some("--permission-mode".to_string());
            }
            saw_dont_ask = true;
            index += 2;
            continue;
        }
        if arg == "--tools" || arg == "--allowedTools" || arg == "--allowed-tools" {
            let listed = args.get(index + 1).map(String::as_str).unwrap_or("");
            if !review_tools_only(listed) {
                return Some(arg.to_string());
            }
            if arg == "--tools" {
                saw_tools = true;
            } else {
                saw_allowed = true;
            }
            index += 2;
            continue;
        }
        index += 1;
    }
    if !saw_dont_ask {
        return Some("--permission-mode dontAsk".to_string());
    }
    if !saw_tools {
        return Some("--tools".to_string());
    }
    if !saw_allowed {
        return Some("--allowedTools".to_string());
    }
    None
}

fn review_tools_only(listed: &str) -> bool {
    let mut any = false;
    for tool in listed.split(',') {
        let tool = tool.trim();
        if tool.is_empty() {
            continue;
        }
        any = true;
        if tool != "Read" && tool != "Grep" && tool != "Glob" {
            return false;
        }
    }
    any
}

/// Read-only Claude review. A forbidden argv is refused before the process
/// is resolved. An error, a limit, or a model other than the pin is `Failed`.
pub(crate) fn call_claude_reviewer(
    cwd: &Path,
    prompt: &str,
    packet: &str,
    run_id: i64,
    live: &mut Session,
) -> AgentResult {
    let args = sonnet_review_args(prompt);
    if let Some(flag) = forbidden_claude_review_arg(&args) {
        return AgentResult::Failed {
            output: format!("refusing claude flag {flag}"),
        };
    }
    let Some(program) = crate::exe::Program::resolve("claude") else {
        return AgentResult::Unavailable {
            cli: "claude".to_string(),
        };
    };
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
                cli: "claude".to_string(),
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
                "claude exited {}\n{stdout}\n{stderr}",
                status.code().unwrap_or(-1)
            ),
        };
    }
    match parse_claude_review_envelope(&stdout) {
        Ok((result, model)) => {
            if let Some(model) = model.as_deref()
                && model != relaybook_core::SONNET_REVIEW_MODEL
            {
                return AgentResult::Failed {
                    output: format!("claude model {model} is not the pinned reviewer"),
                };
            }
            AgentResult::Finished {
                session_id: None,
                est_cost_usd: 0.0,
                output: result,
            }
        }
        Err(err) => AgentResult::Failed {
            output: format!("{err}\n{stderr}"),
        },
    }
}

fn parse_claude_review_envelope(stdout: &str) -> Result<(String, Option<String>), String> {
    let text = stdout.trim();
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| "claude output is not one JSON object".to_string())?;
    let Some(object) = value.as_object() else {
        return Err("claude output is not one JSON object".to_string());
    };
    if object.get("is_error").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err("claude reported an error".to_string());
    }
    let result = object
        .get("result")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "claude output has no result".to_string())?;
    let model = object
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Ok((result.to_string(), model))
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
        AccountKind, Accounts, AgentResult, CodexExit, CodexSandbox, account_line, classify,
        classify_codex_exit, claude_account, codex_args, codex_login_status_args,
        codex_output_file, forbidden_claude_review_arg, forbidden_codex_arg,
        parse_codex_retry_after, parse_codex_verdict, path_is_inside, prepare_codex_output,
        read_codex_output, review_input_too_large, sonnet_review_args,
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
            "gpt-6.1-sol",
        )
        .expect("model");
        let joined = args.join(" ");
        assert!(joined.starts_with("exec -s read-only --ephemeral --color never -C /repo "));
        assert!(joined.contains("-m gpt-6.1-sol"));
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
            "gpt-6.1-sol",
        )
        .expect("model");
        assert!(!args.iter().any(|arg| arg == "-s"));
        assert!(!args.iter().any(|arg| arg == "read-only"));
        assert!(args.contains(&"-m".to_string()));
        assert!(args.contains(&"gpt-6.1-sol".to_string()));
        assert!(args.contains(&"--ephemeral".to_string()));
        assert!(args.contains(&"--output-schema".to_string()));
        assert!(forbidden_codex_arg(&args).is_none());
    }

    fn assert_strict_schema(name: &str, text: &str) {
        let schema: serde_json::Value = serde_json::from_str(text).expect(name);
        fn walk(path: &str, node: &serde_json::Value) {
            let Some(object) = node.as_object() else {
                panic!("{path} is not a schema object");
            };
            if let Some(items) = object.get("items") {
                walk(&format!("{path}.items"), items);
            }
            let is_object = object.get("type").and_then(serde_json::Value::as_str)
                == Some("object")
                || object.contains_key("properties");
            if !is_object {
                assert!(
                    object.contains_key("type") || object.contains_key("enum"),
                    "{path} has no type or enum"
                );
                return;
            }
            assert_eq!(
                object
                    .get("additionalProperties")
                    .and_then(serde_json::Value::as_bool),
                Some(false),
                "{path} additionalProperties"
            );
            let properties = object
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .unwrap_or_else(|| panic!("{path} has no properties"));
            let required = object
                .get("required")
                .and_then(serde_json::Value::as_array)
                .unwrap_or_else(|| panic!("{path} has no required"));
            let mut required_keys = std::collections::BTreeSet::new();
            for item in required {
                let key = item
                    .as_str()
                    .unwrap_or_else(|| panic!("{path} required entry is not a string"));
                assert!(required_keys.insert(key), "{path} repeats {key}");
            }
            let property_keys: std::collections::BTreeSet<_> =
                properties.keys().map(String::as_str).collect();
            assert_eq!(required_keys, property_keys, "{path} required");
            for (key, child) in properties {
                let typed = child.get("type").is_some() || child.get("enum").is_some();
                assert!(typed, "{path}.{key} has no type or enum");
                walk(&format!("{path}.{key}"), child);
            }
        }
        walk(name, &schema);
    }

    #[test]
    fn codex_output_schemas_are_strict() {
        assert_strict_schema("TASK_CODEX_SCHEMA", super::TASK_CODEX_SCHEMA);
        assert_strict_schema("RELEASE_CODEX_SCHEMA", super::RELEASE_CODEX_SCHEMA);
        assert_strict_schema("PROBE_CODEX_SCHEMA", super::PROBE_CODEX_SCHEMA);
    }

    #[test]
    fn codex_verdict_must_be_one_json_object() {
        let (verdict, body) = parse_codex_verdict(
            "{\"verdict\":\"approved\",\"body\":{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[]}}\n",
            false,
        )
        .expect("object");
        assert_eq!(verdict, "approved");
        assert!(body.contains("findings"));
        assert!(parse_codex_verdict("", false).is_err());
        assert!(parse_codex_verdict("   ", false).is_err());
        assert!(parse_codex_verdict("not json", false).is_err());
        assert!(parse_codex_verdict("[]", false).is_err());
        assert!(
            parse_codex_verdict(
                "{\"verdict\":\"approved\"}{\"verdict\":\"approved\"}",
                false
            )
            .is_err()
        );
        assert!(parse_codex_verdict("{\"verdict\":\"approved\",\"body\":[]}", false).is_err());
        assert!(parse_codex_verdict("{\"verdict\":\"yes\",\"body\":{}}", false).is_err());
        assert!(
            parse_codex_verdict(
                "{\"verdict\":\"approved\",\"body\":{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[]},\"injected\":true}",
                false
            )
            .is_err()
        );
        assert!(
            parse_codex_verdict(
                "{\"verdict\":\"approved\",\"body\":{\"findings\":[],\"security\":[]}}",
                false
            )
            .is_err()
        );
        assert!(
            parse_codex_verdict(
                "{\"verdict\":\"approved\",\"body\":{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[]}}",
                true
            )
            .is_err()
        );
        assert!(
            parse_codex_verdict(
                "{\"verdict\":\"approved\",\"body\":{\"findings\":\"none\",\"security\":[],\"tests_checked\":[],\"not_checked\":[]}}",
                false
            )
            .is_err()
        );
        assert!(
            parse_codex_verdict(
                "{\"verdict\":\"approved\",\"body\":{\"findings\":[1],\"security\":[],\"tests_checked\":[],\"not_checked\":[]}}",
                false
            )
            .is_err()
        );
        assert!(
            parse_codex_verdict(
                "{\"verdict\":\"approved\",\"body\":{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[],\"token\":\"kestrel\"}}",
                false
            )
            .is_err()
        );
        let (verdict, body) = parse_codex_verdict(
            "{\"verdict\":\"changes-requested\",\"body\":{\"findings\":[\"one\"],\"security\":[],\"tests_checked\":[\"cargo test\"],\"not_checked\":[],\"prd_covered\":[],\"gaps\":[\"gap\"]}}",
            true,
        )
        .expect("release");
        assert_eq!(verdict, "changes-requested");
        assert!(body.contains("prd_covered"));
        assert!(!body.contains("true"));
    }

    #[test]
    fn codex_model_cannot_inject_argv() {
        let bad = [
            "-m",
            "--dangerously-bypass-approvals-and-sandbox",
            "gpt 6",
            "model/name",
            "",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ];
        for model in bad {
            let err = codex_args(
                Path::new("/repo"),
                Path::new("/tmp/codex-schema.json"),
                Path::new("/tmp/codex-last.md"),
                "Review task work. Use only the packet on stdin.",
                CodexSandbox::ReadOnly,
                model,
            )
            .expect_err(model);
            assert!(
                err.contains("invalid codex_model")
                    || err.contains("codex_model is required")
                    || err.contains("at most 64"),
                "{model}: {err}"
            );
        }
        assert_eq!(codex_login_status_args(), ["login", "status"]);
        assert!(!codex_login_status_args().contains(&"-m"));
    }

    #[test]
    fn codex_output_file_stays_outside_the_repository() {
        let root = std::env::temp_dir().join("relaybook-repo-not-temp");
        let out = codex_output_file(&root, 4, 9, 2);
        assert!(out.ends_with("codex-last.md"));
        assert!(!path_is_inside(&root, &out), "{out:?} is inside {root:?}");
    }

    #[cfg(unix)]
    #[test]
    fn codex_output_directory_is_private_and_rejects_substitution() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir().expect("temp");
        for sandbox in [CodexSandbox::ReadOnly, CodexSandbox::Isolated] {
            let out = root
                .path()
                .join(format!("{sandbox:?}"))
                .join("codex-last.md");
            let schema = out.with_file_name("codex-schema.json");
            prepare_codex_output(&out, &schema, "{\"type\":\"object\"}\n", None).expect("prepare");
            let dir = out.parent().expect("dir");
            let mode = std::fs::symlink_metadata(dir)
                .expect("dir meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "{sandbox:?} mode {mode:o}");
            let schema_mode = std::fs::symlink_metadata(&schema)
                .expect("schema meta")
                .permissions()
                .mode();
            assert_eq!(
                schema_mode & 0o777,
                0o600,
                "{sandbox:?} schema {schema_mode:o}"
            );
        }

        let out = root.path().join("invocation").join("codex-last.md");
        let schema = out.with_file_name("codex-schema.json");
        prepare_codex_output(&out, &schema, "{}\n", None).expect("prepare");
        let dir = out.parent().expect("dir");
        let decoy = dir.join("decoy.json");
        let token = "kestrel-symlink";
        std::fs::write(
            &decoy,
            format!(
                "{{\"verdict\":\"approved\",\"body\":{{\"findings\":[],\"security\":[],\"tests_checked\":[],\"not_checked\":[],\"token\":\"{token}\"}}}}\n"
            ),
        )
        .expect("decoy");
        symlink(&decoy, &out).expect("symlink");
        let err = read_codex_output(&out, None).expect_err("symlink verdict");
        assert!(!err.contains(token), "{err}");
        assert!(
            err.contains("not a regular file") || err.contains("missing"),
            "{err}"
        );

        let swapped_out = root.path().join("swapped").join("codex-last.md");
        let swapped_schema = swapped_out.with_file_name("codex-schema.json");
        prepare_codex_output(&swapped_out, &swapped_schema, "{}\n", None).expect("prepare");
        let mut permissions = std::fs::symlink_metadata(swapped_out.parent().expect("parent"))
            .expect("meta")
            .permissions();
        permissions.set_mode(0o777);
        std::fs::set_permissions(swapped_out.parent().expect("parent"), permissions)
            .expect("chmod");
        std::fs::write(&swapped_out, "{\"verdict\":\"approved\"}\n").expect("file");
        let err = read_codex_output(&swapped_out, None).expect_err("world writable");
        assert!(err.contains("world-accessible"), "{err}");
        assert!(!err.contains("approved"), "{err}");

        let target = root.path().join("target");
        std::fs::create_dir(&target).expect("target");
        let link = root.path().join("link");
        symlink(&target, &link).expect("dir symlink");
        let linked_out = link.join("codex-last.md");
        let linked_schema = link.join("codex-schema.json");
        let err = prepare_codex_output(&linked_out, &linked_schema, "{\"secret\":true}\n", None)
            .expect_err("followed symlink");
        assert!(
            err.contains("symlink") || err.contains("already exists"),
            "{err}"
        );
        assert!(
            std::fs::read_dir(&target)
                .expect("target dir")
                .next()
                .is_none(),
            "schema was written through the directory symlink"
        );

        let missing = prepare_codex_output(
            &root.path().join("missing-user").join("codex-last.md"),
            &root.path().join("missing-user").join("codex-schema.json"),
            "{}\n",
            Some("not-a-real-user"),
        );
        let err = missing.expect_err("unknown user");
        assert!(
            err.contains("not found") || err.contains("invalid") || err.contains("getent"),
            "{err}"
        );
        let created = root.path().join("missing-user");
        if created.exists() {
            let mode = std::fs::symlink_metadata(&created)
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o007,
                0,
                "unknown user left a world-accessible directory {mode:o}"
            );
        }
    }

    #[test]
    fn review_packet_limit_fails_closed_before_the_cli_cap() {
        let prompt = "Review task work.";
        assert!(!review_input_too_large(prompt, &"a".repeat(64 * 1024)));
        assert!(review_input_too_large(prompt, &"a".repeat(64 * 1024 + 1)));
        let packet = "a".repeat(1_048_576);
        assert!(review_input_too_large(prompt, &packet));
    }

    const USAGE: &str = "ERROR: You\u{2019}ve hit your usage limit. Visit https://chatgpt.com/settings/usage to purchase more credits or try again at 6:39 AM.";
    const QUOTA: &str = "ERROR: Quota exceeded. Check your plan and billing details.";
    const VERSION: &str = "codex-cli 0.169.0";

    #[test]
    fn codex_limit_needs_every_signal() {
        assert!(matches!(
            classify_codex_exit(Some(1), USAGE, "", "", VERSION),
            CodexExit::Limited { signature } if signature == "usage_limit"
        ));
        assert!(matches!(
            classify_codex_exit(Some(1), QUOTA, "", "", VERSION),
            CodexExit::Limited { signature } if signature == "quota_exceeded"
        ));
        assert!(matches!(
            classify_codex_exit(Some(2), USAGE, "", "", VERSION),
            CodexExit::Ordinary
        ));
        assert!(matches!(
            classify_codex_exit(Some(1), USAGE, "", "{\"ok\":true}", VERSION),
            CodexExit::Ordinary
        ));
        assert!(matches!(
            classify_codex_exit(Some(1), "ERROR: something else", "", "", VERSION),
            CodexExit::Ordinary
        ));
        let with_auth = format!("{USAGE}\nERROR: unexpected status 401 Unauthorized");
        assert!(matches!(
            classify_codex_exit(Some(1), &with_auth, "", "", VERSION),
            CodexExit::Ordinary
        ));
        assert!(matches!(
            classify_codex_exit(Some(1), USAGE, "", "", "codex-cli 0.1.0"),
            CodexExit::Ordinary
        ));
        assert!(matches!(
            classify_codex_exit(Some(1), "", USAGE, "", VERSION),
            CodexExit::Ordinary
        ));
        assert!(matches!(
            classify_codex_exit(Some(1), "", "", USAGE, VERSION),
            CodexExit::Ordinary
        ));
        assert!(matches!(
            classify_codex_exit(
                Some(1),
                "ERROR: exceeded retry limit, last status: 429 Too Many Requests",
                "",
                "",
                VERSION
            ),
            CodexExit::Ordinary
        ));
        assert_eq!(parse_codex_retry_after(USAGE), None);
        assert_eq!(parse_codex_retry_after(QUOTA), None);
        assert_eq!(
            parse_codex_retry_after(
                "ERROR: You\u{2019}ve hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/settings/usage to purchase more credits or try again later."
            ),
            None
        );
    }

    #[test]
    fn sonnet_argv_never_grants_edit() {
        let args = sonnet_review_args("Review the packet on stdin.");
        assert!(forbidden_claude_review_arg(&args).is_none());
        assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(args.iter().any(|arg| arg == "--restricted"));
        assert!(!args.iter().any(|arg| arg == "--mcp-config"));
        for forbidden in [
            "--dangerously-skip-permissions",
            "--add-dir",
            "--resume",
            "--continue",
            "--fallback-model",
            "--mcp-config",
        ] {
            let mut bad = args.clone();
            bad.push(forbidden.to_string());
            assert!(forbidden_claude_review_arg(&bad).is_some(), "{forbidden}");
        }
        for tools in ["--tools", "--allowedTools"] {
            let mut bad = args.clone();
            let index = bad.iter().position(|arg| arg == tools).expect(tools);
            bad[index + 1] = "Read,Bash".to_string();
            assert!(forbidden_claude_review_arg(&bad).is_some(), "{tools}");
        }
        let mut mode = args.clone();
        let index = mode
            .iter()
            .position(|arg| arg == "--permission-mode")
            .expect("mode");
        mode[index + 1] = "acceptEdits".to_string();
        assert!(forbidden_claude_review_arg(&mode).is_some());
        let mut missing = args.clone();
        let index = missing
            .iter()
            .position(|arg| arg == "--permission-mode")
            .expect("mode");
        missing.remove(index);
        missing.remove(index);
        assert!(forbidden_claude_review_arg(&missing).is_some());
    }
}
