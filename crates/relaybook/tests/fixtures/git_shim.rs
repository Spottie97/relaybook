//! Pause one git subcommand so a test can mutate the worktree while Relaybook
//! holds its ledger write lock. `RELAYBOOK_GIT_GATE_CMD` selects that
//! subcommand and defaults to `status`. Other git commands run immediately.

use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{self, Command};
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    let gate = env_var("RELAYBOOK_GIT_GATE");
    let real = env_var("RELAYBOOK_REAL_GIT");
    let real_path = env::var_os("RELAYBOOK_REAL_PATH");
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let gate_cmd = env::var("RELAYBOOK_GIT_GATE_CMD").unwrap_or_else(|_| "status".to_string());
    if args.iter().any(|arg| arg == gate_cmd.as_str()) {
        let n = claim_index(&gate);
        let dir = PathBuf::from(&gate);
        if let Err(err) = fs::write(dir.join(format!("entered-{n}")), b"1") {
            eprintln!("git shim: {err}");
            process::exit(2);
        }
        let release = dir.join(format!("release-{n}"));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !release.exists() {
            if Instant::now() >= deadline {
                eprintln!("git shim: timed out waiting for release-{n}");
                process::exit(2);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    let mut command = Command::new(&real);
    command.args(&args);
    if let Some(path) = real_path {
        command.env("PATH", path);
    }
    command.env_remove("RELAYBOOK_GIT_GATE");
    command.env_remove("RELAYBOOK_GIT_GATE_CMD");
    command.env_remove("RELAYBOOK_REAL_GIT");
    command.env_remove("RELAYBOOK_REAL_PATH");
    match command.status() {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("git shim: {err}");
            process::exit(2);
        }
    }
}

fn env_var(name: &str) -> OsString {
    match env::var_os(name) {
        Some(value) => value,
        None => {
            eprintln!("git shim: {name} is not set");
            process::exit(2);
        }
    }
}

fn claim_index(gate: &OsString) -> u32 {
    let dir = PathBuf::from(gate);
    for n in 1..64 {
        let path = dir.join(format!("claim-{n}"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return n,
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => {
                eprintln!("git shim: {err}");
                process::exit(2);
            }
        }
    }
    eprintln!("git shim: too many status calls");
    process::exit(2);
}
