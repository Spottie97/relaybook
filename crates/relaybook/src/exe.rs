use std::path::{Path, PathBuf};
use std::process::Command;

/// A program found on PATH, including Windows `.exe`, `.cmd`, and `.bat` shims.
pub(crate) struct Program {
    path: PathBuf,
}

impl Program {
    pub(crate) fn resolve(name: &str) -> Option<Self> {
        which::which(name).ok().map(|path| Self { path })
    }

    pub(crate) fn command(&self) -> Command {
        if is_shell_shim(&self.path) {
            let mut command = Command::new(comspec());
            command.arg("/C").arg(&self.path);
            command
        } else {
            Command::new(&self.path)
        }
    }
}

/// `cursor-agent` when it is installed, otherwise `agent` when `--version` names Cursor.
pub(crate) fn resolve_cursor() -> Option<Program> {
    if let Some(program) = Program::resolve("cursor-agent") {
        return Some(program);
    }
    let program = Program::resolve("agent")?;
    version_names_cursor(&program).then_some(program)
}

pub(crate) fn names_cursor(text: &str) -> bool {
    text.to_ascii_lowercase().contains("cursor")
}

fn version_names_cursor(program: &Program) -> bool {
    let Ok(output) = program.command().arg("--version").output() else {
        return false;
    };
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    names_cursor(&text)
}

fn is_shell_shim(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("cmd" | "bat")
    )
}

fn comspec() -> PathBuf {
    std::env::var_os("COMSPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cmd.exe"))
}

#[cfg(test)]
mod tests {
    use super::names_cursor;

    #[test]
    fn cursor_version_text_must_name_cursor() {
        assert!(names_cursor("Cursor Agent 2026.1"));
        assert!(!names_cursor("grok 1.0"));
    }
}
