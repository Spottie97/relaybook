use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

const CONFIG_KEYS: &[&str] = &["merge.graphify.name", "merge.graphify.driver"];

#[derive(Clone, Debug)]
pub(crate) struct Capture {
    files: BTreeMap<String, Option<String>>,
    configs: BTreeMap<String, Option<String>>,
    dirs: BTreeMap<String, bool>,
}

const CREATED_DIRS: &[&str] = &[
    ".claude",
    ".claude/commands",
    ".cursor",
    ".cursor/commands",
    ".github",
    ".github/workflows",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub file: String,
    pub kind: String,
    pub added: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub(crate) struct Manifest {
    pub entries: Vec<Entry>,
}

pub(crate) fn path(root: &Path) -> PathBuf {
    root.join(".relaybook").join("setup.json")
}

pub(crate) fn capture(root: &Path) -> Capture {
    let mut files = BTreeMap::new();
    for relative in watched_files(root) {
        files.insert(relative.clone(), read_text(&root.join(&relative)));
    }
    let mut configs = BTreeMap::new();
    for key in CONFIG_KEYS {
        configs.insert((*key).to_string(), git_config(root, key));
    }
    let mut dirs = BTreeMap::new();
    for dir in CREATED_DIRS {
        dirs.insert((*dir).to_string(), root.join(dir).is_dir());
    }
    Capture {
        files,
        configs,
        dirs,
    }
}

pub(crate) fn record(root: &Path, before: &Capture) -> std::io::Result<()> {
    let after = capture(root);
    let mut manifest = load(root).unwrap_or_default();
    for (file, previous) in &before.files {
        let now = after.files.get(file).cloned().unwrap_or(None);
        if now == *previous {
            continue;
        }
        if let Some(entry) = manifest
            .entries
            .iter_mut()
            .find(|entry| entry.file == *file)
        {
            entry.after = now;
            continue;
        }
        manifest.entries.push(Entry {
            file: file.clone(),
            kind: kind_for(file),
            added: added_for(file),
            before: previous.clone(),
            after: now,
        });
    }
    for (key, previous) in &before.configs {
        let now = after.configs.get(key).cloned().unwrap_or(None);
        if now == *previous {
            continue;
        }
        let file = format!("git-config:{key}");
        if let Some(entry) = manifest.entries.iter_mut().find(|entry| entry.file == file) {
            entry.after = now;
            continue;
        }
        manifest.entries.push(Entry {
            file,
            kind: "git-config".to_string(),
            added: key.clone(),
            before: previous.clone(),
            after: now,
        });
    }
    for (dir, existed) in &before.dirs {
        if *existed || !root.join(dir).is_dir() {
            continue;
        }
        if manifest.entries.iter().any(|entry| entry.file == *dir) {
            continue;
        }
        manifest.entries.push(Entry {
            file: dir.clone(),
            kind: "directory".to_string(),
            added: "created".to_string(),
            before: None,
            after: Some("created".to_string()),
        });
    }
    if manifest.entries.is_empty() {
        return Ok(());
    }
    let path = path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(&manifest)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    fs::write(path, text + "\n")
}

pub(crate) fn load(root: &Path) -> Option<Manifest> {
    let text = fs::read_to_string(path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn watched_files(root: &Path) -> Vec<String> {
    let mut files = vec![
        "CLAUDE.md".to_string(),
        "AGENTS.md".to_string(),
        ".gitignore".to_string(),
        ".gitattributes".to_string(),
        ".claude/settings.json".to_string(),
        ".claude/commands/relaybook.md".to_string(),
        ".cursor/commands/relaybook.md".to_string(),
        ".github/workflows/graphify-update.yml".to_string(),
    ];
    if let Some(dir) = hooks_dir(root) {
        for name in ["post-merge", "post-rewrite"] {
            if let Ok(relative) = dir.join(name).strip_prefix(root) {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files
}

fn kind_for(file: &str) -> String {
    if file.ends_with(".gitignore") {
        "gitignore-line".to_string()
    } else if file.ends_with(".gitattributes") {
        "gitattributes-line".to_string()
    } else if file.contains("/hooks/") {
        "hook-block".to_string()
    } else if file.ends_with("settings.json") {
        "json-entry".to_string()
    } else if file.ends_with("graphify-update.yml") || file.ends_with("relaybook.md") {
        "file".to_string()
    } else {
        "block".to_string()
    }
}

fn added_for(file: &str) -> String {
    match kind_for(file).as_str() {
        "gitignore-line" => {
            "ignore lines between # relaybook:start and # relaybook:end".to_string()
        }
        "gitattributes-line" => {
            "merge attribute between # relaybook:start and # relaybook:end".to_string()
        }
        "hook-block" => "graphify refresh hook".to_string(),
        "json-entry" => "relaybook settings entry".to_string(),
        "file" => file.to_string(),
        _ => "relaybook block".to_string(),
    }
}

fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn hooks_dir(root: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .ok()?;
    if !output.status.success() {
        return Some(root.join(".git").join("hooks"));
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(root.join(path))
    }
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
