//! Gated publish check.
//!
//! `git ls-remote` runs before a ledger write. The recheck inside the write
//! transaction uses local `refs/remotes` only, so the lock is not held across
//! the network.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
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
    let interesting = interesting_commits(root, &head);
    let local = local_refs(root)?;
    let listed = remote_names(root);
    let remotes = listed.as_ref().cloned().unwrap_or_default();
    let urls: Vec<(String, String)> = remotes
        .iter()
        .filter_map(|name| fetch_url(root, name).map(|url| (name.clone(), url)))
        .collect();
    let push_differs = remotes.iter().any(|name| push_url_differs(root, name));
    let check = Check {
        branch: &branch,
        head: &head,
        interesting: &interesting,
        local: &local,
        online: &[],
        local_only: remote_check == "local",
        push_differs,
        urls: &urls,
    };
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

/// Local half of the check. Safe to call while the ledger write lock is held.
pub(crate) fn refuse_local(root: &Path) -> Result<(), Error> {
    let branch = require_branch(root)?;
    let interesting = match gitinfo::verified_head(root) {
        Some(head) => interesting_commits(root, &head),
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
/// by itself mark a feature branch published. `HEAD` is added because that
/// range is empty when the branch is `main`: a tip commit pushed under another
/// name would otherwise be invisible. If `main` cannot be resolved, only
/// `HEAD` is used and the branch-name rule still applies.
fn interesting_commits(root: &Path, head: &str) -> BTreeSet<String> {
    let mut commits = BTreeSet::from([head.to_string()]);
    if let Ok(range) = gitinfo::release_range(root) {
        commits.extend(range.commits);
        commits.insert(range.head);
    }
    commits
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
    if text.len() > 4000 {
        text.truncate(4000);
        while !text.is_char_boundary(text.len()) {
            text.pop();
        }
    }
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
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return LsRemote::Failed(err.to_string()),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_thread = thread::spawn(move || read_pipe(stdout));
    let stderr_thread = thread::spawn(move || read_pipe(stderr));
    let timeout = remote_timeout();
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr = join_lossy(stderr_thread);
                return match join_utf8(stdout_thread) {
                    Some(stdout) if status.success() => LsRemote::Ok(stdout),
                    Some(_) => LsRemote::Failed(stderr),
                    None => LsRemote::Failed(stderr),
                };
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return LsRemote::TimedOut;
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return LsRemote::Failed(err.to_string());
            }
        }
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

fn join_utf8(handle: thread::JoinHandle<Vec<u8>>) -> Option<String> {
    let bytes = handle.join().ok()?;
    String::from_utf8(bytes).ok()
}

fn join_lossy(handle: thread::JoinHandle<Vec<u8>>) -> String {
    let Ok(bytes) = handle.join() else {
        return String::new();
    };
    String::from_utf8_lossy(&bytes).trim().to_string()
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
        let Some((remote, branch)) = split_remote_ref(refname) else {
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

fn split_remote_ref(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("refs/remotes/")?;
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

pub(crate) fn redact_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let (before, after_at) = rest.split_at(at);
        let host = &after_at[1..];
        let start = before
            .rfind(|c: char| {
                c.is_ascii_whitespace() || matches!(c, '"' | '\'' | '<' | '(' | '>' | ',' | ';')
            })
            .map_or(0, |index| index + 1);
        let userinfo = &before[start..];
        if let Some(rewritten) = redact_one(userinfo) {
            out.push_str(&before[..start]);
            out.push_str(&rewritten);
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

fn redact_one(userinfo: &str) -> Option<String> {
    if let Some(scheme) = userinfo.find("://") {
        let creds = &userinfo[scheme + 3..];
        let colon = creds.find(':')?;
        if creds[colon + 1..].is_empty() || creds.contains('/') {
            return None;
        }
        return Some(format!("{}***", &userinfo[..scheme + 3]));
    }
    let colon = userinfo.find(':')?;
    let user = &userinfo[..colon];
    let password = &userinfo[colon + 1..];
    if user.is_empty()
        || password.is_empty()
        || user.contains('/')
        || password.contains('/')
        || matches!(user, "http" | "https" | "ssh")
    {
        return None;
    }
    Some("***".to_string())
}

#[cfg(test)]
mod tests {
    use super::redact_userinfo;

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
    }
}
