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
    let body = normalize_lf(body);
    let path = root.join(relative);
    if path.is_file() {
        let existing = fs::read_to_string(&path)?;
        if normalize_lf(&existing) == body {
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

fn normalize_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_file_is_written_with_line_feeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let relative = Path::new(".claude").join("commands").join("relaybook.md");
        let body = "---\r\n# relaybook:managed\r\ndescription: \"x\"\r\n---\r\n";
        let status = install(dir.path(), relative.clone(), body).expect("install");
        assert!(matches!(status, Status::Updated));
        let written = fs::read_to_string(dir.path().join(&relative)).expect("read");
        assert_eq!(
            written,
            "---\n# relaybook:managed\ndescription: \"x\"\n---\n"
        );
        let again = install(dir.path(), relative, body).expect("second");
        assert!(matches!(again, Status::Unchanged));
    }
}
