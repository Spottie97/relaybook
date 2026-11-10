use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::error::Error;
use crate::model::{
    Context, Decision, Event, EventKind, Status, Step, Task, TaskDetail, checked_slug,
    required_text,
};
use crate::schema::SCHEMA;

/// Open SQLite ledger. Projection tables hold current state; `events` is insert-only.
pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    pub fn initialize(path: &std::path::Path) -> Result<Self, Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        configure(&conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "user_version", 1)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn open(path: &std::path::Path) -> Result<Self, Error> {
        let conn = Connection::open(path)?;
        configure(&conn)?;
        Ok(Self { conn })
    }

    pub fn create_task(&mut self, slug: &str, title: &str) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        let title = required_text("title", title)?;
        self.transaction(|conn| {
            let ts = now(conn)?;
            match conn.execute(
                "INSERT INTO tasks (slug, title, status, claimed_by, created_at, updated_at)
                 VALUES (?1, ?2, 'open', NULL, ?3, ?3)",
                params![slug, title, ts],
            ) {
                Ok(_) => {}
                Err(err)
                    if constraint_message(&err).is_some_and(|msg| msg.contains("tasks.slug")) =>
                {
                    return Err(Error::TaskExists(slug.clone()));
                }
                Err(err) => return Err(err.into()),
            }
            insert_event(
                conn,
                EventKind::TaskCreated,
                serde_json::json!({ "slug": slug, "title": title }),
            )?;
            load_task(conn, &slug)
        })
    }

    pub fn list_tasks(&self, status: Option<Status>) -> Result<Vec<Task>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, slug, title, status, claimed_by, created_at, updated_at
             FROM tasks
             WHERE (?1 IS NULL OR status = ?1)
             ORDER BY id",
        )?;
        let status_text = status.map(|value| value.as_str().to_string());
        let rows = stmt.query_map(params![status_text], read_task_row)?;
        collect_tasks(rows)
    }

    pub fn claim_task(&mut self, slug: &str, by: &str) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        let by = required_text("by", by)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            if task.status != Status::Open {
                return Err(Error::AlreadyClaimed);
            }
            let ts = now(conn)?;
            let changed = conn.execute(
                "UPDATE tasks
                 SET status = 'claimed', claimed_by = ?1, updated_at = ?2
                 WHERE id = ?3 AND status = 'open'",
                params![by, ts, task.id],
            )?;
            if changed == 0 {
                return Err(Error::AlreadyClaimed);
            }
            insert_event(
                conn,
                EventKind::Claimed,
                serde_json::json!({ "slug": slug, "by": by }),
            )?;
            load_task(conn, &slug)
        })
    }

    pub fn complete_task(&mut self, slug: &str) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            if task.status == Status::Done {
                return Ok(task);
            }
            let from = task.status.as_str();
            let ts = now(conn)?;
            conn.execute(
                "UPDATE tasks SET status = 'done', updated_at = ?1 WHERE id = ?2",
                params![ts, task.id],
            )?;
            insert_event(
                conn,
                EventKind::StatusChanged,
                serde_json::json!({ "slug": slug, "from": from, "to": "done" }),
            )?;
            load_task(conn, &slug)
        })
    }

    pub fn block_task(&mut self, slug: &str, reason: &str) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        let reason = required_text("reason", reason)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            match task.status {
                Status::Blocked => return Ok(task),
                Status::Done => return Err(Error::CannotBlockDone),
                Status::Open | Status::Claimed => {}
            }
            let from = task.status.as_str();
            let ts = now(conn)?;
            conn.execute(
                "UPDATE tasks SET status = 'blocked', updated_at = ?1 WHERE id = ?2",
                params![ts, task.id],
            )?;
            insert_event(
                conn,
                EventKind::StatusChanged,
                serde_json::json!({
                    "slug": slug,
                    "from": from,
                    "to": "blocked",
                    "reason": reason,
                }),
            )?;
            load_task(conn, &slug)
        })
    }

    pub fn add_step(&mut self, slug: &str, description: &str) -> Result<Step, Error> {
        let slug = checked_slug(slug)?;
        let description = required_text("description", description)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            let position: i64 = conn.query_row(
                "SELECT COALESCE(MAX(position), 0) + 1 FROM steps WHERE task_id = ?1",
                params![task.id],
                |row| row.get(0),
            )?;
            conn.execute(
                "INSERT INTO steps (task_id, position, description, done)
                 VALUES (?1, ?2, ?3, 0)",
                params![task.id, position, description],
            )?;
            Ok(Step {
                id: conn.last_insert_rowid(),
                task_id: task.id,
                position,
                description,
                done: false,
            })
        })
    }

    pub fn check_step(&mut self, slug: &str, position: i64) -> Result<Step, Error> {
        let slug = checked_slug(slug)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            let changed = conn.execute(
                "UPDATE steps SET done = 1 WHERE task_id = ?1 AND position = ?2",
                params![task.id, position],
            )?;
            if changed == 0 {
                return Err(Error::NoSuchStep(position));
            }
            load_step(conn, task.id, position)
        })
    }

    pub fn report_progress(
        &mut self,
        slug: &str,
        note: &str,
        commit: Option<&str>,
    ) -> Result<Event, Error> {
        let slug = checked_slug(slug)?;
        let note = required_text("note", note)?;
        let commit = commit
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        self.transaction(|conn| {
            load_task(conn, &slug)?;
            if let Some(sha) = &commit
                && progress_exists(conn, &slug, sha)?
            {
                return Err(Error::AlreadyReported(sha.clone()));
            }
            match insert_event(
                conn,
                EventKind::Progress,
                serde_json::json!({ "slug": slug, "note": note, "commit": commit }),
            ) {
                Err(Error::Sqlite(err))
                    if constraint_message(&err)
                        .is_some_and(|msg| msg.contains("events_progress_once")) =>
                {
                    Err(Error::AlreadyReported(commit.unwrap_or_default()))
                }
                other => other,
            }
        })
    }

    pub fn log_decision(&mut self, slug: &str, why: &str, by: &str) -> Result<Decision, Error> {
        let slug = checked_slug(slug)?;
        let why = required_text("why", why)?;
        let by = required_text("by", by)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            let ts = now(conn)?;
            conn.execute(
                "INSERT INTO decisions (task_id, rationale, provenance, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![task.id, why, by, ts],
            )?;
            let id = conn.last_insert_rowid();
            insert_event(
                conn,
                EventKind::Decision,
                serde_json::json!({
                    "slug": slug,
                    "why": why,
                    "by": by,
                    "decision_id": id,
                }),
            )?;
            Ok(Decision {
                id,
                task_id: task.id,
                rationale: why,
                provenance: by,
                created_at: ts,
            })
        })
    }

    pub fn context(&self, slug: Option<&str>, event_limit: usize) -> Result<Context, Error> {
        let slug = match slug {
            Some(value) => Some(checked_slug(value)?),
            None => None,
        };
        let tasks = self.task_details(slug.as_deref())?;
        let events = self.recent_events(slug.as_deref(), event_limit)?;
        Ok(Context { tasks, events })
    }

    fn task_details(&self, slug: Option<&str>) -> Result<Vec<TaskDetail>, Error> {
        let tasks = if let Some(slug) = slug {
            vec![load_task(&self.conn, slug)?]
        } else {
            self.list_tasks(None)?
        };
        let mut details = Vec::with_capacity(tasks.len());
        for task in tasks {
            let steps = load_steps(&self.conn, task.id)?;
            let decisions = load_decisions(&self.conn, task.id)?;
            details.push(TaskDetail {
                task,
                steps,
                decisions,
            });
        }
        Ok(details)
    }

    fn recent_events(&self, slug: Option<&str>, event_limit: usize) -> Result<Vec<Event>, Error> {
        let limit = i64::try_from(event_limit).unwrap_or(i64::MAX);
        let slug_param = slug.map(ToOwned::to_owned);
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, kind, payload
             FROM events
             WHERE (?1 IS NULL OR json_extract(payload, '$.slug') = ?1)
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![slug_param, limit], read_event_row)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(into_event(row?)?);
        }
        Ok(events)
    }

    fn transaction<T>(
        &mut self,
        body: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = body(&tx);
        match result {
            Ok(value) => {
                tx.commit()?;
                let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
                Ok(value)
            }
            Err(err) => Err(err),
        }
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
}

fn configure(conn: &Connection) -> Result<(), Error> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn now(conn: &Connection) -> Result<String, Error> {
    let ts = conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
        row.get(0)
    })?;
    Ok(ts)
}

fn insert_event(
    conn: &Connection,
    kind: EventKind,
    payload: serde_json::Value,
) -> Result<Event, Error> {
    let ts = now(conn)?;
    let payload_text =
        serde_json::to_string(&payload).map_err(|err| Error::Json(err.to_string()))?;
    conn.execute(
        "INSERT INTO events (ts, kind, payload) VALUES (?1, ?2, ?3)",
        params![ts, kind.as_str(), payload_text],
    )?;
    Ok(Event {
        id: conn.last_insert_rowid(),
        ts,
        kind,
        payload,
    })
}

fn progress_exists(conn: &Connection, slug: &str, commit: &str) -> Result<bool, Error> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM events
             WHERE kind = 'progress'
               AND json_extract(payload, '$.slug') = ?1
               AND json_extract(payload, '$.commit') = ?2
             LIMIT 1",
            params![slug, commit],
            |row| row.get(0),
        )
        .optional()?;
    Ok(existing.is_some())
}

fn load_task(conn: &Connection, slug: &str) -> Result<Task, Error> {
    let row = conn
        .query_row(
            "SELECT id, slug, title, status, claimed_by, created_at, updated_at
             FROM tasks WHERE slug = ?1",
            params![slug],
            read_task_row,
        )
        .optional()?;
    match row {
        Some(row) => into_task(row),
        None => Err(Error::NoSuchTask(slug.to_string())),
    }
}

fn load_step(conn: &Connection, task_id: i64, position: i64) -> Result<Step, Error> {
    let row = conn
        .query_row(
            "SELECT id, task_id, position, description, done
             FROM steps WHERE task_id = ?1 AND position = ?2",
            params![task_id, position],
            read_step_row,
        )
        .optional()?;
    row.map(into_step).ok_or(Error::NoSuchStep(position))
}

fn load_steps(conn: &Connection, task_id: i64) -> Result<Vec<Step>, Error> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, position, description, done
         FROM steps WHERE task_id = ?1 ORDER BY position",
    )?;
    let rows = stmt.query_map(params![task_id], read_step_row)?;
    let mut steps = Vec::new();
    for row in rows {
        steps.push(into_step(row?));
    }
    Ok(steps)
}

fn load_decisions(conn: &Connection, task_id: i64) -> Result<Vec<Decision>, Error> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, rationale, provenance, created_at
         FROM decisions WHERE task_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![task_id], |row| {
        Ok(Decision {
            id: row.get(0)?,
            task_id: row.get(1)?,
            rationale: row.get(2)?,
            provenance: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    let mut decisions = Vec::new();
    for row in rows {
        decisions.push(row?);
    }
    Ok(decisions)
}

struct TaskRow {
    id: i64,
    slug: String,
    title: String,
    status: String,
    claimed_by: Option<String>,
    created_at: String,
    updated_at: String,
}

fn read_task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        id: row.get(0)?,
        slug: row.get(1)?,
        title: row.get(2)?,
        status: row.get(3)?,
        claimed_by: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn into_task(row: TaskRow) -> Result<Task, Error> {
    Ok(Task {
        id: row.id,
        slug: row.slug,
        title: row.title,
        status: row.status.parse()?,
        claimed_by: row.claimed_by,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn collect_tasks(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<TaskRow>>,
) -> Result<Vec<Task>, Error> {
    let mut tasks = Vec::new();
    for row in rows {
        tasks.push(into_task(row?)?);
    }
    Ok(tasks)
}

struct StepRow {
    id: i64,
    task_id: i64,
    position: i64,
    description: String,
    done: i64,
}

fn read_step_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StepRow> {
    Ok(StepRow {
        id: row.get(0)?,
        task_id: row.get(1)?,
        position: row.get(2)?,
        description: row.get(3)?,
        done: row.get(4)?,
    })
}

fn into_step(row: StepRow) -> Step {
    Step {
        id: row.id,
        task_id: row.task_id,
        position: row.position,
        description: row.description,
        done: row.done != 0,
    }
}

struct EventRow {
    id: i64,
    ts: String,
    kind: String,
    payload: String,
}

fn read_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        id: row.get(0)?,
        ts: row.get(1)?,
        kind: row.get(2)?,
        payload: row.get(3)?,
    })
}

fn into_event(row: EventRow) -> Result<Event, Error> {
    let payload = serde_json::from_str(&row.payload).map_err(|err| Error::Json(err.to_string()))?;
    Ok(Event {
        id: row.id,
        ts: row.ts,
        kind: row.kind.parse()?,
        payload,
    })
}

fn constraint_message(err: &rusqlite::Error) -> Option<&str> {
    match err {
        rusqlite::Error::SqliteFailure(_, Some(message)) => Some(message.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::Ledger;
    use crate::model::{EventKind, Status};

    fn open_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = Ledger::initialize(&dir.path().join("ledger.db")).expect("init");
        (dir, ledger)
    }

    #[test]
    fn rejects_bad_slug_and_blank_title() {
        let (_dir, mut ledger) = open_ledger();
        assert_eq!(
            ledger
                .create_task("Auth", "Add auth")
                .expect_err("slug")
                .to_string(),
            "invalid slug: Auth"
        );
        assert_eq!(
            ledger
                .create_task("auth", "  ")
                .expect_err("title")
                .to_string(),
            "title is required"
        );
    }

    #[test]
    fn duplicate_slug_names_the_task() {
        let (_dir, mut ledger) = open_ledger();
        ledger.create_task("auth", "Add auth").expect("create");
        assert_eq!(
            ledger
                .create_task("auth", "Again")
                .expect_err("dup")
                .to_string(),
            "task already exists: auth"
        );
    }

    #[test]
    fn missing_claim_and_second_claim_use_distinct_errors() {
        let (_dir, mut ledger) = open_ledger();
        assert_eq!(
            ledger
                .claim_task("auth", "cursor")
                .expect_err("missing")
                .to_string(),
            "no such task: auth"
        );
        ledger.create_task("auth", "Add auth").expect("create");
        ledger.claim_task("auth", "cursor").expect("claim");
        assert_eq!(
            ledger
                .claim_task("auth", "other")
                .expect_err("taken")
                .to_string(),
            "already claimed"
        );
    }

    #[test]
    fn steps_start_at_one_and_unknown_positions_fail() {
        let (_dir, mut ledger) = open_ledger();
        ledger.create_task("auth", "Add auth").expect("create");
        let first = ledger.add_step("auth", "hash passwords").expect("step");
        let second = ledger.add_step("auth", "write tests").expect("step");
        assert_eq!(first.position, 1);
        assert_eq!(second.position, 2);
        assert!(!first.done);
        let checked = ledger.check_step("auth", 1).expect("check");
        assert!(checked.done);
        assert_eq!(
            ledger
                .check_step("auth", 9)
                .expect_err("missing")
                .to_string(),
            "no such step: 9"
        );
    }

    #[test]
    fn list_filters_status_and_done_is_idempotent() {
        let (_dir, mut ledger) = open_ledger();
        ledger.create_task("auth", "Add auth").expect("create");
        ledger
            .create_task("ledger", "Open the ledger")
            .expect("create");
        ledger.claim_task("auth", "cursor").expect("claim");
        let open = ledger.list_tasks(Some(Status::Open)).expect("list");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].slug, "ledger");
        ledger.complete_task("auth").expect("done");
        let before = ledger.context(Some("auth"), 20).expect("context");
        let status_events = before
            .events
            .iter()
            .filter(|event| event.kind == EventKind::StatusChanged)
            .count();
        ledger.complete_task("auth").expect("done again");
        let after = ledger.context(Some("auth"), 20).expect("context");
        assert_eq!(
            after
                .events
                .iter()
                .filter(|event| event.kind == EventKind::StatusChanged)
                .count(),
            status_events
        );
        assert_eq!(after.tasks[0].task.status, Status::Done);
    }
}
