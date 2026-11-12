use std::collections::HashSet;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::claude_hook::{
    BARE_PROMPT, BARE_SESSION, GUARDED_PROMPT, GUARDED_SESSION, PLANNER_ALLOW, POWERSHELL_PROMPT,
    POWERSHELL_SESSION,
};
use crate::commands::MARKER;
use crate::manifest::{self, Entry};
use crate::paths;
use crate::workflow;
const GUARD_PREFIX: &str = "command -v graphify >/dev/null 2>&1 && graphify ";
const POWERSHELL_GUARD_PREFIX: &str = "if (Get-Command graphify -ErrorAction SilentlyContinue) { ";
const GRAPHIFY_NAME: &str = "graphify graph.json union merge";
const GRAPHIFY_DRIVER: &str = "graphify merge-driver %O %A %B";

pub(crate) struct Options {
    pub yes: bool,
    pub dry_run: bool,
    pub purge: bool,
    pub graphify: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct Change {
    pub item: String,
    pub action: String,
    pub detail: String,
}

pub(crate) struct Report {
    pub changes: Vec<Change>,
    pub error: Option<String>,
}

pub(crate) fn run(cwd: &Path, options: Options) -> Result<Report, String> {
    let root = paths::init_root(cwd);
    if active_run(&root)? {
        return Ok(Report {
            changes: vec![change(
                "run",
                "skipped",
                "a run is active; relaybook run stop first",
            )],
            error: Some("a run is active; relaybook run stop first".to_string()),
        });
    }
    let manifest = manifest::load(&root);
    let mut changes = planned(&root, manifest.as_ref(), options.graphify);
    if options.purge {
        changes.push(change(
            ".relaybook/",
            "removed",
            "deleted the ledger and setup manifest",
        ));
        if root.join("graphify-out").exists() {
            changes.push(change("graphify-out/", "removed", "--purge"));
        }
    } else if root.join(".relaybook").exists() {
        changes.push(change(
            ".relaybook/",
            "kept",
            "ledger is project data; relaybook uninstall --purge deletes it",
        ));
    }
    if options.dry_run {
        for change in &mut changes {
            change.action = dry_action(&change.action);
        }
        return Ok(Report {
            changes,
            error: None,
        });
    }
    if !options.yes {
        if !io::stdin().is_terminal() {
            changes.insert(0, change("uninstall", "skipped", "re-run with --yes"));
            return Ok(Report {
                changes,
                error: None,
            });
        }
        if !confirm("Remove Relaybook from this project?")? {
            return Ok(Report {
                changes: vec![change("uninstall", "skipped", "cancelled")],
                error: None,
            });
        }
        if options.purge && !confirm("Delete .relaybook/ including the ledger?")? {
            changes.retain(|item| item.item != ".relaybook/" && item.item != "graphify-out/");
            changes.push(change(".relaybook/", "kept", "purge cancelled"));
        }
    }
    apply(&root, manifest.as_ref(), options.graphify)?;
    let purge = options.purge && !changes.iter().any(|item| item.detail == "purge cancelled");
    if purge {
        if root.join("graphify-out").is_dir() {
            fs::remove_dir_all(root.join("graphify-out")).map_err(|err| err.to_string())?;
        }
        if root.join(".relaybook").exists() {
            fs::remove_dir_all(root.join(".relaybook")).map_err(|err| err.to_string())?;
        }
    }
    Ok(Report {
        changes,
        error: None,
    })
}

pub(crate) fn render(report: &Report) -> String {
    let mut out = String::new();
    for item in &report.changes {
        out.push_str(&item.action);
        out.push(' ');
        out.push_str(&item.item);
        if !item.detail.is_empty() {
            out.push_str(" (");
            out.push_str(&item.detail);
            out.push(')');
        }
        out.push('\n');
    }
    out
}

fn planned(root: &Path, manifest: Option<&manifest::Manifest>, graphify: bool) -> Vec<Change> {
    let mut changes = Vec::new();
    if let Some(manifest) = manifest {
        for entry in &manifest.entries {
            if entry.kind == "directory" {
                continue;
            }
            changes.push(plan_entry(root, entry));
        }
    }
    changes = merge_plans(changes, plan_markers(root, manifest.is_some()));
    if let Some(manifest) = manifest {
        changes.extend(plan_dirs(root, manifest));
    }
    if graphify {
        for command in ["claude uninstall", "cursor uninstall", "hook uninstall"] {
            let item = format!("graphify {command}");
            if crate::graphify::installed() {
                changes.push(change(item, "removed", "graphify uninstaller"));
            } else {
                changes.push(change(item, "skipped", "graphify is not on PATH"));
            }
        }
    }
    changes
}

fn plan_entry(root: &Path, entry: &Entry) -> Change {
    if entry.kind == "git-config" {
        let key = entry.file.trim_start_matches("git-config:");
        let current = git_config(root, key);
        if current.as_deref() == entry.after.as_deref() {
            return change(entry.file.clone(), file_action(&entry.before), "");
        }
        return change(
            entry.file.clone(),
            "skipped",
            "git config changed since setup",
        );
    }
    let current = fs::read_to_string(root.join(&entry.file)).ok();
    if current == entry.after {
        return change(entry.file.clone(), file_action(&entry.before), "");
    }
    change(
        entry.file.clone(),
        "skipped",
        "changed since setup; markers still apply",
    )
}

fn file_action(before: &Option<String>) -> &'static str {
    if before.is_some() {
        "restored"
    } else {
        "removed"
    }
}

fn dry_action(action: &str) -> String {
    match action {
        "removed" => "would remove".to_string(),
        "restored" => "would restore".to_string(),
        "kept" => "would keep".to_string(),
        other => other.to_string(),
    }
}

fn merge_plans(manifest_changes: Vec<Change>, markers: Vec<Change>) -> Vec<Change> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for item in manifest_changes {
        if item.action == "skipped"
            && let Some(marker) = markers.iter().find(|marker| marker.item == item.item)
        {
            if seen.insert(marker.item.clone()) {
                out.push(marker.clone());
            }
            continue;
        }
        if seen.insert(item.item.clone()) {
            out.push(item);
        }
    }
    for marker in markers {
        if seen.insert(marker.item.clone()) {
            out.push(marker);
        }
    }
    out
}

fn files_removed_by_manifest(root: &Path, manifest: &manifest::Manifest) -> HashSet<String> {
    manifest
        .entries
        .iter()
        .filter(|entry| entry.kind != "directory" && entry.kind != "git-config")
        .filter(|entry| entry.before.is_none())
        .filter(|entry| fs::read_to_string(root.join(&entry.file)).ok() == entry.after)
        .map(|entry| entry.file.clone())
        .collect()
}

fn plan_dirs(root: &Path, manifest: &manifest::Manifest) -> Vec<Change> {
    dirs_to_remove(root, manifest)
        .into_iter()
        .map(|dir| change(dir, "removed", ""))
        .collect()
}

fn dirs_to_remove(root: &Path, manifest: &manifest::Manifest) -> Vec<String> {
    let deleting = files_removed_by_manifest(root, manifest);
    let mut dirs: Vec<String> = manifest
        .entries
        .iter()
        .filter(|entry| entry.kind == "directory" && entry.before.is_none())
        .map(|entry| entry.file.clone())
        .filter(|dir| dir_clears(root, dir, &deleting))
        .collect();
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.matches('/').count()));
    dirs
}

fn dir_clears(root: &Path, dir: &str, deleting: &HashSet<String>) -> bool {
    let Ok(entries) = fs::read_dir(root.join(dir)) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let child = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path().as_path())
            .to_string_lossy()
            .replace('\\', "/");
        if entry.path().is_dir() {
            if !dir_clears(root, &child, deleting) {
                return false;
            }
        } else if !deleting.contains(&child) {
            return false;
        }
    }
    true
}

fn plan_markers(root: &Path, has_manifest: bool) -> Vec<Change> {
    let mut changes = Vec::new();
    for file in ["CLAUDE.md", "AGENTS.md"] {
        if let Some(text) = fs::read_to_string(root.join(file)).ok()
            && has_block(&text)
        {
            changes.push(change(file, "removed", "relaybook block"));
        }
    }
    for file in [
        ".claude/commands/relaybook.md",
        ".cursor/commands/relaybook.md",
        ".claude/skills/relaybook/SKILL.md",
    ] {
        match fs::read_to_string(root.join(file)) {
            Ok(text) if text.contains(MARKER) => {
                changes.push(change(file, "removed", "relaybook:managed"));
            }
            Ok(_) => changes.push(change(file, "skipped", "no relaybook:managed marker")),
            Err(_) => {}
        }
    }
    match (settings_have_prompt(root), settings_have_session(root)) {
        (true, true) => changes.push(change(
            ".claude/settings.json",
            "removed",
            "UserPromptSubmit hook and SessionStart hook",
        )),
        (true, false) => changes.push(change(
            ".claude/settings.json",
            "removed",
            "UserPromptSubmit hook",
        )),
        (false, true) => changes.push(change(
            ".claude/settings.json",
            "removed",
            "SessionStart hook",
        )),
        (false, false) => {}
    }
    if settings_have_planner_allow(root) {
        note_settings(&mut changes, "safe relaybook commands");
    }
    if settings_have_guard(root) {
        changes.push(change(
            ".claude/settings.json",
            "removed",
            "graphify command guards",
        ));
    }
    if let Some(dir) = hooks_dir(root) {
        for name in ["post-merge", "post-rewrite"] {
            let path = dir.join(name);
            if fs::read_to_string(&path)
                .ok()
                .is_some_and(|text| text.contains("# relaybook:hooks:start"))
            {
                let item = path
                    .strip_prefix(root)
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| name.to_string());
                changes.push(change(item, "removed", "hook block"));
            }
        }
    }
    if let Ok(text) = fs::read_to_string(root.join(".gitignore"))
        && text.lines().any(|line| line.contains("# relaybook"))
    {
        changes.push(change(".gitignore", "removed", "# relaybook lines"));
    }
    if let Ok(text) = fs::read_to_string(root.join(".gitattributes"))
        && text.contains("# relaybook")
    {
        changes.push(change(".gitattributes", "removed", "merge attribute"));
    }
    let workflow = root
        .join(".github")
        .join("workflows")
        .join("graphify-update.yml");
    if fs::read_to_string(&workflow)
        .ok()
        .is_some_and(|text| text == workflow::TEMPLATE)
    {
        changes.push(change(
            ".github/workflows/graphify-update.yml",
            "removed",
            "unchanged template",
        ));
    }
    for key in ["merge.graphify.name", "merge.graphify.driver"] {
        if git_config(root, key).is_some() && !has_manifest {
            changes.push(change(
                format!("git-config:{key}"),
                "removed",
                "graphify merge driver",
            ));
        }
    }
    changes
}

fn apply(root: &Path, manifest: Option<&manifest::Manifest>, graphify: bool) -> Result<(), String> {
    let dirs = manifest
        .map(|manifest| dirs_to_remove(root, manifest))
        .unwrap_or_default();
    if let Some(manifest) = manifest {
        for entry in &manifest.entries {
            restore_entry(root, entry)?;
        }
    }
    clean_markers(root, manifest.is_some())?;
    for dir in dirs {
        let path = root.join(&dir);
        if path.is_dir() && dir_is_empty(&path) {
            fs::remove_dir(&path).map_err(|err| err.to_string())?;
        }
    }
    if graphify && crate::graphify::installed() {
        for args in [
            ["claude", "uninstall"],
            ["cursor", "uninstall"],
            ["hook", "uninstall"],
        ] {
            let _ = crate::graphify::run(root, &args);
        }
    }
    Ok(())
}

fn restore_entry(root: &Path, entry: &Entry) -> Result<(), String> {
    if entry.kind == "directory" {
        return Ok(());
    }
    if entry.kind == "git-config" {
        let key = entry.file.trim_start_matches("git-config:");
        if git_config(root, key).as_deref() != entry.after.as_deref() {
            return Ok(());
        }
        return match &entry.before {
            Some(value) => git_config_set(root, key, value),
            None => git_config_unset(root, key),
        };
    }
    let path = root.join(&entry.file);
    if fs::read_to_string(&path).ok() != entry.after {
        return Ok(());
    }
    match &entry.before {
        Some(text) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|err| err.to_string())?;
            }
            fs::write(path, text).map_err(|err| err.to_string())
        }
        None => remove_file(&path),
    }
}

fn clean_markers(root: &Path, has_manifest: bool) -> Result<(), String> {
    for file in ["CLAUDE.md", "AGENTS.md"] {
        clean_doc(&root.join(file))?;
    }
    for file in [
        ".claude/commands/relaybook.md",
        ".cursor/commands/relaybook.md",
        ".claude/skills/relaybook/SKILL.md",
    ] {
        let path = root.join(file);
        if fs::read_to_string(&path)
            .ok()
            .is_some_and(|text| text.contains(MARKER))
        {
            remove_file(&path)?;
            remove_empty_parents(&path, root)?;
        }
    }
    clean_settings(root)?;
    if let Some(dir) = hooks_dir(root) {
        for name in ["post-merge", "post-rewrite"] {
            clean_hook(&dir.join(name))?;
        }
    }
    clean_ignore(root, has_manifest)?;
    clean_attributes(root, has_manifest)?;
    let workflow = root
        .join(".github")
        .join("workflows")
        .join("graphify-update.yml");
    if fs::read_to_string(&workflow)
        .ok()
        .is_some_and(|text| text == workflow::TEMPLATE)
    {
        remove_file(&workflow)?;
    }
    if !has_manifest {
        for key in ["merge.graphify.name", "merge.graphify.driver"] {
            let expected = if key.ends_with("name") {
                GRAPHIFY_NAME
            } else {
                GRAPHIFY_DRIVER
            };
            if git_config(root, key).as_deref() == Some(expected) {
                git_config_unset(root, key)?;
            }
        }
    }
    Ok(())
}

fn clean_doc(path: &Path) -> Result<(), String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(());
    };
    let Some(next) = strip_block(&text, "<!-- relaybook:start -->", "<!-- relaybook:end -->")
        .or_else(|| strip_block(&text, "<!-- agentmesh:start -->", "<!-- agentmesh:end -->"))
    else {
        return Ok(());
    };
    write_or_remove(path, &next)
}

fn clean_settings(root: &Path) -> Result<(), String> {
    let path = root.join(".claude").join("settings.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let mut value: serde_json::Value =
        serde_json::from_str(&text).map_err(|err| err.to_string())?;
    let mut changed = false;
    changed |= strip_hook_commands(
        &mut value,
        "UserPromptSubmit",
        &[GUARDED_PROMPT, BARE_PROMPT, POWERSHELL_PROMPT],
    );
    changed |= strip_hook_commands(
        &mut value,
        "SessionStart",
        &[GUARDED_SESSION, BARE_SESSION, POWERSHELL_SESSION],
    );
    changed |= strip_planner_allow(&mut value);
    changed |= unguard_all_hooks(&mut value);
    if !changed {
        return Ok(());
    }
    prune_empty(&mut value);
    if value.as_object().is_some_and(|object| object.is_empty()) {
        return remove_file(&path);
    }
    let pretty = serde_json::to_string_pretty(&value).map_err(|err| err.to_string())?;
    fs::write(path, pretty + "\n").map_err(|err| err.to_string())
}

fn clean_hook(path: &Path) -> Result<(), String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(());
    };
    let Some(next) = strip_block(&text, "# relaybook:hooks:start", "# relaybook:hooks:end") else {
        return Ok(());
    };
    let trimmed = next.trim();
    if trimmed.is_empty() || trimmed == "#!/bin/sh" || trimmed == "#!/bin/bash" {
        return remove_file(path);
    }
    fs::write(path, next).map_err(|err| err.to_string())
}

fn clean_ignore(root: &Path, _has_manifest: bool) -> Result<(), String> {
    let path = root.join(".gitignore");
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let next = crate::managed::strip_ignore(&text);
    if next == text {
        return Ok(());
    }
    write_or_remove(&path, &next)
}

fn clean_attributes(root: &Path, _has_manifest: bool) -> Result<(), String> {
    let path = root.join(".gitattributes");
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let next = crate::managed::strip_attributes(&text);
    if next == text {
        return Ok(());
    }
    write_or_remove(&path, &next)
}

fn strip_block(text: &str, start: &str, end: &str) -> Option<String> {
    let start_at = text.find(start)?;
    let end_at = text[start_at..].find(end)? + start_at + end.len();
    let mut end_at = end_at;
    if text[end_at..].starts_with('\n') {
        end_at += 1;
    }
    let mut from = start_at;
    if from >= 2 && text.as_bytes()[from - 1] == b'\n' && text.as_bytes()[from - 2] == b'\n' {
        from -= 1;
    }
    let mut next = String::new();
    next.push_str(&text[..from]);
    next.push_str(&text[end_at..]);
    Some(next)
}

fn write_or_remove(path: &Path, text: &str) -> Result<(), String> {
    if text.is_empty() {
        remove_file(path)
    } else {
        fs::write(path, text).map_err(|err| err.to_string())
    }
}

fn remove_file(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_file(path).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn dir_is_empty(path: &Path) -> bool {
    fs::read_dir(path)
        .ok()
        .is_some_and(|mut dir| dir.next().is_none())
}

fn note_settings(changes: &mut Vec<Change>, detail: &str) {
    if let Some(existing) = changes
        .iter_mut()
        .find(|item| item.item == ".claude/settings.json" && item.action == "removed")
    {
        if !existing.detail.contains(detail) {
            if !existing.detail.is_empty() {
                existing.detail.push_str("; ");
            }
            existing.detail.push_str(detail);
        }
        return;
    }
    changes.push(change(".claude/settings.json", "removed", detail));
}

fn settings_have_planner_allow(root: &Path) -> bool {
    fs::read_to_string(root.join(".claude").join("settings.json"))
        .ok()
        .is_some_and(|text| PLANNER_ALLOW.iter().any(|rule| text.contains(rule)))
}

fn strip_planner_allow(value: &mut serde_json::Value) -> bool {
    let kept = {
        let Some(allow) = value
            .pointer("/permissions/allow")
            .and_then(|item| item.as_array())
        else {
            return false;
        };
        let next: Vec<serde_json::Value> = allow
            .iter()
            .filter(|item| {
                item.as_str()
                    .is_none_or(|text| !PLANNER_ALLOW.contains(&text))
            })
            .cloned()
            .collect();
        if next.len() == allow.len() {
            return false;
        }
        next
    };
    if kept.is_empty() {
        let Some(root) = value.as_object_mut() else {
            return false;
        };
        if let Some(permissions) = root
            .get_mut("permissions")
            .and_then(|item| item.as_object_mut())
        {
            permissions.remove("allow");
            if permissions.is_empty() {
                root.remove("permissions");
            }
        }
        return true;
    }
    if let Some(allow) = value
        .pointer_mut("/permissions/allow")
        .and_then(|item| item.as_array_mut())
    {
        *allow = kept;
    }
    true
}

fn strip_hook_commands(value: &mut serde_json::Value, event: &str, commands: &[&str]) -> bool {
    let Some(groups) = value
        .pointer_mut(&format!("/hooks/{event}"))
        .and_then(|value| value.as_array_mut())
    else {
        return false;
    };
    let before = groups.len();
    groups.retain(|group| {
        !commands
            .iter()
            .any(|command| group_has_command(group, command))
    });
    groups.len() != before
}

fn remove_empty_parents(path: &Path, root: &Path) -> Result<(), String> {
    let mut dir = path.parent();
    while let Some(current) = dir {
        if current == root || !dir_is_empty(current) {
            break;
        }
        fs::remove_dir(current).map_err(|err| err.to_string())?;
        dir = current.parent();
    }
    Ok(())
}

fn settings_have_session(root: &Path) -> bool {
    fs::read_to_string(root.join(".claude").join("settings.json"))
        .ok()
        .is_some_and(|text| {
            text.contains(GUARDED_SESSION)
                || text.contains(BARE_SESSION)
                || text.contains(POWERSHELL_SESSION)
        })
}

fn settings_have_prompt(root: &Path) -> bool {
    fs::read_to_string(root.join(".claude").join("settings.json"))
        .ok()
        .is_some_and(|text| {
            text.contains(GUARDED_PROMPT)
                || text.contains(BARE_PROMPT)
                || text.contains(POWERSHELL_PROMPT)
        })
}

fn settings_have_guard(root: &Path) -> bool {
    fs::read_to_string(root.join(".claude").join("settings.json"))
        .ok()
        .is_some_and(|text| text.contains(GUARD_PREFIX) || text.contains(POWERSHELL_GUARD_PREFIX))
}

fn unguard_all_hooks(value: &mut serde_json::Value) -> bool {
    let Some(hooks) = value.get_mut("hooks").and_then(|item| item.as_object_mut()) else {
        return false;
    };
    let mut changed = false;
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups {
            changed |= unguard_group(group);
        }
    }
    changed
}

fn group_has_command(group: &serde_json::Value, command: &str) -> bool {
    group
        .pointer("/hooks/0/command")
        .and_then(|value| value.as_str())
        == Some(command)
        && group
            .pointer("/hooks")
            .and_then(|value| value.as_array())
            .is_some_and(|hooks| hooks.len() == 1)
}

fn unguard_command(command: &str) -> Option<String> {
    if let Some(rest) = command.strip_prefix(GUARD_PREFIX) {
        let inner = rest.strip_suffix(" || true")?.trim();
        return Some(format!("graphify {inner}"));
    }
    let rest = command.strip_prefix(POWERSHELL_GUARD_PREFIX)?;
    let inner = rest.trim().trim_end_matches('}').trim();
    if inner.starts_with("graphify ") || inner == "graphify" {
        return Some(inner.to_string());
    }
    None
}

fn unguard_group(group: &mut serde_json::Value) -> bool {
    let Some(hooks) = group
        .get_mut("hooks")
        .and_then(|value| value.as_array_mut())
    else {
        return false;
    };
    let mut changed = false;
    for hook in hooks {
        let Some(command) = hook
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::to_string)
        else {
            continue;
        };
        let Some(restored) = unguard_command(&command) else {
            continue;
        };
        if let Some(object) = hook.as_object_mut() {
            if command.starts_with(POWERSHELL_GUARD_PREFIX) {
                object.remove("shell");
            }
            object.insert("command".to_string(), serde_json::Value::String(restored));
            changed = true;
        }
    }
    changed
}

fn prune_empty(value: &mut serde_json::Value) {
    let Some(root) = value.as_object_mut() else {
        return;
    };
    let Some(hooks) = root
        .get_mut("hooks")
        .and_then(|value| value.as_object_mut())
    else {
        return;
    };
    hooks.retain(|_, child| match child {
        serde_json::Value::Array(items) => !items.is_empty(),
        serde_json::Value::Object(map) => !map.is_empty(),
        _ => true,
    });
    if hooks.is_empty() {
        root.remove("hooks");
    }
}

fn has_block(text: &str) -> bool {
    (text.contains("<!-- relaybook:start -->") && text.contains("<!-- relaybook:end -->"))
        || (text.contains("<!-- agentmesh:start -->") && text.contains("<!-- agentmesh:end -->"))
}

fn active_run(root: &Path) -> Result<bool, String> {
    let Some(path) = paths::ledger_path(root) else {
        return Ok(false);
    };
    let ledger = relaybook_core::Ledger::open(&path).map_err(|err| err.to_string())?;
    Ok(ledger
        .active_run()
        .map_err(|err| err.to_string())?
        .is_some())
}

fn confirm(prompt: &str) -> Result<bool, String> {
    if !io::stdin().is_terminal() {
        println!("{prompt} re-run with --yes");
        return Ok(false);
    }
    print!("{prompt} [y/N] ");
    io::stdout().flush().map_err(|err| err.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|err| err.to_string())?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

fn change(item: impl Into<String>, action: &str, detail: &str) -> Change {
    Change {
        item: item.into(),
        action: action.to_string(),
        detail: detail.to_string(),
    }
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

fn git_config_set(root: &Path, key: &str, value: &str) -> Result<(), String> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", key, value])
        .status()
        .map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git config {key} failed"))
    }
}

fn git_config_unset(root: &Path, key: &str) -> Result<(), String> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--unset", key])
        .status()
        .map_err(|err| err.to_string())?;
    if status.success() || git_config(root, key).is_none() {
        Ok(())
    } else {
        Err(format!("git config --unset {key} failed"))
    }
}
