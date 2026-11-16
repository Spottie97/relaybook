//! Shared git worktree checks.
//!
//! `dirty_paths` is the clean-tree rule for gated claim and done. It ignores
//! `.relaybook` so ledger writes are not a dirty tree. The auto runner uses
//! the same function.

use std::path::Path;
use std::process::Command;

pub(crate) fn dirty_paths(cwd: &Path) -> Result<Vec<String>, String> {
    let text = git(cwd, &["status", "--porcelain"])?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let path = line.get(3..).unwrap_or(line).trim().trim_matches('"');
            let path = path.rsplit(" -> ").next().unwrap_or(path);
            let path = git_path(path);
            if path == ".relaybook" || path.starts_with(".relaybook/") {
                None
            } else {
                Some(path)
            }
        })
        .collect())
}

fn git_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
