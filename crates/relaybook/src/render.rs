use std::fmt::Write as _;

use relaybook_core::{Context, Task};

pub(crate) fn tasks(tasks: &[Task]) -> String {
    if tasks.is_empty() {
        return "no tasks\n".to_string();
    }
    let mut out = String::new();
    for task in tasks {
        let by = task.claimed_by.as_deref().unwrap_or("-");
        let _ = writeln!(
            out,
            "{:<8} {:<24} {:<16} {}{}",
            task.status,
            task.slug,
            by,
            task.title,
            needs_suffix(task)
        );
    }
    out
}

pub(crate) fn context(ctx: &Context, compact: bool, summarize_events: bool) -> String {
    let mut out = String::new();
    if ctx.tasks.is_empty() {
        out.push_str("no tasks\n");
    }
    for detail in &ctx.tasks {
        write_task(&mut out, detail, compact);
    }
    if compact && ctx.hidden_done > 0 {
        let _ = writeln!(
            out,
            "{} done tasks hidden (relaybook context --all)",
            ctx.hidden_done
        );
    }
    if summarize_events {
        for event in &ctx.events {
            let _ = writeln!(out, "  {}", event_summary(event));
        }
    } else if !ctx.events.is_empty() {
        out.push_str("events:\n");
        for event in &ctx.events {
            let _ = writeln!(out, "  {} {} {}", event.id, event.kind, event.payload);
        }
    } else if compact {
        let _ = writeln!(out, "events omitted (relaybook context --events 20)");
    }
    out
}

fn write_task(out: &mut String, detail: &relaybook_core::TaskDetail, compact: bool) {
    let task = &detail.task;
    let by = match &task.claimed_by {
        Some(agent) => format!(" by {agent}"),
        None => String::new(),
    };
    let _ = writeln!(
        out,
        "{} [{}] {}{by}{}",
        task.slug,
        task.status,
        task.title,
        needs_suffix(task)
    );
    if compact {
        write_compact_steps(out, detail);
        write_latest_decision(out, detail);
        return;
    }
    for step in &detail.steps {
        let mark = if step.done { "x" } else { " " };
        let _ = writeln!(out, "  {}. [{mark}] {}", step.position, step.description);
    }
    for decision in &detail.decisions {
        let _ = writeln!(
            out,
            "  decision by {}: {}",
            decision.provenance, decision.rationale
        );
    }
}

fn write_compact_steps(out: &mut String, detail: &relaybook_core::TaskDetail) {
    let done = detail.steps.iter().filter(|step| step.done).count();
    if done > 0 {
        let _ = writeln!(
            out,
            "  {done}/{} done (relaybook context {})",
            detail.steps.len(),
            detail.task.slug
        );
    }
    for step in detail.steps.iter().filter(|step| !step.done) {
        let _ = writeln!(out, "  {}. [ ] {}", step.position, step.description);
    }
}

fn write_latest_decision(out: &mut String, detail: &relaybook_core::TaskDetail) {
    let Some(decision) = detail.decisions.last() else {
        return;
    };
    let mut text: String = decision.rationale.chars().take(200).collect();
    let trimmed = decision.rationale.chars().count() > 200;
    if trimmed {
        text.push('…');
    }
    if trimmed || detail.decisions.len() > 1 {
        let _ = writeln!(
            out,
            "  decision by {}: {text} (relaybook context {})",
            decision.provenance, detail.task.slug
        );
    } else {
        let _ = writeln!(out, "  decision by {}: {text}", decision.provenance);
    }
}

fn event_summary(event: &relaybook_core::Event) -> String {
    let slug = event
        .payload
        .get("slug")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-");
    let extra = ["note", "to", "title", "why", "on", "action", "by"]
        .iter()
        .find_map(|key| {
            event
                .payload
                .get(*key)
                .and_then(serde_json::Value::as_str)
                .map(|value| {
                    let short: String = value.chars().take(80).collect();
                    format!("{key} {short}")
                })
        });
    match extra {
        Some(extra) => format!("{} {slug} {extra}", event.kind),
        None => format!("{} {slug}", event.kind),
    }
}

pub(crate) fn line(text: &str) -> String {
    format!("{text}\n")
}

fn needs_suffix(task: &Task) -> String {
    if task.needs.is_empty() {
        String::new()
    } else {
        format!(" needs: {}", task.needs.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::{context, line, tasks};
    use relaybook_core::{Context, Decision, Event, EventKind, Status, Step, Task, TaskDetail};

    fn task(status: Status, claimed_by: Option<&str>) -> Task {
        Task {
            id: 1,
            slug: "auth".to_string(),
            title: "Add auth".to_string(),
            status,
            claimed_by: claimed_by.map(str::to_string),
            created_at: "t".to_string(),
            updated_at: "t".to_string(),
            depends_on: Vec::new(),
            needs: Vec::new(),
        }
    }

    #[test]
    fn empty_task_list_says_so() {
        assert_eq!(tasks(&[]), "no tasks\n");
        assert_eq!(line("done auth"), "done auth\n");
    }

    #[test]
    fn task_line_shows_claimant_or_a_dash() {
        let open = tasks(&[task(Status::Open, None)]);
        assert!(open.contains("open"));
        assert!(open.contains("auth"));
        assert!(open.contains('-'));
        let claimed = tasks(&[task(Status::Claimed, Some("cursor"))]);
        assert!(claimed.contains("cursor"));
    }

    #[test]
    fn context_shows_steps_decisions_and_events() {
        let rendered = context(
            &Context {
                tasks: vec![TaskDetail {
                    task: task(Status::Claimed, Some("cursor")),
                    steps: vec![Step {
                        id: 1,
                        task_id: 1,
                        position: 1,
                        description: "hash passwords".to_string(),
                        done: true,
                    }],
                    decisions: vec![Decision {
                        id: 1,
                        task_id: 1,
                        rationale: "use argon2".to_string(),
                        provenance: "claude-code".to_string(),
                        created_at: "t".to_string(),
                    }],
                }],
                events: vec![Event {
                    id: 4,
                    ts: "t".to_string(),
                    kind: EventKind::Claimed,
                    payload: serde_json::json!({"slug": "auth"}),
                }],
                hidden_done: 0,
            },
            false,
            false,
        );
        assert!(rendered.contains("auth [claimed] Add auth by cursor"));
        assert!(rendered.contains("1. [x] hash passwords"));
        assert!(rendered.contains("decision by claude-code: use argon2"));
        assert!(rendered.contains("events:"));
        assert!(rendered.contains("4 claimed"));
    }

    #[test]
    fn compact_cuts_name_their_recovery_command() {
        let rendered = context(
            &Context {
                tasks: vec![TaskDetail {
                    task: task(Status::Open, None),
                    steps: vec![
                        Step {
                            id: 1,
                            task_id: 1,
                            position: 1,
                            description: "done step".to_string(),
                            done: true,
                        },
                        Step {
                            id: 2,
                            task_id: 1,
                            position: 2,
                            description: "open step".to_string(),
                            done: false,
                        },
                    ],
                    decisions: vec![
                        Decision {
                            id: 1,
                            task_id: 1,
                            rationale: "old".to_string(),
                            provenance: "claude-code".to_string(),
                            created_at: "t".to_string(),
                        },
                        Decision {
                            id: 2,
                            task_id: 1,
                            rationale: "latest".to_string(),
                            provenance: "cursor".to_string(),
                            created_at: "t".to_string(),
                        },
                    ],
                }],
                events: Vec::new(),
                hidden_done: 2,
            },
            true,
            false,
        );
        for line in rendered.lines() {
            let cut = line.contains("done (relaybook")
                || line.contains("hidden")
                || line.contains("omitted")
                || line.contains("decision by");
            if cut {
                assert!(
                    line.contains("relaybook context"),
                    "elision missing a command: {line}"
                );
            }
        }
        assert!(rendered.contains("2. [ ] open step"));
        assert!(!rendered.contains("done step"));
    }
}
