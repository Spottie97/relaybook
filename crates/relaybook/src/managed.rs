use std::collections::HashSet;

pub(crate) const START: &str = "# relaybook:start";
pub(crate) const END: &str = "# relaybook:end";
const MARK: &str = "# relaybook";

pub(crate) const IGNORE_ENTRIES: &[&str] = &[
    ".relaybook/*.db-wal",
    ".relaybook/*.db-shm",
    ".claude/*.graphify-bak",
    "graphify-out/",
    ".relaybook/runs/",
];

pub(crate) const ATTRIBUTE_ENTRY: &str = "graphify-out/graph.json merge=graphify";

pub(crate) fn rewrite_ignore(original: &str, track_graphify_out: bool) -> String {
    let drop: &[&str] = if track_graphify_out {
        &["graphify-out/"]
    } else {
        &[]
    };
    let managed: Vec<&str> = IGNORE_ENTRIES
        .iter()
        .copied()
        .filter(|entry| !drop.contains(entry))
        .collect();
    rewrite_marked(original, &managed, drop)
}

pub(crate) fn rewrite_attributes(original: &str) -> String {
    rewrite_marked(original, &[ATTRIBUTE_ENTRY], &[])
}

pub(crate) fn repair_attributes(original: &str) -> String {
    if !attribute_needs_repair(original) {
        return original.to_string();
    }
    rewrite_attributes(original)
}

fn attribute_needs_repair(text: &str) -> bool {
    let trailing = format!("{ATTRIBUTE_ENTRY} # relaybook");
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    lines.iter().enumerate().any(|(index, line)| {
        *line == trailing
            || (*line == MARK && lines.get(index + 1).copied() == Some(ATTRIBUTE_ENTRY))
    })
}

pub(crate) fn strip_ignore(original: &str) -> String {
    strip_marked(original, IGNORE_ENTRIES)
}

pub(crate) fn strip_attributes(original: &str) -> String {
    strip_marked(original, &[ATTRIBUTE_ENTRY])
}

fn rewrite_marked(original: &str, managed: &[&str], drop: &[&str]) -> String {
    let lines: Vec<&str> = original.lines().collect();
    let outside = entries_outside(&lines, managed, drop);
    let missing: Vec<&str> = managed
        .iter()
        .copied()
        .filter(|entry| !outside.contains(entry))
        .collect();
    let mut kept = Vec::new();
    let mut emitted = false;
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed == START {
            if !emitted && !missing.is_empty() {
                push_block(&mut kept, &missing);
                emitted = true;
            }
            index += 1;
            while index < lines.len() && lines[index].trim() != END {
                let inner = lines[index].trim();
                if !inner.is_empty() && !managed.contains(&inner) && !drop.contains(&inner) {
                    kept.push(lines[index].to_string());
                }
                index += 1;
            }
            if index < lines.len() {
                index += 1;
            }
            continue;
        }
        if consume_marked_pair(&lines, index, managed, drop).is_some() {
            index += 2;
            continue;
        }
        if is_trailing(trimmed, managed) || is_trailing(trimmed, drop) || drop.contains(&trimmed) {
            index += 1;
            continue;
        }
        kept.push(lines[index].to_string());
        index += 1;
    }
    if !emitted && !missing.is_empty() {
        push_block(&mut kept, &missing);
    }
    finish(original, kept)
}

fn strip_marked(original: &str, managed: &[&str]) -> String {
    let lines: Vec<&str> = original.lines().collect();
    let mut kept = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed == START {
            index += 1;
            while index < lines.len() && lines[index].trim() != END {
                let inner = lines[index].trim();
                if !inner.is_empty() && !managed.contains(&inner) {
                    kept.push(lines[index].to_string());
                }
                index += 1;
            }
            if index < lines.len() {
                index += 1;
            }
            continue;
        }
        if consume_marked_pair(&lines, index, managed, &[]).is_some() {
            index += 2;
            continue;
        }
        if is_trailing(trimmed, managed) {
            index += 1;
            continue;
        }
        kept.push(lines[index].to_string());
        index += 1;
    }
    finish(original, kept)
}

fn entries_outside<'a>(lines: &[&'a str], managed: &[&str], drop: &[&str]) -> HashSet<&'a str> {
    let mut outside = HashSet::new();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed == START {
            index += 1;
            while index < lines.len() && lines[index].trim() != END {
                index += 1;
            }
            if index < lines.len() {
                index += 1;
            }
            continue;
        }
        if consume_marked_pair(lines, index, managed, drop).is_some() {
            index += 2;
            continue;
        }
        if is_trailing(trimmed, managed) || is_trailing(trimmed, drop) || drop.contains(&trimmed) {
            index += 1;
            continue;
        }
        if managed.contains(&trimmed) {
            outside.insert(trimmed);
        }
        index += 1;
    }
    outside
}

fn consume_marked_pair<'a>(
    lines: &[&'a str],
    index: usize,
    managed: &[&str],
    drop: &[&str],
) -> Option<&'a str> {
    if lines[index].trim() != MARK || index + 1 >= lines.len() {
        return None;
    }
    let next = lines[index + 1].trim();
    if managed.contains(&next) || drop.contains(&next) {
        Some(next)
    } else {
        None
    }
}

fn is_trailing(trimmed: &str, entries: &[&str]) -> bool {
    entries
        .iter()
        .any(|entry| trimmed == format!("{entry} # relaybook"))
}

fn push_block(kept: &mut Vec<String>, entries: &[&str]) {
    kept.push(START.to_string());
    for entry in entries {
        kept.push((*entry).to_string());
    }
    kept.push(END.to_string());
}

fn finish(original: &str, kept: Vec<String>) -> String {
    let next = if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    };
    if next == original {
        original.to_string()
    } else {
        next
    }
}
