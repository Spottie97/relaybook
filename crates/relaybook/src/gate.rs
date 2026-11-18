//! Review, release, and graph-refresh checks.
//!
//! Callers print JSON and text. These functions return [`Error`] or plain data.

use std::path::{Path, PathBuf};

use relaybook_core::{Error, Ledger, OwnedReviewExpect, Review, Run, ScopedTask, Status};

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
    record_graph_refresh_for(ledger, cwd, step, task, None)
}

/// Like [`record_graph_refresh`]. `run_id` is stored on the audit row when set,
/// and otherwise the `RELAYBOOK_RUN_ID` environment value is used.
pub(crate) fn record_graph_refresh_for(
    ledger: &mut Ledger,
    cwd: &Path,
    step: &str,
    task: Option<&str>,
    run_id: Option<i64>,
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
    let run_id = run_id.or_else(env_run_id);
    ledger.record_graph_build(step, task, run_id, &head, status, detail.as_deref())?;
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
    refuse_live_task(ledger, slug)?;
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
    refuse_live_task(ledger, slug)?;
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
    // Confirmation can sit at a TTY for a long time. The publish check, HEAD,
    // cleanliness, and the fingerprint are read again after it returns.
    // Inside the write transaction only the local half runs, immediately
    // before commit.
    let reviewer_override = reviewer_override(&configured, by, sha)?;
    require_gated_clean_tree(cwd, ledger)?;
    require_unpublished(ledger, &root, "review")?;
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
    refuse_live_release(ledger)?;
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
    refuse_live_release(ledger)?;
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
    require_unpublished(ledger, &checkout.root, "release-review")?;
    let sha = sha.trim();
    if sha != checkout.range.head {
        return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
    }
    ledger.accepted_tasks_in_scope(&checkout.range.commits)?;
    let reviewer_override = reviewer_override(&configured, by, sha)?;
    let checkout = release_checkout(ledger, cwd, "review record")?;
    require_unpublished(ledger, &checkout.root, "release-review")?;
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

/// Who records the next owned review. Codex is the default. Sonnet is only
/// the fallback reviewer, and a re-review never accepts or reopens a task.
pub(crate) struct ReviewerChoice {
    pub reviewer: String,
    pub model: Option<String>,
    pub fallback_reason: Option<String>,
    pub note_fallback: bool,
    pub fallback_retry_after: Option<String>,
    pub codex_version: Option<String>,
    pub rereview_of: Option<i64>,
    pub accept_approved_task: bool,
    pub reopen_on_changes: bool,
}

impl ReviewerChoice {
    pub(crate) fn codex(model: &str) -> Self {
        Self {
            reviewer: "codex".to_string(),
            model: Some(model.to_string()),
            fallback_reason: None,
            note_fallback: false,
            fallback_retry_after: None,
            codex_version: None,
            rereview_of: None,
            accept_approved_task: true,
            reopen_on_changes: true,
        }
    }

    fn extras(&self) -> relaybook_core::ReviewColumnExtras {
        relaybook_core::ReviewColumnExtras {
            reviewer_model: self.model.clone(),
            fallback_reason: self.fallback_reason.clone(),
            rereview_of: self.rereview_of,
            note_fallback: self.note_fallback,
            fallback_retry_after: self.fallback_retry_after.clone(),
            codex_version: self.codex_version.clone(),
        }
    }
}

/// Packet fields for one auto review. The runner records this review itself.
pub(crate) struct PreparedReview {
    pub review_id: i64,
    pub invocation_token: String,
    pub run_generation: i64,
    pub slug: Option<String>,
    pub base_sha: Option<String>,
    pub commit_sha: String,
    /// When set, the packet diffs this spec instead of `base_sha..commit_sha`.
    /// Re-reviews keep the original range while `commit_sha` is current HEAD.
    pub diff_spec: Option<String>,
    pub handoff: serde_json::Value,
}

/// Start the owning run's task review and refresh the graph.
/// Manual `review start` still refuses a live run. A failed refresh is recorded
/// and does not stop the review.
///
/// `run` is the driver's snapshot, including the `write_stamp` and stage claimed
/// before preparation. This function does not reload the run.
pub(crate) fn start_task_review(
    ledger: &mut Ledger,
    cwd: &Path,
    run: &Run,
    slug: &str,
    choice: &ReviewerChoice,
) -> Result<PreparedReview, Error> {
    if ledger.settings()?.workflow != "gated" {
        return Err(Error::InvalidSetting(
            "review start requires workflow=gated".to_string(),
        ));
    }
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
    if run.stage != "review" {
        return Err(Error::InvalidSetting(
            relaybook_core::STALE_REVIEW_INVOCATION.to_string(),
        ));
    }
    // Fixed for this driver before the pause. A later write_stamp must not be adopted.
    let run_id = run.id;
    let generation = run.write_stamp;
    let stage = run.stage.clone();
    let agent_calls = run.agent_calls;
    // Tests pause here, before the start transaction reads the live run.
    hold_owned_review_prepare()?;
    let expect = OwnedReviewExpect {
        generation,
        stage,
        agent_calls: agent_calls + 1,
        report_id: Some(report.id),
        report_body: Some(report.body.clone()),
    };
    let refresh = record_graph_refresh_for(ledger, cwd, "review", Some(slug), Some(run_id))?;
    ensure_gated_clean(&root)?;
    if gitinfo::verified_head(&root).as_deref() != Some(commit_sha.as_str()) {
        return Err(Error::InvalidSetting(
            "HEAD changed during graph refresh".to_string(),
        ));
    }
    let fingerprint = crate::git::worktree_fingerprint(&root).map_err(Error::InvalidSetting)?;
    let branch = gitinfo::branch(&root)
        .ok_or_else(|| Error::InvalidSetting("review start requires a branch".to_string()))?;
    let base_sha = report.base_sha.clone();
    let graph_context = review_graph_context(&root, &refresh, base_sha.as_deref(), &commit_sha);
    let started = ledger.start_owned_review(
        "task",
        Some(slug),
        Some(run_id),
        &branch,
        &choice.reviewer,
        base_sha.as_deref(),
        &commit_sha,
        refresh.graph_sha.as_deref(),
        &fingerprint,
        &expect,
        &choice.extras(),
    )?;
    let report_body = match started.report_body.as_deref() {
        Some(body) => {
            serde_json::from_str::<serde_json::Value>(body).unwrap_or(serde_json::Value::Null)
        }
        None => serde_json::Value::Null,
    };
    let range = base_sha
        .as_ref()
        .map(|base| format!("{base}..{commit_sha}"));
    let handoff = serde_json::json!({
        "slug": slug,
        "base_sha": base_sha,
        "commit_sha": commit_sha,
        "range": range,
        "graph_sha": refresh.graph_sha,
        "graph_status": refresh.status,
        "detail": refresh.detail,
        "report": report_body,
        "graph_context": graph_context,
    });
    Ok(PreparedReview {
        review_id: started.review.id,
        invocation_token: started.invocation_token,
        run_generation: started.run_generation,
        slug: Some(slug.to_string()),
        base_sha,
        commit_sha,
        diff_spec: None,
        handoff,
    })
}

/// Start the owning run's release review and refresh the graph.
/// A failed refresh is recorded and does not stop the review.
///
/// `run` is the driver's snapshot, including the `write_stamp` and stage claimed
/// before preparation. This function does not reload the run.
pub(crate) fn start_release_review(
    ledger: &mut Ledger,
    cwd: &Path,
    run: &Run,
    choice: &ReviewerChoice,
) -> Result<PreparedReview, Error> {
    if ledger.settings()?.workflow != "gated" {
        return Err(Error::InvalidSetting(
            "review start requires workflow=gated".to_string(),
        ));
    }
    let checkout = release_checkout(ledger, cwd, "review start")?;
    ledger.accepted_tasks_in_scope(&checkout.range.commits)?;
    if run.stage != "release_review" {
        return Err(Error::InvalidSetting(
            relaybook_core::STALE_REVIEW_INVOCATION.to_string(),
        ));
    }
    let run_id = run.id;
    let generation = run.write_stamp;
    let stage = run.stage.clone();
    let agent_calls = run.agent_calls;
    // Same pause point as the task starter, before the start transaction.
    hold_owned_review_prepare()?;
    let expect = OwnedReviewExpect {
        generation,
        stage,
        agent_calls: agent_calls + 1,
        report_id: None,
        report_body: None,
    };
    let refresh = record_graph_refresh_for(ledger, cwd, "release-review", None, Some(run_id))?;
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
    let base_sha = checkout.range.base.clone();
    let commit_sha = checkout.range.head.clone();
    let graph_context = review_graph_context(
        &checkout.root,
        &refresh,
        Some(base_sha.as_str()),
        &commit_sha,
    );
    let started = ledger.start_owned_review(
        "release",
        None,
        Some(run_id),
        &checkout.branch,
        &choice.reviewer,
        Some(base_sha.as_str()),
        &commit_sha,
        refresh.graph_sha.as_deref(),
        &fingerprint,
        &expect,
        &choice.extras(),
    )?;
    let range = format!("{base_sha}..{commit_sha}");
    let handoff = serde_json::json!({
        "slug": serde_json::Value::Null,
        "base_sha": base_sha,
        "commit_sha": commit_sha,
        "range": range,
        "graph_sha": refresh.graph_sha,
        "graph_status": refresh.status,
        "detail": refresh.detail,
        "tasks": accepted,
        "graph_context": graph_context,
    });
    Ok(PreparedReview {
        review_id: started.review.id,
        invocation_token: started.invocation_token,
        run_generation: started.run_generation,
        slug: None,
        base_sha: Some(base_sha),
        commit_sha,
        diff_spec: None,
        handoff,
    })
}

/// Start a Codex re-review of one approved fallback review.
///
/// The row's `commit_sha` is current HEAD. The packet diff is the original
/// review's `base_sha..commit_sha`, with graph context `no graph`. No graph
/// refresh. The row does not accept or reopen a task.
pub(crate) fn start_rereview(
    ledger: &mut Ledger,
    cwd: &Path,
    run: &Run,
    original: &relaybook_core::PendingRereviewRow,
    model: &str,
) -> Result<PreparedReview, Error> {
    if ledger.settings()?.workflow != "gated" {
        return Err(Error::InvalidSetting(
            "review start requires workflow=gated".to_string(),
        ));
    }
    if run.stage != "review" && run.stage != "release_review" {
        return Err(Error::InvalidSetting(
            relaybook_core::STALE_REVIEW_INVOCATION.to_string(),
        ));
    }
    require_gated_clean_tree(cwd, ledger)?;
    let root = repository_root(cwd)?;
    let head = gitinfo::verified_head(&root).ok_or_else(|| {
        Error::InvalidSetting("review start requires a commit at HEAD".to_string())
    })?;
    let run_id = run.id;
    let (report_id, report_body) = if original.scope == "task" {
        let slug = original.slug.as_deref().ok_or_else(|| {
            Error::InvalidSetting("task is required for a task review".to_string())
        })?;
        let report = ledger.latest_task_report(slug)?.ok_or(Error::NotDone)?;
        (Some(report.id), Some(report.body))
    } else {
        (None, None)
    };
    let expect = OwnedReviewExpect {
        generation: run.write_stamp,
        stage: run.stage.clone(),
        agent_calls: run.agent_calls + 1,
        report_id,
        report_body,
    };
    hold_owned_review_prepare()?;
    let fingerprint = crate::git::worktree_fingerprint(&root).map_err(Error::InvalidSetting)?;
    let branch = gitinfo::branch(&root)
        .ok_or_else(|| Error::InvalidSetting("review start requires a branch".to_string()))?;
    let mut choice = ReviewerChoice::codex(model);
    choice.accept_approved_task = false;
    choice.reopen_on_changes = false;
    choice.rereview_of = Some(original.review_id);
    let task = if original.scope == "task" {
        original.slug.as_deref()
    } else {
        None
    };
    let started = ledger.start_owned_review(
        &original.scope,
        task,
        Some(run_id),
        &branch,
        "codex",
        Some(original.base_sha.as_str()),
        &head,
        None,
        &fingerprint,
        &expect,
        &choice.extras(),
    )?;
    let diff_spec = Some(format!("{}..{}", original.base_sha, original.commit_sha));
    let handoff = serde_json::json!({
        "slug": original.slug,
        "base_sha": original.base_sha,
        "commit_sha": head,
        "reviewed_commit": original.commit_sha,
        "range": diff_spec,
        "rereview_of": original.review_id,
        "report": original.body,
        "graph_context": crate::graphify::NO_GRAPH,
    });
    Ok(PreparedReview {
        review_id: started.review.id,
        invocation_token: started.invocation_token,
        run_generation: started.run_generation,
        slug: original.slug.clone(),
        base_sha: Some(original.base_sha.clone()),
        commit_sha: head,
        diff_spec,
        handoff,
    })
}

fn review_graph_context(
    root: &Path,
    refresh: &GraphRefresh,
    base: Option<&str>,
    commit: &str,
) -> String {
    if refresh.status == "built" {
        match base {
            Some(base) => crate::graphify::graph_context(root, base, commit),
            None => crate::graphify::NO_GRAPH.to_string(),
        }
    } else {
        crate::graphify::NO_GRAPH.to_string()
    }
}

#[allow(clippy::too_many_arguments)] // record fields plus the invocation that owns them
pub(crate) fn record_owned_task_review(
    ledger: &mut Ledger,
    cwd: &Path,
    slug: &str,
    sha: &str,
    verdict: &str,
    text: &str,
    review_id: i64,
    invocation_token: &str,
    run_generation: i64,
    choice: &ReviewerChoice,
    advance: relaybook_core::OwnedRunAdvance,
) -> Result<Review, Error> {
    if ledger.settings()?.workflow != "gated" {
        return Err(Error::InvalidSetting(
            "review record requires workflow=gated".to_string(),
        ));
    }
    require_gated_clean_tree(cwd, ledger)?;
    let root = repository_root(cwd)?;
    require_unpublished(ledger, &root, "review")?;
    let head = gitinfo::verified_head(&root).ok_or_else(|| {
        Error::InvalidSetting("review record requires a commit at HEAD".to_string())
    })?;
    let fingerprint = crate::git::worktree_fingerprint(&root).map_err(Error::InvalidSetting)?;
    let sha_owned = sha.trim().to_string();
    let fingerprint_now = fingerprint.clone();
    let accept = choice.accept_approved_task;
    let reopen = choice.reopen_on_changes;
    ledger.record_owned_review(
        "task",
        Some(slug),
        &choice.reviewer,
        sha,
        &head,
        &fingerprint,
        verdict,
        text,
        false,
        None,
        review_id,
        invocation_token,
        run_generation,
        accept,
        reopen,
        advance,
        move || recheck_review_record(&root, &sha_owned, &fingerprint_now),
    )
}

#[allow(clippy::too_many_arguments)] // record fields plus the invocation that owns them
pub(crate) fn record_owned_release_review(
    ledger: &mut Ledger,
    cwd: &Path,
    sha: &str,
    verdict: &str,
    text: &str,
    review_id: i64,
    invocation_token: &str,
    run_generation: i64,
    choice: &ReviewerChoice,
    advance: relaybook_core::OwnedRunAdvance,
) -> Result<Review, Error> {
    if ledger.settings()?.workflow != "gated" {
        return Err(Error::InvalidSetting(
            "review record requires workflow=gated".to_string(),
        ));
    }
    let checkout = release_checkout(ledger, cwd, "review record")?;
    require_unpublished(ledger, &checkout.root, "release-review")?;
    let sha = sha.trim();
    if sha != checkout.range.head {
        return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
    }
    ledger.accepted_tasks_in_scope(&checkout.range.commits)?;
    let checkout = release_checkout(ledger, cwd, "review record")?;
    require_unpublished(ledger, &checkout.root, "release-review")?;
    if sha != checkout.range.head {
        return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
    }
    let fingerprint =
        crate::git::worktree_fingerprint(&checkout.root).map_err(Error::InvalidSetting)?;
    let sha_owned = sha.to_string();
    let fingerprint_now = fingerprint.clone();
    let root = checkout.root.clone();
    let commits = checkout.range.commits.clone();
    let reopen = choice.reopen_on_changes;
    ledger.record_owned_review(
        "release",
        None,
        &choice.reviewer,
        sha,
        &checkout.range.head,
        &fingerprint,
        verdict,
        text,
        false,
        Some(&commits),
        review_id,
        invocation_token,
        run_generation,
        false,
        reopen,
        advance,
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
    if sha != checkout.range.head {
        return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
    }
    ledger.accepted_tasks_in_scope(&checkout.range.commits)?;
    refuse_pending_rereview(ledger, sha)?;
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
    crate::remote::refuse_local(root)?;
    let head = gitinfo::verified_head(root).ok_or_else(|| {
        Error::InvalidSetting("release authorize requires a commit at HEAD".to_string())
    })?;
    if head != sha {
        return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
    }
    Ok(())
}

/// Cleanliness, HEAD, fingerprint, and local publish absence for one record
/// attempt. `sha` and `fingerprint` were captured for this attempt; they must
/// still describe the worktree when the verdict commits.
pub(crate) fn recheck_review_record(
    root: &Path,
    sha: &str,
    fingerprint: &str,
) -> Result<(), Error> {
    ensure_gated_clean(root)?;
    crate::remote::refuse_local(root)?;
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

pub(crate) enum PublishScreen {
    Clear,
    Present(String),
    Unknown { message: String, check_id: i64 },
}

pub(crate) fn screen_publish(
    ledger: &mut Ledger,
    root: &Path,
    context: &str,
) -> Result<PublishScreen, Error> {
    let mode = ledger.settings()?.remote_check;
    let view = crate::remote::screen(root, &mode)?;
    let check_id = ledger.record_remote_check(&relaybook_core::RemoteCheckDraft {
        context: context.to_string(),
        branch: view.branch.clone(),
        head_sha: view.head_sha.clone(),
        remote: view.remote.clone(),
        status: view.status.to_string(),
        detail: view.detail.clone(),
        overridden: false,
    })?;
    match view.status {
        "present" => Ok(PublishScreen::Present(
            view.message.unwrap_or_else(|| "published".to_string()),
        )),
        "unknown" => Ok(PublishScreen::Unknown {
            message: view
                .message
                .unwrap_or_else(|| "remote check failed".to_string()),
            check_id,
        }),
        _ => Ok(PublishScreen::Clear),
    }
}

pub(crate) fn require_unpublished(
    ledger: &mut Ledger,
    root: &Path,
    context: &str,
) -> Result<(), Error> {
    match screen_publish(ledger, root, context)? {
        PublishScreen::Clear => Ok(()),
        PublishScreen::Present(message) | PublishScreen::Unknown { message, .. } => {
            Err(Error::InvalidSetting(message))
        }
    }
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

const LIVE_RUN_OWNS: &str = "a gated run owns this; use relaybook approve / run --resume";

fn live_gated_runs(ledger: &Ledger) -> Result<Vec<relaybook_core::Run>, Error> {
    ledger.live_gated_runs()
}

pub(crate) fn refuse_live_task(ledger: &Ledger, slug: &str) -> Result<(), Error> {
    if ledger.live_gated_owns_task(slug.trim())? {
        Err(Error::InvalidSetting(LIVE_RUN_OWNS.to_string()))
    } else {
        Ok(())
    }
}

pub(crate) fn refuse_live_release(ledger: &Ledger) -> Result<(), Error> {
    if live_gated_runs(ledger)?.is_empty() {
        Ok(())
    } else {
        Err(Error::InvalidSetting(LIVE_RUN_OWNS.to_string()))
    }
}

pub(crate) fn refuse_live_workflow(ledger: &Ledger) -> Result<(), Error> {
    refuse_live_release(ledger)
}

/// `release authorize` is allowed when no gated run is live, or when that run
/// is waiting at `gate_release`.
/// `before-release` refuses while an approved Claude review has no approved
/// Codex re-review. Live runs are checked, and so is the run on the approved
/// release review at `sha`, including a stopped run. The database trigger is
/// the backstop for the insert.
pub(crate) fn refuse_pending_rereview(ledger: &Ledger, sha: &str) -> Result<(), Error> {
    if ledger.settings()?.fallback_rereview == "off" {
        return Ok(());
    }
    for run in live_gated_runs(ledger)? {
        if !ledger.pending_rereviews(run.id)?.is_empty() {
            return Err(Error::InvalidSetting("codex re-review pending".to_string()));
        }
    }
    if let Some(review) = ledger.release_review_at(sha)?
        && review.verdict.as_deref() == Some("approved")
        && let Some(run_id) = review.run_id
        && !ledger.pending_rereviews(run_id)?.is_empty()
    {
        return Err(Error::InvalidSetting("codex re-review pending".to_string()));
    }
    Ok(())
}

/// True when the current `fallback_rereview` policy blocks publication.
/// `off` does not block. The check does not look at the run's stage.
pub(crate) fn rereview_blocks_publication(ledger: &Ledger, run_id: i64) -> Result<bool, Error> {
    if ledger.settings()?.fallback_rereview == "off" {
        return Ok(false);
    }
    Ok(!ledger.pending_rereviews(run_id)?.is_empty())
}

/// True when approve must not pass a release checkpoint while Codex still
/// owes a re-review. Plan and task-review checkpoints stay open.
pub(crate) fn rereview_blocks_approve(ledger: &Ledger, run: &Run) -> Result<bool, Error> {
    if ledger.settings()?.fallback_rereview == "off" {
        return Ok(false);
    }
    let release_stage = run.stage == "gate_release"
        || run.stage == "release_review"
        || run
            .stop_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("codex-rereview|"));
    if !release_stage {
        return Ok(false);
    }
    Ok(!ledger.pending_rereviews(run.id)?.is_empty())
}

pub(crate) fn allow_release_authorize(ledger: &Ledger) -> Result<(), Error> {
    match live_gated_runs(ledger)?.as_slice() {
        [] => Ok(()),
        [run] if run.status == "waiting" && run.stage == "gate_release" => Ok(()),
        _ => Err(Error::InvalidSetting(LIVE_RUN_OWNS.to_string())),
    }
}

/// Run id to store when this branch is waiting at `gate_release`.
pub(crate) fn gate_release_run_id(ledger: &Ledger, branch: &str) -> Result<Option<i64>, Error> {
    match live_gated_runs(ledger)?.as_slice() {
        [run] if run.status == "waiting" && run.stage == "gate_release" && run.branch == branch => {
            Ok(Some(run.id))
        }
        _ => Ok(None),
    }
}

pub(crate) fn task_is_done(ledger: &Ledger, slug: &str) -> Result<bool, Error> {
    Ok(ledger
        .list_tasks(Some(Status::Done), false, true)?
        .iter()
        .any(|task| task.slug == slug))
}

/// Test seam. `RELAYBOOK_REVIEW_PREPARE_HOLD` is a directory; the starter
/// writes `started-N` and waits for `release-N` before the start transaction.
/// Unset, this returns immediately. Release builds ignore the variable.
fn hold_owned_review_prepare() -> Result<(), Error> {
    #[cfg(debug_assertions)]
    {
        hold_owned_review_prepare_for_tests()
    }
    #[cfg(not(debug_assertions))]
    {
        Ok(())
    }
}

#[cfg(debug_assertions)]
fn hold_owned_review_prepare_for_tests() -> Result<(), Error> {
    let Ok(dir) = std::env::var("RELAYBOOK_REVIEW_PREPARE_HOLD") else {
        return Ok(());
    };
    if dir.is_empty() {
        return Ok(());
    }
    let root = PathBuf::from(dir);
    std::fs::create_dir_all(&root)
        .map_err(|err| Error::InvalidSetting(format!("review prepare hold: {err}")))?;
    let seq = claim_prepare_slot(&root)?;
    std::fs::write(root.join(format!("started-{seq}")), b"prepare")
        .map_err(|err| Error::InvalidSetting(format!("review prepare hold: {err}")))?;
    let release = root.join(format!("release-{seq}"));
    for _ in 0..1600 {
        if release.is_file() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(Error::InvalidSetting(
        "review prepare hold timed out".to_string(),
    ))
}

#[cfg(debug_assertions)]
fn claim_prepare_slot(dir: &Path) -> Result<u32, Error> {
    for n in 1..100 {
        let path = dir.join(format!("claim-{n}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(n),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(Error::InvalidSetting(format!("review prepare hold: {err}")));
            }
        }
    }
    Err(Error::InvalidSetting(
        "review prepare hold slot failed".to_string(),
    ))
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

fn reviewer_override(configured: &str, by: &str, sha: &str) -> Result<bool, Error> {
    let by = by.trim();
    if by == configured {
        return Ok(false);
    }
    if by == "claude" {
        confirm_claude_review(sha)?;
        return Ok(true);
    }
    Err(Error::InvalidSetting(format!(
        "unconfigured reviewer: {by}"
    )))
}

fn confirm_claude_review(sha: &str) -> Result<(), Error> {
    crate::signoff::confirm(&crate::signoff::Challenge {
        ask: "Record this review as claude?",
        verb: "claude",
        token: crate::signoff::token_from_sha(sha)?,
        missing_tty: "claude review requires confirmation at a TTY",
        declined: "claude review was not confirmed",
    })
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
