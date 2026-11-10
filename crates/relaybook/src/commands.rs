use std::fs;
use std::path::Path;

pub(crate) const MARKER: &str = "relaybook:managed";

const CLAUDE_COMMAND: &str = include_str!("../templates/claude-command.md");
const CURSOR_COMMAND: &str = include_str!("../templates/cursor-command.md");

pub(crate) enum Status {
    Updated,
    Unchanged,
    LeftAlone,
}

pub(crate) fn install_claude(root: &Path) -> std::io::Result<Status> {
    install(
        root,
        Path::new(".claude").join("commands").join("relaybook.md"),
        CLAUDE_COMMAND,
    )
}

pub(crate) fn install_cursor(root: &Path) -> std::io::Result<Status> {
    install(
        root,
        Path::new(".cursor").join("commands").join("relaybook.md"),
        CURSOR_COMMAND,
    )
}

fn install(root: &Path, relative: std::path::PathBuf, body: &str) -> std::io::Result<Status> {
    let path = root.join(relative);
    if path.is_file() {
        let existing = fs::read_to_string(&path)?;
        if existing == body {
            return Ok(Status::Unchanged);
        }
        if !existing.contains(MARKER) {
            return Ok(Status::LeftAlone);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, body)?;
    Ok(Status::Updated)
}
