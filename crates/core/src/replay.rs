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
    pub depends_on: Vec<String>,
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
                depends_on: Vec::new(),
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
        EventKind::DependencyChanged => {
            let slug = required_str(&event.payload, "slug")?;
            let on = required_str(&event.payload, "on")?;
            let action = required_str(&event.payload, "action")?;
            let task = find_mut(tasks, &slug)?;
            match action.as_str() {
                "added" => {
                    if !task.depends_on.contains(&on) {
                        task.depends_on.push(on);
                    }
                    Ok(())
                }
                "removed" => {
                    task.depends_on.retain(|dependency| dependency != &on);
                    Ok(())
                }
                other => Err(Error::BadEvent(format!("unknown action {other}"))),
            }
        }
        EventKind::TaskRenamed => {
            let slug = required_str(&event.payload, "slug")?;
            let to = required_str(&event.payload, "to")?;
            let task = find_mut(tasks, &slug)?;
            task.title = to;
            Ok(())
        }
        EventKind::RunLinked => Ok(()),
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

#[cfg(test)]
mod tests {
    use super::{EventKind, Status, replay};
    use crate::model::Event;
    use serde_json::json;

    fn event(id: i64, kind: EventKind, payload: serde_json::Value) -> Event {
        Event {
            id,
            ts: "2026-01-01T00:00:00.000Z".to_string(),
            kind,
            payload,
        }
    }

    #[test]
    fn folds_in_id_order_and_ignores_progress() {
        let events = vec![
            event(
                3,
                EventKind::Progress,
                json!({"slug": "auth", "note": "half", "commit": "abc"}),
            ),
            event(
                1,
                EventKind::TaskCreated,
                json!({"slug": "auth", "title": "Add auth"}),
            ),
            event(
                4,
                EventKind::StatusChanged,
                json!({"slug": "auth", "from": "claimed", "to": "done"}),
            ),
            event(
                2,
                EventKind::Claimed,
                json!({"slug": "auth", "by": "cursor"}),
            ),
            event(
                5,
                EventKind::Decision,
                json!({"slug": "auth", "why": "use argon2", "by": "claude-code"}),
            ),
        ];
        let tasks = replay(&events).expect("replay");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Add auth");
        assert_eq!(tasks[0].status, Status::Done);
        assert_eq!(tasks[0].claimed_by.as_deref(), Some("cursor"));
        assert_eq!(tasks[0].decisions[0].rationale, "use argon2");
        assert_eq!(tasks[0].decisions[0].provenance, "claude-code");
    }

    #[test]
    fn progress_does_not_create_a_task() {
        let events = vec![event(
            1,
            EventKind::Progress,
            json!({"slug": "auth", "note": "note", "commit": null}),
        )];
        assert!(replay(&events).expect("replay").is_empty());
    }

    #[test]
    fn duplicate_create_and_unknown_claim_fail() {
        let created = event(
            1,
            EventKind::TaskCreated,
            json!({"slug": "auth", "title": "Add auth"}),
        );
        let duplicate = event(
            2,
            EventKind::TaskCreated,
            json!({"slug": "auth", "title": "Again"}),
        );
        assert!(replay(&[created.clone(), duplicate]).is_err());
        let claim = event(
            2,
            EventKind::Claimed,
            json!({"slug": "missing", "by": "cursor"}),
        );
        assert!(replay(&[claim]).is_err());
    }

    #[test]
    fn missing_or_empty_fields_fail() {
        let missing = event(1, EventKind::TaskCreated, json!({"slug": "auth"}));
        assert!(replay(&[missing]).is_err());
        let empty = event(
            1,
            EventKind::TaskCreated,
            json!({"slug": "auth", "title": ""}),
        );
        assert!(replay(&[empty]).is_err());
    }

    #[test]
    fn status_can_move_to_blocked() {
        let events = [
            event(
                1,
                EventKind::TaskCreated,
                json!({"slug": "auth", "title": "Add auth"}),
            ),
            event(
                2,
                EventKind::StatusChanged,
                json!({"slug": "auth", "from": "open", "to": "blocked"}),
            ),
        ];
        let tasks = replay(&events).expect("replay");
        assert_eq!(tasks[0].status, Status::Blocked);
        assert_eq!(tasks[0].claimed_by, None);
    }

    #[test]
    fn rename_replaces_the_title_and_keeps_the_slug() {
        let events = [
            event(
                1,
                EventKind::TaskCreated,
                json!({"slug": "auth", "title": "Add auth"}),
            ),
            event(
                2,
                EventKind::TaskRenamed,
                json!({"slug": "auth", "from": "Add auth", "to": "Add login"}),
            ),
        ];
        let tasks = replay(&events).expect("replay");
        assert_eq!(tasks[0].slug, "auth");
        assert_eq!(tasks[0].title, "Add login");
    }
}
