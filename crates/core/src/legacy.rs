use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::Error;

pub const DIR: &str = ".agentmesh";
pub const MARKER_START: &str = "<!-- agentmesh:start -->";
pub const MARKER_END: &str = "<!-- agentmesh:end -->";
pub const PLANNER_HEADING: &str = "## AgentMesh coordination (planner)";
pub const EXECUTOR_HEADING: &str = "## AgentMesh coordination (executor)";
pub const HOOK_START: &str = "# agentmesh:hooks:start";
pub const HOOK_END: &str = "# agentmesh:hooks:end";

const LEDGER_DIR: &str = ".relaybook";
const LEDGER_FILE: &str = "ledger.db";

pub struct LegacyMove {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Move `<root>/.agentmesh` to `<root>/.relaybook` when only the old folder has a ledger.
pub fn migrate_legacy_ledger(root: &Path) -> Result<Option<LegacyMove>, Error> {
    let new_dir = root.join(LEDGER_DIR);
    let new_db = new_dir.join(LEDGER_FILE);
    if new_db.is_file() {
        return Ok(None);
    }
    let old_dir = root.join(DIR);
    let old_db = old_dir.join(LEDGER_FILE);
    if !old_db.is_file() {
        return Ok(None);
    }
    checkpoint(&old_db)?;
    rename_dir(&old_dir, &new_dir)?;
    Ok(Some(LegacyMove {
        from: old_db,
        to: new_db,
    }))
}

fn rename_dir(from: &Path, to: &Path) -> Result<(), Error> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    std::fs::rename(from, to)?;
    Ok(())
}

fn checkpoint(database: &Path) -> Result<(), Error> {
    let connection = Connection::open(database)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let _: (i64, i64, i64) =
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::migrate_legacy_ledger;
    use crate::Ledger;
    use rusqlite::Connection;

    #[test]
    fn moves_a_checkpointed_ledger() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = dir.path().join(".agentmesh").join("ledger.db");
        let mut ledger = Ledger::initialize(&old).expect("init");
        ledger.create_task("auth", "Add auth").expect("create");
        drop(ledger);

        let moved = migrate_legacy_ledger(dir.path())
            .expect("move")
            .expect("moved");
        assert!(!dir.path().join(".agentmesh").exists());
        assert_eq!(moved.to, dir.path().join(".relaybook").join("ledger.db"));
        let ledger = Ledger::open(&moved.to).expect("open");
        let tasks = ledger.list_tasks(None, false, true).expect("list");
        assert_eq!(tasks[0].slug, "auth");
        assert!(migrate_legacy_ledger(dir.path()).expect("again").is_none());
    }

    #[test]
    fn moves_a_ledger_that_still_has_wal_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old_dir = dir.path().join(".agentmesh");
        std::fs::create_dir_all(&old_dir).expect("mkdir");
        let old_db = old_dir.join("ledger.db");
        let connection = Connection::open(&old_db).expect("open");
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA wal_autocheckpoint=0;
                 CREATE TABLE sample (id INTEGER);
                 INSERT INTO sample (id) VALUES (7);",
            )
            .expect("write");
        let names: Vec<_> = std::fs::read_dir(&old_dir)
            .expect("read")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert!(
            old_dir.join("ledger.db-wal").is_file(),
            "expected a WAL sidecar, found {names:?}"
        );

        let moved = migrate_legacy_ledger(dir.path())
            .expect("move")
            .expect("moved");
        drop(connection);
        assert!(!old_dir.exists());
        let reopened = Connection::open(&moved.to).expect("reopen");
        let id: i64 = reopened
            .query_row("SELECT id FROM sample", [], |row| row.get(0))
            .expect("row");
        assert_eq!(id, 7);
    }
}
