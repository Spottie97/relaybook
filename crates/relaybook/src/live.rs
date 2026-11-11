use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::stream::Piece;

pub(crate) struct Session {
    quiet: bool,
    color: bool,
    width: usize,
    log: Option<File>,
    saw_text: bool,
    spinner: Option<Spinner>,
}

impl Session {
    pub(crate) fn open(root: &Path, run_id: i64, quiet: bool) -> Self {
        let path = log_path(root, run_id);
        let log = if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
            OpenOptions::new().create(true).append(true).open(path).ok()
        } else {
            None
        };
        let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self {
            quiet,
            color,
            width: columns(),
            log,
            saw_text: false,
            spinner: Spinner::start(color),
        }
    }

    pub(crate) fn begin(&mut self, title: &str) {
        self.saw_text = false;
        self.emit(title, false, false);
    }

    pub(crate) fn observe(&mut self, piece: &Piece) {
        match piece {
            Piece::Tool(label) => self.emit(&format!("  ⎿ {label}"), true, false),
            Piece::Text(line) => {
                if self.saw_text {
                    return;
                }
                self.saw_text = true;
                let first = line.lines().next().unwrap_or(line);
                self.emit(&format!("  {first}"), true, true);
            }
        }
    }

    pub(crate) fn end(&mut self, summary: &str) {
        self.emit(summary, false, false);
    }

    fn emit(&mut self, text: &str, skip_when_quiet: bool, dim: bool) {
        let plain = fit(text, self.width);
        if let Some(log) = self.log.as_mut() {
            let _ = writeln!(log, "{plain}");
            let _ = log.flush();
        }
        if self.quiet && skip_when_quiet {
            return;
        }
        if dim && self.color {
            println!("\u{1b}[2m{plain}\u{1b}[0m");
        } else {
            println!("{plain}");
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.spinner.take();
    }
}

pub(crate) fn log_path(root: &Path, run_id: i64) -> PathBuf {
    root.join(".relaybook")
        .join("runs")
        .join(run_id.to_string())
        .join("live.log")
}

pub(crate) fn tail(root: &Path, run_id: i64, count: usize) -> Vec<String> {
    let Ok(text) = fs::read_to_string(log_path(root, run_id)) else {
        return Vec::new();
    };
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let start = lines.len().saturating_sub(count);
    lines[start..].to_vec()
}

pub(crate) fn follow(path: &Path, mut finished: impl FnMut() -> Option<String>) -> io::Result<()> {
    let mut seen = String::new();
    loop {
        if path.is_file() {
            let text = fs::read_to_string(path)?;
            if let Some(fresh) = text.strip_prefix(&seen) {
                print!("{fresh}");
                let _ = io::stdout().flush();
                seen = text;
            }
        }
        if let Some(message) = finished() {
            println!("{message}");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn fit(line: &str, width: usize) -> String {
    let width = width.max(8);
    if line.chars().count() <= width {
        return line.to_string();
    }
    let mut out: String = line.chars().take(width - 1).collect();
    out.push('…');
    out
}

fn columns() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value: &usize| *value >= 8)
        .unwrap_or(80)
}

struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Spinner {
    fn start(enabled: bool) -> Option<Self> {
        if !enabled {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let started = Instant::now();
        let handle = thread::spawn(move || {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut index = 0_usize;
            while !flag.load(Ordering::Relaxed) {
                let seconds = started.elapsed().as_secs();
                eprint!("\r{} {seconds}s", frames[index % frames.len()]);
                let _ = io::stderr().flush();
                index += 1;
                thread::sleep(Duration::from_millis(120));
            }
            eprint!("\r\u{1b}[2K");
            let _ = io::stderr().flush();
        });
        Some(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn long_lines_shrink_to_the_terminal_width() {
        let line = "  ⎿ Bash cargo test --all-targets --offline";
        let fitted = fit(line, 16);
        assert_eq!(fitted.chars().count(), 16);
        assert!(fitted.ends_with('…'));
    }
}
