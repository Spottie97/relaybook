//! Pause one git subcommand so a test can mutate the worktree while Relaybook
//! holds its ledger write lock. `RELAYBOOK_GIT_GATE_CMD` selects that
//! subcommand and defaults to `status`. Other git commands run immediately.

use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::process::{self, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    if env::var_os("RELAYBOOK_GIT_HOLD_STDERR_CHILD").is_some() {
        // Detached descendant: keep the inherited stderr open until killed.
        thread::sleep(Duration::from_secs(60));
        process::exit(0);
    }
    let gate = env_var("RELAYBOOK_GIT_GATE");
    let real = env_var("RELAYBOOK_REAL_GIT");
    let real_path = env::var_os("RELAYBOOK_REAL_PATH");
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    log_env(&args);
    hold_stderr(&args);
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
    command.env_remove("RELAYBOOK_GIT_ENV_LOG");
    match command.status() {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("git shim: {err}");
            process::exit(2);
        }
    }
}

fn log_env(args: &[OsString]) {
    let Some(path) = env::var_os("RELAYBOOK_GIT_ENV_LOG") else {
        return;
    };
    let prompt = env::var("GIT_TERMINAL_PROMPT").unwrap_or_else(|_| "unset".to_string());
    let stdin = stdin_target();
    let argv = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let line = format!("GIT_TERMINAL_PROMPT={prompt} stdin={stdin} argv={argv}\n");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

fn hold_stderr(args: &[OsString]) {
    let Some(pid_path) = env::var_os("RELAYBOOK_GIT_HOLD_STDERR") else {
        return;
    };
    if !args.iter().any(|arg| arg == "ls-remote") {
        return;
    }
    spawn_holder(&pid_path);
}

fn spawn_holder(pid_path: &std::ffi::OsStr) {
    let exe = match env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            eprintln!("git shim: {err}");
            process::exit(2);
        }
    };
    let mut child = Command::new(exe);
    child
        .env("RELAYBOOK_GIT_HOLD_STDERR_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            let _ = child.pre_exec(|| {
                extern "C" {
                    fn setsid() -> i32;
                }
                if setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    match child.spawn() {
        Ok(child) => {
            if fs::write(pid_path, child.id().to_string()).is_err() {
                eprintln!("git shim: could not record holder pid");
                process::exit(2);
            }
        }
        Err(err) => {
            eprintln!("git shim: {err}");
            process::exit(2);
        }
    }
}

fn stdin_target() -> String {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return "tty".to_string();
    }
    #[cfg(unix)]
    {
        if unix_stdin_is_dev_null() {
            return "/dev/null".to_string();
        }
    }
    "not-a-tty".to_string()
}

/// True when fd 0 is the null device. Linux exposes that through procfs;
/// other Unix systems expose it through `/dev/fd` or a device-id match.
#[cfg(unix)]
fn unix_stdin_is_dev_null() -> bool {
    use std::os::unix::fs::MetadataExt;
    for path in ["/proc/self/fd/0", "/dev/fd/0"] {
        if let Ok(target) = fs::read_link(path) {
            if target.as_os_str() == "/dev/null" {
                return true;
            }
        }
    }
    let Ok(null_meta) = fs::metadata("/dev/null") else {
        return false;
    };
    for path in ["/proc/self/fd/0", "/dev/fd/0"] {
        if let Ok(meta) = fs::metadata(path) {
            if meta.dev() == null_meta.dev() && meta.ino() == null_meta.ino() {
                return true;
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if macos_fd_path(0).as_deref() == Some("/dev/null") {
            return true;
        }
    }
    false
}

#[cfg(target_os = "macos")]
fn macos_fd_path(fd: i32) -> Option<String> {
    extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }
    const F_GETPATH: i32 = 50;
    let mut buf = [0u8; 1024];
    let rc = unsafe { fcntl(fd, F_GETPATH, buf.as_mut_ptr()) };
    if rc == -1 {
        return None;
    }
    let len = buf.iter().position(|byte| *byte == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..len]).into_owned())
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
