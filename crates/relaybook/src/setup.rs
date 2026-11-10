use std::fmt::Write as _;
use std::path::Path;

use relaybook_core::{Error as CoreError, Ledger};
use serde::Serialize;

use crate::graphify::{self, INSTALL_HINT};
use crate::hooks;
use crate::install;
use crate::paths;
use crate::workflow;

pub(crate) enum Error {
    Core(CoreError),
    Io(std::io::Error),
}

impl From<CoreError> for Error {
    fn from(err: CoreError) -> Self {
        Self::Core(err)
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

pub(crate) struct Options {
    pub no_graphify: bool,
    pub no_hooks: bool,
    pub no_claude_hook: bool,
    pub no_commands: bool,
    pub ci: bool,
}

#[derive(Serialize)]
pub(crate) struct Step {
    pub name: String,
    pub status: String,
    pub detail: String,
}

pub(crate) struct Report {
    pub steps: Vec<Step>,
    pub error: Option<String>,
}

pub(crate) fn run(cwd: &Path, options: Options) -> Result<Report, Error> {
    let root = paths::init_root(cwd);
    let mut steps = Vec::new();
    record_ledger(&root, &mut steps)?;
    record_docs(&root, &mut steps)?;
    record_commands(&root, &options, &mut steps);
    record_prompt_hook(&root, &options, &mut steps);
    record_graphify(&root, &options, &mut steps);
    record_graphify_guards(&root, &mut steps);
    record_ignore(&root, &options, &mut steps)?;
    record_hooks(&root, &options, &mut steps);
    record_rtk(&mut steps);
    if options.ci {
        record_ci(&root, &mut steps)?;
    }
    let error = failure_message(&steps);
    Ok(Report { steps, error })
}

pub(crate) fn render(report: &Report) -> String {
    let mut out = String::new();
    for step in &report.steps {
        let _ = writeln!(out, "{}: {} ({})", step.name, step.status, step.detail);
    }
    out
}

fn record_ledger(root: &Path, steps: &mut Vec<Step>) -> Result<(), Error> {
    let path = root.join(".relaybook").join("ledger.db");
    let existed = path.exists();
    Ledger::initialize(&path)?;
    if existed {
        push(steps, "init", "skipped", "ledger already exists");
    } else {
        push(steps, "init", "done", "created .relaybook/ledger.db");
    }
    Ok(())
}

fn record_docs(root: &Path, steps: &mut Vec<Step>) -> Result<(), Error> {
    let outcome = install::apply(root, true, true)?;
    let claude = outcome
        .claude
        .map(|state| state.as_str())
        .unwrap_or("skipped");
    let cursor = outcome
        .cursor
        .map(|state| state.as_str())
        .unwrap_or("skipped");
    let status = if claude == "updated" || cursor == "updated" {
        "done"
    } else {
        "skipped"
    };
    push(
        steps,
        "install",
        status,
        format!("{claude} CLAUDE.md, {cursor} AGENTS.md"),
    );
    Ok(())
}

fn record_commands(root: &Path, options: &Options, steps: &mut Vec<Step>) {
    if options.no_commands {
        push(steps, "claude command", "skipped", "--no-commands");
        return;
    }
    match crate::commands::install_claude(root) {
        Ok(crate::commands::Status::Updated) => {
            push(
                steps,
                "claude command",
                "done",
                "wrote .claude/commands/relaybook.md",
            );
        }
        Ok(crate::commands::Status::Unchanged) => {
            push(steps, "claude command", "skipped", "already installed");
        }
        Ok(crate::commands::Status::LeftAlone) => {
            push(
                steps,
                "claude command",
                "skipped",
                "file has no relaybook:managed marker",
            );
        }
        Err(err) => push(steps, "claude command", "failed", err.to_string()),
    }
}

fn record_prompt_hook(root: &Path, options: &Options, steps: &mut Vec<Step>) {
    if options.no_claude_hook {
        push(steps, "prompt hook", "skipped", "--no-claude-hook");
        return;
    }
    match crate::claude_hook::install(root) {
        Ok(_) if !command_on_path("relaybook") => {
            push(
                steps,
                "prompt hook",
                "done",
                "relaybook is not on PATH; the prompt hook stays silent until it is",
            );
        }
        Ok(crate::claude_hook::Status::Updated) => {
            push(steps, "prompt hook", "done", "added UserPromptSubmit");
        }
        Ok(crate::claude_hook::Status::Unchanged) => {
            push(
                steps,
                "prompt hook",
                "skipped",
                "UserPromptSubmit already installed",
            );
        }
        Err(err) => push(steps, "prompt hook", "failed", err.to_string()),
    }
}

fn record_graphify_guards(root: &Path, steps: &mut Vec<Step>) {
    match crate::claude_hook::guard_graphify(root) {
        Ok(true) => push(steps, "graphify guards", "done", "wrapped PreToolUse"),
        Ok(false) => push(
            steps,
            "graphify guards",
            "skipped",
            "PreToolUse already guarded",
        ),
        Err(err) => push(steps, "graphify guards", "failed", err.to_string()),
    }
}

fn record_ignore(root: &Path, options: &Options, steps: &mut Vec<Step>) -> Result<(), Error> {
    let path = root.join(".gitignore");
    let original = if path.is_file() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let mut text = original;
    let mut changed = insert_ignore(&mut text, ".claude/*.graphify-bak");
    if options.ci {
        changed |= delete_ignore(&mut text, "graphify-out/");
    } else {
        changed |= insert_ignore(&mut text, "graphify-out/");
    }
    if changed {
        std::fs::write(&path, text)?;
        let detail = if options.ci {
            "graphify-out stays tracked"
        } else {
            "ignored .claude/*.graphify-bak and graphify-out/"
        };
        push(steps, "gitignore", "done", detail);
    } else {
        let detail = if options.ci {
            "graphify-out stays tracked"
        } else {
            "already ignored"
        };
        push(steps, "gitignore", "skipped", detail);
    }
    Ok(())
}

fn insert_ignore(text: &mut String, line: &str) -> bool {
    if text.lines().any(|item| item.trim() == line) {
        return false;
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
    true
}

fn delete_ignore(text: &mut String, line: &str) -> bool {
    if !text.lines().any(|item| item.trim() == line) {
        return false;
    }
    let mut kept = String::new();
    for item in text.lines() {
        if item.trim() == line {
            continue;
        }
        kept.push_str(item);
        kept.push('\n');
    }
    *text = kept;
    true
}

fn record_graphify(root: &Path, options: &Options, steps: &mut Vec<Step>) {
    const COMMANDS: &[(&str, &[&str])] = &[
        ("graphify claude install", &["claude", "install"]),
        ("graphify cursor install", &["cursor", "install"]),
        ("graphify hook install", &["hook", "install"]),
        ("graphify update", &["update", "."]),
    ];
    for (name, args) in COMMANDS {
        if options.no_graphify {
            push(steps, name, "skipped", "--no-graphify");
            continue;
        }
        if options.no_hooks && *name == "graphify hook install" {
            push(steps, name, "skipped", "--no-hooks");
            continue;
        }
        if !graphify::installed() {
            push(steps, name, "skipped", INSTALL_HINT);
            continue;
        }
        let result = graphify::run(root, args);
        let status = if result.ok { "done" } else { "failed" };
        push(steps, name, status, result.detail);
    }
}

fn record_rtk(steps: &mut Vec<Step>) {
    if !command_on_path("rtk") {
        push(
            steps,
            "rtk",
            "skipped",
            "install with `install.sh --with-rtk`",
        );
        return;
    }
    let verified = std::process::Command::new("rtk")
        .arg("verify")
        .output()
        .ok()
        .is_some_and(|output| output.status.success());
    if verified {
        push(steps, "rtk", "done", "rtk verify");
    } else {
        push(
            steps,
            "rtk",
            "skipped",
            "rtk is installed; run `rtk init -g` to add its hook",
        );
    }
}

fn command_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(name);
        candidate.is_file()
    })
}

fn record_hooks(root: &Path, options: &Options, steps: &mut Vec<Step>) {
    if options.no_graphify {
        push(steps, "hooks", "skipped", "--no-graphify");
        return;
    }
    if options.no_hooks {
        push(steps, "hooks", "skipped", "--no-hooks");
        return;
    }
    if !graphify::installed() {
        push(steps, "hooks", "skipped", INSTALL_HINT);
        return;
    }
    match hooks::install(root) {
        Ok(hooks::Status::Done(detail)) => push(steps, "hooks", "done", detail),
        Ok(hooks::Status::Skipped(detail)) => push(steps, "hooks", "skipped", detail),
        Err(err) => push(steps, "hooks", "failed", err.to_string()),
    }
}

fn record_ci(root: &Path, steps: &mut Vec<Step>) -> Result<(), Error> {
    let workflow = workflow::install_workflow(root)?;
    push(steps, "ci workflow", workflow.status, workflow.detail);
    let attributes = workflow::install_attributes(root)?;
    push(steps, "gitattributes", attributes.status, attributes.detail);
    Ok(())
}

fn failure_message(steps: &[Step]) -> Option<String> {
    let failed: Vec<&str> = steps
        .iter()
        .filter(|step| step.status == "failed")
        .map(|step| step.name.as_str())
        .collect();
    if failed.is_empty() {
        None
    } else {
        Some(format!("failed: {}", failed.join(", ")))
    }
}

fn push(steps: &mut Vec<Step>, name: &str, status: &str, detail: impl Into<String>) {
    steps.push(Step {
        name: name.to_string(),
        status: status.to_string(),
        detail: detail.into(),
    });
}
