use std::path::{Path, PathBuf};
use std::process::Command;

use relaybook_core::legacy;

const START: &str = "# relaybook:hooks:start";
const END: &str = "# relaybook:hooks:end";
const BLOCK: &str = "\
# relaybook:hooks:start
# Refresh the Graphify graph after a merge or rebase without blocking the pull.
if command -v graphify >/dev/null 2>&1; then
  (graphify update . >/dev/null 2>&1 &)
fi
# relaybook:hooks:end
";

pub(crate) enum Status {
    Done(String),
    Skipped(String),
}

enum HookResult {
    Installed,
    Unchanged,
    Foreign { interpreter: String },
}

pub(crate) fn install(root: &Path) -> Result<Status, std::io::Error> {
    if !inside_work_tree(root) {
        return Ok(Status::Skipped("not a git repository".to_string()));
    }
    if let Some(reason) = external_hooks(root) {
        return Ok(Status::Skipped(reason));
    }
    let dir = hooks_dir(root)?;
    std::fs::create_dir_all(&dir)?;
    let merge = place_hook(&dir, "post-merge")?;
    let rewrite = place_hook(&dir, "post-rewrite")?;
    let detail = format!(
        "post-merge: {}; post-rewrite: {}",
        describe(&merge),
        describe(&rewrite)
    );
    if matches!(merge, HookResult::Installed) || matches!(rewrite, HookResult::Installed) {
        Ok(Status::Done(detail))
    } else {
        Ok(Status::Skipped(detail))
    }
}

fn describe(result: &HookResult) -> String {
    match result {
        HookResult::Installed => "installed".to_string(),
        HookResult::Unchanged => "already installed".to_string(),
        HookResult::Foreign { interpreter } => {
            format!("skipped ({interpreter} hook; add 'graphify update . &' yourself)")
        }
    }
}

fn external_hooks(root: &Path) -> Option<String> {
    if let Some(path) = git_config(root, "core.hooksPath") {
        return Some(format!(
            "core.hooksPath is {path}. Add post-merge and post-rewrite hooks there that run `graphify update .` in the background. Relaybook did not write .git/hooks."
        ));
    }
    if root.join(".husky").is_dir() {
        return Some(
            "Husky is present. Add post-merge and post-rewrite hooks that run `graphify update .` in the background."
                .to_string(),
        );
    }
    if root.join("lefthook.yml").is_file() || root.join("lefthook.yaml").is_file() {
        return Some(
            "Lefthook is present. Add post-merge and post-rewrite commands that run `graphify update .` in the background."
                .to_string(),
        );
    }
    None
}

fn place_hook(dir: &Path, name: &str) -> Result<HookResult, std::io::Error> {
    let path = dir.join(name);
    if !path.is_file() {
        std::fs::write(&path, format!("#!/bin/sh\n{BLOCK}"))?;
        make_executable(&path)?;
        return Ok(HookResult::Installed);
    }
    let existing = std::fs::read_to_string(&path)?;
    if existing.contains(START) && existing.contains(END) {
        return Ok(HookResult::Unchanged);
    }
    if existing.contains(legacy::HOOK_START) && existing.contains(legacy::HOOK_END) {
        std::fs::write(
            &path,
            replace_span(&existing, legacy::HOOK_START, legacy::HOOK_END, BLOCK),
        )?;
        make_executable(&path)?;
        return Ok(HookResult::Installed);
    }
    if let Some(interpreter) = foreign_interpreter(&existing) {
        return Ok(HookResult::Foreign { interpreter });
    }
    std::fs::write(&path, insert_block(&existing))?;
    make_executable(&path)?;
    Ok(HookResult::Installed)
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
    let mut block = replacement.to_string();
    if !block.ends_with('\n') {
        block.push('\n');
    }
    format!("{}{block}{}", &existing[..start_at], &existing[end_at..])
}

fn foreign_interpreter(script: &str) -> Option<String> {
    let first = script.lines().next()?.trim();
    if !first.starts_with("#!") {
        return None;
    }
    let rest = first.trim_start_matches("#!").trim();
    let mut parts = rest.split_whitespace();
    let program = parts.next().unwrap_or("");
    let base = program.rsplit('/').next().unwrap_or(program);
    let name = if base == "env" {
        parts.find(|part| !part.starts_with('-')).unwrap_or("")
    } else {
        base
    };
    if matches!(name, "sh" | "bash" | "zsh") {
        None
    } else if name.is_empty() {
        Some("unknown".to_string())
    } else {
        Some(name.to_string())
    }
}

fn insert_block(existing: &str) -> String {
    let Some(first) = existing.lines().next() else {
        return BLOCK.to_string();
    };
    if !first.starts_with("#!") {
        return format!("{BLOCK}{existing}");
    }
    match existing.find('\n') {
        Some(index) => {
            let head = &existing[..=index];
            let tail = &existing[index + 1..];
            format!("{head}\n{BLOCK}{tail}")
        }
        None => format!("{existing}\n{BLOCK}"),
    }
}

fn hooks_dir(root: &Path) -> Result<PathBuf, std::io::Error> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()?;
    if !output.status.success() {
        return Ok(root.join(".git").join("hooks"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let raw = text.trim();
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(root.join(path))
    }
}

fn inside_work_tree(root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .ok()
        .is_some_and(|output| output.status.success())
}

fn git_config(root: &Path, key: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get", key])
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

fn make_executable(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
