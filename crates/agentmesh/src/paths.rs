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

#[cfg(test)]
mod tests {
    use super::{init_root, install_root, ledger_path};

    #[test]
    fn ledger_in_a_parent_sets_the_install_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let nested = root.join("crates").join("agentmesh");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let database = root.join(".agentmesh").join("ledger.db");
        std::fs::create_dir_all(database.parent().expect("parent")).expect("mkdir");
        std::fs::write(&database, b"db").expect("write");

        assert_eq!(ledger_path(&nested).expect("ledger"), database);
        assert_eq!(install_root(&nested), root);
    }

    #[test]
    fn missing_ledger_falls_back_to_the_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(ledger_path(dir.path()), None);
        assert_eq!(init_root(dir.path()), dir.path());
        assert_eq!(install_root(dir.path()), dir.path());
    }
}
