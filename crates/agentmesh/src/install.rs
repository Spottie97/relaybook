use std::path::Path;

pub(crate) const PLANNER_HEADING: &str = "## AgentMesh coordination (planner)";
pub(crate) const EXECUTOR_HEADING: &str = "## AgentMesh coordination (executor)";
const MARKER_START: &str = "<!-- agentmesh:start -->";
const MARKER_END: &str = "<!-- agentmesh:end -->";

const CLAUDE_BODY: &str = include_str!("../templates/claude.md");
const AGENTS_BODY: &str = include_str!("../templates/agents.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallState {
    Updated,
    Unchanged,
}

impl InstallState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstallOutcome {
    pub(crate) claude: Option<InstallState>,
    pub(crate) cursor: Option<InstallState>,
}

pub(crate) fn apply(
    root: &Path,
    claude: bool,
    cursor: bool,
) -> Result<InstallOutcome, std::io::Error> {
    let claude = if claude {
        Some(install_file(
            &root.join("CLAUDE.md"),
            PLANNER_HEADING,
            CLAUDE_BODY,
        )?)
    } else {
        None
    };
    let cursor = if cursor {
        Some(install_file(
            &root.join("AGENTS.md"),
            EXECUTOR_HEADING,
            AGENTS_BODY,
        )?)
    } else {
        None
    };
    Ok(InstallOutcome { claude, cursor })
}

fn install_file(path: &Path, heading: &str, body: &str) -> Result<InstallState, std::io::Error> {
    let existing = if path.is_file() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };
    if existing.contains(MARKER_START) || existing.contains(heading) {
        return Ok(InstallState::Unchanged);
    }
    std::fs::write(path, append_block(&existing, body))?;
    Ok(InstallState::Updated)
}

fn append_block(existing: &str, body: &str) -> String {
    let mut out = existing.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(MARKER_START);
    out.push('\n');
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(MARKER_END);
    out.push('\n');
    out
}
