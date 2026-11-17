//! Review, release, and graph-refresh checks.
//!
//! Callers print JSON and text. These functions return [`Error`] or plain data.

use std::path::{Path, PathBuf};

use relaybook_core::{Error, Ledger, Review, ScopedTask, Status};

use crate::gitinfo;

/// Result of one graph refresh. `graph_sha` is set only when `status` is `built`.
pub(crate) struct GraphRefresh {
    pub commit_sha: String,
    pub graph_sha: Option<String>,
    pub status: String,
    pub detail: Option<String>,
}

pub(crate) enum GraphOutcome {
    Skipped,
    Failed(String),
    Built,
}

pub(crate) struct ReleaseCheckout {
    pub root: PathBuf,
    pub branch: String,
    pub range: gitinfo::ReleaseRange,
}

/// Handoff for `review start <slug>`. The command layer prints this.
pub(crate) struct TaskReviewStart {
    pub slug: String,
    pub base_sha: Option<String>,
    pub commit_sha: String,
    pub range: Option<String>,
    pub graph_sha: Option<String>,
    pub graph_status: String,
    pub detail: Option<String>,
    pub report: serde_json::Value,
    pub graph_context: String,
}

/// Handoff for `review start --release`. The command layer prints this.
pub(crate) struct ReleaseReviewStart {
    pub base_sha: String,
    pub commit_sha: String,
    pub range: String,
    pub graph_sha: Option<String>,
    pub graph_status: String,
    pub detail: Option<String>,
    pub tasks: Vec<ScopedTask>,
    pub graph_context: String,
}

pub(crate) fn ensure_gated_clean(cwd: &Path) -> Result<(), Error> {
    let paths = crate::git::gated_dirty_paths(cwd).map_err(Error::DirtyWorktree)?;
    if paths.is_empty() {
        Ok(())
    } else {
        Err(Error::DirtyWorktree(paths.join(", ")))
    }
}

pub(crate) fn recheck_gated_completion(cwd: &Path, expected: &str) -> Result<(), Error> {
    ensure_gated_clean(cwd)?;
    match gitinfo::verified_head(cwd).as_deref() {
        Some(sha) if sha == expected => Ok(()),
        Some(_) => Err(Error::StaleHead),
        None => Err(Error::NoHeadCommit),
    }
}

pub(crate) fn record_graph_refresh(
    ledger: &mut Ledger,
    cwd: &Path,
    step: &str,
    task: Option<&str>,
) -> Result<GraphRefresh, Error> {
    if ledger.settings()?.workflow != "gated" {
        return Err(Error::InvalidSetting(
            "graph refresh requires workflow=gated".to_string(),
        ));
    }
    let task = paired_graph_task(step, task)?;
    let root = repository_root(cwd)?;
    let head = gitinfo::verified_head(&root).ok_or_else(|| {
        Error::InvalidSetting("graph refresh requires a commit at HEAD".to_string())
    })?;
    let (status, detail, graph_sha) = match decide_graph_refresh(&root, &head)? {
        GraphOutcome::Skipped => ("skipped", Some(crate::graphify::NO_GRAPH.to_string()), None),
        GraphOutcome::Failed(reason) => ("failed", Some(reason), None),
        GraphOutcome::Built => ("built", None, Some(head.clone())),
    };
    ledger.record_graph_build(step, task, env_run_id(), &head, status, detail.as_deref())?;
    Ok(GraphRefresh {
        commit_sha: head,
        graph_sha,
        status: status.to_string(),
        detail,
    })
}

pub(crate) fn decide_graph_refresh(root: &Path, head: &str) -> Result<GraphOutcome, Error> {
    if !crate::graphify::installed() {
        return Ok(GraphOutcome::Skipped);
    }
    if let Some(paths) = graph_dirty(root)? {
        return Ok(GraphOutcome::Failed(crate::graphify::no_graph(&format!(
            "working tree is not clean: {}",
            paths.join(", ")
        ))));
    }
    if crate::git::graphify_out_tracked(root).map_err(Error::InvalidSetting)? {
        return Ok(GraphOutcome::Failed(crate::graphify::no_graph(
            "graphify-out/ is tracked",
        )));
    }
    if !crate::git::graphify_out_ignored(root).map_err(Error::InvalidSetting)? {
        return Ok(GraphOutcome::Failed(crate::graphify::no_graph(
            "graphify-out/ is not gitignored",
        )));
    }
    let updated = crate::graphify::run_full(root, &["update", "."]);
    if !updated.ok {
        return Ok(GraphOutcome::Failed(crate::graphify::no_graph(
            &updated.detail,
        )));
    }
    if !root.join("graphify-out").join("graph.json").is_file() {
        return Ok(GraphOutcome::Failed(crate::graphify::no_graph(
            "graphify-out/graph.json is missing",
        )));
    }
    if let Some(paths) = graph_dirty(root)? {
        return Ok(GraphOutcome::Failed(crate::graphify::no_graph(&format!(
            "working tree is not clean: {}",
            paths.join(", ")
        ))));
    }
    if gitinfo::verified_head(root).as_deref() != Some(head) {
        return Ok(GraphOutcome::Failed(crate::graphify::no_graph(
            "HEAD changed during graph refresh",
        )));
    }
    Ok(GraphOutcome::Built)
}

pub(crate) fn settle_claim_graph(
    ledger: &mut Ledger,
    root: &Path,
    head: &str,
    graph_build_id: i64,
) -> Result<GraphRefresh, Error> {
    let (status, detail, graph_sha) = match decide_graph_refresh(root, head)? {
        GraphOutcome::Skipped => ("skipped", Some(crate::graphify::NO_GRAPH.to_string()), None),
        GraphOutcome::Failed(reason) => ("failed", Some(reason), None),
        GraphOutcome::Built => ("built", None, Some(head.to_string())),
    };
    ledger.set_graph_build_result(graph_build_id, status, detail.as_deref())?;
    Ok(GraphRefresh {
        commit_sha: head.to_string(),
        graph_sha,
        status: status.to_string(),
        detail,
    })
}

pub(crate) fn review_start_task(
    ledger: &mut Ledger,
    cwd: &Path,
    slug: &str,
) -> Result<TaskReviewStart, Error> {
    let reviewer = {
        let settings = ledger.settings()?;
        if settings.workflow != "gated" {
            return Err(Error::InvalidSetting(
                "review start requires workflow=gated".to_string(),
            ));
        }
        settings.reviewer
    };
    require_gated_clean_tree(cwd, ledger)?;
    let report = ledger.latest_task_report(slug)?.ok_or(Error::NotDone)?;
    if !task_is_done(ledger, slug)? {
        return Err(Error::NotDone);
    }
    let Some(commit_sha) = report.commit_sha.clone() else {
        return Err(Error::InvalidSetting(
            "task report has no commit".to_string(),
        ));
    };
    let root = repository_root(cwd)?;
    match gitinfo::verified_head(&root).as_deref() {
        Some(head) if head == commit_sha => {}
        Some(_) => {
            return Err(Error::InvalidSetting(
                "HEAD does not match the reported commit".to_string(),
            ));
        }
        None => {
            return Err(Error::InvalidSetting(
                "review start requires a commit at HEAD".to_string(),
            ));
        }
    }
    let refresh = record_graph_refresh(ledger, cwd, "review", Some(slug))?;
    ensure_gated_clean(&root)?;
    if gitinfo::verified_head(&root).as_deref() != Some(commit_sha.as_str()) {
        return Err(Error::InvalidSetting(
            "HEAD changed during graph refresh".to_string(),
        ));
    }
    let fingerprint = crate::git::worktree_fingerprint(&root).map_err(Error::InvalidSetting)?;
    let branch = gitinfo::branch(&root)
        .ok_or_else(|| Error::InvalidSetting("review start requires a branch".to_string()))?;
    ledger.start_review(
        "task",
        Some(slug),
        env_run_id(),
        &branch,
        &reviewer,
        report.base_sha.as_deref(),
        &commit_sha,
        refresh.graph_sha.as_deref(),
        &fingerprint,
    )?;
    let graph_context = if refresh.status == "built" {
        match report.base_sha.as_deref() {
            Some(base) => crate::graphify::graph_context(&root, base, &commit_sha),
            None => crate::graphify::NO_GRAPH.to_string(),
        }
    } else {
        crate::graphify::NO_GRAPH.to_string()
    };
    let report_body =
        serde_json::from_str::<serde_json::Value>(&report.body).unwrap_or(serde_json::Value::Null);
    let range = report
        .base_sha
        .as_ref()
        .map(|base| format!("{base}..{commit_sha}"));
    Ok(TaskReviewStart {
        slug: slug.to_string(),
        base_sha: report.base_sha,
        commit_sha,
        range,
        graph_sha: refresh.graph_sha,
        graph_status: refresh.status,
        detail: refresh.detail,
        report: report_body,
        graph_context,
    })
}

pub(crate) fn review_record_task(
    ledger: &mut Ledger,
    cwd: &Path,
    slug: &str,
    by: &str,
    sha: &str,
    verdict: &str,
    text: &str,
) -> Result<Review, Error> {
    let configured = {
        let settings = ledger.settings()?;
        if settings.workflow != "gated" {
            return Err(Error::InvalidSetting(
                "review record requires workflow=gated".to_string(),
            ));
        }
        settings.reviewer
    };
    require_gated_clean_tree(cwd, ledger)?;
    let root = repository_root(cwd)?;
    // Confirmation can sit at a TTY for a long time. Origin, HEAD, cleanliness,
    // and the fingerprint are read again after it returns, and once more inside
    // the write transaction immediately before commit.
    let reviewer_override = reviewer_override(&configured, by)?;
    require_gated_clean_tree(cwd, ledger)?;
    published_origin(&root)?;
    let head = gitinfo::verified_head(&root).ok_or_else(|| {
        Error::InvalidSetting("review record requires a commit at HEAD".to_string())
    })?;
    let fingerprint = crate::git::worktree_fingerprint(&root).map_err(Error::InvalidSetting)?;
    let sha_owned = sha.trim().to_string();
    let fingerprint_now = fingerprint.clone();
    ledger.record_review_with(
        "task",
        Some(slug),
        by.trim(),
        sha,
        &head,
        &fingerprint,
        verdict,
        text,
        reviewer_override,
        None,
        move || recheck_review_record(&root, &sha_owned, &fingerprint_now),
    )
}

pub(crate) fn review_start_release(
    ledger: &mut Ledger,
    cwd: &Path,
) -> Result<ReleaseReviewStart, Error> {
    let reviewer = {
        let settings = ledger.settings()?;
        if settings.workflow != "gated" {
            return Err(Error::InvalidSetting(
                "review start requires workflow=gated".to_string(),
            ));
        }
        settings.reviewer
    };
    let checkout = release_checkout(ledger, cwd, "review start")?;
    ledger.accepted_tasks_in_scope(&checkout.range.commits)?;
    let refresh = record_graph_refresh(ledger, cwd, "release-review", None)?;
    ensure_gated_clean(&checkout.root)?;
    if gitinfo::verified_head(&checkout.root).as_deref() != Some(checkout.range.head.as_str()) {
        return Err(Error::InvalidSetting(
            "HEAD changed during graph refresh".to_string(),
        ));
    }
    ledger.accepted_tasks_in_scope(&checkout.range.commits)?;
    let fingerprint =
        crate::git::worktree_fingerprint(&checkout.root).map_err(Error::InvalidSetting)?;
    let accepted = ledger.accepted_tasks_in_scope(&checkout.range.commits)?;
    ledger.start_review(
        "release",
        None,
        env_run_id(),
        &checkout.branch,
        &reviewer,
        Some(checkout.range.base.as_str()),
        &checkout.range.head,
        refresh.graph_sha.as_deref(),
        &fingerprint,
    )?;
    let graph_context = if refresh.status == "built" {
        crate::graphify::graph_context(&checkout.root, &checkout.range.base, &checkout.range.head)
    } else {
        crate::graphify::NO_GRAPH.to_string()
    };
    let range = format!("{}..{}", checkout.range.base, checkout.range.head);
    Ok(ReleaseReviewStart {
        base_sha: checkout.range.base,
        commit_sha: checkout.range.head,
        range,
        graph_sha: refresh.graph_sha,
        graph_status: refresh.status,
        detail: refresh.detail,
        tasks: accepted,
        graph_context,
    })
}

pub(crate) fn review_record_release(
    ledger: &mut Ledger,
    cwd: &Path,
    by: &str,
    sha: &str,
    verdict: &str,
    text: &str,
) -> Result<Review, Error> {
    let configured = {
        let settings = ledger.settings()?;
        if settings.workflow != "gated" {
            return Err(Error::InvalidSetting(
                "review record requires workflow=gated".to_string(),
            ));
        }
        settings.reviewer
    };
    let checkout = release_checkout(ledger, cwd, "review record")?;
    published_origin(&checkout.root)?;
    let sha = sha.trim();
    if sha != checkout.range.head {
        return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
    }
    ledger.accepted_tasks_in_scope(&checkout.range.commits)?;
    let reviewer_override = reviewer_override(&configured, by)?;
    let checkout = release_checkout(ledger, cwd, "review record")?;
    published_origin(&checkout.root)?;
    if sha != checkout.range.head {
        return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
    }
    let fingerprint =
        crate::git::worktree_fingerprint(&checkout.root).map_err(Error::InvalidSetting)?;
    let sha_owned = sha.to_string();
    let fingerprint_now = fingerprint.clone();
    let root = checkout.root.clone();
    let commits = checkout.range.commits.clone();
    ledger.record_review_with(
        "release",
        None,
        by.trim(),
        sha,
        &checkout.range.head,
        &fingerprint,
        verdict,
        text,
        reviewer_override,
        Some(&commits),
        move || recheck_review_record(&root, &sha_owned, &fingerprint_now),
    )
}

pub(crate) fn release_checkout(
    ledger: &Ledger,
    cwd: &Path,
    action: &str,
) -> Result<ReleaseCheckout, Error> {
    require_gated_clean_tree(cwd, ledger)?;
    let root = gitinfo::toplevel(cwd)
        .ok_or_else(|| Error::InvalidSetting(format!("{action} requires a git repository")))?;
    let branch = require_named_branch(&root, action)?;
    let range = gitinfo::release_range(&root).map_err(Error::InvalidSetting)?;
    Ok(ReleaseCheckout {
        root,
        branch,
        range,
    })
}

pub(crate) fn prepare_authorize(
    ledger: &Ledger,
    cwd: &Path,
    sha: &str,
) -> Result<ReleaseCheckout, Error> {
    if ledger.settings()?.workflow != "gated" {
        return Err(Error::InvalidSetting(
            "release authorize requires workflow=gated".to_string(),
        ));
    }
    let checkout = release_checkout(ledger, cwd, "release authorize")?;
    published_origin(&checkout.root)?;
    if sha != checkout.range.head {
        return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
    }
    ledger.accepted_tasks_in_scope(&checkout.range.commits)?;
    match ledger.release_review_at(sha)? {
        Some(review) if review.verdict.as_deref() == Some("approved") => {}
        _ => {
            return Err(Error::InvalidSetting(
                "no approved release review".to_string(),
            ));
        }
    }
    Ok(checkout)
}

pub(crate) fn recheck_release_authorize(root: &Path, sha: &str) -> Result<(), Error> {
    ensure_gated_clean(root)?;
    published_origin(root)?;
    let head = gitinfo::verified_head(root).ok_or_else(|| {
        Error::InvalidSetting("release authorize requires a commit at HEAD".to_string())
    })?;
    if head != sha {
        return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
    }
    Ok(())
}

/// Cleanliness, HEAD, fingerprint, and origin-ref absence for one record
/// attempt. `sha` and `fingerprint` were captured for this attempt; they must
/// still describe the worktree when the verdict commits.
pub(crate) fn recheck_review_record(
    root: &Path,
    sha: &str,
    fingerprint: &str,
) -> Result<(), Error> {
    ensure_gated_clean(root)?;
    published_origin(root)?;
    let head = gitinfo::verified_head(root).ok_or_else(|| {
        Error::InvalidSetting("review record requires a commit at HEAD".to_string())
    })?;
    if head != sha {
        return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
    }
    let fresh = crate::git::worktree_fingerprint(root).map_err(Error::InvalidSetting)?;
    if fresh != fingerprint {
        return Err(Error::InvalidSetting(
            "reviewer changed the repository".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn published_origin(root: &Path) -> Result<(), Error> {
    let branch = gitinfo::branch(root)
        .ok_or_else(|| Error::InvalidSetting("a branch is required".to_string()))?;
    let origin = format!("refs/remotes/origin/{branch}");
    if gitinfo::rev_exists(root, &origin).map_err(Error::InvalidSetting)? {
        return Err(Error::InvalidSetting(format!("{origin} exists")));
    }
    Ok(())
}

pub(crate) fn repository_root(cwd: &Path) -> Result<PathBuf, Error> {
    gitinfo::toplevel(cwd)
        .ok_or_else(|| Error::InvalidSetting("graph refresh requires a git repository".to_string()))
}

pub(crate) fn env_run_id() -> Option<i64> {
    std::env::var("RELAYBOOK_RUN_ID")
        .ok()
        .and_then(|value| value.parse().ok())
}

pub(crate) fn task_is_done(ledger: &Ledger, slug: &str) -> Result<bool, Error> {
    Ok(ledger
        .list_tasks(Some(Status::Done), false, true)?
        .iter()
        .any(|task| task.slug == slug))
}

fn require_gated_clean_tree(cwd: &Path, ledger: &Ledger) -> Result<(), Error> {
    if ledger.settings()?.workflow != "gated" {
        return Ok(());
    }
    ensure_gated_clean(cwd)
}

fn require_named_branch(root: &Path, action: &str) -> Result<String, Error> {
    match gitinfo::branch(root).as_deref() {
        Some(name) if name != "HEAD" => Ok(name.to_string()),
        _ => Err(Error::InvalidSetting(format!("{action} requires a branch"))),
    }
}

fn reviewer_override(configured: &str, by: &str) -> Result<bool, Error> {
    let by = by.trim();
    if by == configured {
        return Ok(false);
    }
    if by == "claude" {
        confirm_claude_review()?;
        return Ok(true);
    }
    Err(Error::InvalidSetting(format!(
        "unconfigured reviewer: {by}"
    )))
}

fn confirm_claude_review() -> Result<(), Error> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(Error::InvalidSetting(
            "claude review requires confirmation at a TTY".to_string(),
        ));
    }
    match crate::uninstall::confirm("Record this review as claude?") {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::InvalidSetting(
            "claude review was not confirmed".to_string(),
        )),
        Err(message) => Err(Error::InvalidSetting(message)),
    }
}

fn graph_dirty(cwd: &Path) -> Result<Option<Vec<String>>, Error> {
    let paths = crate::git::gated_dirty_paths(cwd).map_err(Error::InvalidSetting)?;
    let paths: Vec<String> = paths
        .into_iter()
        .filter(|path| !crate::git::graphify_out_path(path))
        .collect();
    if paths.is_empty() {
        Ok(None)
    } else {
        Ok(Some(paths))
    }
}

fn paired_graph_task<'a>(step: &str, task: Option<&'a str>) -> Result<Option<&'a str>, Error> {
    match step.trim() {
        "plan" | "release-review" => {
            if task.is_some() {
                Err(Error::InvalidSetting(
                    "this graph step has no task".to_string(),
                ))
            } else {
                Ok(None)
            }
        }
        "implement" | "commit" | "review" => task.map(Some).ok_or_else(|| {
            Error::InvalidSetting("task is required for this graph step".to_string())
        }),
        other => Err(Error::InvalidSetting(format!(
            "invalid graph step: {other}"
        ))),
    }
}
