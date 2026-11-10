use rusqlite::{Connection, params};

use crate::error::Error;

pub const TASK_LIMIT: usize = 1500;
pub const NOTE_LIMIT: usize = 500;

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

pub fn insert(
    conn: &Connection,
    task_id: Option<i64>,
    kind: &str,
    author: &str,
    body: &str,
    created_at: &str,
) -> Result<i64, Error> {
    conn.execute(
        "INSERT INTO reports (task_id, kind, author, body, bytes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![task_id, kind, author, body, body.len() as i64, created_at],
    )?;
    Ok(conn.last_insert_rowid())
}

fn check_size(body: &str, limit: usize) -> Result<(), Error> {
    let bytes = body.len();
    if bytes > limit {
        Err(Error::ReportTooLarge { bytes, limit })
    } else {
        Ok(())
    }
}
