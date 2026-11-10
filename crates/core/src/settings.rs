use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::error::Error;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub mode: String,
    pub gates: String,
    pub max_cycles: i64,
    pub max_reopens_per_task: i64,
    pub max_agent_calls: i64,
    pub cursor_timeout_min: i64,
    pub est_cost_cap_usd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunView {
    pub id: i64,
    pub prd: String,
    pub branch: String,
    pub stage: String,
    pub cycle: i64,
    pub agent_calls: i64,
    pub max_agent_calls: i64,
    pub est_cost_usd: f64,
    pub est_cost_label: &'static str,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusView {
    pub mode: String,
    pub gates: String,
    pub max_cycles: i64,
    pub max_reopens_per_task: i64,
    pub max_agent_calls: i64,
    pub cursor_timeout_min: i64,
    pub est_cost_cap_usd: Option<String>,
    pub run: Option<RunView>,
    pub waiting_on: Option<String>,
}

struct RunRow {
    id: i64,
    prd: String,
    branch: String,
    stage: String,
    cycle: i64,
    agent_calls: i64,
    est_cost_usd: f64,
    status: String,
    stop_reason: Option<String>,
}

pub fn load(conn: &Connection) -> Result<Snapshot, Error> {
    Ok(Snapshot {
        mode: value_or(conn, "mode", "manual")?,
        gates: value_or(conn, "gates", "plan")?,
        max_cycles: number_or(conn, "max_cycles", 20)?,
        max_reopens_per_task: number_or(conn, "max_reopens_per_task", 2)?,
        max_agent_calls: number_or(conn, "max_agent_calls", 60)?,
        cursor_timeout_min: number_or(conn, "cursor_timeout_min", 30)?,
        est_cost_cap_usd: optional_value(conn, "est_cost_cap_usd")?,
    })
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<Snapshot, Error> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::InvalidSetting(format!("{key} is required")));
    }
    if key == "est_cost_cap_usd" && value == "unset" {
        conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        return load(conn);
    }
    let stored = validate(key, value)?;
    let ts = now(conn)?;
    conn.execute(
        "INSERT INTO settings (key, value, updated_at, updated_by)
         VALUES (?1, ?2, ?3, 'cli')
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at = excluded.updated_at,
            updated_by = excluded.updated_by",
        params![key, stored, ts],
    )?;
    load(conn)
}

pub fn status(conn: &Connection) -> Result<StatusView, Error> {
    let settings = load(conn)?;
    let run = active_run(conn)?;
    let waiting_on = waiting_on(&settings, run.as_ref());
    let max_agent_calls = settings.max_agent_calls;
    Ok(StatusView {
        mode: settings.mode,
        gates: settings.gates,
        max_cycles: settings.max_cycles,
        max_reopens_per_task: settings.max_reopens_per_task,
        max_agent_calls,
        cursor_timeout_min: settings.cursor_timeout_min,
        est_cost_cap_usd: settings.est_cost_cap_usd,
        run: run.map(|row| RunView {
            id: row.id,
            prd: row.prd,
            branch: row.branch,
            stage: row.stage,
            cycle: row.cycle,
            agent_calls: row.agent_calls,
            max_agent_calls,
            est_cost_usd: row.est_cost_usd,
            est_cost_label: "estimate",
            status: row.status,
        }),
        waiting_on,
    })
}

pub fn hook_wait_line(conn: &Connection) -> Result<Option<String>, Error> {
    let Some(run) = active_run(conn)? else {
        return Ok(None);
    };
    Ok(match run.status.as_str() {
        "waiting" => Some("waiting for approval: relaybook approve".to_string()),
        "paused" => Some(usage_hook_line(run.stop_reason.as_deref())),
        _ => None,
    })
}

pub fn format_config(settings: &Snapshot) -> String {
    format!(
        "mode {}\ngates {}\nmax_cycles {}\nmax_reopens_per_task {}\nmax_agent_calls {}\ncursor_timeout_min {}\nest_cost_cap_usd {}\n",
        settings.mode,
        settings.gates,
        settings.max_cycles,
        settings.max_reopens_per_task,
        settings.max_agent_calls,
        settings.cursor_timeout_min,
        settings.est_cost_cap_usd.as_deref().unwrap_or("unset")
    )
}

pub fn format_status(view: &StatusView) -> String {
    let cap = view.est_cost_cap_usd.as_deref().unwrap_or("unset");
    let mut out = format!(
        "mode {}\ngates {}\nlimits cycles {} reopens {} calls {} timeout {}m cap {}\n",
        view.mode,
        view.gates,
        view.max_cycles,
        view.max_reopens_per_task,
        view.max_agent_calls,
        view.cursor_timeout_min,
        cap
    );
    match &view.run {
        Some(run) => {
            writeln_line(
                &mut out,
                &format!(
                    "run {} {} {} {} cycle {} calls {}/{} est {:.2} USD (estimate)",
                    run.id,
                    run.prd,
                    run.branch,
                    run.stage,
                    run.cycle,
                    run.agent_calls,
                    run.max_agent_calls,
                    run.est_cost_usd
                ),
            );
        }
        None => out.push_str("no active run\n"),
    }
    if let Some(waiting) = &view.waiting_on {
        writeln_line(&mut out, &format!("waiting on {waiting}"));
    }
    out
}

fn writeln_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

fn waiting_on(settings: &Snapshot, run: Option<&RunRow>) -> Option<String> {
    let run = run?;
    match run.status.as_str() {
        "waiting" => Some("approval".to_string()),
        "paused" => Some(usage_wait_label(run.stop_reason.as_deref())),
        "active" if settings.mode == "manual" => Some("stop at next boundary".to_string()),
        _ => None,
    }
}

fn usage_hook_line(reason: Option<&str>) -> String {
    let (cli, until) = split_pause(reason);
    match until {
        Some(time) => {
            format!("usage limit reached on {cli}; relaybook run --resume after {time}")
        }
        None => format!("usage limit reached on {cli}; relaybook run --resume"),
    }
}

fn usage_wait_label(reason: Option<&str>) -> String {
    let (cli, until) = split_pause(reason);
    match until {
        Some(time) => format!("usage limit ({cli}, until {time})"),
        None => format!("usage limit ({cli})"),
    }
}

fn split_pause(reason: Option<&str>) -> (&str, Option<&str>) {
    let Some(reason) = reason.map(str::trim).filter(|value| !value.is_empty()) else {
        return ("claude", None);
    };
    match reason.split_once('|') {
        Some((cli, time)) if !time.is_empty() => (cli, Some(time)),
        Some((cli, _)) => (cli, None),
        None => (reason, None),
    }
}

fn active_run(conn: &Connection) -> Result<Option<RunRow>, Error> {
    let present = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'runs'",
            [],
            |_| Ok(1_i64),
        )
        .optional()?;
    if present.is_none() {
        return Ok(None);
    }
    let row = conn
        .query_row(
            "SELECT id, prd_path, branch, stage, cycle, agent_calls, est_cost_usd, status, stop_reason
             FROM runs
             WHERE status IN ('active', 'waiting', 'paused')
             ORDER BY id DESC
             LIMIT 1",
            [],
            |row| {
                Ok(RunRow {
                    id: row.get(0)?,
                    prd: row.get(1)?,
                    branch: row.get(2)?,
                    stage: row.get(3)?,
                    cycle: row.get(4)?,
                    agent_calls: row.get(5)?,
                    est_cost_usd: row.get(6)?,
                    status: row.get(7)?,
                    stop_reason: row.get(8)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn validate(key: &str, value: &str) -> Result<String, Error> {
    match key {
        "mode" => one_of(key, value, &["manual", "auto"]),
        "gates" => one_of(key, value, &["none", "plan", "plan,review"]),
        "max_cycles" | "max_reopens_per_task" | "max_agent_calls" | "cursor_timeout_min" => {
            let parsed = value
                .parse::<i64>()
                .map_err(|_| Error::InvalidSetting(format!("invalid {key}: {value}")))?;
            if parsed < 1 {
                return Err(Error::InvalidSetting(format!("{key} must be at least 1")));
            }
            Ok(parsed.to_string())
        }
        "est_cost_cap_usd" => {
            let parsed = value
                .parse::<f64>()
                .map_err(|_| Error::InvalidSetting(format!("invalid est_cost_cap_usd: {value}")))?;
            if parsed <= 0.0 {
                return Err(Error::InvalidSetting(
                    "est_cost_cap_usd must be greater than 0".to_string(),
                ));
            }
            Ok(format!("{parsed:.2}"))
        }
        _ => Err(Error::InvalidSetting(format!("unknown setting: {key}"))),
    }
}

fn one_of(key: &str, value: &str, allowed: &[&str]) -> Result<String, Error> {
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(Error::InvalidSetting(format!("invalid {key}: {value}")))
    }
}

fn value_or(conn: &Connection, key: &str, default: &str) -> Result<String, Error> {
    Ok(optional_value(conn, key)?.unwrap_or_else(|| default.to_string()))
}

fn optional_value(conn: &Connection, key: &str) -> Result<Option<String>, Error> {
    let value = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value)
}

fn number_or(conn: &Connection, key: &str, default: i64) -> Result<i64, Error> {
    let Some(value) = optional_value(conn, key)? else {
        return Ok(default);
    };
    value
        .parse::<i64>()
        .map_err(|_| Error::InvalidSetting(format!("invalid {key}: {value}")))
}

fn now(conn: &Connection) -> Result<String, Error> {
    let ts = conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
        row.get(0)
    })?;
    Ok(ts)
}
