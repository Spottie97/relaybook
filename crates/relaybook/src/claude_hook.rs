use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value};

pub(crate) const GUARDED_PROMPT: &str =
    "command -v relaybook >/dev/null 2>&1 && relaybook hook prompt || true";
pub(crate) const BARE_PROMPT: &str = "relaybook hook prompt";
pub(crate) const POWERSHELL_PROMPT: &str =
    "if (Get-Command relaybook -ErrorAction SilentlyContinue) { relaybook hook prompt }";
pub(crate) const GUARDED_SESSION: &str =
    "command -v relaybook >/dev/null 2>&1 && relaybook hook session || true";
pub(crate) const BARE_SESSION: &str = "relaybook hook session";
pub(crate) const POWERSHELL_SESSION: &str =
    "if (Get-Command relaybook -ErrorAction SilentlyContinue) { relaybook hook session }";

/// Read and planning commands. Claim, done, run, and uninstall stay prompted.
pub(crate) const PLANNER_ALLOW: &[&str] = &[
    "Bash(relaybook context:*)",
    "Bash(relaybook inbox:*)",
    "Bash(relaybook guide:*)",
    "Bash(relaybook status:*)",
    "Bash(relaybook task create:*)",
    "Bash(relaybook task list:*)",
    "Bash(relaybook task reopen:*)",
    "Bash(relaybook task depend:*)",
    "Bash(relaybook step add:*)",
    "Bash(relaybook step edit:*)",
    "Bash(relaybook decision log:*)",
    "Bash(relaybook report accept:*)",
    "Bash(relaybook report note:*)",
];
const POWERSHELL_SHELL: &str = "powershell";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Updated,
    Unchanged,
}

pub(crate) fn guard_graphify(root: &Path) -> io::Result<bool> {
    let path = root.join(".claude").join("settings.json");
    if !path.exists() {
        return Ok(false);
    }
    let mut value = read_settings(&path)?;
    if !guard_all_hooks(&mut value)? {
        return Ok(false);
    }
    let text = serde_json::to_string_pretty(&value).map_err(|err| invalid(err.to_string()))?;
    fs::write(&path, text + "\n")?;
    Ok(true)
}

pub(crate) fn home_path_hook_commands(root: &Path) -> io::Result<Vec<String>> {
    let path = root.join(".claude").join("settings.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let value = read_settings(&path)?;
    Ok(collect_hook_commands(&value)
        .into_iter()
        .filter(|command| is_home_path(command))
        .collect())
}

fn guard_all_hooks(value: &mut Value) -> io::Result<bool> {
    let Some(hooks) = value.get_mut("hooks") else {
        return Ok(false);
    };
    let Some(hooks) = hooks.as_object_mut() else {
        return Err(invalid("hooks must be a JSON object"));
    };
    let mut changed = false;
    for (event, groups) in hooks {
        let Some(groups) = groups.as_array_mut() else {
            return Err(invalid(format!("{event} must be a JSON array")));
        };
        for group in groups {
            changed |= guard_group(group);
        }
    }
    Ok(changed)
}

fn guard_group(group: &mut Value) -> bool {
    let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    for hook in hooks {
        let Some(command) = hook
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(guarded) = guard_command(&command) else {
            continue;
        };
        if let Some(object) = hook.as_object_mut() {
            if guarded.starts_with("if (Get-Command graphify ") {
                object.insert(
                    "shell".to_string(),
                    Value::String(POWERSHELL_SHELL.to_string()),
                );
            }
            object.insert("command".to_string(), Value::String(guarded));
            changed = true;
        }
    }
    changed
}

fn guard_command(command: &str) -> Option<String> {
    let trimmed = command.trim();
    let args = graphify_hook_args(trimmed)?;
    let desired = if use_powershell() {
        format!("if (Get-Command graphify -ErrorAction SilentlyContinue) {{ graphify {args} }}")
    } else {
        format!("command -v graphify >/dev/null 2>&1 && graphify {args} || true")
    };
    if trimmed == desired {
        return None;
    }
    Some(desired)
}

fn graphify_hook_args(command: &str) -> Option<String> {
    let index = command.find("hook-guard")?;
    if !command[..index].contains("graphify") {
        return None;
    }
    let mut rest = command[index..].trim();
    if let Some(inner) = rest.strip_suffix("|| true") {
        rest = inner.trim();
    }
    rest = rest.trim_end_matches('}').trim();
    if !rest.starts_with("hook-guard") {
        return None;
    }
    Some(rest.to_string())
}

fn collect_hook_commands(value: &Value) -> Vec<String> {
    let Some(hooks) = value.get("hooks").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for groups in hooks.values() {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for group in groups {
            let Some(items) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for hook in items {
                if let Some(command) = hook.get("command").and_then(Value::as_str) {
                    found.push(command.to_string());
                }
            }
        }
    }
    found
}

fn is_home_path(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.contains("/users/")
        || lower.contains("/home/")
        || lower.contains("c:/users/")
        || lower.contains("c:\\users\\")
}

pub(crate) struct Installed {
    pub prompt: Status,
    pub session: Status,
}

pub(crate) fn install(root: &Path) -> io::Result<Installed> {
    let path = root.join(".claude").join("settings.json");
    let mut value = read_settings(&path)?;
    let prompt = ensure_hook(
        &mut value,
        "UserPromptSubmit",
        BARE_PROMPT,
        GUARDED_PROMPT,
        POWERSHELL_PROMPT,
    )?;
    let session = ensure_hook(
        &mut value,
        "SessionStart",
        BARE_SESSION,
        GUARDED_SESSION,
        POWERSHELL_SESSION,
    )?;
    if prompt == Status::Updated || session == Status::Updated {
        write_settings(&path, &value)?;
    }
    Ok(Installed { prompt, session })
}

pub(crate) fn install_permissions(root: &Path) -> io::Result<Status> {
    let path = root.join(".claude").join("settings.json");
    let mut value = read_settings(&path)?;
    let Some(root_object) = value.as_object_mut() else {
        return Err(invalid("settings.json must be a JSON object"));
    };
    let permissions = object_entry(root_object, "permissions")?;
    let Some(permissions) = permissions.as_object_mut() else {
        return Err(invalid("permissions must be a JSON object"));
    };
    if let Some(current) = permissions.get("allow")
        && !current.is_array()
    {
        return Err(invalid("permissions.allow must be a JSON array"));
    }
    let allow = permissions
        .entry("allow")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(allow) = allow.as_array_mut() else {
        return Err(invalid("permissions.allow must be a JSON array"));
    };
    let mut changed = false;
    for rule in PLANNER_ALLOW {
        let present = allow.iter().any(|item| item.as_str() == Some(*rule));
        if !present {
            allow.push(Value::String((*rule).to_string()));
            changed = true;
        }
    }
    if !changed {
        return Ok(Status::Unchanged);
    }
    write_settings(&path, &value)?;
    Ok(Status::Updated)
}

fn ensure_hook(
    value: &mut Value,
    event: &str,
    bare: &'static str,
    guarded: &'static str,
    powershell: &'static str,
) -> io::Result<Status> {
    let Some(root_object) = value.as_object_mut() else {
        return Err(invalid("settings.json must be a JSON object"));
    };
    let hooks = object_entry(root_object, "hooks")?;
    let list = array_entry(hooks, event)?;
    let migrated = migrate_bare(list, bare, guarded);
    if contains_command(list, guarded) || contains_command(list, powershell) {
        return Ok(if migrated {
            Status::Updated
        } else {
            Status::Unchanged
        });
    }
    list.as_array_mut()
        .expect("hook event is an array")
        .push(hook_group(desired_command(guarded, powershell)));
    Ok(Status::Updated)
}

fn write_settings(path: &Path, value: &Value) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|err| invalid(err.to_string()))?;
    fs::write(path, text + "\n")
}

fn desired_command(guarded: &'static str, powershell: &'static str) -> &'static str {
    if use_powershell() {
        powershell
    } else {
        guarded
    }
}

fn use_powershell() -> bool {
    #[cfg(windows)]
    {
        !git_bash_on_path()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
fn git_bash_on_path() -> bool {
    which::which("bash").is_ok_and(|path| !is_wsl_stub(&path))
}

#[cfg(windows)]
fn is_wsl_stub(path: &Path) -> bool {
    path.components().any(|component| {
        let text = component.as_os_str().to_string_lossy();
        text.eq_ignore_ascii_case("system32") || text.eq_ignore_ascii_case("sysnative")
    })
}

fn read_settings(path: &Path) -> io::Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|err| invalid(err.to_string()))
}

fn object_entry<'a>(object: &'a mut Map<String, Value>, key: &str) -> io::Result<&'a mut Value> {
    if let Some(current) = object.get(key)
        && !current.is_object()
    {
        return Err(invalid(format!("{key} must be a JSON object")));
    }
    Ok(object
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new())))
}

fn array_entry<'a>(object: &'a mut Value, key: &str) -> io::Result<&'a mut Value> {
    let Some(map) = object.as_object_mut() else {
        return Err(invalid("hooks must be a JSON object"));
    };
    if let Some(current) = map.get(key)
        && !current.is_array()
    {
        return Err(invalid(format!("{key} must be a JSON array")));
    }
    Ok(map.entry(key).or_insert_with(|| Value::Array(Vec::new())))
}

fn hook_group(command: &str) -> Value {
    if command == POWERSHELL_PROMPT || command == POWERSHELL_SESSION {
        serde_json::json!({
            "hooks": [
                {
                    "type": "command",
                    "command": command,
                    "shell": POWERSHELL_SHELL
                }
            ]
        })
    } else {
        serde_json::json!({
            "hooks": [
                {
                    "type": "command",
                    "command": command
                }
            ]
        })
    }
}

fn migrate_bare(value: &mut Value, bare: &str, guarded: &str) -> bool {
    match value {
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= migrate_bare(item, bare, guarded);
            }
            changed
        }
        Value::Object(map) => {
            let migrated = map.get("command").and_then(Value::as_str) == Some(bare);
            if migrated {
                map.insert("command".to_string(), Value::String(guarded.to_string()));
                map.remove("shell");
            }
            let mut changed = migrated;
            for child in map.values_mut() {
                changed |= migrate_bare(child, bare, guarded);
            }
            changed
        }
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => false,
    }
}

fn contains_command(value: &Value, command: &str) -> bool {
    match value {
        Value::Array(items) => items.iter().any(|item| contains_command(item, command)),
        Value::Object(map) => map.iter().any(|(key, child)| {
            (key == "command" && child.as_str() == Some(command))
                || contains_command(child, command)
        }),
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => false,
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::{graphify_hook_args, guard_command, is_home_path};

    #[test]
    fn absolute_graphify_paths_become_a_guard_without_a_path() {
        let unix =
            guard_command("/Users/ada/.local/bin/graphify hook-guard compact").expect("unix");
        assert_eq!(
            unix,
            "command -v graphify >/dev/null 2>&1 && graphify hook-guard compact || true"
        );
        assert!(!is_home_path(&unix));
        let windows =
            guard_command(r"C:\Users\ada\.local\bin\graphify.exe hook-guard startup").expect("win");
        assert!(windows.contains("graphify hook-guard startup"));
        assert!(!windows.contains('\\'));
        assert!(graphify_hook_args("echo hook-guard").is_none());
        assert!(guard_command(&unix).is_none());
    }
}
