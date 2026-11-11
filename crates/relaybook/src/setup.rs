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
    let before = crate::manifest::capture(&root);
    let mut steps = Vec::new();
    record_ledger(&root, &mut steps)?;
    record_docs(&root, &mut steps)?;
    record_commands(&root, &options, &mut steps);
    record_skill(&root, &mut steps);
    record_prompt_hook(&root, &options, &mut steps);
    record_graphify(&root, &options, &mut steps);
    record_graphify_guards(&root, &mut steps);
    record_ignore(&root, &options, &mut steps)?;
    record_attribute_repair(&root, &mut steps)?;
    record_hooks(&root, &options, &mut steps);
    record_rtk(&mut steps);
    if options.ci {
        record_ci(&root, &mut steps)?;
    }
    crate::manifest::record(&root, &before)?;
    let error = failure_message(&steps);
    Ok(Report { steps, error })
}

pub(crate) fn render(report: &Report) -> String {
    let mut out = String::new();
    for step in &report.steps {
        let _ = writeln!(out, "{}: {} ({})", step.name, step.status, step.detail);
    }
    let _ = writeln!(out, "In Claude Code: Plan <feature> in the ledger");
    let _ = writeln!(out, "In Cursor: Do the next relaybook task");
    let _ = writeln!(out, "In Claude Code: Check that it has been done");
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
    let replacing = has_instruction_markers(&root.join("CLAUDE.md"))
        || has_instruction_markers(&root.join("AGENTS.md"));
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
    let detail = if replacing && status == "done" {
        "updated agent instructions".to_string()
    } else {
        format!("{claude} CLAUDE.md, {cursor} AGENTS.md")
    };
    push(steps, "install", status, detail);
    Ok(())
}

fn has_instruction_markers(path: &Path) -> bool {
    std::fs::read_to_string(path).ok().is_some_and(|text| {
        text.contains("<!-- relaybook:start -->") && text.contains("<!-- relaybook:end -->")
    })
}

fn record_commands(root: &Path, options: &Options, steps: &mut Vec<Step>) {
    if options.no_commands {
        push(steps, "claude command", "skipped", "--no-commands");
        push(steps, "cursor command", "skipped", "--no-commands");
        return;
    }
    record_one_command(
        steps,
        "claude command",
        ".claude/commands/relaybook.md",
        crate::commands::install_claude(root),
    );
    record_one_command(
        steps,
        "cursor command",
        ".cursor/commands/relaybook.md",
        crate::commands::install_cursor(root),
    );
}

fn record_one_command(
    steps: &mut Vec<Step>,
    name: &str,
    path: &str,
    result: std::io::Result<crate::commands::Status>,
) {
    match result {
        Ok(crate::commands::Status::Updated) => push(steps, name, "done", format!("wrote {path}")),
        Ok(crate::commands::Status::Unchanged) => {
            push(steps, name, "skipped", "already installed");
        }
        Ok(crate::commands::Status::LeftAlone) => {
            push(
                steps,
                name,
                "skipped",
                "file has no relaybook:managed marker",
            );
        }
        Err(err) => push(steps, name, "failed", err.to_string()),
    }
}

fn record_skill(root: &Path, steps: &mut Vec<Step>) {
    record_one_command(
        steps,
        "claude skill",
        ".claude/skills/relaybook/SKILL.md",
        crate::commands::install_skill(root),
    );
}

fn record_prompt_hook(root: &Path, options: &Options, steps: &mut Vec<Step>) {
    if options.no_claude_hook {
        push(steps, "prompt hook", "skipped", "--no-claude-hook");
        push(steps, "session hook", "skipped", "--no-claude-hook");
        return;
    }
    match crate::claude_hook::install(root) {
        Ok(installed) => {
            record_hook_status(
                steps,
                "prompt hook",
                installed.prompt,
                "added UserPromptSubmit",
                "UserPromptSubmit already installed",
                "relaybook is not on PATH; the prompt hook stays silent until it is",
            );
            record_hook_status(
                steps,
                "session hook",
                installed.session,
                "added SessionStart",
                "SessionStart already installed",
                "relaybook is not on PATH; the session hook stays silent until it is",
            );
        }
        Err(err) => {
            push(steps, "prompt hook", "failed", err.to_string());
            push(steps, "session hook", "failed", err.to_string());
        }
    }
}

fn record_hook_status(
    steps: &mut Vec<Step>,
    name: &str,
    status: crate::claude_hook::Status,
    added: &str,
    present: &str,
    silent: &str,
) {
    if !command_on_path("relaybook") {
        push(steps, name, "done", silent);
        return;
    }
    match status {
        crate::claude_hook::Status::Updated => push(steps, name, "done", added),
        crate::claude_hook::Status::Unchanged => push(steps, name, "skipped", present),
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
    let text = crate::managed::rewrite_ignore(&original, options.ci);
    if text != original {
        if text.is_empty() {
            if path.is_file() {
                std::fs::remove_file(&path)?;
            }
        } else {
            std::fs::write(&path, text)?;
        }
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

fn record_attribute_repair(root: &Path, steps: &mut Vec<Step>) -> Result<(), Error> {
    let path = root.join(".gitattributes");
    if !path.is_file() {
        return Ok(());
    }
    let original = std::fs::read_to_string(&path)?;
    let next = crate::managed::repair_attributes(&original);
    if next == original {
        return Ok(());
    }
    if next.is_empty() {
        std::fs::remove_file(&path)?;
    } else {
        std::fs::write(&path, next)?;
    }
    crate::workflow::register_driver(root);
    push(
        steps,
        "gitattributes",
        "done",
        "repaired the merge attribute",
    );
    Ok(())
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
    crate::exe::Program::resolve(name).is_some()
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
