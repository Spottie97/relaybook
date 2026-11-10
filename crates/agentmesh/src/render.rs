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
