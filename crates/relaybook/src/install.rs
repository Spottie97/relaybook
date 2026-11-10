use std::path::Path;

use relaybook_core::legacy;

pub(crate) const PLANNER_HEADING: &str = "## Relaybook coordination (planner)";
pub(crate) const EXECUTOR_HEADING: &str = "## Relaybook coordination (executor)";
const MARKER_START: &str = "<!-- relaybook:start -->";
const MARKER_END: &str = "<!-- relaybook:end -->";

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
    if let Some(replaced) = replace_legacy(&existing, body) {
        std::fs::write(path, replaced)?;
        return Ok(InstallState::Updated);
    }
    std::fs::write(path, append_block(&existing, body))?;
    Ok(InstallState::Updated)
}

fn replace_legacy(existing: &str, body: &str) -> Option<String> {
    let block = append_block("", body);
    if existing.contains(legacy::MARKER_START) && existing.contains(legacy::MARKER_END) {
        return Some(replace_span(
            existing,
            legacy::MARKER_START,
            legacy::MARKER_END,
            &block,
        ));
    }
    let old_heading = if body.contains("(planner)") {
        legacy::PLANNER_HEADING
    } else {
        legacy::EXECUTOR_HEADING
    };
    let start = existing.find(old_heading)?;
    let after = start + old_heading.len();
    let next = existing[after..]
        .find("\n## ")
        .map(|index| after + index)
        .unwrap_or(existing.len());
    let mut out = String::new();
    out.push_str(&existing[..start]);
    out.push_str(block.trim_start_matches('\n'));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&existing[next..]);
    Some(out)
}

fn replace_span(existing: &str, start: &str, end: &str, replacement: &str) -> String {
    let start_at = existing.find(start).unwrap_or(0);
    let end_at = existing[start_at..]
        .find(end)
        .map(|index| start_at + index + end.len())
        .unwrap_or(existing.len());
    let end_at = if existing[end_at..].starts_with('\n') {
        end_at + 1
    } else {
        end_at
    };
    let mut block = replacement.trim_start_matches('\n').to_string();
    if !block.ends_with('\n') {
        block.push('\n');
    }
    format!("{}{block}{}", &existing[..start_at], &existing[end_at..])
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

#[cfg(test)]
mod tests {
    use super::{InstallState, MARKER_START, PLANNER_HEADING, apply};

    #[test]
    fn appends_once_and_keeps_text_without_a_trailing_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, "hello").expect("write");

        let first = apply(dir.path(), true, false).expect("install");
        assert_eq!(first.claude, Some(InstallState::Updated));
        assert_eq!(first.cursor, None);

        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.starts_with("hello\n"), "{text}");
        assert_eq!(text.matches(MARKER_START).count(), 1);
        assert!(text.contains(PLANNER_HEADING));

        let second = apply(dir.path(), true, false).expect("install");
        assert_eq!(second.claude, Some(InstallState::Unchanged));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
    }

    #[test]
    fn replaces_a_legacy_marker_block_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(
            &path,
            "keep\n<!-- agentmesh:start -->\nold\n<!-- agentmesh:end -->\n",
        )
        .expect("write");

        let first = apply(dir.path(), true, false).expect("install");
        assert_eq!(first.claude, Some(InstallState::Updated));
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.starts_with("keep\n"), "{text}");
        assert_eq!(text.matches(MARKER_START).count(), 1);
        assert!(!text.contains("agentmesh"));
        assert!(text.contains(PLANNER_HEADING));

        let second = apply(dir.path(), true, false).expect("install");
        assert_eq!(second.claude, Some(InstallState::Unchanged));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
    }

    #[test]
    fn replaces_a_legacy_heading() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("AGENTS.md");
        std::fs::write(
            &path,
            "keep\n## AgentMesh coordination (executor)\nold steps\n",
        )
        .expect("write");

        let outcome = apply(dir.path(), false, true).expect("install");
        assert_eq!(outcome.cursor, Some(InstallState::Updated));
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.starts_with("keep\n"), "{text}");
        assert!(text.contains(super::EXECUTOR_HEADING));
        assert!(!text.contains("AgentMesh"));
        assert_eq!(text.matches(MARKER_START).count(), 1);
    }

    #[test]
    fn existing_heading_is_left_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("AGENTS.md");
        let original = format!("{}\ncustom\n", super::EXECUTOR_HEADING);
        std::fs::write(&path, &original).expect("write");

        let outcome = apply(dir.path(), false, true).expect("install");
        assert_eq!(outcome.cursor, Some(InstallState::Unchanged));
        assert_eq!(outcome.claude, None);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), original);
    }
}
