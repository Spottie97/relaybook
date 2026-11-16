use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Error;

pub const TASK_LIMIT: usize = 1500;
pub const NOTE_LIMIT: usize = 500;
pub const REVIEW_LIMIT: usize = 1500;

const TASK_REVIEW_KEYS: &[&str] = &["findings", "security", "tests_checked", "not_checked"];
const RELEASE_REVIEW_KEYS: &[&str] = &[
    "findings",
    "security",
    "tests_checked",
    "not_checked",
    "prd_covered",
    "gaps",
];

#[derive(Clone, Debug)]
pub struct Sections {
    pub changed: Vec<String>,
    pub verified: Vec<String>,
    pub not_verified: Vec<String>,
    pub deviations: Vec<String>,
    pub review: Vec<String>,
}

pub fn task_body(sections: &Sections) -> Result<String, Error> {
    let body = serde_json::to_string(&serde_json::json!({
        "changed": sections.changed,
        "verified": sections.verified,
        "not_verified": sections.not_verified,
        "deviations": sections.deviations,
        "review": sections.review,
    }))
    .map_err(|err| Error::Json(err.to_string()))?;
    check_size(&body, TASK_LIMIT)?;
    Ok(body)
}

pub fn note_body(text: &str) -> Result<String, Error> {
    let body = serde_json::to_string(&serde_json::json!({ "text": text }))
        .map_err(|err| Error::Json(err.to_string()))?;
    check_size(&body, NOTE_LIMIT)?;
    Ok(body)
}

#[allow(clippy::too_many_arguments)] // commit, base, and graph SHAs are separate nullable columns
pub fn insert(
    conn: &Connection,
    task_id: Option<i64>,
    kind: &str,
    author: &str,
    body: &str,
    created_at: &str,
    commit_sha: Option<&str>,
    base_sha: Option<&str>,
    graph_sha: Option<&str>,
) -> Result<i64, Error> {
    conn.execute(
        "INSERT INTO reports (
            task_id, kind, author, body, bytes, created_at, commit_sha, base_sha, graph_sha
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            task_id,
            kind,
            author,
            body,
            body.len() as i64,
            created_at,
            commit_sha,
            base_sha,
            graph_sha
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InboxItem {
    pub id: i64,
    pub slug: Option<String>,
    pub title: Option<String>,
    pub kind: String,
    pub author: String,
    pub body: serde_json::Value,
}

pub fn list_unreviewed(conn: &Connection) -> Result<Vec<InboxItem>, Error> {
    let mut statement = conn.prepare(
        "SELECT r.id, t.slug, t.title, r.kind, r.author, r.body
         FROM reports AS r
         LEFT JOIN tasks AS t ON t.id = r.task_id
         WHERE r.reviewed_at IS NULL
         ORDER BY r.id",
    )?;
    let rows = statement.query_map([], |row| {
        let body: String = row.get(5)?;
        Ok(InboxItem {
            id: row.get(0)?,
            slug: row.get(1)?,
            title: row.get(2)?,
            kind: row.get(3)?,
            author: row.get(4)?,
            body: serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
        })
    })?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub fn count_unreviewed(conn: &Connection) -> Result<usize, Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM reports WHERE reviewed_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count).unwrap_or(0))
}

pub fn accept_task(conn: &Connection, task_id: i64, reviewed_at: &str) -> Result<usize, Error> {
    let changed = conn.execute(
        "UPDATE reports SET reviewed_at = ?1, verdict = 'accepted'
         WHERE task_id = ?2 AND reviewed_at IS NULL",
        params![reviewed_at, task_id],
    )?;
    Ok(changed)
}

pub fn accept_note(conn: &Connection, id: i64, reviewed_at: &str) -> Result<usize, Error> {
    let changed = conn.execute(
        "UPDATE reports SET reviewed_at = ?1, verdict = 'accepted'
         WHERE id = ?2 AND kind = 'note' AND reviewed_at IS NULL",
        params![reviewed_at, id],
    )?;
    Ok(changed)
}

pub fn accept_all(conn: &Connection, reviewed_at: &str) -> Result<usize, Error> {
    let changed = conn.execute(
        "UPDATE reports SET reviewed_at = ?1, verdict = 'accepted' WHERE reviewed_at IS NULL",
        params![reviewed_at],
    )?;
    Ok(changed)
}

pub fn latest_body(conn: &Connection, task_id: i64) -> Result<Option<serde_json::Value>, Error> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM reports WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(body.and_then(|text| serde_json::from_str(&text).ok()))
}

pub fn mark_latest_reopened(
    conn: &Connection,
    task_id: i64,
    reason: &str,
    reviewed_at: &str,
) -> Result<(), Error> {
    conn.execute(
        "UPDATE reports SET reviewed_at = ?1, verdict = 'reopened', review_note = ?2
         WHERE id = (
             SELECT id FROM reports WHERE task_id = ?3 ORDER BY id DESC LIMIT 1
         )",
        params![reviewed_at, reason, task_id],
    )?;
    Ok(())
}

pub fn reopened_reason(conn: &Connection, task_id: i64) -> Result<Option<String>, Error> {
    let reason: Option<String> = conn
        .query_row(
            "SELECT review_note FROM reports
             WHERE task_id = ?1
             ORDER BY id DESC
             LIMIT 1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let verdict: Option<String> = conn
        .query_row(
            "SELECT verdict FROM reports WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if verdict.as_deref() == Some("reopened") {
        Ok(reason)
    } else {
        Ok(None)
    }
}

pub(crate) fn review_body(scope: &str, text: &str) -> Result<String, Error> {
    let bytes = text.len();
    if bytes > REVIEW_LIMIT {
        return Err(Error::InvalidSetting(format!(
            "review is {bytes} bytes; limit {REVIEW_LIMIT}"
        )));
    }
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|err| Error::Json(err.to_string()))?;
    let Some(object) = value.as_object() else {
        return Err(Error::InvalidSetting(
            "review body must be a JSON object".to_string(),
        ));
    };
    let keys = if scope == "release" {
        RELEASE_REVIEW_KEYS
    } else {
        TASK_REVIEW_KEYS
    };
    for key in keys {
        if !object.contains_key(*key) {
            return Err(Error::InvalidSetting(format!(
                "review body is missing {key}"
            )));
        }
    }
    Ok(text.to_string())
}

fn check_size(body: &str, limit: usize) -> Result<(), Error> {
    let bytes = body.len();
    if bytes > limit {
        Err(Error::ReportTooLarge { bytes, limit })
    } else {
        Ok(())
    }
}
