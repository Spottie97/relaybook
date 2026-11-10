use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value};

const PROMPT_COMMAND: &str =
    "command -v relaybook >/dev/null 2>&1 && relaybook hook prompt || true";

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
    if !guard_pretooluse(&mut value)? {
        return Ok(false);
    }
    let text = serde_json::to_string_pretty(&value).map_err(|err| invalid(err.to_string()))?;
    fs::write(&path, text + "\n")?;
    Ok(true)
}

fn guard_pretooluse(value: &mut Value) -> io::Result<bool> {
    let Some(hooks) = value.get_mut("hooks") else {
        return Ok(false);
    };
    let Some(hooks) = hooks.as_object_mut() else {
        return Err(invalid("hooks must be a JSON object"));
    };
    let Some(pre_tool) = hooks.get_mut("PreToolUse") else {
        return Ok(false);
    };
    let Some(groups) = pre_tool.as_array_mut() else {
        return Err(invalid("PreToolUse must be a JSON array"));
    };
    let mut changed = false;
    for group in groups {
        changed |= guard_group(group);
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
            object.insert("command".to_string(), Value::String(guarded));
            changed = true;
        }
    }
    changed
}

fn guard_command(command: &str) -> Option<String> {
    let trimmed = command.trim();
    if trimmed.starts_with("command -v graphify ") && trimmed.ends_with("|| true") {
        return None;
    }
    let rest = trimmed[trimmed.find("hook-guard")?..].trim();
    if rest.is_empty() {
        return None;
    }
    Some(format!(
        "command -v graphify >/dev/null 2>&1 && graphify {rest} || true"
    ))
}

pub(crate) fn install(root: &Path) -> io::Result<Status> {
    let path = root.join(".claude").join("settings.json");
    let mut value = read_settings(&path)?;
    let Some(root_object) = value.as_object_mut() else {
        return Err(invalid("settings.json must be a JSON object"));
    };
    let hooks = object_entry(root_object, "hooks")?;
    let prompt = array_entry(hooks, "UserPromptSubmit")?;
    if contains_command(prompt, PROMPT_COMMAND) {
        return Ok(Status::Unchanged);
    }
    prompt
        .as_array_mut()
        .expect("UserPromptSubmit is an array")
        .push(hook_group());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(&value).map_err(|err| invalid(err.to_string()))?;
    fs::write(&path, text + "\n")?;
    Ok(Status::Updated)
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

fn hook_group() -> Value {
    serde_json::json!({
        "hooks": [
            {
                "type": "command",
                "command": PROMPT_COMMAND
            }
        ]
    })
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
