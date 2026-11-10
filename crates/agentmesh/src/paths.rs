use std::path::{Path, PathBuf};

use crate::gitinfo;

pub(crate) fn ledger_path(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let candidate = current.join(".agentmesh").join("ledger.db");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

pub(crate) fn init_root(cwd: &Path) -> PathBuf {
    gitinfo::toplevel(cwd).unwrap_or_else(|| cwd.to_path_buf())
}

pub(crate) fn install_root(cwd: &Path) -> PathBuf {
    ledger_path(cwd)
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| init_root(cwd))
}
