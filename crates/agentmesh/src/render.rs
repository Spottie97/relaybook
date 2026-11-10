use std::fmt::Write as _;

use agentmesh_core::{Context, Task};

pub(crate) fn tasks(tasks: &[Task]) -> String {
    if tasks.is_empty() {
        return "no tasks\n".to_string();
    }
    let mut out = String::new();
    for task in tasks {
        let by = task.claimed_by.as_deref().unwrap_or("-");
        let _ = writeln!(
            out,
            "{:<8} {:<24} {:<16} {}",
            task.status, task.slug, by, task.title
        );
    }
    out
}

pub(crate) fn context(ctx: &Context) -> String {
    let mut out = String::new();
    if ctx.tasks.is_empty() {
        out.push_str("no tasks\n");
    }
    for detail in &ctx.tasks {
        let task = &detail.task;
        let by = match &task.claimed_by {
            Some(agent) => format!(" by {agent}"),
            None => String::new(),
        };
        let _ = writeln!(out, "{} [{}] {}{by}", task.slug, task.status, task.title);
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
    if !ctx.events.is_empty() {
        out.push_str("events:\n");
        for event in &ctx.events {
            let _ = writeln!(out, "  {} {} {}", event.id, event.kind, event.payload);
        }
    }
    out
}

pub(crate) fn line(text: &str) -> String {
    format!("{text}\n")
}

#[cfg(test)]
mod tests {
    use super::{context, line, tasks};
    use agentmesh_core::{Context, Decision, Event, EventKind, Status, Step, Task, TaskDetail};

    fn task(status: Status, claimed_by: Option<&str>) -> Task {
        Task {
            id: 1,
            slug: "auth".to_string(),
            title: "Add auth".to_string(),
            status,
            claimed_by: claimed_by.map(str::to_string),
            created_at: "t".to_string(),
            updated_at: "t".to_string(),
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
        let rendered = context(&Context {
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
        });
        assert!(rendered.contains("auth [claimed] Add auth by cursor"));
        assert!(rendered.contains("1. [x] hash passwords"));
        assert!(rendered.contains("decision by claude-code: use argon2"));
        assert!(rendered.contains("events:"));
        assert!(rendered.contains("4 claimed"));
    }
}
