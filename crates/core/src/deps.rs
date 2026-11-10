use std::collections::{HashMap, HashSet, VecDeque};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Error;
use crate::model::{EventKind, Task};

pub(crate) fn add_link(conn: &Connection, slug: &str, on: &str) -> Result<(), Error> {
    let task_id = lookup_id(conn, slug)?;
    let depends_on_id = lookup_id(conn, on)?;
    if task_id == depends_on_id {
        return Err(Error::SelfDependency);
    }
    let existing: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM task_deps WHERE task_id = ?1 AND depends_on_id = ?2",
            params![task_id, depends_on_id],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_some() {
        return Ok(());
    }
    if let Some(path) = cycle_slugs(conn, task_id, depends_on_id, slug, on)? {
        return Err(Error::DependencyCycle(path));
    }
    let ts = crate::ledger::timestamp(conn)?;
    conn.execute(
        "INSERT INTO task_deps (task_id, depends_on_id, created_at) VALUES (?1, ?2, ?3)",
        params![task_id, depends_on_id, ts],
    )?;
    crate::ledger::record_event(
        conn,
        EventKind::DependencyChanged,
        serde_json::json!({
            "slug": slug,
            "on": on,
            "action": "added",
        }),
    )?;
    Ok(())
}

pub(crate) fn remove_link(conn: &Connection, slug: &str, on: &str) -> Result<(), Error> {
    let task_id = lookup_id(conn, slug)?;
    let depends_on_id = lookup_id(conn, on)?;
    if task_id == depends_on_id {
        return Err(Error::SelfDependency);
    }
    let changed = conn.execute(
        "DELETE FROM task_deps WHERE task_id = ?1 AND depends_on_id = ?2",
        params![task_id, depends_on_id],
    )?;
    if changed == 0 {
        return Ok(());
    }
    crate::ledger::record_event(
        conn,
        EventKind::DependencyChanged,
        serde_json::json!({
            "slug": slug,
            "on": on,
            "action": "removed",
        }),
    )?;
    Ok(())
}

pub(crate) fn fill_dependencies(conn: &Connection, tasks: &mut [Task]) -> Result<(), Error> {
    if tasks.is_empty() {
        return Ok(());
    }
    let mut statement = conn.prepare(
        "SELECT d.task_id, dep.slug, dep.status
         FROM task_deps AS d
         JOIN tasks AS dep ON dep.id = d.depends_on_id
         ORDER BY dep.slug",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut grouped: HashMap<i64, Vec<(String, String)>> = HashMap::new();
    for row in rows {
        let (id, slug, status) = row?;
        grouped.entry(id).or_default().push((slug, status));
    }
    for task in tasks.iter_mut() {
        let dependencies = grouped.remove(&task.id).unwrap_or_default();
        task.needs = dependencies
            .iter()
            .filter(|(_, status)| status != "done")
            .map(|(slug, _)| slug.clone())
            .collect();
        task.depends_on = dependencies.into_iter().map(|(slug, _)| slug).collect();
    }
    Ok(())
}

fn lookup_id(conn: &Connection, slug: &str) -> Result<i64, Error> {
    conn.query_row(
        "SELECT id FROM tasks WHERE slug = ?1",
        params![slug],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| Error::NoSuchTask(slug.to_string()))
}

fn cycle_slugs(
    conn: &Connection,
    task_id: i64,
    depends_on_id: i64,
    slug: &str,
    on: &str,
) -> Result<Option<String>, Error> {
    let edges = dependency_edges(conn)?;
    let Some(route) = route_between(&edges, depends_on_id, task_id) else {
        return Ok(None);
    };
    let mut slugs = vec![slug.to_string(), on.to_string()];
    for id in route.into_iter().skip(1) {
        slugs.push(slug_for(conn, id)?);
    }
    Ok(Some(slugs.join(" -> ")))
}

fn dependency_edges(conn: &Connection) -> Result<HashMap<i64, Vec<i64>>, Error> {
    let mut statement = conn.prepare("SELECT task_id, depends_on_id FROM task_deps")?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
    let mut edges: HashMap<i64, Vec<i64>> = HashMap::new();
    for row in rows {
        let (task, depends_on) = row?;
        edges.entry(task).or_default().push(depends_on);
    }
    Ok(edges)
}

fn route_between(edges: &HashMap<i64, Vec<i64>>, start: i64, goal: i64) -> Option<Vec<i64>> {
    let mut previous: HashMap<i64, i64> = HashMap::new();
    let mut queue = VecDeque::from([start]);
    let mut seen = HashSet::from([start]);
    let mut found = false;
    while let Some(current) = queue.pop_front() {
        if current == goal {
            found = true;
            break;
        }
        let Some(next_ids) = edges.get(&current) else {
            continue;
        };
        for next in next_ids {
            if seen.insert(*next) {
                previous.insert(*next, current);
                queue.push_back(*next);
            }
        }
    }
    if !found {
        return None;
    }
    let mut path = vec![goal];
    let mut cursor = goal;
    while cursor != start {
        cursor = *previous.get(&cursor)?;
        path.push(cursor);
    }
    path.reverse();
    Some(path)
}

fn slug_for(conn: &Connection, id: i64) -> Result<String, Error> {
    conn.query_row("SELECT slug FROM tasks WHERE id = ?1", params![id], |row| {
        row.get(0)
    })
    .map_err(Error::from)
}
