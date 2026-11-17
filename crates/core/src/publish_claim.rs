//! Pid, host, and heartbeat recorded when a gated publish claim is taken.
//!
//! A pid check can be fooled: pid namespaces, a reused pid when the start
//! time cannot be read, and a process that exits between the check and the
//! write. The lease is a clock. A live publisher refreshes it; a stopped
//! heartbeat expires. Neither check is proof that a push did not happen.

use std::path::Path;
use std::process::Command;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Error;

/// How long a publish claim stays valid without a heartbeat refresh.
/// Documented in `docs/auto-mode.md`. A live publisher refreshes more often
/// than this.
pub const PUBLISH_LEASE_SECS: i64 = 90;

pub(crate) const IN_PROGRESS: &str = "a publish claim is in progress";
pub(crate) const COMPLETE_HELD: &str =
    "a publish claim is in progress; complete does not change it";
pub(crate) const PUBLISHER_ALIVE: &str = "publisher is still running";
pub(crate) const LEASE_VALID: &str = "publish lease is still valid";
pub(crate) const NO_LEASE: &str = "publish claim has no lease";

pub(crate) struct Assessment {
    pub recoverable: bool,
    pub reason: String,
    pub pid: i64,
    pub host: String,
    pub process_start: String,
    pub heartbeat_at: String,
    pub same_host: bool,
    pub pid_state: &'static str,
    pub lease_expired: bool,
}

struct Identity {
    pid: i64,
    host: String,
    process_start: String,
}

enum PidState {
    Alive,
    Dead,
    Reused,
    Unchecked,
}

impl PidState {
    fn label(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Dead => "dead",
            Self::Reused => "reused",
            Self::Unchecked => "unchecked",
        }
    }
}

pub(crate) fn stage_is_publishing(conn: &Connection) -> Result<bool, Error> {
    let found = conn
        .query_row(
            "SELECT 1 FROM runs
             WHERE workflow = 'gated' AND stage = 'publishing'
             LIMIT 1",
            [],
            |_| Ok(1_i64),
        )
        .optional()?;
    Ok(found.is_some())
}

/// True when this process is the one that took the claim.
pub(crate) fn same_publisher(conn: &Connection, run_id: i64) -> Result<bool, Error> {
    let Some(lease) = load(conn, run_id)? else {
        return Ok(false);
    };
    let identity = identity();
    if lease.pid != identity.pid || lease.host != identity.host {
        return Ok(false);
    }
    if !lease.process_start.is_empty()
        && !identity.process_start.is_empty()
        && lease.process_start != identity.process_start
    {
        return Ok(false);
    }
    Ok(true)
}

pub(crate) fn record(conn: &Connection, run_id: i64) -> Result<(), Error> {
    let identity = identity();
    let heartbeat = now(conn)?;
    conn.execute(
        "INSERT INTO publish_claims (run_id, pid, host, process_start, heartbeat_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(run_id) DO UPDATE SET
            pid = excluded.pid,
            host = excluded.host,
            process_start = excluded.process_start,
            heartbeat_at = excluded.heartbeat_at",
        params![
            run_id,
            identity.pid,
            identity.host,
            identity.process_start,
            heartbeat,
        ],
    )?;
    Ok(())
}

pub(crate) fn clear(conn: &Connection, run_id: i64) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM publish_claims WHERE run_id = ?1",
        params![run_id],
    )?;
    Ok(())
}

pub(crate) fn assess(conn: &Connection, run_id: i64) -> Result<Assessment, Error> {
    let Some(lease) = load(conn, run_id)? else {
        return Ok(Assessment {
            recoverable: false,
            reason: NO_LEASE.to_string(),
            pid: 0,
            host: String::new(),
            process_start: String::new(),
            heartbeat_at: String::new(),
            same_host: false,
            pid_state: "missing",
            lease_expired: false,
        });
    };
    let identity = identity();
    let same_host = lease.host == identity.host;
    let pid_state = if same_host {
        inspect_pid(lease.pid, &lease.process_start)
    } else {
        PidState::Unchecked
    };
    let lease_expired = heartbeat_expired(conn, &lease.heartbeat_at)?;
    let proven_gone = matches!(pid_state, PidState::Dead | PidState::Reused);
    // Same host: a dead or reused pid is enough. Any host: an expired lease
    // is enough. A different host cannot use the pid check.
    let recoverable = (same_host && proven_gone) || lease_expired;
    let reason = if recoverable {
        String::new()
    } else if same_host && matches!(pid_state, PidState::Alive) {
        PUBLISHER_ALIVE.to_string()
    } else {
        LEASE_VALID.to_string()
    };
    Ok(Assessment {
        recoverable,
        reason,
        pid: lease.pid,
        host: lease.host,
        process_start: lease.process_start,
        heartbeat_at: lease.heartbeat_at,
        same_host,
        pid_state: pid_state.label(),
        lease_expired,
    })
}

/// Refresh the heartbeat from a second connection. Does not touch `runs`.
pub fn touch_publish_lease(path: &Path, run_id: i64) -> Result<(), Error> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute(
        "UPDATE publish_claims
         SET heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE run_id = ?1",
        params![run_id],
    )?;
    Ok(())
}

struct Lease {
    pid: i64,
    host: String,
    process_start: String,
    heartbeat_at: String,
}

fn load(conn: &Connection, run_id: i64) -> Result<Option<Lease>, Error> {
    let row = conn
        .query_row(
            "SELECT pid, host, process_start, heartbeat_at
             FROM publish_claims WHERE run_id = ?1",
            params![run_id],
            |row| {
                Ok(Lease {
                    pid: row.get(0)?,
                    host: row.get(1)?,
                    process_start: row.get(2)?,
                    heartbeat_at: row.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn now(conn: &Connection) -> Result<String, Error> {
    let ts = conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
        row.get(0)
    })?;
    Ok(ts)
}

fn heartbeat_expired(conn: &Connection, heartbeat_at: &str) -> Result<bool, Error> {
    let expired: i64 = conn.query_row(
        "SELECT CASE
            WHEN julianday(?1) IS NULL THEN 1
            WHEN (julianday('now') - julianday(?1)) * 86400.0 >= ?2 THEN 1
            ELSE 0
         END",
        params![heartbeat_at, PUBLISH_LEASE_SECS],
        |row| row.get(0),
    )?;
    Ok(expired != 0)
}

fn identity() -> Identity {
    let pid = i64::from(std::process::id());
    Identity {
        pid,
        host: current_host(),
        process_start: process_start(pid).unwrap_or_default(),
    }
}

fn current_host() -> String {
    if let Ok(text) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let name = text.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    if let Ok(output) = Command::new("hostname").output()
        && output.status.success()
    {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    "unknown".to_string()
}

fn process_start(pid: i64) -> Option<String> {
    if let Some(start) = proc_start(pid) {
        return Some(start);
    }
    ps_lstart(pid)
}

fn inspect_pid(pid: i64, recorded_start: &str) -> PidState {
    if proc_available() {
        return match proc_start(pid) {
            Some(start) => compare_start(recorded_start, &start),
            None => PidState::Dead,
        };
    }
    match ps_lstart(pid) {
        Some(start) => compare_start(recorded_start, &start),
        None if ps_can_query() => PidState::Dead,
        None => PidState::Unchecked,
    }
}

fn compare_start(recorded: &str, current: &str) -> PidState {
    if !recorded.is_empty() && recorded != current {
        PidState::Reused
    } else {
        PidState::Alive
    }
}

fn proc_available() -> bool {
    Path::new("/proc/self/stat").is_file()
}

fn proc_start(pid: i64) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    start_ticks(&text)
}

/// Field 22 of `/proc/<pid>/stat`, after the comm field in parentheses.
pub(crate) fn start_ticks(stat: &str) -> Option<String> {
    let rest = stat.rsplit_once(')')?.1.trim();
    rest.split_whitespace().nth(19).map(str::to_string)
}

fn ps_lstart(pid: i64) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn ps_can_query() -> bool {
    Command::new("ps")
        .args(["-p", &std::process::id().to_string(), "-o", "pid="])
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(test)]
mod tests {
    use super::start_ticks;

    #[test]
    fn start_ticks_reads_field_22_after_comm() {
        let stat = "42 (a) b) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 555";
        assert_eq!(start_ticks(stat).as_deref(), Some("555"));
    }

    #[test]
    fn start_ticks_matches_this_process() {
        let Ok(text) = std::fs::read_to_string("/proc/self/stat") else {
            return;
        };
        let start = start_ticks(&text).expect("ticks");
        assert!(!start.is_empty());
        let again = std::fs::read_to_string("/proc/self/stat").expect("stat");
        assert_eq!(start_ticks(&again).as_deref(), Some(start.as_str()));
    }
}
