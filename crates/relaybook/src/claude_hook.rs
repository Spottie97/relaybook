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
