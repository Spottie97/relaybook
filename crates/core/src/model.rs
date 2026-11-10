use serde::Serialize;

use crate::error::Error;

/// How many events `context` returns unless a caller asks for a different limit.
pub const RECENT_EVENT_LIMIT: usize = 20;

/// Current task state stored on the projection row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Open,
    Claimed,
    Done,
    Blocked,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Claimed => "claimed",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for Status {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "open" => Ok(Self::Open),
            "claimed" => Ok(Self::Claimed),
            "done" => Ok(Self::Done),
            "blocked" => Ok(Self::Blocked),
            other => Err(Error::BadStatus(other.to_string())),
        }
    }
}

/// Append-only event kinds. The string form is what SQLite stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    TaskCreated,
    Claimed,
    Progress,
    Decision,
    StatusChanged,
    DependencyChanged,
    TaskRenamed,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskCreated => "task_created",
            Self::Claimed => "claimed",
            Self::Progress => "progress",
            Self::Decision => "decision",
            Self::StatusChanged => "status_changed",
            Self::DependencyChanged => "dependency_changed",
            Self::TaskRenamed => "task_renamed",
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for EventKind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "task_created" => Ok(Self::TaskCreated),
            "claimed" => Ok(Self::Claimed),
            "progress" => Ok(Self::Progress),
            "decision" => Ok(Self::Decision),
            "status_changed" => Ok(Self::StatusChanged),
            "dependency_changed" => Ok(Self::DependencyChanged),
            "task_renamed" => Ok(Self::TaskRenamed),
            other => Err(Error::BadEvent(format!("unknown kind {other}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Task {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub status: Status,
    pub claimed_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub depends_on: Vec<String>,
    pub needs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub id: i64,
    pub task_id: i64,
    pub position: i64,
    pub description: String,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub id: i64,
    pub task_id: i64,
    pub rationale: String,
    pub provenance: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub id: i64,
    pub ts: String,
    pub kind: EventKind,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskDetail {
    #[serde(flatten)]
    pub task: Task,
    pub steps: Vec<Step>,
    pub decisions: Vec<Decision>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Context {
    pub tasks: Vec<TaskDetail>,
    pub events: Vec<Event>,
    pub hidden_done: usize,
}

pub(crate) fn checked_slug(slug: &str) -> Result<String, Error> {
    if slug.split('-').all(is_slug_part) && !slug.is_empty() {
        Ok(slug.to_string())
    } else {
        Err(Error::InvalidSlug(slug.to_string()))
    }
}

fn is_slug_part(part: &str) -> bool {
    !part.is_empty()
        && part
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

pub(crate) fn required_text(field: &'static str, value: &str) -> Result<String, Error> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(Error::EmptyField(field))
    } else {
        Ok(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{EventKind, Status, checked_slug, required_text};

    #[test]
    fn accepts_lowercase_slugs() {
        assert!(checked_slug("auth").is_ok());
        assert!(checked_slug("add-auth").is_ok());
        assert!(checked_slug("a1").is_ok());
    }

    #[test]
    fn rejects_slugs_outside_the_pattern() {
        for slug in [
            "",
            "Auth",
            "add_auth",
            "-auth",
            "auth-",
            "add--auth",
            "add auth",
        ] {
            assert!(checked_slug(slug).is_err(), "{slug}");
        }
    }

    #[test]
    fn status_and_kind_roundtrip() {
        for status in [Status::Open, Status::Claimed, Status::Done, Status::Blocked] {
            let parsed: Status = status.as_str().parse().expect("status");
            assert_eq!(parsed, status);
        }
        assert!("nope".parse::<Status>().is_err());
        for kind in [
            EventKind::TaskCreated,
            EventKind::Claimed,
            EventKind::Progress,
            EventKind::Decision,
            EventKind::StatusChanged,
            EventKind::DependencyChanged,
            EventKind::TaskRenamed,
        ] {
            let parsed: EventKind = kind.as_str().parse().expect("kind");
            assert_eq!(parsed, kind);
        }
        assert!("nope".parse::<EventKind>().is_err());
    }

    #[test]
    fn required_text_trims_and_rejects_blank() {
        assert_eq!(
            required_text("title", "  Add auth  ").expect("title"),
            "Add auth"
        );
        assert_eq!(
            required_text("note", " \n").expect_err("blank").to_string(),
            "note is required"
        );
    }
}
