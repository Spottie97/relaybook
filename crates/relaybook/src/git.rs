//! Shared git worktree checks.
//!
//! `dirty_paths` is the historical check. The auto runner uses it, and that
//! result stays as it is. Gated claim and done use `gated_dirty_paths`.
//!
//! `gated_dirty_paths` ignores `.relaybook` so ledger writes are not a dirty
//! tree. It asks git for every untracked file and every submodule change, and
//! it parses NUL-delimited porcelain without trimming. A rename counts both
//! paths, so moving a tracked file into `.relaybook` stays dirty.

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

pub(crate) fn gated_dirty_paths(cwd: &Path) -> Result<Vec<String>, String> {
    let bytes = git_bytes(
        cwd,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    parse_porcelain_z(&bytes)
}

fn git_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let stdout = git_bytes(cwd, args)?;
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

fn git_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
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
    Ok(output.stdout)
}

/// Porcelain v1 `-z` records are `XY PATH\0`. A rename or copy adds the
/// original path: `XY PATH\0ORIG\0`. `PATH` is the current path. Neither
/// field is trimmed; a leading status space is part of `XY`.
fn parse_porcelain_z(bytes: &[u8]) -> Result<Vec<String>, String> {
    let mut dirty = Vec::new();
    let mut parts = bytes.split(|byte| *byte == b'\0');
    while let Some(record) = parts.next() {
        if record.is_empty() {
            continue;
        }
        if record.len() < 3 || record[2] != b' ' {
            return Err("unreadable git status".to_string());
        }
        let renamed = matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C');
        let current = porcelain_path(&record[3..]);
        if renamed {
            let Some(original) = parts.next() else {
                return Err("unreadable git status".to_string());
            };
            if original.is_empty() {
                return Err("unreadable git status".to_string());
            }
            push_outside_ledger(&mut dirty, &current);
            push_outside_ledger(&mut dirty, &porcelain_path(original));
        } else {
            push_outside_ledger(&mut dirty, &current);
        }
    }
    Ok(dirty)
}

fn porcelain_path(bytes: &[u8]) -> String {
    git_path(&String::from_utf8_lossy(bytes))
}

fn push_outside_ledger(dirty: &mut Vec<String>, path: &str) {
    if path == ".relaybook" || path.starts_with(".relaybook/") {
        return;
    }
    dirty.push(path.to_string());
}

#[cfg(test)]
mod tests {
    use super::parse_porcelain_z;

    #[test]
    fn nul_porcelain_keeps_rename_sources_and_leading_status_spaces() {
        let rename = b"R  .relaybook/src.rs\0src.rs\0";
        assert_eq!(
            parse_porcelain_z(rename).expect("rename"),
            vec!["src.rs".to_string()]
        );

        let spaced = b" M a.relaybook/x\0";
        assert_eq!(
            parse_porcelain_z(spaced).expect("space"),
            vec!["a.relaybook/x".to_string()]
        );

        let ledger = b"?? .relaybook/ledger.db\0";
        assert!(parse_porcelain_z(ledger).expect("ledger").is_empty());
    }
}
