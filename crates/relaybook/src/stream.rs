//! Live agent events.
//!
//! Claude Code (`claude -p --output-format stream-json --verbose`) writes one
//! JSON object per line. `--verbose` is required for `stream-json`.
//! <https://code.claude.com/docs/en/cli-reference>
//! Assistant messages carry `message.content[]` blocks of `text` or `tool_use`
//! (`name`, `input.file_path` / `input.command`). The final `result` event has
//! `session_id` and `total_cost_usd`.
//!
//! Cursor CLI (`agent -p --output-format stream-json --stream-partial-output`)
//! also writes one JSON object per line.
//! <https://cursor.com/docs/cli/reference/output-format>
//! `--stream-partial-output` adds text deltas. Assistant events with
//! `model_call_id` repeat text that already streamed, so they are skipped.
//! Tool calls use `type: tool_call` and `subtype: started`, with a key such as
//! `readToolCall` / `writeToolCall` under `tool_call.args`.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Piece {
    Tool(String),
    Text(String),
}

pub(crate) fn pieces(value: &Value) -> Vec<Piece> {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "assistant" => {
            if value.get("model_call_id").is_some() {
                return Vec::new();
            }
            assistant_pieces(value)
        }
        "tool_call" => {
            if value.get("subtype").and_then(Value::as_str) == Some("completed") {
                return Vec::new();
            }
            cursor_tool(value).into_iter().collect()
        }
        _ => Vec::new(),
    }
}

fn assistant_pieces(value: &Value) -> Vec<Piece> {
    let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for block in blocks {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str)
                    && let Some(line) = text.lines().find(|line| !line.trim().is_empty())
                {
                    found.push(Piece::Text(line.trim().to_string()));
                }
            }
            "tool_use" => {
                if let Some(label) = claude_tool(block) {
                    found.push(Piece::Tool(label));
                }
            }
            _ => {}
        }
    }
    found
}

fn claude_tool(block: &Value) -> Option<String> {
    let name = block.get("name").and_then(Value::as_str)?;
    let input = block.get("input");
    let detail = match name {
        "Bash" | "bash" => input.and_then(|item| field(item, &["command"])),
        _ => input.and_then(|item| field(item, &["file_path", "path", "pattern", "target_file"])),
    };
    Some(join_label(&display_name(name), detail.as_deref()))
}

fn cursor_tool(value: &Value) -> Option<Piece> {
    let call = value.get("tool_call")?;
    let (key, body) = call.as_object()?.iter().next()?;
    let args = body.get("args").unwrap_or(body);
    let detail = field(
        args,
        &["path", "file_path", "target_file", "filePath", "command"],
    );
    Some(Piece::Tool(join_label(
        &cursor_name(key),
        detail.as_deref(),
    )))
}

fn cursor_name(key: &str) -> String {
    let bare = key.strip_suffix("ToolCall").unwrap_or(key);
    match bare {
        "read" | "Read" => "Read".to_string(),
        "write" | "edit" | "strReplace" | "Write" | "Edit" => "Edit".to_string(),
        "shell" | "bash" | "runTerminalCmd" | "Bash" => "Bash".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => "Tool".to_string(),
            }
        }
    }
}

fn display_name(name: &str) -> String {
    match name {
        "bash" => "Bash".to_string(),
        other => other.to_string(),
    }
}

fn field(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str)
            && !text.is_empty()
        {
            let one = text.lines().next().unwrap_or(text).trim();
            if !one.is_empty() {
                return Some(one.to_string());
            }
        }
    }
    None
}

fn join_label(name: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) => format!("{name} {detail}"),
        None => name.to_string(),
    }
}

pub(crate) fn read_lines<R: std::io::BufRead>(reader: R, mut on_line: impl FnMut(&str)) {
    let mut reader = reader;
    let mut buffer = String::new();
    while reader
        .read_line(&mut buffer)
        .ok()
        .is_some_and(|count| count > 0)
    {
        if buffer.ends_with('\n') {
            buffer.pop();
            if buffer.ends_with('\r') {
                buffer.pop();
            }
        }
        on_line(&buffer);
        buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{Piece, pieces, read_lines};
    use serde_json::json;
    use std::io::Write;
    use std::time::{Duration, Instant};

    #[test]
    fn claude_text_and_tools_render_once_each() {
        let text = json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": "Planning the next step.\nmore"}]}
        });
        assert_eq!(
            pieces(&text),
            vec![Piece::Text("Planning the next step.".to_string())]
        );
        let read = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "Read", "input": {"file_path": "src/x.rs"}}]}
        });
        assert_eq!(
            pieces(&read),
            vec![Piece::Tool("Read src/x.rs".to_string())]
        );
        let edit = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "Edit", "input": {"file_path": "src/y.rs"}}]}
        });
        assert_eq!(
            pieces(&edit),
            vec![Piece::Tool("Edit src/y.rs".to_string())]
        );
        let bash = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "Bash", "input": {"command": "cargo test"}}]}
        });
        assert_eq!(
            pieces(&bash),
            vec![Piece::Tool("Bash cargo test".to_string())]
        );
    }

    #[test]
    fn cursor_duplicates_are_skipped_and_tools_are_named() {
        let duplicate = json!({
            "type": "assistant",
            "model_call_id": "call-1",
            "timestamp_ms": 1,
            "message": {"content": [{"type": "text", "text": "again"}]}
        });
        assert!(pieces(&duplicate).is_empty());
        let tool = json!({
            "type": "tool_call",
            "subtype": "started",
            "tool_call": {"readToolCall": {"args": {"path": "src/x.rs"}}}
        });
        assert_eq!(
            pieces(&tool),
            vec![Piece::Tool("Read src/x.rs".to_string())]
        );
        let done =
            json!({"type": "tool_call", "subtype": "completed", "tool_call": {"readToolCall": {}}});
        assert!(pieces(&done).is_empty());
    }

    #[test]
    fn a_large_stream_is_read_while_it_is_written() {
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        let writer_done = std::thread::spawn(move || {
            let line = b"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"x\"}]}}\n";
            for _ in 0..40_000 {
                writer.write_all(line).expect("write");
            }
        });
        let started = Instant::now();
        let mut count = 0_usize;
        read_lines(std::io::BufReader::new(reader), |_| count += 1);
        writer_done.join().expect("writer");
        assert_eq!(count, 40_000);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
