use std::fmt::Write as _;
use std::path::Path;

use agentmesh_core::{Error as CoreError, Ledger};
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
    record_graphify(&root, &options, &mut steps);
    record_hooks(&root, &options, &mut steps);
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
    let path = root.join(".agentmesh").join("ledger.db");
    let existed = path.exists();
    Ledger::initialize(&path)?;
    if existed {
        push(steps, "init", "skipped", "ledger already exists");
    } else {
        push(steps, "init", "done", "created .agentmesh/ledger.db");
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
