use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Error;
use crate::model::{Status, Task};

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub id: i64,
    pub prd_path: String,
    pub prd_sha256: String,
    pub branch: String,
    pub stage: String,
    pub cycle: i64,
    pub agent_calls: i64,
    pub est_cost_usd: f64,
    pub status: String,
    pub stop_reason: Option<String>,
    pub started_at: String,
    pub failure_streak: i64,
    pub current_slug: Option<String>,
    pub complete_requested: bool,
}

pub fn latest(conn: &Connection) -> Result<Option<Run>, Error> {
    let row = conn
        .query_row(
            "SELECT id, prd_path, prd_sha256, branch, stage, cycle, agent_calls, est_cost_usd,
                    status, stop_reason, started_at, failure_streak, current_slug, complete_requested
             FROM runs
             ORDER BY id DESC
             LIMIT 1",
            [],
            read_run,
        )
        .optional()?;
    Ok(row)
}

pub fn active(conn: &Connection) -> Result<Option<Run>, Error> {
    let row = conn
        .query_row(
            "SELECT id, prd_path, prd_sha256, branch, stage, cycle, agent_calls, est_cost_usd,
                    status, stop_reason, started_at, failure_streak, current_slug, complete_requested
             FROM runs
             WHERE status IN ('active', 'waiting', 'paused')
             ORDER BY id DESC
             LIMIT 1",
            [],
            read_run,
        )
        .optional()?;
    Ok(row)
}

pub fn resumable(conn: &Connection) -> Result<Option<Run>, Error> {
    let row = conn
        .query_row(
            "SELECT id, prd_path, prd_sha256, branch, stage, cycle, agent_calls, est_cost_usd,
                    status, stop_reason, started_at, failure_streak, current_slug, complete_requested
             FROM runs
             WHERE status IN ('waiting', 'paused', 'stopped')
             ORDER BY id DESC
             LIMIT 1",
            [],
            read_run,
        )
        .optional()?;
    Ok(row)
}

pub fn insert(
    conn: &Connection,
    prd_path: &str,
    prd_sha256: &str,
    branch: &str,
    started_at: &str,
) -> Result<Run, Error> {
    conn.execute(
        "INSERT INTO runs (
            prd_path, prd_sha256, branch, stage, cycle, agent_calls, est_cost_usd,
            status, started_at, failure_streak, complete_requested
         ) VALUES (?1, ?2, ?3, 'plan', 0, 0, 0, 'active', ?4, 0, 0)",
        params![prd_path, prd_sha256, branch, started_at],
    )?;
    load(conn, conn.last_insert_rowid())
}

pub fn save(conn: &Connection, run: &Run) -> Result<(), Error> {
    conn.execute(
        "UPDATE runs SET
            stage = ?1, cycle = ?2, agent_calls = ?3, est_cost_usd = ?4, status = ?5,
            stop_reason = ?6, failure_streak = ?7, current_slug = ?8, complete_requested = ?9
         WHERE id = ?10",
        params![
            run.stage,
            run.cycle,
            run.agent_calls,
            run.est_cost_usd,
            run.status,
            run.stop_reason,
            run.failure_streak,
            run.current_slug,
            i64::from(run.complete_requested),
            run.id,
        ],
    )?;
    Ok(())
}

pub fn assign_task(conn: &Connection, slug: &str, run_id: i64) -> Result<(), Error> {
    conn.execute(
        "UPDATE tasks SET run_id = ?1 WHERE slug = ?2",
        params![run_id, slug],
    )?;
    Ok(())
}

pub fn task_slugs(conn: &Connection, run_id: i64) -> Result<Vec<String>, Error> {
    let mut statement = conn.prepare("SELECT slug FROM tasks WHERE run_id = ?1 ORDER BY id")?;
    let rows = statement.query_map(params![run_id], |row| row.get::<_, String>(0))?;
    let mut slugs = Vec::new();
    for row in rows {
        slugs.push(row?);
    }
    Ok(slugs)
}

pub fn reopen_count(conn: &Connection, slug: &str) -> Result<i64, Error> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM reports AS r
         JOIN tasks AS t ON t.id = r.task_id
         WHERE t.slug = ?1 AND r.verdict = 'reopened'",
        params![slug],
        |row| row.get(0),
    )?;
    Ok(count)
}

pub fn latest_report(
    conn: &Connection,
    slug: &str,
) -> Result<Option<(i64, Option<String>)>, Error> {
    let row = conn
        .query_row(
            "SELECT r.bytes, r.verdict FROM reports AS r
             JOIN tasks AS t ON t.id = r.task_id
             WHERE t.slug = ?1
             ORDER BY r.id DESC
             LIMIT 1",
            params![slug],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(row)
}

pub fn load(conn: &Connection, id: i64) -> Result<Run, Error> {
    conn.query_row(
        "SELECT id, prd_path, prd_sha256, branch, stage, cycle, agent_calls, est_cost_usd,
                status, stop_reason, started_at, failure_streak, current_slug, complete_requested
         FROM runs WHERE id = ?1",
        params![id],
        read_run,
    )
    .map_err(Error::from)
}

fn read_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    let complete: i64 = row.get(13)?;
    Ok(Run {
        id: row.get(0)?,
        prd_path: row.get(1)?,
        prd_sha256: row.get(2)?,
        branch: row.get(3)?,
        stage: row.get(4)?,
        cycle: row.get(5)?,
        agent_calls: row.get(6)?,
        est_cost_usd: row.get(7)?,
        status: row.get(8)?,
        stop_reason: row.get(9)?,
        started_at: row.get(10)?,
        failure_streak: row.get(11)?,
        current_slug: row.get(12)?,
        complete_requested: complete != 0,
    })
}

pub fn ready_slug(tasks: &[Task]) -> Option<String> {
    tasks
        .iter()
        .find(|task| task.status == Status::Open && task.needs.is_empty())
        .map(|task| task.slug.clone())
}

pub fn blocked_slug(tasks: &[Task]) -> Option<String> {
    tasks
        .iter()
        .find(|task| task.status == Status::Blocked)
        .map(|task| task.slug.clone())
}

pub fn unfinished(tasks: &[Task]) -> bool {
    tasks.iter().any(|task| task.status != Status::Done)
}
