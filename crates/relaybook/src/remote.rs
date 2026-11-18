//! Gated publish check.
//!
//! `git ls-remote` runs before a ledger write. The recheck inside the write
//! transaction uses local `refs/remotes` only, so the lock is not held across
//! the network.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use relaybook_core::Error;

use crate::gitinfo;

pub(crate) struct RemoteView {
    pub branch: String,
    pub head_sha: String,
    pub remote: Option<String>,
    pub status: &'static str,
    pub detail: String,
    pub message: Option<String>,
}

struct ListedRef {
    sha: String,
    name: String,
}

struct LocalRef {
    sha: String,
    refname: String,
    remote: String,
    branch: String,
}

struct OnlineQuery {
    name: String,
    result: OnlineResult,
    stderr: String,
}

enum OnlineResult {
    Refs(Vec<ListedRef>),
    Failed { timed_out: bool },
}

/// Online check, or the local-only downgrade when `remote_check` is `local`.
pub(crate) fn screen(root: &Path, remote_check: &str) -> Result<RemoteView, Error> {
    let branch = require_branch(root)?;
    let head = gitinfo::verified_head(root)
        .ok_or_else(|| Error::InvalidSetting("a commit at HEAD is required".to_string()))?;
    let range = interesting_commits(root, &head);
    let local = local_refs(root)?;
    let listed = remote_names(root);
    let remotes = listed.as_ref().cloned().unwrap_or_default();
    let urls: Vec<(String, String)> = remotes
        .iter()
        .filter_map(|name| fetch_url(root, name).map(|url| (name.clone(), url)))
        .collect();
    let push_differs = remotes.iter().any(|name| push_url_differs(root, name));
    let empty = BTreeSet::new();
    let interesting = range.as_ref().unwrap_or(&empty);
    let check = Check {
        branch: &branch,
        head: &head,
        interesting,
        local: &local,
        online: &[],
        local_only: remote_check == "local",
        push_differs,
        urls: &urls,
    };
    if let Err(message) = &range {
        // Branch-name evidence does not need the commit list. Anything else
        // must not be reported as absent when that list could not be built.
        if let Some((remote, hit)) = local_hit(&branch, interesting, &local, None) {
            return Ok(finish(&check, remote, "present", Some(hit)));
        }
        return Ok(finish(
            &check,
            None,
            "unknown",
            Some(format!("remote check failed: {message}")),
        ));
    }
    if remote_check == "local" {
        return Ok(decide(&check));
    }
    if let Err(message) = listed {
        let local_view = decide(&check);
        if local_view.status == "present" {
            return Ok(local_view);
        }
        return Ok(finish(
            &check,
            None,
            "unknown",
            Some(format!("remote check failed: {message}")),
        ));
    }
    let mut online = Vec::with_capacity(remotes.len());
    for name in &remotes {
        online.push(query_remote(root, name));
    }
    Ok(decide(&Check {
        online: &online,
        local_only: false,
        ..check
    }))
}

struct Check<'a> {
    branch: &'a str,
    head: &'a str,
    interesting: &'a BTreeSet<String>,
    local: &'a [LocalRef],
    online: &'a [OnlineQuery],
    local_only: bool,
    push_differs: bool,
    urls: &'a [(String, String)],
}

/// Whether `refs/heads/<branch>` is already on a remote, or a local
/// `refs/remotes/*/<branch>` still names it.
///
/// This is the run-branch half of the publish check. It does not treat commits
/// that are already on `main` as a publish of the new branch.
pub(crate) fn screen_named_branch(
    root: &Path,
    branch: &str,
    remote_check: &str,
) -> Result<RemoteView, Error> {
    let head = gitinfo::verified_head(root).unwrap_or_else(|| "none".to_string());
    let local = local_refs(root)?;
    let listed = remote_names(root);
    let remotes = listed.as_ref().cloned().unwrap_or_default();
    let urls: Vec<(String, String)> = remotes
        .iter()
        .filter_map(|name| fetch_url(root, name).map(|url| (name.clone(), url)))
        .collect();
    let push_differs = remotes.iter().any(|name| push_url_differs(root, name));
    let local_only = remote_check == "local";
    let check = NamedCheck {
        branch,
        head: &head,
        push_differs,
        urls: &urls,
    };
    if local_only {
        if let Some((remote, message)) = named_branch_local(&local, branch, None) {
            return Ok(check.view(Some(remote), "present", Some(message), &[]));
        }
        return Ok(check.view(None, "local", None, &[]));
    }
    if let Err(message) = &listed {
        if let Some((remote, hit)) = named_branch_local(&local, branch, None) {
            return Ok(check.view(Some(remote), "present", Some(hit), &[]));
        }
        return Ok(check.view(
            None,
            "unknown",
            Some(format!("remote check failed: {message}")),
            &[],
        ));
    }
    let online: Vec<OnlineQuery> = remotes
        .iter()
        .map(|name| query_remote(root, name))
        .collect();
    if let Some((remote, message)) = named_branch_local(&local, branch, Some(&online)) {
        return Ok(check.view(Some(remote), "present", Some(message), &online));
    }
    if let Some((remote, message)) = named_branch_online(&online, branch) {
        return Ok(check.view(Some(remote), "present", Some(message), &online));
    }
    if let Some((remote, message)) = online_unknown(&online) {
        return Ok(check.view(Some(remote), "unknown", Some(message), &online));
    }
    Ok(check.view(None, "absent", None, &online))
}

/// Sorted `sha refname` lines for every `refs/remotes` entry.
pub(crate) fn tracking_ref_lines(root: &Path) -> Result<Vec<String>, String> {
    let text = git_stdout(
        root,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "--end-of-options",
            "refs/remotes",
        ],
    )?;
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    lines.sort();
    Ok(lines)
}

struct NamedCheck<'a> {
    branch: &'a str,
    head: &'a str,
    push_differs: bool,
    urls: &'a [(String, String)],
}

impl NamedCheck<'_> {
    fn view(
        &self,
        remote: Option<String>,
        status: &'static str,
        message: Option<String>,
        online: &[OnlineQuery],
    ) -> RemoteView {
        let message = message.map(|text| redact_userinfo(&text));
        RemoteView {
            branch: self.branch.to_string(),
            head_sha: self.head.to_string(),
            remote,
            status,
            detail: detail(self.push_differs, self.urls, message.as_deref(), online),
            message,
        }
    }
}

fn named_branch_local(
    local: &[LocalRef],
    branch: &str,
    online: Option<&[OnlineQuery]>,
) -> Option<(String, String)> {
    let hits: Vec<&LocalRef> = local.iter().filter(|hit| hit.branch == branch).collect();
    if let Some(hit) = hits
        .iter()
        .find(|hit| online.is_some_and(|rows| remote_branch_missing(rows, &hit.remote, branch)))
    {
        return Some((
            hit.remote.clone(),
            "published earlier; deleting the remote branch does not unpublish it".to_string(),
        ));
    }
    hits.first()
        .map(|hit| (hit.remote.clone(), format!("{} exists", hit.refname)))
}

fn named_branch_online(online: &[OnlineQuery], branch: &str) -> Option<(String, String)> {
    let branch_ref = format!("refs/heads/{branch}");
    for row in online {
        let OnlineResult::Refs(refs) = &row.result else {
            continue;
        };
        if refs.iter().any(|item| item.name == branch_ref) {
            return Some((
                row.name.clone(),
                format!("{branch_ref} exists on {}", row.name),
            ));
        }
    }
    None
}

/// Local half of the check. Safe to call while the ledger write lock is held.
pub(crate) fn refuse_local(root: &Path) -> Result<(), Error> {
    let branch = require_branch(root)?;
    let interesting = match gitinfo::verified_head(root) {
        Some(head) => interesting_commits(root, &head).map_err(Error::InvalidSetting)?,
        None => BTreeSet::new(),
    };
    let local = local_refs(root)?;
    if let Some((_, message)) = local_hit(&branch, &interesting, &local, None) {
        return Err(Error::InvalidSetting(message));
    }
    Ok(())
}

/// Release-range commits, plus `HEAD`.
///
/// The range excludes the merge base, so an already published `main` does not
/// by itself mark a feature branch published. `HEAD` is included because that
/// range is empty when the branch is `main`. A missing `main` or a failed
/// `rev-list` is an error: checking only `HEAD` would hide an earlier commit
/// published under another name.
fn interesting_commits(root: &Path, head: &str) -> Result<BTreeSet<String>, String> {
    let range = gitinfo::release_range(root)?;
    let mut commits = BTreeSet::from([head.to_string()]);
    commits.extend(range.commits);
    commits.insert(range.head);
    Ok(commits)
}

fn decide(check: &Check<'_>) -> RemoteView {
    let online = if check.local_only {
        None
    } else {
        Some(check.online)
    };
    if let Some((remote, message)) = local_hit(check.branch, check.interesting, check.local, online)
    {
        return finish(check, remote, "present", Some(message));
    }
    if let Some((remote, message)) = online_present(check.online, check.branch, check.interesting) {
        return finish(check, Some(remote), "present", Some(message));
    }
    if let Some((remote, message)) = online_unknown(check.online) {
        return finish(check, Some(remote), "unknown", Some(message));
    }
    let status = if check.local_only { "local" } else { "absent" };
    finish(check, None, status, None)
}

fn finish(
    check: &Check<'_>,
    remote: Option<String>,
    status: &'static str,
    message: Option<String>,
) -> RemoteView {
    let message = message.map(|text| redact_userinfo(&text));
    RemoteView {
        branch: check.branch.to_string(),
        head_sha: check.head.to_string(),
        remote,
        status,
        detail: detail(
            check.push_differs,
            check.urls,
            message.as_deref(),
            check.online,
        ),
        message,
    }
}

fn detail(
    push_differs: bool,
    urls: &[(String, String)],
    message: Option<&str>,
    online: &[OnlineQuery],
) -> String {
    let flag = if push_differs { "true" } else { "false" };
    let mut lines = vec![format!("push_url_differs={flag}")];
    for (name, url) in urls {
        lines.push(format!("url.{name}={}", redact_userinfo(url)));
    }
    if let Some(message) = message {
        lines.push(message.to_string());
    }
    for query in online {
        if query.stderr.is_empty() {
            continue;
        }
        let err = redact_userinfo(&query.stderr).replace(['\n', '\r'], " ");
        lines.push(format!("stderr.{}={}", query.name, trim_chars(&err, 400)));
    }
    let mut text = lines.join("\n");
    text.push('\n');
    truncate_utf8(&mut text, 4000);
    text
}

fn local_hit(
    branch: &str,
    interesting: &BTreeSet<String>,
    local: &[LocalRef],
    online: Option<&[OnlineQuery]>,
) -> Option<(Option<String>, String)> {
    let tracking: Vec<&LocalRef> = local.iter().filter(|hit| hit.branch == branch).collect();
    if let Some(hit) = tracking
        .iter()
        .find(|hit| online.is_some_and(|rows| remote_branch_missing(rows, &hit.remote, branch)))
    {
        return Some((
            Some(hit.remote.clone()),
            "published earlier; deleting the remote branch does not unpublish it".to_string(),
        ));
    }
    if let Some(hit) = tracking.first() {
        return Some((Some(hit.remote.clone()), format!("{} exists", hit.refname)));
    }
    for hit in local {
        if interesting.contains(&hit.sha) {
            return Some((
                Some(hit.remote.clone()),
                format!("published {} as {}", hit.sha, hit.refname),
            ));
        }
    }
    None
}

fn remote_branch_missing(online: &[OnlineQuery], remote: &str, branch: &str) -> bool {
    let branch_ref = format!("refs/heads/{branch}");
    online.iter().any(|row| match &row.result {
        OnlineResult::Refs(refs) if row.name == remote => {
            refs.iter().all(|item| item.name != branch_ref)
        }
        _ => false,
    })
}

fn online_present(
    online: &[OnlineQuery],
    branch: &str,
    interesting: &BTreeSet<String>,
) -> Option<(String, String)> {
    for row in online {
        let OnlineResult::Refs(refs) = &row.result else {
            continue;
        };
        if let Some(message) = present_message(&row.name, refs, branch, interesting) {
            return Some((row.name.clone(), message));
        }
    }
    None
}

fn present_message(
    remote: &str,
    refs: &[ListedRef],
    branch: &str,
    interesting: &BTreeSet<String>,
) -> Option<String> {
    let branch_ref = format!("refs/heads/{branch}");
    if refs.iter().any(|item| item.name == branch_ref) {
        return Some(format!("{branch_ref} exists on {remote}"));
    }
    let mut head = None;
    for item in refs {
        if !interesting.contains(&item.sha) {
            continue;
        }
        if item.name == "HEAD" {
            head = Some(item);
            continue;
        }
        return Some(format!(
            "published {} as {} on {remote}",
            item.sha, item.name
        ));
    }
    head.map(|item| format!("published {} as HEAD on {remote}", item.sha))
}

fn online_unknown(online: &[OnlineQuery]) -> Option<(String, String)> {
    let mut names = Vec::new();
    let mut messages = Vec::new();
    for row in online {
        let OnlineResult::Failed { timed_out } = row.result else {
            continue;
        };
        names.push(row.name.clone());
        if timed_out {
            messages.push(format!("remote {}: timed out", row.name));
        } else {
            messages.push(format!("remote {}: ls-remote failed", row.name));
        }
    }
    let remote = names.into_iter().next()?;
    Some((remote, messages.join("; ")))
}

fn query_remote(root: &Path, name: &str) -> OnlineQuery {
    match ls_remote(root, name) {
        LsRemote::TimedOut => OnlineQuery {
            name: name.to_string(),
            result: OnlineResult::Failed { timed_out: true },
            stderr: String::new(),
        },
        LsRemote::Failed(stderr) => OnlineQuery {
            name: name.to_string(),
            result: OnlineResult::Failed { timed_out: false },
            stderr,
        },
        LsRemote::Ok(stdout) => match parse_ls_remote(&stdout) {
            Some(refs) => OnlineQuery {
                name: name.to_string(),
                result: OnlineResult::Refs(refs),
                stderr: String::new(),
            },
            None => OnlineQuery {
                name: name.to_string(),
                result: OnlineResult::Failed { timed_out: false },
                stderr: "unreadable ls-remote output".to_string(),
            },
        },
    }
}

enum LsRemote {
    Ok(String),
    TimedOut,
    Failed(String),
}

fn ls_remote(root: &Path, name: &str) -> LsRemote {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["ls-remote", "--quiet", "--end-of-options"])
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0");
    // Own process group so the timeout can signal git and helpers it spawned.
    // A helper that calls setsid escapes that group; pipe reads are still bounded.
    isolate_process_group(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return LsRemote::Failed(err.to_string()),
    };
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_thread = thread::spawn(move || read_pipe(stdout));
    let stderr_thread = thread::spawn(move || read_pipe(stderr));
    let deadline = Instant::now() + remote_timeout();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (stdout, stderr) = collect_output(stdout_thread, stderr_thread, deadline);
                return classify_exit(status.success(), stdout, stderr);
            }
            Ok(None) if Instant::now() >= deadline => {
                // Kill while this pid is still the live group leader. Reaping
                // first would let that pid be reused before the group signal.
                stop_process_group(pid);
                let _ = child.kill();
                reap_child(&mut child, Duration::from_millis(500));
                abandon_output(stdout_thread, stderr_thread, Duration::from_millis(300));
                return LsRemote::TimedOut;
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(err) => {
                stop_process_group(pid);
                let _ = child.kill();
                reap_child(&mut child, Duration::from_millis(500));
                abandon_output(stdout_thread, stderr_thread, Duration::from_millis(300));
                return LsRemote::Failed(err.to_string());
            }
        }
    }
}

fn isolate_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

fn stop_process_group(pid: u32) {
    #[cfg(unix)]
    {
        if pid == 0 {
            return;
        }
        let Ok(pid) = i32::try_from(pid) else {
            return;
        };
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        const SIGKILL: i32 = 9;
        // SAFETY: `kill` with SIGKILL targets the process group of the child
        // spawned above. The pid is still alive, so it has not been reused.
        unsafe {
            let _ = kill(-pid, SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

fn reap_child(child: &mut Child, bound: Duration) {
    let deadline = Instant::now() + bound;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if Instant::now() >= deadline => return,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Read both pipes until `deadline`. A reader that is still blocked is
/// detached so a descendant holding the write end cannot stall the check.
fn collect_output(
    stdout_thread: thread::JoinHandle<Vec<u8>>,
    stderr_thread: thread::JoinHandle<Vec<u8>>,
    deadline: Instant,
) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut stdout_thread = Some(stdout_thread);
    let mut stderr_thread = Some(stderr_thread);
    let mut stdout = None;
    let mut stderr = None;
    loop {
        if stdout.is_none()
            && stdout_thread
                .as_ref()
                .is_some_and(|handle| handle.is_finished())
        {
            stdout = stdout_thread.take().and_then(|handle| handle.join().ok());
        }
        if stderr.is_none()
            && stderr_thread
                .as_ref()
                .is_some_and(|handle| handle.is_finished())
        {
            stderr = stderr_thread.take().and_then(|handle| handle.join().ok());
        }
        if stdout_thread.is_none() && stderr_thread.is_none() {
            return (stdout, stderr);
        }
        if Instant::now() >= deadline {
            return (stdout, stderr);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn abandon_output(
    stdout_thread: thread::JoinHandle<Vec<u8>>,
    stderr_thread: thread::JoinHandle<Vec<u8>>,
    bound: Duration,
) {
    let _ = collect_output(stdout_thread, stderr_thread, Instant::now() + bound);
}

fn classify_exit(success: bool, stdout: Option<Vec<u8>>, stderr: Option<Vec<u8>>) -> LsRemote {
    let stderr = stderr
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
        .unwrap_or_default();
    match stdout {
        Some(bytes) if success => match String::from_utf8(bytes) {
            Ok(text) => LsRemote::Ok(text),
            Err(_) => LsRemote::Failed(stderr),
        },
        Some(_) => LsRemote::Failed(stderr),
        None => LsRemote::Failed(if stderr.is_empty() {
            "ls-remote output was not read before the deadline".to_string()
        } else {
            stderr
        }),
    }
}

fn remote_timeout() -> Duration {
    match std::env::var("RELAYBOOK_REMOTE_TIMEOUT_SECS") {
        Ok(value) => match value.parse::<u64>() {
            Ok(seconds) => Duration::from_secs(seconds),
            Err(_) => Duration::from_secs(20),
        },
        Err(_) => Duration::from_secs(20),
    }
}

fn read_pipe(pipe: Option<impl Read>) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    buf
}

fn parse_ls_remote(text: &str) -> Option<Vec<ListedRef>> {
    let mut refs = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (sha, name) = line.split_once('\t').or_else(|| line.split_once(' '))?;
        if sha.len() < 4 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        refs.push(ListedRef {
            sha: sha.to_string(),
            name: name.trim().to_string(),
        });
    }
    Some(refs)
}

fn local_refs(root: &Path) -> Result<Vec<LocalRef>, Error> {
    let remotes = remote_names(root).unwrap_or_default();
    let text = git_stdout(
        root,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "--end-of-options",
            "refs/remotes",
        ],
    )
    .map_err(|_| Error::InvalidSetting("git for-each-ref failed".to_string()))?;
    let mut refs = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((sha, refname)) = line.split_once(' ') else {
            return Err(Error::InvalidSetting("git for-each-ref failed".to_string()));
        };
        let Some((remote, branch)) = split_remote_ref(refname, &remotes) else {
            continue;
        };
        // for-each-ref listed the name. Confirm it still resolves. A missing
        // name is not a publish; a git failure is.
        if !gitinfo::rev_exists(root, refname).map_err(Error::InvalidSetting)? {
            continue;
        }
        refs.push(LocalRef {
            sha: sha.to_string(),
            refname: refname.to_string(),
            remote,
            branch,
        });
    }
    Ok(refs)
}

/// Split `refs/remotes/<remote>/<branch>`.
///
/// Configured remote names win, longest first, so `team/upstream` is not read
/// as remote `team`. A ref whose remote is no longer configured keeps the
/// first-slash split, so a stale `refs/remotes/origin/main` still refuses.
fn split_remote_ref(name: &str, remotes: &[String]) -> Option<(String, String)> {
    let rest = name.strip_prefix("refs/remotes/")?;
    let mut best: Option<(&str, &str)> = None;
    for remote in remotes {
        let Some(after) = rest.strip_prefix(remote.as_str()) else {
            continue;
        };
        let Some(branch) = after.strip_prefix('/') else {
            continue;
        };
        if branch.is_empty() {
            continue;
        }
        let replace = best.is_none_or(|(current, _)| remote.len() > current.len());
        if replace {
            best = Some((remote.as_str(), branch));
        }
    }
    if let Some((remote, branch)) = best {
        return Some((remote.to_string(), branch.to_string()));
    }
    let (remote, branch) = rest.split_once('/')?;
    if remote.is_empty() || branch.is_empty() {
        return None;
    }
    Some((remote.to_string(), branch.to_string()))
}

fn remote_names(root: &Path) -> Result<Vec<String>, String> {
    let text = git_stdout(root, &["remote"])?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn push_url_differs(root: &Path, name: &str) -> bool {
    match (fetch_url(root, name), push_url(root, name)) {
        (Some(fetch), Some(push)) => fetch != push,
        _ => false,
    }
}

fn fetch_url(root: &Path, name: &str) -> Option<String> {
    git_line(root, &["remote", "get-url", "--end-of-options", name])
}

fn push_url(root: &Path, name: &str) -> Option<String> {
    git_line(
        root,
        &["remote", "get-url", "--push", "--end-of-options", name],
    )
}

fn git_line(root: &Path, args: &[&str]) -> Option<String> {
    git_stdout(root, args)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        if detail.is_empty() {
            return Err("git command failed".to_string());
        }
        return Err(detail.to_string());
    }
    String::from_utf8(output.stdout).map_err(|err| err.to_string())
}

fn require_branch(root: &Path) -> Result<String, Error> {
    gitinfo::branch(root).ok_or_else(|| Error::InvalidSetting("a branch is required".to_string()))
}

fn trim_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

fn truncate_utf8(text: &mut String, max: usize) {
    if text.len() <= max {
        return;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

/// Replace URL userinfo that contains a password. The userinfo runs up to `@`
/// and may include punctuation (`sub-delims` in RFC 3986); those characters
/// are not treated as the start of a new URL.
pub(crate) fn redact_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let (before, after_at) = rest.split_at(at);
        let host = &after_at[1..];
        if let Some(start) = userinfo_start(before) {
            out.push_str(&before[..start]);
            out.push_str("***");
            out.push('@');
            rest = host;
            continue;
        }
        out.push_str(before);
        out.push('@');
        rest = host;
    }
    out.push_str(rest);
    out
}

/// Byte index where the userinfo before `@` begins, when that userinfo holds a
/// colon. Only ASCII userinfo characters are consumed, so the index is a char
/// boundary.
fn userinfo_start(before: &str) -> Option<usize> {
    let bytes = before.as_bytes();
    let mut start = bytes.len();
    while start > 0 && is_userinfo_byte(bytes[start - 1]) {
        start -= 1;
    }
    let userinfo = &before[start..];
    if userinfo.contains(':') && userinfo != ":" {
        Some(start)
    } else {
        None
    }
}

fn is_userinfo_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'.'
            | b'_'
            | b'~'
            | b'!'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b';'
            | b'='
            | b':'
            | b'%'
    )
}

#[cfg(test)]
mod tests {
    use super::{detail, redact_userinfo, split_remote_ref};

    #[test]
    fn redacts_userinfo_in_urls() {
        assert_eq!(
            redact_userinfo("https://user:super-secret-token@example.invalid/repo.git"),
            "https://***@example.invalid/repo.git"
        );
        assert_eq!(redact_userinfo("/tmp/bare"), "/tmp/bare");
        assert_eq!(
            redact_userinfo("see https://user:token@host/a and ok"),
            "see https://***@host/a and ok"
        );
        assert_eq!(
            redact_userinfo("git@github.com:org/repo.git"),
            "git@github.com:org/repo.git"
        );
    }

    #[test]
    fn redacts_authority_passwords_with_punctuation() {
        for (i, password) in ["sec;ret", "sec,ret", "sec'ret", "sec(ret)", "sec)ret"]
            .into_iter()
            .enumerate()
        {
            let url = format!("https://user:{password}@host/repo");
            let redacted = redact_userinfo(&url);
            assert_eq!(redacted, "https://***@host/repo", "case {i}");
            assert!(!redacted.contains(password), "case {i}");
        }
    }

    #[test]
    fn detail_truncates_on_a_utf8_boundary() {
        let url = "é".repeat(2500);
        let text = detail(false, &[("origin".to_string(), url)], None, &[]);
        assert!(text.len() <= 4000, "{}", text.len());
        assert!(text.is_char_boundary(text.len()));
    }

    #[test]
    fn split_remote_ref_prefers_configured_remote_names() {
        let remotes = vec!["team/upstream".to_string(), "team".to_string()];
        assert_eq!(
            split_remote_ref("refs/remotes/team/upstream/feature", &remotes).expect("split"),
            ("team/upstream".to_string(), "feature".to_string())
        );
        assert_eq!(
            split_remote_ref("refs/remotes/origin/main", &remotes).expect("stale"),
            ("origin".to_string(), "main".to_string())
        );
        assert_eq!(
            split_remote_ref("refs/remotes/team/upstream/feature", &["team".to_string()])
                .expect("shorter"),
            ("team".to_string(), "upstream/feature".to_string())
        );
    }
}
