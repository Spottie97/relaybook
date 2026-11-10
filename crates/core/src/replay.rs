use crate::error::Error;
use crate::model::{Event, EventKind, Status};

/// Task state rebuilt from the event log. Steps are not events, so they are absent here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayedTask {
    pub slug: String,
    pub title: String,
    pub status: Status,
    pub claimed_by: Option<String>,
    pub decisions: Vec<ReplayedDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayedDecision {
    pub rationale: String,
    pub provenance: String,
}

/// Fold events into task state. Progress events do not change status.
pub fn replay(events: &[Event]) -> Result<Vec<ReplayedTask>, Error> {
    let mut ordered: Vec<&Event> = events.iter().collect();
    ordered.sort_by_key(|event| event.id);
    let mut tasks = Vec::new();
    for event in ordered {
        apply(&mut tasks, event)?;
    }
    Ok(tasks)
}

fn apply(tasks: &mut Vec<ReplayedTask>, event: &Event) -> Result<(), Error> {
    match event.kind {
        EventKind::TaskCreated => {
            let slug = required_str(&event.payload, "slug")?;
            let title = required_str(&event.payload, "title")?;
            if tasks.iter().any(|task| task.slug == slug) {
                return Err(Error::BadEvent(format!("duplicate task {slug}")));
            }
            tasks.push(ReplayedTask {
                slug,
                title,
                status: Status::Open,
                claimed_by: None,
                decisions: Vec::new(),
            });
            Ok(())
        }
        EventKind::Claimed => {
            let slug = required_str(&event.payload, "slug")?;
            let by = required_str(&event.payload, "by")?;
            let task = find_mut(tasks, &slug)?;
            task.status = Status::Claimed;
            task.claimed_by = Some(by);
            Ok(())
        }
        EventKind::Progress => Ok(()),
        EventKind::Decision => {
            let slug = required_str(&event.payload, "slug")?;
            let why = required_str(&event.payload, "why")?;
            let by = required_str(&event.payload, "by")?;
            let task = find_mut(tasks, &slug)?;
            task.decisions.push(ReplayedDecision {
                rationale: why,
                provenance: by,
            });
            Ok(())
        }
        EventKind::StatusChanged => {
            let slug = required_str(&event.payload, "slug")?;
            let to = required_str(&event.payload, "to")?;
            let status = to.parse::<Status>()?;
            let task = find_mut(tasks, &slug)?;
            task.status = status;
            Ok(())
        }
    }
}

fn find_mut<'a>(tasks: &'a mut [ReplayedTask], slug: &str) -> Result<&'a mut ReplayedTask, Error> {
    tasks
        .iter_mut()
        .find(|task| task.slug == slug)
        .ok_or_else(|| Error::BadEvent(format!("unknown task {slug}")))
}

fn required_str(payload: &serde_json::Value, key: &str) -> Result<String, Error> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::BadEvent(format!("missing {key}")))
}
