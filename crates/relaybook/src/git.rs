//! Shared git worktree checks.
//!
//! `dirty_paths` is the historical check. The auto runner uses it, and that
//! result stays as it is. Gated claim and done use `gated_dirty_paths`.
//!
//! `gated_dirty_paths` ignores `.relaybook` so ledger writes are not a dirty
//! tree. It asks git for every untracked file and every submodule change, and
//! it parses NUL-delimited porcelain without trimming. `-z` paths stay
//! literal: a backslash is part of the name, and ledger exclusion is decided
//! on that literal path. Separator folding belongs only to the historical
//! `dirty_paths` parser. A rename counts both paths, so moving a tracked file
//! into `.relaybook` stays dirty.

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

/// Fingerprint of the worktree at `cwd`. `git diff HEAD` plus each untracked
/// file that git does not ignore. Paths under `.relaybook` are omitted.
pub(crate) fn worktree_fingerprint(cwd: &Path) -> Result<String, String> {
    let diff = git(cwd, &["diff", "HEAD", "--", ".", ":(exclude).relaybook"])?;
    let untracked = git(cwd, &["ls-files", "--others", "--exclude-standard"])?;
    let mut body = diff;
    body.push('\n');
    for path in untracked.lines() {
        let path = git_path(path);
        if path == ".relaybook" || path.starts_with(".relaybook/") {
            continue;
        }
        body.push_str(&path);
        body.push('\n');
        let bytes = std::fs::read(cwd.join(&path)).unwrap_or_default();
        body.push_str(&content_hash(&bytes));
        body.push('\n');
    }
    Ok(content_hash(body.as_bytes()))
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
/// field is trimmed; a leading status space is part of `XY`. Path bytes are
/// not unescaped: `-z` does not quote them.
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

/// Decode one `-z` path. Git writes the bytes as stored, with `/` as the
/// separator and no escape sequences. Folding `\` to `/` before the ledger
/// check would treat a Unix file named `.relaybook\payload.rs` as ledger data.
fn porcelain_path(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn push_outside_ledger(dirty: &mut Vec<String>, literal: &str) {
    if ledger_path(literal) {
        return;
    }
    // Keep the literal path. Folding separators here would report a different
    // filename from the one git and the worktree use.
    dirty.push(literal.to_string());
}

fn ledger_path(literal: &str) -> bool {
    literal == ".relaybook" || literal.starts_with(".relaybook/")
}

fn content_hash(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

pub(crate) fn graphify_out_path(literal: &str) -> bool {
    literal == "graphify-out" || literal.starts_with("graphify-out/")
}

pub(crate) fn graphify_out_tracked(cwd: &Path) -> Result<bool, String> {
    let text = git(cwd, &["ls-files", "--", "graphify-out"])?;
    Ok(!text.is_empty())
}

/// True when gitignore (or exclude) would ignore `graphify-out/graph.json`.
/// Exit 1 means the path is not ignored. Any other failure is an error.
pub(crate) fn graphify_out_ignored(cwd: &Path) -> Result<bool, String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(["check-ignore", "-q", "--", "graphify-out/graph.json"])
        .output()
        .map_err(|err| err.to_string())?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(code) => Err(format!(
            "git check-ignore failed: {code} {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        None => Err("git check-ignore failed".to_string()),
    }
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

    #[test]
    fn nul_porcelain_keeps_literal_backslash_filenames() {
        let untracked = b"?? .relaybook\\payload.rs\0";
        assert_eq!(
            parse_porcelain_z(untracked).expect("untracked"),
            vec![".relaybook\\payload.rs".to_string()]
        );

        let tracked = b" M .relaybook\\payload.rs\0";
        assert_eq!(
            parse_porcelain_z(tracked).expect("tracked"),
            vec![".relaybook\\payload.rs".to_string()]
        );

        let renamed = b"R  .relaybook\\payload.rs\0src.rs\0";
        assert_eq!(
            parse_porcelain_z(renamed).expect("rename to backslash name"),
            vec![".relaybook\\payload.rs".to_string(), "src.rs".to_string()]
        );

        let into_ledger = b"R  .relaybook/payload.rs\0.relaybook\\payload.rs\0";
        assert_eq!(
            parse_porcelain_z(into_ledger).expect("rename into ledger"),
            vec![".relaybook\\payload.rs".to_string()]
        );

        let ordinary = b"?? .relaybook/payload.rs\0";
        assert!(parse_porcelain_z(ordinary).expect("ledger file").is_empty());
    }
}
