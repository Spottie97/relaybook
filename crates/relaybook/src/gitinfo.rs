use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn toplevel(cwd: &Path) -> Option<PathBuf> {
    let text = git(cwd, ["rev-parse", "--show-toplevel"])?;
    Some(PathBuf::from(text))
}

pub(crate) fn head(cwd: &Path) -> Option<String> {
    git(cwd, ["rev-parse", "HEAD"])
}

pub(crate) fn branch(cwd: &Path) -> Option<String> {
    git(cwd, ["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Whether `rev` names an object. Exit 1 means the name is missing.
/// A spawn failure or any other status is an error, so a broken git
/// does not look like an absent ref.
pub(crate) fn rev_exists(cwd: &Path, rev: &str) -> Result<bool, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(rev)
        .output()
        .map_err(|err| err.to_string())?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(code) => Err(format!(
            "git rev-parse failed: {code} {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        None => Err("git rev-parse failed".to_string()),
    }
}

/// HEAD peeled to a commit. `None` when the repository has no commit yet.
pub(crate) fn verified_head(cwd: &Path) -> Option<String> {
    git(
        cwd,
        ["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"],
    )
}

/// Commits in `merge-base(main, HEAD)..HEAD`, excluding the merge base.
pub(crate) struct ReleaseRange {
    pub base: String,
    pub head: String,
    pub commits: Vec<String>,
}

pub(crate) fn release_range(cwd: &Path) -> Result<ReleaseRange, String> {
    let head = verified_head(cwd).ok_or_else(|| "a commit at HEAD is required".to_string())?;
    let base = git_text(cwd, &["merge-base", "--end-of-options", "main", &head])
        .map_err(|message| format!("release review requires main: {message}"))?;
    let base = base.trim().to_string();
    if base.is_empty() {
        return Err("release review requires main".to_string());
    }
    let spec = format!("{base}..{head}");
    let listed = git_text(
        cwd,
        &["rev-list", "--reverse", "--end-of-options", spec.as_str()],
    )
    .map_err(|message| format!("release review requires main: {message}"))?;
    let commits = listed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    Ok(ReleaseRange {
        base,
        head,
        commits,
    })
}

fn git_text(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        if detail.is_empty() {
            return Err("git command failed".to_string());
        }
        return Err(detail.to_string());
    }
    String::from_utf8(output.stdout).map_err(|err| err.to_string())
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
