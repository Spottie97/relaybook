use std::path::Path;
use std::process::Command;

pub(crate) const INSTALL_HINT: &str = "graphify is not on PATH. Install it with `uv tool install graphifyy` or `pipx install graphifyy`.";

/// Handoff text when no graph SHA can be recorded.
pub(crate) const NO_GRAPH: &str = "no graph";

/// Cap for Graphify context injected into a handoff. The marker is included.
pub(crate) const GRAPH_CONTEXT_LIMIT: usize = 4000;

pub(crate) const GRAPH_CONTEXT_MARKER: &str = "\n[truncated]";

pub(crate) struct CommandResult {
    pub ok: bool,
    pub detail: String,
}

pub(crate) struct FullResult {
    pub ok: bool,
    pub stdout: String,
    pub detail: String,
}

pub(crate) fn installed() -> bool {
    crate::exe::Program::resolve("graphify").is_some()
}

pub(crate) fn run(root: &Path, args: &[&str]) -> CommandResult {
    let full = run_full(root, args);
    CommandResult {
        ok: full.ok,
        detail: full.detail,
    }
}

/// Like [`run`], but keeps the whole stdout. `run` still returns one short line.
pub(crate) fn run_full(root: &Path, args: &[&str]) -> FullResult {
    let Some(program) = crate::exe::Program::resolve("graphify") else {
        return FullResult {
            ok: false,
            stdout: String::new(),
            detail: "graphify is not on PATH".to_string(),
        };
    };
    match program.command().args(args).current_dir(root).output() {
        Ok(output) if output.status.success() => FullResult {
            ok: true,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            detail: summary(&output.stdout, &output.stderr, "ok"),
        },
        Ok(output) => {
            let fallback = format!("exit {}", output.status.code().unwrap_or(1));
            FullResult {
                ok: false,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                detail: summary(&output.stderr, &output.stdout, &fallback),
            }
        }
        Err(err) => FullResult {
            ok: false,
            stdout: String::new(),
            detail: err.to_string(),
        },
    }
}

/// `graphify affected` for each file in `git diff --name-only base..commit`,
/// then cut to [`GRAPH_CONTEXT_LIMIT`].
pub(crate) fn graph_context(root: &Path, base: &str, commit: &str) -> String {
    let mut combined = String::new();
    for (index, file) in diff_names(root, base, commit).into_iter().enumerate() {
        if index > 0 {
            combined.push('\n');
        }
        let result = run_full(root, &["affected", &file]);
        if result.ok {
            combined.push_str(&result.stdout);
        } else if !result.detail.is_empty() {
            combined.push_str(&result.detail);
        }
    }
    truncate_graph_context(&combined)
}

pub(crate) fn no_graph(reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() || reason == NO_GRAPH {
        NO_GRAPH.to_string()
    } else {
        format!("{NO_GRAPH}: {reason}")
    }
}

fn diff_names(root: &Path, base: &str, commit: &str) -> Vec<String> {
    let range = format!("{base}..{commit}");
    let Ok(output) = Command::new("git")
        .current_dir(root)
        .args([
            "diff",
            "--name-only",
            "-z",
            "--end-of-options",
            range.as_str(),
        ])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

fn truncate_graph_context(text: &str) -> String {
    if text.len() <= GRAPH_CONTEXT_LIMIT {
        return text.to_string();
    }
    let marker = GRAPH_CONTEXT_MARKER;
    let mut keep = GRAPH_CONTEXT_LIMIT.saturating_sub(marker.len());
    while keep > 0 && !text.is_char_boundary(keep) {
        keep -= 1;
    }
    let mut out = text[..keep].to_string();
    out.push_str(marker);
    out
}

fn summary(primary: &[u8], secondary: &[u8], fallback: &str) -> String {
    let primary = first_line(primary);
    if !primary.is_empty() {
        return primary;
    }
    let secondary = first_line(secondary);
    if !secondary.is_empty() {
        return secondary;
    }
    fallback.to_string()
}

fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().next().unwrap_or("").trim();
    let mut shortened = line.chars().take(200).collect::<String>();
    if line.chars().count() > 200 {
        shortened.push('…');
    }
    shortened
}

#[cfg(test)]
mod tests {
    use super::{GRAPH_CONTEXT_LIMIT, GRAPH_CONTEXT_MARKER, truncate_graph_context};

    #[test]
    fn short_graph_context_is_unchanged() {
        assert_eq!(truncate_graph_context("caller"), "caller");
    }

    #[test]
    fn graph_context_limit_adds_a_truncation_marker() {
        let text = "a".repeat(GRAPH_CONTEXT_LIMIT + 50);
        let capped = truncate_graph_context(&text);
        assert_eq!(capped.len(), GRAPH_CONTEXT_LIMIT);
        assert!(capped.ends_with(GRAPH_CONTEXT_MARKER));
        assert!(capped.is_char_boundary(capped.len()));
    }

    #[test]
    fn truncation_keeps_a_utf8_boundary() {
        let mut text = "a".repeat(GRAPH_CONTEXT_LIMIT - GRAPH_CONTEXT_MARKER.len() - 1);
        text.push('é');
        text.push_str(&"b".repeat(20));
        let capped = truncate_graph_context(&text);
        assert!(capped.len() <= GRAPH_CONTEXT_LIMIT);
        assert!(capped.ends_with(GRAPH_CONTEXT_MARKER));
        assert!(std::str::from_utf8(capped.as_bytes()).is_ok());
    }
}
