use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn toplevel(cwd: &Path) -> Option<PathBuf> {
    let text = git(cwd, ["rev-parse", "--show-toplevel"])?;
    Some(PathBuf::from(text))
}

pub(crate) fn head(cwd: &Path) -> Option<String> {
    git(cwd, ["rev-parse", "HEAD"])
}

fn git<const N: usize>(cwd: &Path, args: [&str; N]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let line = text.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}
