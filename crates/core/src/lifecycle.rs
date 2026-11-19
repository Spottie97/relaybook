//! Derived workflow phase. Nothing here is stored on `tasks.status`.
//!
//! Task phases use the latest task report and the latest task review.
//! A task is in a release's scope when any of its task reports has a
//! `commit_sha` in the caller-supplied range (`merge-base(main, HEAD)..HEAD`).
//! Release-reviewed requires the latest release review at `head` to be
//! approved, every in-scope task to be accepted, and no task to be claimed.
//! Release-reviewed and signed-off are recomputed against the head the caller
//! passes; a different head invalidates both.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPhase {
    Planned,
    Dispatched,
    Implemented,
    Reviewed,
    Accepted,
}

impl TaskPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Dispatched => "dispatched",
            Self::Implemented => "implemented",
            Self::Reviewed => "reviewed",
            Self::Accepted => "accepted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleasePhase {
    ReleaseReviewed,
    SignedOff,
}

impl ReleasePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReleaseReviewed => "release-reviewed",
            Self::SignedOff => "signed-off",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLifecycle {
    pub slug: String,
    pub phase: Option<TaskPhase>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lifecycle {
    pub tasks: Vec<TaskLifecycle>,
    pub release: Option<ReleasePhase>,
}

struct TaskRow {
    id: i64,
    slug: String,
    status: String,
}

struct ReportSnap {
    commit_sha: Option<String>,
    verdict: Option<String>,
}

struct ReviewSnap {
    commit_sha: String,
    verdict: Option<String>,
}

struct TaskSnap {
    shas: Vec<String>,
    phase: Option<TaskPhase>,
}

pub fn lifecycle(
    conn: &Connection,
    head: &str,
    commits_in_range: &[String],
) -> Result<Lifecycle, Error> {
    let rows = task_rows(conn)?;
    let mut tasks = Vec::with_capacity(rows.len());
    let mut snaps = Vec::with_capacity(rows.len());
    for row in &rows {
        let reports = task_reports(conn, row.id)?;
        let review = latest_task_review(conn, row.id)?;
        let phase = task_phase(&row.status, reports.first(), review.as_ref());
        let shas = reports
            .iter()
            .filter_map(|report| report.commit_sha.clone())
            .collect();
        tasks.push(TaskLifecycle {
            slug: row.slug.clone(),
            phase,
        });
        snaps.push(TaskSnap { shas, phase });
    }
    let any_claimed = rows.iter().any(|row| row.status == "claimed");
    let release = release_phase(conn, head, commits_in_range, &snaps, any_claimed)?;
    Ok(Lifecycle { tasks, release })
}

fn task_phase(
    status: &str,
    report: Option<&ReportSnap>,
    review: Option<&ReviewSnap>,
) -> Option<TaskPhase> {
    let commit = report.and_then(|row| row.commit_sha.as_deref());
    let verdict = report.and_then(|row| row.verdict.as_deref());
    if status == "done" {
        if commit.is_some() && verdict == Some("accepted") {
            return Some(TaskPhase::Accepted);
        }
        if let Some(sha) = commit {
            let reviewed = review.is_some_and(|row| {
                row.verdict.as_deref() == Some("approved") && row.commit_sha == sha
            });
            if reviewed {
                return Some(TaskPhase::Reviewed);
            }
            return Some(TaskPhase::Implemented);
        }
        return None;
    }
    if status == "claimed" {
        return Some(TaskPhase::Dispatched);
    }
    let live = report.is_some_and(|row| row.verdict.as_deref() != Some("reopened"));
    if (status == "open" || status == "blocked") && !live {
        Some(TaskPhase::Planned)
    } else {
        None
    }
}

fn release_phase(
    conn: &Connection,
    head: &str,
    commits_in_range: &[String],
    snaps: &[TaskSnap],
    any_claimed: bool,
) -> Result<Option<ReleasePhase>, Error> {
    if authorization_at(conn, head)? {
        return Ok(Some(ReleasePhase::SignedOff));
    }
    if any_claimed || !approved_release_at(conn, head)? {
        return Ok(None);
    }
    for snap in snaps {
        let in_scope = snap
            .shas
            .iter()
            .any(|sha| commits_in_range.iter().any(|item| item == sha));
        if in_scope && snap.phase != Some(TaskPhase::Accepted) {
            return Ok(None);
        }
    }
    Ok(Some(ReleasePhase::ReleaseReviewed))
}

fn task_rows(conn: &Connection) -> Result<Vec<TaskRow>, Error> {
    let mut statement = conn.prepare("SELECT id, slug, status FROM tasks ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok(TaskRow {
            id: row.get(0)?,
            slug: row.get(1)?,
            status: row.get(2)?,
        })
    })?;
    let mut tasks = Vec::new();
    for row in rows {
        tasks.push(row?);
    }
    Ok(tasks)
}

fn task_reports(conn: &Connection, task_id: i64) -> Result<Vec<ReportSnap>, Error> {
    let mut statement = conn.prepare(
        "SELECT commit_sha, verdict FROM reports
         WHERE task_id = ?1 AND kind = 'task'
         ORDER BY id DESC",
    )?;
    let rows = statement.query_map(params![task_id], |row| {
        Ok(ReportSnap {
            commit_sha: row.get(0)?,
            verdict: row.get(1)?,
        })
    })?;
    let mut reports = Vec::new();
    for row in rows {
        reports.push(row?);
    }
    Ok(reports)
}

fn latest_task_review(conn: &Connection, task_id: i64) -> Result<Option<ReviewSnap>, Error> {
    let review = conn
        .query_row(
            "SELECT commit_sha, verdict FROM reviews
             WHERE scope = 'task' AND task_id = ?1
             ORDER BY id DESC
             LIMIT 1",
            params![task_id],
            |row| {
                Ok(ReviewSnap {
                    commit_sha: row.get(0)?,
                    verdict: row.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(review)
}

fn authorization_at(conn: &Connection, head: &str) -> Result<bool, Error> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM release_authorizations WHERE commit_sha = ?1 LIMIT 1",
            params![head],
            |row| row.get(0),
        )
        .optional()?;
    Ok(id.is_some())
}

/// Latest release review at `commit_sha`, including a still-pending row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReleaseReviewAt {
    pub id: i64,
    pub verdict: Option<String>,
}

pub(crate) fn latest_release_review_at(
    conn: &Connection,
    commit_sha: &str,
) -> Result<Option<ReleaseReviewAt>, Error> {
    let review = conn
        .query_row(
            "SELECT id, verdict FROM reviews
             WHERE scope = 'release' AND commit_sha = ?1
             ORDER BY id DESC
             LIMIT 1",
            params![commit_sha],
            |row| {
                Ok(ReleaseReviewAt {
                    id: row.get(0)?,
                    verdict: row.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(review)
}

fn approved_release_at(conn: &Connection, head: &str) -> Result<bool, Error> {
    Ok(latest_release_review_at(conn, head)?
        .is_some_and(|review| review.verdict.as_deref() == Some("approved")))
}
