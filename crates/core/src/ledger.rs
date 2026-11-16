use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::deps::{self, fill_dependencies};
use crate::reports::{self, Sections};

fn empty_sections() -> Sections {
    Sections {
        changed: Vec::new(),
        verified: Vec::new(),
        not_verified: Vec::new(),
        deviations: Vec::new(),
        review: Vec::new(),
    }
}
use crate::error::Error;
use crate::migrate::migrate;
use crate::model::{
    Context, Decision, Event, EventKind, Status, Step, Task, TaskDetail, checked_slug,
    required_text,
};
use crate::schema::{SCHEMA, SCHEMA_VERSION};

/// Open SQLite ledger. Projection tables hold current state; `events` is insert-only.
pub struct Ledger {
    conn: Connection,
}

/// One row of `reviews`. `verdict` is empty while the review is pending.
///
/// `ledger_mark` is the stored JSON snapshot. `fingerprint` is kept in that
/// JSON because schema v7 has no fingerprint column; it is not one of the
/// five ledger fields compared for "ledger changed during review".
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Review {
    pub id: i64,
    pub scope: String,
    pub task_id: Option<i64>,
    pub report_id: Option<i64>,
    pub run_id: Option<i64>,
    pub branch: String,
    pub reviewer: String,
    pub reviewer_override: bool,
    pub base_sha: Option<String>,
    pub commit_sha: String,
    pub graph_sha: Option<String>,
    pub ledger_mark: String,
    pub fingerprint: String,
    pub verdict: Option<String>,
    pub body: Option<serde_json::Value>,
    pub bytes: Option<i64>,
    pub started_at: String,
    pub recorded_at: Option<String>,
}

/// Latest task report, including the SHAs stored at completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReport {
    pub body: String,
    pub commit_sha: Option<String>,
    pub base_sha: Option<String>,
    pub graph_sha: Option<String>,
}

/// SHAs frozen when a gated claim is recorded.
///
/// `graph_sha` is set only when that claim's own graph build is `built`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimAnchor {
    pub base_sha: Option<String>,
    pub graph_sha: Option<String>,
}

/// Result of [`Ledger::claim_task_anchored`].
///
/// `graph_build_id` is the `implement` row whose commit SHA is `base_sha`.
/// It stays null when the claim had no HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchoredClaim {
    pub task: Task,
    pub base_sha: Option<String>,
    pub graph_build_id: Option<i64>,
}

/// One row of `graph_builds`. `step` is `plan`, `implement`, `commit`,
/// `review`, or `release-review`. When `status` is `built`, `commit_sha` is
/// the graph SHA.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphBuild {
    pub id: i64,
    pub step: String,
    pub task_id: Option<i64>,
    pub run_id: Option<i64>,
    pub commit_sha: String,
    pub status: String,
    pub detail: Option<String>,
    pub created_at: String,
}

/// One row of `release_authorizations`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReleaseAuthorization {
    pub id: i64,
    pub run_id: Option<i64>,
    pub branch: String,
    pub commit_sha: String,
    pub review_id: i64,
    pub authorized_by: String,
    pub created_at: String,
}

impl Ledger {
    pub fn initialize(path: &std::path::Path) -> Result<Self, Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let fresh = !path.exists();
        let mut conn = Connection::open(path)?;
        configure(&conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        if fresh {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn open(path: &std::path::Path) -> Result<Self, Error> {
        let mut conn = Connection::open(path)?;
        configure(&conn)?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Open without migrating. Used by the prompt hook, which must stay read-only.
    pub fn open_readonly(path: &std::path::Path) -> Result<Self, Error> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure(&conn)?;
        Ok(Self { conn })
    }

    pub fn create_task(&mut self, slug: &str, title: &str) -> Result<Task, Error> {
        self.create_task_depending(slug, title, &[])
    }

    pub fn create_task_depending(
        &mut self,
        slug: &str,
        title: &str,
        depends_on: &[String],
    ) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        let title = required_text("title", title)?;
        let mut dependencies = Vec::with_capacity(depends_on.len());
        for dependency in depends_on {
            dependencies.push(checked_slug(dependency)?);
        }
        self.transaction(|conn| {
            let ts = now(conn)?;
            match conn.execute(
                "INSERT INTO tasks (slug, title, status, claimed_by, created_at, updated_at)
                 VALUES (?1, ?2, 'open', NULL, ?3, ?3)",
                params![slug, title, ts],
            ) {
                Ok(_) => {}
                Err(err)
                    if constraint_message(&err).is_some_and(|msg| msg.contains("tasks.slug")) =>
                {
                    return Err(Error::TaskExists(slug.clone()));
                }
                Err(err) => return Err(err.into()),
            }
            insert_event(
                conn,
                EventKind::TaskCreated,
                serde_json::json!({ "slug": slug, "title": title }),
            )?;
            for dependency in &dependencies {
                deps::add_link(conn, &slug, dependency)?;
            }
            load_task(conn, &slug)
        })
    }

    pub fn add_dependency(&mut self, slug: &str, on: &str) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        let on = checked_slug(on)?;
        self.transaction(|conn| {
            deps::add_link(conn, &slug, &on)?;
            load_task(conn, &slug)
        })
    }

    pub fn remove_dependency(&mut self, slug: &str, on: &str) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        let on = checked_slug(on)?;
        self.transaction(|conn| {
            deps::remove_link(conn, &slug, &on)?;
            load_task(conn, &slug)
        })
    }

    pub fn list_tasks(
        &self,
        status: Option<Status>,
        ready: bool,
        include_done: bool,
    ) -> Result<Vec<Task>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, slug, title, status, claimed_by, created_at, updated_at
             FROM tasks
             WHERE (?1 IS NULL OR status = ?1)
             ORDER BY id",
        )?;
        let status_text = status.map(|value| value.as_str().to_string());
        let rows = stmt.query_map(params![status_text], read_task_row)?;
        let mut tasks = collect_tasks(rows)?;
        fill_dependencies(&self.conn, &mut tasks)?;
        for task in &mut tasks {
            task.reopened = reports::reopened_reason(&self.conn, task.id)?;
        }
        if ready {
            tasks.retain(|task| task.status == Status::Open && task.needs.is_empty());
        } else if status.is_none() && !include_done {
            tasks.retain(|task| task.status != Status::Done);
        }
        Ok(tasks)
    }

    pub fn claim_task(&mut self, slug: &str, by: &str) -> Result<Task, Error> {
        self.claim_task_with(slug, by, || Ok(()))
    }

    /// Same as [`Self::claim_task`]. `revalidate` runs inside this write
    /// transaction after the claim is recorded and before commit. An error
    /// rolls the claim back, including a cleanliness check that failed after
    /// the lock wait.
    pub fn claim_task_with(
        &mut self,
        slug: &str,
        by: &str,
        revalidate: impl FnOnce() -> Result<(), Error>,
    ) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        let by = required_text("by", by)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            if task.status != Status::Open {
                return Err(Error::AlreadyClaimed);
            }
            if !task.needs.is_empty() {
                return Err(Error::WaitingOn(task.needs.join(", ")));
            }
            let ts = now(conn)?;
            let changed = conn.execute(
                "UPDATE tasks
                 SET status = 'claimed', claimed_by = ?1, updated_at = ?2
                 WHERE id = ?3 AND status = 'open'",
                params![by, ts, task.id],
            )?;
            if changed == 0 {
                return Err(Error::AlreadyClaimed);
            }
            insert_event(
                conn,
                EventKind::Claimed,
                serde_json::json!({ "slug": slug, "by": by }),
            )?;
            let task = load_task(conn, &slug)?;
            revalidate()?;
            Ok(task)
        })
    }

    /// Gated claim. The claim event stores `base_sha` and `graph_build_id`.
    ///
    /// `prepare` runs inside the write transaction after the task is known to
    /// be open and before the claim is written. It returns HEAD, or `None`
    /// when the repository has no commit. When HEAD exists, this inserts the
    /// claim's `implement` graph row in the same transaction. Graphify runs
    /// after commit; [`Self::set_graph_build_result`] then sets that row's
    /// status without changing its commit SHA. Later `implement` rows do not
    /// replace this anchor. The event log stays insert-only.
    pub fn claim_task_anchored(
        &mut self,
        slug: &str,
        by: &str,
        run_id: Option<i64>,
        prepare: impl FnOnce() -> Result<Option<String>, Error>,
    ) -> Result<AnchoredClaim, Error> {
        let slug = checked_slug(slug)?;
        let by = required_text("by", by)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            if task.status != Status::Open {
                return Err(Error::AlreadyClaimed);
            }
            if !task.needs.is_empty() {
                return Err(Error::WaitingOn(task.needs.join(", ")));
            }
            let base_sha = match prepare()? {
                Some(sha) => Some(required_text("base_sha", &sha)?),
                None => None,
            };
            let ts = now(conn)?;
            let changed = conn.execute(
                "UPDATE tasks
                 SET status = 'claimed', claimed_by = ?1, updated_at = ?2
                 WHERE id = ?3 AND status = 'open'",
                params![by, ts, task.id],
            )?;
            if changed == 0 {
                return Err(Error::AlreadyClaimed);
            }
            let graph_build_id = match &base_sha {
                Some(sha) => {
                    let step = graph_step("implement")?;
                    let status = graph_status("failed")?;
                    let build =
                        insert_graph_build(conn, step, Some(task.id), run_id, sha, status, None)?;
                    Some(build.id)
                }
                None => None,
            };
            insert_event(
                conn,
                EventKind::Claimed,
                serde_json::json!({
                    "slug": slug,
                    "by": by,
                    "base_sha": base_sha,
                    "graph_build_id": graph_build_id,
                }),
            )?;
            let task = load_task(conn, &slug)?;
            Ok(AnchoredClaim {
                task,
                base_sha,
                graph_build_id,
            })
        })
    }

    /// Anchor stored for the latest claim of `slug`.
    ///
    /// `None` when that claim did not record one. `graph_sha` is the claim
    /// HEAD when the linked graph row settled as `built`, and null otherwise.
    pub fn claim_anchor(&self, slug: &str) -> Result<Option<ClaimAnchor>, Error> {
        let slug = checked_slug(slug)?;
        let payload: Option<String> = self
            .conn
            .query_row(
                "SELECT payload FROM events
                 WHERE kind = 'claimed' AND json_extract(payload, '$.slug') = ?1
                 ORDER BY id DESC
                 LIMIT 1",
                params![slug],
                |row| row.get(0),
            )
            .optional()?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let Some(stored) = parse_claim_anchor(&payload)? else {
            return Ok(None);
        };
        let base_sha = stored.base_sha;
        let graph_build_id = stored.graph_build_id;
        let built = match graph_build_id {
            Some(id) => load_graph_build(&self.conn, id)?.status == "built",
            None => false,
        };
        let graph_sha = if built { base_sha.clone() } else { None };
        Ok(Some(ClaimAnchor {
            base_sha,
            graph_sha,
        }))
    }

    pub fn complete_task(&mut self, slug: &str) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            if task.status == Status::Done {
                return Ok(task);
            }
            if task.status == Status::Open {
                return Err(Error::NeedsClaim(slug.clone()));
            }
            let from = task.status.as_str();
            let ts = now(conn)?;
            conn.execute(
                "UPDATE tasks SET status = 'done', updated_at = ?1 WHERE id = ?2",
                params![ts, task.id],
            )?;
            insert_event(
                conn,
                EventKind::StatusChanged,
                serde_json::json!({ "slug": slug, "from": from, "to": "done" }),
            )?;
            load_task(conn, &slug)
        })
    }

    /// Mark a claimed task done and store `commit_sha` on that report.
    ///
    /// Pass HEAD when it can be read. This applies in every workflow. A task
    /// that is already done keeps the SHA from the first completion.
    pub fn finish_task(
        &mut self,
        slug: &str,
        sections: Option<Sections>,
        commit_sha: Option<&str>,
    ) -> Result<Task, Error> {
        self.finish_task_with(slug, sections, commit_sha, || Ok(()))
    }

    /// Same as [`Self::finish_task`]. `revalidate` runs inside this write
    /// transaction after the report insert and before commit. An error rolls
    /// the completion back, including a HEAD or cleanliness check that failed
    /// after the lock wait. `base_sha` and `graph_sha` stay null.
    pub fn finish_task_with(
        &mut self,
        slug: &str,
        sections: Option<Sections>,
        commit_sha: Option<&str>,
        revalidate: impl FnOnce() -> Result<(), Error>,
    ) -> Result<Task, Error> {
        self.finish_task_anchored(slug, sections, commit_sha, None, None, revalidate)
    }

    /// Same as [`Self::finish_task_with`], and stores the gated claim's
    /// `base_sha` and `graph_sha` on that report. A task that is already done
    /// keeps the first report unchanged.
    pub fn finish_task_anchored(
        &mut self,
        slug: &str,
        sections: Option<Sections>,
        commit_sha: Option<&str>,
        base_sha: Option<&str>,
        graph_sha: Option<&str>,
        revalidate: impl FnOnce() -> Result<(), Error>,
    ) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            if task.status == Status::Done {
                return Ok(task);
            }
            if task.status == Status::Open {
                return Err(Error::NeedsClaim(slug.clone()));
            }
            let body = match &sections {
                Some(sections) => reports::task_body(sections)?,
                None => reports::task_body(&empty_sections())?,
            };
            let from = task.status.as_str();
            let ts = now(conn)?;
            conn.execute(
                "UPDATE tasks SET status = 'done', updated_at = ?1 WHERE id = ?2",
                params![ts, task.id],
            )?;
            insert_event(
                conn,
                EventKind::StatusChanged,
                serde_json::json!({ "slug": slug, "from": from, "to": "done" }),
            )?;
            let author = task
                .claimed_by
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            reports::insert(
                conn,
                Some(task.id),
                "task",
                &author,
                &body,
                &ts,
                commit_sha,
                base_sha,
                graph_sha,
            )?;
            let task = load_task(conn, &slug)?;
            revalidate()?;
            Ok(task)
        })
    }

    /// `reports.commit_sha` on the latest task report, if one was stored.
    pub fn report_commit_sha(&self, slug: &str) -> Result<Option<String>, Error> {
        let slug = checked_slug(slug)?;
        let task = load_task(&self.conn, &slug)?;
        let sha = self
            .conn
            .query_row(
                "SELECT commit_sha FROM reports
                 WHERE task_id = ?1 AND kind = 'task'
                 ORDER BY id DESC
                 LIMIT 1",
                params![task.id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        Ok(sha)
    }

    /// Latest `kind = 'task'` report for this slug, if one was stored.
    pub fn latest_task_report(&self, slug: &str) -> Result<Option<TaskReport>, Error> {
        let slug = checked_slug(slug)?;
        let task = load_task(&self.conn, &slug)?;
        let report = self
            .conn
            .query_row(
                "SELECT body, commit_sha, base_sha, graph_sha
                 FROM reports
                 WHERE task_id = ?1 AND kind = 'task'
                 ORDER BY id DESC
                 LIMIT 1",
                params![task.id],
                |row| {
                    Ok(TaskReport {
                        body: row.get(0)?,
                        commit_sha: row.get(1)?,
                        base_sha: row.get(2)?,
                        graph_sha: row.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(report)
    }

    pub fn inbox(&self) -> Result<Vec<reports::InboxItem>, Error> {
        reports::list_unreviewed(&self.conn)
    }

    pub fn unreviewed_count(&self) -> Result<usize, Error> {
        reports::count_unreviewed(&self.conn)
    }

    pub fn settings(&self) -> Result<crate::settings::Snapshot, Error> {
        crate::settings::load(&self.conn)
    }

    pub fn set_setting(
        &mut self,
        key: &str,
        value: &str,
    ) -> Result<crate::settings::Snapshot, Error> {
        self.transaction(|conn| crate::settings::set(conn, key, value))
    }

    pub fn status_view(&self) -> Result<crate::settings::StatusView, Error> {
        crate::settings::status(&self.conn)
    }

    pub fn hook_wait_line(&self) -> Result<Option<String>, Error> {
        crate::settings::hook_wait_line(&self.conn)
    }

    pub fn assign_run(&mut self, slug: &str, run_id: i64) -> Result<bool, Error> {
        let slug = checked_slug(slug)?;
        self.transaction(|conn| link_run(conn, &slug, run_id))
    }

    pub fn adopt_backlog(&mut self, run_id: i64) -> Result<Vec<String>, Error> {
        let slugs = unassigned_work(&self.conn)?;
        let mut linked = Vec::new();
        for slug in slugs {
            if self.assign_run(&slug, run_id)? {
                linked.push(slug);
            }
        }
        Ok(linked)
    }

    pub fn absorb_dependencies(&mut self, run_id: i64) -> Result<Absorb, Error> {
        let mut linked = Vec::new();
        loop {
            let tasks = self.run_tasks(run_id)?;
            let mut added = false;
            for task in &tasks {
                if task.status == Status::Done {
                    continue;
                }
                for dependency in &task.depends_on {
                    match outside_dependency(&self.conn, dependency, run_id)? {
                        Outside::Ready => {
                            if self.assign_run(dependency, run_id)? {
                                linked.push(dependency.clone());
                                added = true;
                            }
                        }
                        Outside::Waiting => {
                            return Ok(Absorb::Waiting {
                                task: task.slug.clone(),
                                dependency: dependency.clone(),
                            });
                        }
                        Outside::Inside => {}
                    }
                }
            }
            if !added {
                break;
            }
        }
        Ok(Absorb::Linked(linked))
    }

    pub fn active_run(&self) -> Result<Option<crate::runs::Run>, Error> {
        crate::runs::active(&self.conn)
    }

    pub fn latest_run(&self) -> Result<Option<crate::runs::Run>, Error> {
        crate::runs::latest(&self.conn)
    }

    pub fn resumable_run(&self) -> Result<Option<crate::runs::Run>, Error> {
        crate::runs::resumable(&self.conn)
    }

    pub fn start_run(
        &mut self,
        prd_path: &str,
        prd_sha256: &str,
        branch: &str,
    ) -> Result<crate::runs::Run, Error> {
        self.transaction(|conn| {
            let ts = now(conn)?;
            crate::runs::insert(conn, prd_path, prd_sha256, branch, &ts)
        })
    }

    pub fn save_run(&mut self, run: &crate::runs::Run) -> Result<(), Error> {
        self.transaction(|conn| crate::runs::save(conn, run))
    }

    pub fn run_tasks(&self, run_id: i64) -> Result<Vec<Task>, Error> {
        let slugs = crate::runs::task_slugs(&self.conn, run_id)?;
        let mut tasks = Vec::new();
        for slug in slugs {
            tasks.push(load_task(&self.conn, &slug)?);
        }
        fill_dependencies(&self.conn, &mut tasks)?;
        Ok(tasks)
    }

    pub fn reopen_count(&self, slug: &str) -> Result<i64, Error> {
        crate::runs::reopen_count(&self.conn, slug)
    }

    pub fn latest_report_meta(&self, slug: &str) -> Result<Option<(i64, Option<String>)>, Error> {
        crate::runs::latest_report(&self.conn, slug)
    }

    pub fn request_run_complete(&mut self) -> Result<(), Error> {
        self.transaction(|conn| {
            let Some(mut run) = crate::runs::active(conn)? else {
                return Err(Error::InvalidSetting("no active run".to_string()));
            };
            run.complete_requested = true;
            crate::runs::save(conn, &run)
        })
    }

    pub fn accept_report(&mut self, slug: &str) -> Result<usize, Error> {
        let slug = checked_slug(slug)?;
        self.transaction(|conn| accept_unreviewed(conn, &slug))
    }

    /// Gated accept. The latest task review must be `approved` at `head`, and
    /// that commit must be the latest task report's `commit_sha`.
    /// `revalidate` runs inside the write transaction after the verdict is
    /// stored. An error rolls the accept back.
    pub fn accept_gated_report(
        &mut self,
        slug: &str,
        head: &str,
        revalidate: impl FnOnce() -> Result<(), Error>,
    ) -> Result<usize, Error> {
        let slug = checked_slug(slug)?;
        let head = required_text("head", head)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            if !approved_task_review_at(conn, task.id, &head)? {
                return Err(Error::InvalidSetting(
                    "no approved review at HEAD".to_string(),
                ));
            }
            if task.status != Status::Done {
                return Err(Error::NotDone);
            }
            let changed = accept_unreviewed(conn, &slug)?;
            revalidate()?;
            Ok(changed)
        })
    }

    pub fn accept_note(&mut self, id: i64) -> Result<(), Error> {
        self.transaction(|conn| {
            let changed = reports::accept_note(conn, id, &now(conn)?)?;
            if changed == 0 {
                return Err(Error::NoUnreviewed(id.to_string()));
            }
            Ok(())
        })
    }

    pub fn accept_all_reports(&mut self) -> Result<usize, Error> {
        self.transaction(|conn| reports::accept_all(conn, &now(conn)?))
    }

    pub fn reopen_task(&mut self, slug: &str, reason: &str) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        let reason = required_text("reason", reason)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            if task.status != Status::Done {
                return Err(Error::NotDone);
            }
            reopen_done_task(conn, &task, &reason)?;
            load_task(conn, &slug)
        })
    }

    /// Start a pending task or release review. `commit_sha` is HEAD.
    /// `fingerprint` is the worktree fingerprint captured with that HEAD.
    /// Any older pending review for the same scope and task is dropped.
    #[allow(clippy::too_many_arguments)] // one argument per reviews column the caller supplies
    pub fn start_review(
        &mut self,
        scope: &str,
        task: Option<&str>,
        run_id: Option<i64>,
        branch: &str,
        reviewer: &str,
        base_sha: Option<&str>,
        commit_sha: &str,
        graph_sha: Option<&str>,
        fingerprint: &str,
    ) -> Result<Review, Error> {
        let scope = review_scope(scope)?;
        let branch = required_text("branch", branch)?;
        let reviewer = required_text("reviewer", reviewer)?;
        let commit_sha = required_text("commit_sha", commit_sha)?;
        let task = checked_task_slug(scope, task)?;
        self.transaction(|conn| {
            let task_id = scoped_task_id(conn, scope, task.as_deref())?;
            let report_id = match task_id {
                Some(id) => latest_task_report_id(conn, id)?,
                None => None,
            };
            conn.execute(
                "DELETE FROM reviews
                 WHERE scope = ?1 AND verdict IS NULL
                   AND ((?2 IS NULL AND task_id IS NULL) OR task_id = ?2)",
                params![scope, task_id],
            )?;
            let ledger_mark = encode_mark(&capture_mark(conn)?, fingerprint)?;
            let started_at = now(conn)?;
            conn.execute(
                "INSERT INTO reviews (
                    scope, task_id, report_id, run_id, branch, reviewer, reviewer_override,
                    base_sha, commit_sha, graph_sha, ledger_mark, started_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, ?10, ?11)",
                params![
                    scope,
                    task_id,
                    report_id,
                    run_id,
                    branch,
                    reviewer,
                    base_sha,
                    commit_sha,
                    graph_sha,
                    ledger_mark,
                    started_at,
                ],
            )?;
            load_review(conn, conn.last_insert_rowid())
        })
    }

    /// Record the latest review for this scope. `sha` must equal `head` and the
    /// HEAD stored at `start_review`, and that latest review must still be
    /// pending. A task verdict of `changes_requested` reopens that task.
    #[allow(clippy::too_many_arguments)] // sha, head, fingerprint, and the verdict are distinct
    pub fn record_review(
        &mut self,
        scope: &str,
        task: Option<&str>,
        reviewer: &str,
        sha: &str,
        head: &str,
        fingerprint: &str,
        verdict: &str,
        body: &str,
        reviewer_override: bool,
    ) -> Result<Review, Error> {
        self.record_review_with(
            scope,
            task,
            reviewer,
            sha,
            head,
            fingerprint,
            verdict,
            body,
            reviewer_override,
            || Ok(()),
        )
    }

    /// Same as [`Self::record_review`]. `revalidate` runs inside this write
    /// transaction after the verdict is stored and before commit. An error
    /// rolls the recording back, including a repository check that failed
    /// after the lock wait.
    #[allow(clippy::too_many_arguments)] // same arguments as record_review, plus the recheck
    pub fn record_review_with(
        &mut self,
        scope: &str,
        task: Option<&str>,
        reviewer: &str,
        sha: &str,
        head: &str,
        fingerprint: &str,
        verdict: &str,
        body: &str,
        reviewer_override: bool,
        revalidate: impl FnOnce() -> Result<(), Error>,
    ) -> Result<Review, Error> {
        let scope = review_scope(scope)?;
        let reviewer = required_text("reviewer", reviewer)?;
        let sha = required_text("sha", sha)?;
        let head = required_text("head", head)?;
        let verdict = review_verdict(&required_text("verdict", verdict)?)?;
        let task = checked_task_slug(scope, task)?;
        let body = reports::review_body(scope, body)?;
        if sha != head {
            return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
        }
        self.transaction(|conn| {
            let task_id = scoped_task_id(conn, scope, task.as_deref())?;
            let id = pending_review_id(conn, scope, task_id)?;
            let review = load_review(conn, id)?;
            if review.commit_sha != head {
                return Err(Error::InvalidSetting(
                    "reviewer changed the repository".to_string(),
                ));
            }
            let (stored_mark, stored_fingerprint) = decode_mark(&review.ledger_mark)?;
            if stored_fingerprint != fingerprint {
                return Err(Error::InvalidSetting(
                    "reviewer changed the repository".to_string(),
                ));
            }
            if capture_mark(conn)? != stored_mark {
                return Err(Error::InvalidSetting(
                    "ledger changed during review".to_string(),
                ));
            }
            let recorded_at = now(conn)?;
            conn.execute(
                "UPDATE reviews
                 SET reviewer = ?1, reviewer_override = ?2, verdict = ?3,
                     body = ?4, bytes = ?5, recorded_at = ?6
                 WHERE id = ?7",
                params![
                    reviewer,
                    i64::from(reviewer_override),
                    verdict,
                    body,
                    body.len() as i64,
                    recorded_at,
                    id,
                ],
            )?;
            if scope == "task"
                && verdict == "changes_requested"
                && let Some(slug) = task.as_deref()
            {
                let current = load_task(conn, slug)?;
                if current.status == Status::Done {
                    reopen_done_task(conn, &current, "changes requested")?;
                }
            }
            revalidate()?;
            load_review(conn, id)
        })
    }

    pub fn latest_review(&self, scope: &str, task: Option<&str>) -> Result<Option<Review>, Error> {
        let scope = review_scope(scope)?;
        let task = checked_task_slug(scope, task)?;
        let task_id = scoped_task_id(&self.conn, scope, task.as_deref())?;
        let id = latest_review_id(&self.conn, scope, task_id)?;
        match id {
            Some(id) => Ok(Some(load_review(&self.conn, id)?)),
            None => Ok(None),
        }
    }

    pub fn record_graph_build(
        &mut self,
        step: &str,
        task: Option<&str>,
        run_id: Option<i64>,
        commit_sha: &str,
        status: &str,
        detail: Option<&str>,
    ) -> Result<GraphBuild, Error> {
        let step = graph_step(step)?;
        let status = graph_status(status)?;
        let commit_sha = required_text("commit_sha", commit_sha)?;
        let detail = match detail {
            Some(text) => Some(required_text("detail", text)?),
            None => None,
        };
        let task = match task {
            Some(slug) => Some(checked_slug(slug)?),
            None => None,
        };
        self.transaction(|conn| {
            let task_id = match &task {
                Some(slug) => Some(load_task(conn, slug)?.id),
                None => None,
            };
            insert_graph_build(
                conn,
                step,
                task_id,
                run_id,
                &commit_sha,
                status,
                detail.as_deref(),
            )
        })
    }

    /// Set `status` and `detail` on an existing graph row.
    ///
    /// `commit_sha` stays as it was inserted. Gated claim uses this to settle
    /// the anchor row after Graphify, without letting a later refresh replace
    /// that SHA.
    pub fn set_graph_build_result(
        &mut self,
        id: i64,
        status: &str,
        detail: Option<&str>,
    ) -> Result<GraphBuild, Error> {
        let status = graph_status(status)?;
        let detail = match detail {
            Some(text) => Some(required_text("detail", text)?),
            None => None,
        };
        self.transaction(|conn| {
            let updated = conn.execute(
                "UPDATE graph_builds SET status = ?1, detail = ?2 WHERE id = ?3",
                params![status, detail, id],
            )?;
            if updated == 0 {
                return Err(Error::InvalidSetting(format!("no such graph build: {id}")));
            }
            load_graph_build(conn, id)
        })
    }

    /// Latest `graph_builds` row for this step. `task` is null for `plan` and
    /// `release-review`.
    ///
    /// Not the gated claim anchor. That anchor is the claim event's
    /// `graph_build_id`.
    pub fn latest_graph_build(
        &self,
        step: &str,
        task: Option<&str>,
    ) -> Result<Option<GraphBuild>, Error> {
        let step = graph_step(step)?;
        let task = match task {
            Some(slug) => Some(checked_slug(slug)?),
            None => None,
        };
        let task_id = match &task {
            Some(slug) => Some(load_task(&self.conn, slug)?.id),
            None => None,
        };
        let id = self
            .conn
            .query_row(
                "SELECT id FROM graph_builds
                 WHERE step = ?1
                   AND ((?2 IS NULL AND task_id IS NULL) OR task_id = ?2)
                 ORDER BY id DESC
                 LIMIT 1",
                params![step, task_id],
                |row| row.get(0),
            )
            .optional()?;
        match id {
            Some(id) => Ok(Some(load_graph_build(&self.conn, id)?)),
            None => Ok(None),
        }
    }

    /// Record sign-off. `sha` must equal `head`, and the latest release review
    /// at that commit must be approved. TTY confirmation stays with the CLI.
    pub fn authorize_release(
        &mut self,
        branch: &str,
        sha: &str,
        head: &str,
        authorized_by: &str,
        run_id: Option<i64>,
    ) -> Result<ReleaseAuthorization, Error> {
        let branch = required_text("branch", branch)?;
        let sha = required_text("sha", sha)?;
        let head = required_text("head", head)?;
        let authorized_by = required_text("authorized_by", authorized_by)?;
        if sha != head {
            return Err(Error::InvalidSetting("sha does not match HEAD".to_string()));
        }
        self.transaction(|conn| {
            let review_id = match crate::lifecycle::latest_release_review_at(conn, &sha)? {
                Some(review) if review.verdict.as_deref() == Some("approved") => review.id,
                _ => {
                    return Err(Error::InvalidSetting(
                        "no approved release review".to_string(),
                    ));
                }
            };
            let created_at = now(conn)?;
            conn.execute(
                "INSERT INTO release_authorizations (
                    run_id, branch, commit_sha, review_id, authorized_by, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![run_id, branch, sha, review_id, authorized_by, created_at],
            )?;
            load_authorization(conn, conn.last_insert_rowid())
        })
    }

    pub fn lifecycle(
        &self,
        head: &str,
        commits_in_range: &[String],
    ) -> Result<crate::lifecycle::Lifecycle, Error> {
        crate::lifecycle::lifecycle(&self.conn, head, commits_in_range)
    }

    pub fn add_note(&mut self, author: &str, text: &str) -> Result<i64, Error> {
        let author = required_text("by", author)?;
        let text = required_text("text", text)?;
        let body = reports::note_body(&text)?;
        self.transaction(|conn| {
            let ts = now(conn)?;
            reports::insert(conn, None, "note", &author, &body, &ts, None, None, None)
        })
    }

    pub fn block_task(&mut self, slug: &str, reason: &str) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        let reason = required_text("reason", reason)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            match task.status {
                Status::Blocked => return Ok(task),
                Status::Done => return Err(Error::CannotBlockDone),
                Status::Open | Status::Claimed => {}
            }
            let from = task.status.as_str();
            let ts = now(conn)?;
            conn.execute(
                "UPDATE tasks SET status = 'blocked', updated_at = ?1 WHERE id = ?2",
                params![ts, task.id],
            )?;
            insert_event(
                conn,
                EventKind::StatusChanged,
                serde_json::json!({
                    "slug": slug,
                    "from": from,
                    "to": "blocked",
                    "reason": reason,
                }),
            )?;
            load_task(conn, &slug)
        })
    }

    pub fn unblock_task(&mut self, slug: &str, note: Option<&str>) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        let note = match note {
            Some(value) => Some(required_text("note", value)?),
            None => None,
        };
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            if task.status != Status::Blocked {
                return Ok(task);
            }
            let to = if task.claimed_by.is_some() {
                "claimed"
            } else {
                "open"
            };
            let ts = now(conn)?;
            conn.execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![to, ts, task.id],
            )?;
            insert_event(
                conn,
                EventKind::StatusChanged,
                serde_json::json!({
                    "slug": slug,
                    "from": "blocked",
                    "to": to,
                    "note": note,
                }),
            )?;
            load_task(conn, &slug)
        })
    }

    pub fn rename_task(&mut self, slug: &str, title: &str) -> Result<Task, Error> {
        let slug = checked_slug(slug)?;
        let title = required_text("title", title)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            if task.title == title {
                return Ok(task);
            }
            let from = task.title.clone();
            let ts = now(conn)?;
            conn.execute(
                "UPDATE tasks SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![title, ts, task.id],
            )?;
            insert_event(
                conn,
                EventKind::TaskRenamed,
                serde_json::json!({ "slug": slug, "from": from, "to": title }),
            )?;
            load_task(conn, &slug)
        })
    }

    pub fn add_step(&mut self, slug: &str, description: &str) -> Result<Step, Error> {
        let slug = checked_slug(slug)?;
        let description = required_text("description", description)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            let position: i64 = conn.query_row(
                "SELECT COALESCE(MAX(position), 0) + 1 FROM steps WHERE task_id = ?1",
                params![task.id],
                |row| row.get(0),
            )?;
            conn.execute(
                "INSERT INTO steps (task_id, position, description, done)
                 VALUES (?1, ?2, ?3, 0)",
                params![task.id, position, description],
            )?;
            Ok(Step {
                id: conn.last_insert_rowid(),
                task_id: task.id,
                position,
                description,
                done: false,
            })
        })
    }

    pub fn check_step(&mut self, slug: &str, position: i64) -> Result<Step, Error> {
        let slug = checked_slug(slug)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            let changed = conn.execute(
                "UPDATE steps SET done = 1 WHERE task_id = ?1 AND position = ?2",
                params![task.id, position],
            )?;
            if changed == 0 {
                return Err(Error::NoSuchStep(position));
            }
            load_step(conn, task.id, position)
        })
    }

    pub fn edit_step(&mut self, slug: &str, position: i64, text: &str) -> Result<Step, Error> {
        let slug = checked_slug(slug)?;
        let text = required_text("text", text)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            let step = load_step(conn, task.id, position)?;
            if step.description == text {
                return Ok(step);
            }
            conn.execute(
                "UPDATE steps SET description = ?1 WHERE task_id = ?2 AND position = ?3",
                params![text, task.id, position],
            )?;
            load_step(conn, task.id, position)
        })
    }

    pub fn report_progress(
        &mut self,
        slug: &str,
        note: &str,
        commit: Option<&str>,
    ) -> Result<Event, Error> {
        let slug = checked_slug(slug)?;
        let note = required_text("note", note)?;
        let commit = commit
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        self.transaction(|conn| {
            load_task(conn, &slug)?;
            if let Some(sha) = &commit
                && progress_exists(conn, &slug, sha)?
            {
                return Err(Error::AlreadyReported(sha.clone()));
            }
            match insert_event(
                conn,
                EventKind::Progress,
                serde_json::json!({ "slug": slug, "note": note, "commit": commit }),
            ) {
                Err(Error::Sqlite(err))
                    if constraint_message(&err)
                        .is_some_and(|msg| msg.contains("events_progress_once")) =>
                {
                    Err(Error::AlreadyReported(commit.unwrap_or_default()))
                }
                other => other,
            }
        })
    }

    pub fn log_decision(&mut self, slug: &str, why: &str, by: &str) -> Result<Decision, Error> {
        let slug = checked_slug(slug)?;
        let why = required_text("why", why)?;
        let by = required_text("by", by)?;
        self.transaction(|conn| {
            let task = load_task(conn, &slug)?;
            let ts = now(conn)?;
            conn.execute(
                "INSERT INTO decisions (task_id, rationale, provenance, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![task.id, why, by, ts],
            )?;
            let id = conn.last_insert_rowid();
            insert_event(
                conn,
                EventKind::Decision,
                serde_json::json!({
                    "slug": slug,
                    "why": why,
                    "by": by,
                    "decision_id": id,
                }),
            )?;
            Ok(Decision {
                id,
                task_id: task.id,
                rationale: why,
                provenance: by,
                created_at: ts,
            })
        })
    }

    pub fn context(&self, slug: Option<&str>, event_limit: usize) -> Result<Context, Error> {
        self.context_view(slug, true, Some(event_limit))
    }

    pub fn context_view(
        &self,
        slug: Option<&str>,
        include_done: bool,
        event_limit: Option<usize>,
    ) -> Result<Context, Error> {
        let slug = match slug {
            Some(value) => Some(checked_slug(value)?),
            None => None,
        };
        let tasks = self.scoped_tasks(slug.as_deref(), include_done)?;
        let hidden_done = self.hidden_done_count(&tasks)?;
        let events = match event_limit {
            Some(limit) => self.recent_events(slug.as_deref(), limit)?,
            None => Vec::new(),
        };
        let unreviewed_reports = reports::count_unreviewed(&self.conn)?;
        let latest_report = if slug.is_some() {
            tasks
                .first()
                .map(|detail| reports::latest_body(&self.conn, detail.task.id))
                .transpose()?
                .flatten()
        } else {
            None
        };
        let (latest_review, graph_sha) = self.task_context_extras(slug.as_deref())?;
        Ok(Context {
            tasks,
            events,
            hidden_done,
            unreviewed_reports,
            latest_report,
            latest_review,
            graph_sha,
        })
    }

    /// Review row and report graph SHA for a single-task view.
    ///
    /// Read-only opens do not migrate. A v6 ledger has no `reviews` table and
    /// no `reports.graph_sha` column, so those extras are absent instead of
    /// failing the read that older ledgers already supported.
    fn task_context_extras(
        &self,
        slug: Option<&str>,
    ) -> Result<(Option<serde_json::Value>, Option<String>), Error> {
        let Some(slug) = slug else {
            return Ok((None, None));
        };
        let latest_review = if sqlite_table_exists(&self.conn, "reviews")? {
            self.latest_review("task", Some(slug))?
                .map(|review| {
                    serde_json::to_value(review).map_err(|err| Error::Json(err.to_string()))
                })
                .transpose()?
        } else {
            None
        };
        let graph_sha = if reports_have_column(&self.conn, "graph_sha")? {
            report_graph_sha(&self.conn, slug)?
        } else {
            None
        };
        Ok((latest_review, graph_sha))
    }

    fn scoped_tasks(
        &self,
        slug: Option<&str>,
        include_done: bool,
    ) -> Result<Vec<TaskDetail>, Error> {
        let tasks = if let Some(slug) = slug {
            vec![load_task(&self.conn, slug)?]
        } else {
            let mut tasks = self.list_tasks(None, false, true)?;
            if !include_done {
                tasks.retain(|task| task.status != Status::Done);
            }
            tasks
        };
        let mut details = Vec::with_capacity(tasks.len());
        for task in tasks {
            let steps = load_steps(&self.conn, task.id)?;
            let decisions = load_decisions(&self.conn, task.id)?;
            details.push(TaskDetail {
                task,
                steps,
                decisions,
            });
        }
        Ok(details)
    }

    fn hidden_done_count(&self, shown: &[TaskDetail]) -> Result<usize, Error> {
        let done = self.list_tasks(Some(Status::Done), false, true)?;
        let shown_done = shown
            .iter()
            .filter(|detail| detail.task.status == Status::Done)
            .count();
        Ok(done.len().saturating_sub(shown_done))
    }

    fn recent_events(&self, slug: Option<&str>, event_limit: usize) -> Result<Vec<Event>, Error> {
        let limit = i64::try_from(event_limit).unwrap_or(i64::MAX);
        let slug_param = slug.map(ToOwned::to_owned);
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, kind, payload
             FROM events
             WHERE (?1 IS NULL OR json_extract(payload, '$.slug') = ?1)
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![slug_param, limit], read_event_row)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(into_event(row?)?);
        }
        Ok(events)
    }

    fn transaction<T>(
        &mut self,
        body: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = body(&tx);
        match result {
            Ok(value) => {
                tx.commit()?;
                let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
                Ok(value)
            }
            Err(err) => Err(err),
        }
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
}

fn sqlite_table_exists(conn: &Connection, name: &str) -> Result<bool, Error> {
    let found = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(1_i64),
        )
        .optional()?;
    Ok(found.is_some())
}

fn reports_have_column(conn: &Connection, column: &str) -> Result<bool, Error> {
    let mut statement = conn.prepare("PRAGMA table_info(reports)")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `reports.graph_sha` for the latest task report. Callers check the column
/// first so a v6 `reports` table, which has no SHA columns, is not queried.
fn report_graph_sha(conn: &Connection, slug: &str) -> Result<Option<String>, Error> {
    let task = load_task(conn, slug)?;
    let sha = conn
        .query_row(
            "SELECT graph_sha FROM reports
             WHERE task_id = ?1 AND kind = 'task'
             ORDER BY id DESC
             LIMIT 1",
            params![task.id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(sha.flatten())
}

fn configure(conn: &Connection) -> Result<(), Error> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

pub(crate) fn timestamp(conn: &Connection) -> Result<String, Error> {
    now(conn)
}

pub(crate) fn record_event(
    conn: &Connection,
    kind: EventKind,
    payload: serde_json::Value,
) -> Result<Event, Error> {
    insert_event(conn, kind, payload)
}

fn now(conn: &Connection) -> Result<String, Error> {
    let ts = conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
        row.get(0)
    })?;
    Ok(ts)
}

pub enum Absorb {
    Linked(Vec<String>),
    Waiting { task: String, dependency: String },
}

enum Outside {
    Inside,
    Ready,
    Waiting,
}

fn link_run(conn: &Connection, slug: &str, run_id: i64) -> Result<bool, Error> {
    let task = load_task(conn, slug)?;
    if task.status == Status::Done {
        return Err(Error::InvalidSetting(format!("{slug} is done")));
    }
    let owner: Option<i64> = conn.query_row(
        "SELECT run_id FROM tasks WHERE slug = ?1",
        params![slug],
        |row| row.get(0),
    )?;
    if owner == Some(run_id) {
        return Ok(false);
    }
    crate::runs::assign_task(conn, slug, run_id)?;
    record_event(
        conn,
        EventKind::RunLinked,
        serde_json::json!({ "slug": slug, "run_id": run_id }),
    )?;
    Ok(true)
}

fn unassigned_work(conn: &Connection) -> Result<Vec<String>, Error> {
    let mut statement = conn.prepare(
        "SELECT slug FROM tasks
         WHERE run_id IS NULL AND status IN ('open', 'claimed')
         ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut slugs = Vec::new();
    for row in rows {
        slugs.push(row?);
    }
    Ok(slugs)
}

fn outside_dependency(conn: &Connection, dependency: &str, run_id: i64) -> Result<Outside, Error> {
    let (status, owner): (String, Option<i64>) = conn.query_row(
        "SELECT status, run_id FROM tasks WHERE slug = ?1",
        params![dependency],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if status == Status::Done.as_str() || owner == Some(run_id) {
        return Ok(Outside::Inside);
    }
    if status == Status::Blocked.as_str() || owner.is_some() {
        return Ok(Outside::Waiting);
    }
    Ok(Outside::Ready)
}

fn insert_event(
    conn: &Connection,
    kind: EventKind,
    payload: serde_json::Value,
) -> Result<Event, Error> {
    let ts = now(conn)?;
    let payload_text =
        serde_json::to_string(&payload).map_err(|err| Error::Json(err.to_string()))?;
    conn.execute(
        "INSERT INTO events (ts, kind, payload) VALUES (?1, ?2, ?3)",
        params![ts, kind.as_str(), payload_text],
    )?;
    Ok(Event {
        id: conn.last_insert_rowid(),
        ts,
        kind,
        payload,
    })
}

fn progress_exists(conn: &Connection, slug: &str, commit: &str) -> Result<bool, Error> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM events
             WHERE kind = 'progress'
               AND json_extract(payload, '$.slug') = ?1
               AND json_extract(payload, '$.commit') = ?2
             LIMIT 1",
            params![slug, commit],
            |row| row.get(0),
        )
        .optional()?;
    Ok(existing.is_some())
}

fn load_task(conn: &Connection, slug: &str) -> Result<Task, Error> {
    let row = conn
        .query_row(
            "SELECT id, slug, title, status, claimed_by, created_at, updated_at
             FROM tasks WHERE slug = ?1",
            params![slug],
            read_task_row,
        )
        .optional()?;
    let Some(row) = row else {
        return Err(Error::NoSuchTask(slug.to_string()));
    };
    let mut task = into_task(row)?;
    fill_dependencies(conn, std::slice::from_mut(&mut task))?;
    task.reopened = reports::reopened_reason(conn, task.id)?;
    Ok(task)
}

fn load_step(conn: &Connection, task_id: i64, position: i64) -> Result<Step, Error> {
    let row = conn
        .query_row(
            "SELECT id, task_id, position, description, done
             FROM steps WHERE task_id = ?1 AND position = ?2",
            params![task_id, position],
            read_step_row,
        )
        .optional()?;
    row.map(into_step).ok_or(Error::NoSuchStep(position))
}

fn load_steps(conn: &Connection, task_id: i64) -> Result<Vec<Step>, Error> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, position, description, done
         FROM steps WHERE task_id = ?1 ORDER BY position",
    )?;
    let rows = stmt.query_map(params![task_id], read_step_row)?;
    let mut steps = Vec::new();
    for row in rows {
        steps.push(into_step(row?));
    }
    Ok(steps)
}

fn load_decisions(conn: &Connection, task_id: i64) -> Result<Vec<Decision>, Error> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, rationale, provenance, created_at
         FROM decisions WHERE task_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![task_id], |row| {
        Ok(Decision {
            id: row.get(0)?,
            task_id: row.get(1)?,
            rationale: row.get(2)?,
            provenance: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    let mut decisions = Vec::new();
    for row in rows {
        decisions.push(row?);
    }
    Ok(decisions)
}

struct TaskRow {
    id: i64,
    slug: String,
    title: String,
    status: String,
    claimed_by: Option<String>,
    created_at: String,
    updated_at: String,
}

fn read_task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        id: row.get(0)?,
        slug: row.get(1)?,
        title: row.get(2)?,
        status: row.get(3)?,
        claimed_by: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn into_task(row: TaskRow) -> Result<Task, Error> {
    Ok(Task {
        id: row.id,
        slug: row.slug,
        title: row.title,
        status: row.status.parse()?,
        claimed_by: row.claimed_by,
        reopened: None,
        created_at: row.created_at,
        updated_at: row.updated_at,
        depends_on: Vec::new(),
        needs: Vec::new(),
    })
}

fn collect_tasks(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<TaskRow>>,
) -> Result<Vec<Task>, Error> {
    let mut tasks = Vec::new();
    for row in rows {
        tasks.push(into_task(row?)?);
    }
    Ok(tasks)
}

struct StepRow {
    id: i64,
    task_id: i64,
    position: i64,
    description: String,
    done: i64,
}

fn read_step_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StepRow> {
    Ok(StepRow {
        id: row.get(0)?,
        task_id: row.get(1)?,
        position: row.get(2)?,
        description: row.get(3)?,
        done: row.get(4)?,
    })
}

fn into_step(row: StepRow) -> Step {
    Step {
        id: row.id,
        task_id: row.task_id,
        position: row.position,
        description: row.description,
        done: row.done != 0,
    }
}

struct EventRow {
    id: i64,
    ts: String,
    kind: String,
    payload: String,
}

fn read_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        id: row.get(0)?,
        ts: row.get(1)?,
        kind: row.get(2)?,
        payload: row.get(3)?,
    })
}

fn into_event(row: EventRow) -> Result<Event, Error> {
    let payload = serde_json::from_str(&row.payload).map_err(|err| Error::Json(err.to_string()))?;
    Ok(Event {
        id: row.id,
        ts: row.ts,
        kind: row.kind.parse()?,
        payload,
    })
}

fn accept_unreviewed(conn: &Connection, slug: &str) -> Result<usize, Error> {
    let task = load_task(conn, slug)?;
    let changed = reports::accept_task(conn, task.id, &now(conn)?)?;
    if changed == 0 {
        return Err(Error::NoUnreviewed(slug.to_string()));
    }
    Ok(changed)
}

fn approved_task_review_at(conn: &Connection, task_id: i64, head: &str) -> Result<bool, Error> {
    let report_sha: Option<String> = conn
        .query_row(
            "SELECT commit_sha FROM reports
             WHERE task_id = ?1 AND kind = 'task'
             ORDER BY id DESC
             LIMIT 1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let review: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT commit_sha, verdict FROM reviews
             WHERE scope = 'task' AND task_id = ?1
             ORDER BY id DESC
             LIMIT 1",
            params![task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(match review {
        Some((sha, verdict)) => {
            verdict.as_deref() == Some("approved")
                && sha == head
                && report_sha.as_deref() == Some(head)
        }
        None => false,
    })
}

fn reopen_done_task(conn: &Connection, task: &Task, reason: &str) -> Result<(), Error> {
    let ts = now(conn)?;
    conn.execute(
        "UPDATE tasks SET status = 'open', updated_at = ?1 WHERE id = ?2",
        params![ts, task.id],
    )?;
    insert_event(
        conn,
        EventKind::StatusChanged,
        serde_json::json!({
            "slug": task.slug,
            "from": "done",
            "to": "open",
            "reason": reason,
        }),
    )?;
    reports::mark_latest_reopened(conn, task.id, reason, &ts)?;
    Ok(())
}

fn review_scope(scope: &str) -> Result<&'static str, Error> {
    match scope.trim() {
        "task" => Ok("task"),
        "release" => Ok("release"),
        other => Err(Error::InvalidSetting(format!(
            "invalid review scope: {other}"
        ))),
    }
}

fn review_verdict(verdict: &str) -> Result<&'static str, Error> {
    match verdict {
        "approved" => Ok("approved"),
        "changes_requested" | "changes-requested" => Ok("changes_requested"),
        other => Err(Error::InvalidSetting(format!("invalid verdict: {other}"))),
    }
}

fn graph_step(step: &str) -> Result<&'static str, Error> {
    match step.trim() {
        "plan" => Ok("plan"),
        "implement" => Ok("implement"),
        "commit" => Ok("commit"),
        "review" => Ok("review"),
        "release-review" => Ok("release-review"),
        other => Err(Error::InvalidSetting(format!(
            "invalid graph step: {other}"
        ))),
    }
}

fn graph_status(status: &str) -> Result<&'static str, Error> {
    match status.trim() {
        "built" => Ok("built"),
        "skipped" => Ok("skipped"),
        "failed" => Ok("failed"),
        other => Err(Error::InvalidSetting(format!(
            "invalid graph status: {other}"
        ))),
    }
}

fn checked_task_slug(scope: &str, task: Option<&str>) -> Result<Option<String>, Error> {
    match (scope, task) {
        ("task", Some(slug)) => Ok(Some(checked_slug(slug)?)),
        ("task", None) => Err(Error::InvalidSetting(
            "task is required for a task review".to_string(),
        )),
        ("release", None) => Ok(None),
        ("release", Some(_)) => Err(Error::InvalidSetting(
            "a release review has no task".to_string(),
        )),
        _ => Err(Error::InvalidSetting(format!(
            "invalid review scope: {scope}"
        ))),
    }
}

fn scoped_task_id(
    conn: &Connection,
    scope: &str,
    task: Option<&str>,
) -> Result<Option<i64>, Error> {
    match (scope, task) {
        ("task", Some(slug)) => Ok(Some(load_task(conn, slug)?.id)),
        ("task", None) => Err(Error::InvalidSetting(
            "task is required for a task review".to_string(),
        )),
        ("release", _) => Ok(None),
        _ => Err(Error::InvalidSetting(format!(
            "invalid review scope: {scope}"
        ))),
    }
}

fn latest_task_report_id(conn: &Connection, task_id: i64) -> Result<Option<i64>, Error> {
    let id = conn
        .query_row(
            "SELECT id FROM reports
             WHERE task_id = ?1 AND kind = 'task'
             ORDER BY id DESC
             LIMIT 1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(id)
}

fn pending_review_id(conn: &Connection, scope: &str, task_id: Option<i64>) -> Result<i64, Error> {
    let row: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT id, verdict FROM reviews
             WHERE scope = ?1
               AND ((?2 IS NULL AND task_id IS NULL) OR task_id = ?2)
             ORDER BY id DESC
             LIMIT 1",
            params![scope, task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match row {
        Some((id, None)) => Ok(id),
        _ => Err(Error::InvalidSetting("no pending review".to_string())),
    }
}

fn latest_review_id(
    conn: &Connection,
    scope: &str,
    task_id: Option<i64>,
) -> Result<Option<i64>, Error> {
    let id = conn
        .query_row(
            "SELECT id FROM reviews
             WHERE scope = ?1
               AND ((?2 IS NULL AND task_id IS NULL) OR task_id = ?2)
             ORDER BY id DESC
             LIMIT 1",
            params![scope, task_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(id)
}

#[derive(Debug, PartialEq, Eq)]
struct LedgerMark {
    events: Option<i64>,
    reports: Option<i64>,
    decisions: Option<i64>,
    steps: Option<i64>,
    tasks_updated_at: Option<String>,
}

fn capture_mark(conn: &Connection) -> Result<LedgerMark, Error> {
    Ok(LedgerMark {
        events: max_id(conn, "events")?,
        reports: max_id(conn, "reports")?,
        decisions: max_id(conn, "decisions")?,
        steps: max_id(conn, "steps")?,
        tasks_updated_at: conn
            .query_row("SELECT MAX(updated_at) FROM tasks", [], |row| row.get(0))?,
    })
}

fn max_id(conn: &Connection, table: &str) -> Result<Option<i64>, Error> {
    let sql = match table {
        "events" => "SELECT MAX(id) FROM events",
        "reports" => "SELECT MAX(id) FROM reports",
        "decisions" => "SELECT MAX(id) FROM decisions",
        "steps" => "SELECT MAX(id) FROM steps",
        other => {
            return Err(Error::InvalidSetting(format!(
                "unknown mark table: {other}"
            )));
        }
    };
    let value = conn.query_row(sql, [], |row| row.get(0))?;
    Ok(value)
}

fn encode_mark(mark: &LedgerMark, fingerprint: &str) -> Result<String, Error> {
    serde_json::to_string(&serde_json::json!({
        "events": mark.events,
        "reports": mark.reports,
        "decisions": mark.decisions,
        "steps": mark.steps,
        "tasks_updated_at": mark.tasks_updated_at,
        "fingerprint": fingerprint,
    }))
    .map_err(|err| Error::Json(err.to_string()))
}

fn decode_mark(text: &str) -> Result<(LedgerMark, String), Error> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|err| Error::Json(err.to_string()))?;
    let number = |key: &str| value.get(key).and_then(serde_json::Value::as_i64);
    let updated = value
        .get("tasks_updated_at")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let fingerprint = value
        .get("fingerprint")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok((
        LedgerMark {
            events: number("events"),
            reports: number("reports"),
            decisions: number("decisions"),
            steps: number("steps"),
            tasks_updated_at: updated,
        },
        fingerprint,
    ))
}

struct ReviewRow {
    id: i64,
    scope: String,
    task_id: Option<i64>,
    report_id: Option<i64>,
    run_id: Option<i64>,
    branch: String,
    reviewer: String,
    reviewer_override: i64,
    base_sha: Option<String>,
    commit_sha: String,
    graph_sha: Option<String>,
    ledger_mark: String,
    verdict: Option<String>,
    body: Option<String>,
    bytes: Option<i64>,
    started_at: String,
    recorded_at: Option<String>,
}

fn load_review(conn: &Connection, id: i64) -> Result<Review, Error> {
    let row = conn
        .query_row(
            "SELECT id, scope, task_id, report_id, run_id, branch, reviewer, reviewer_override,
                    base_sha, commit_sha, graph_sha, ledger_mark, verdict, body, bytes,
                    started_at, recorded_at
             FROM reviews WHERE id = ?1",
            params![id],
            |row| {
                Ok(ReviewRow {
                    id: row.get(0)?,
                    scope: row.get(1)?,
                    task_id: row.get(2)?,
                    report_id: row.get(3)?,
                    run_id: row.get(4)?,
                    branch: row.get(5)?,
                    reviewer: row.get(6)?,
                    reviewer_override: row.get(7)?,
                    base_sha: row.get(8)?,
                    commit_sha: row.get(9)?,
                    graph_sha: row.get(10)?,
                    ledger_mark: row.get(11)?,
                    verdict: row.get(12)?,
                    body: row.get(13)?,
                    bytes: row.get(14)?,
                    started_at: row.get(15)?,
                    recorded_at: row.get(16)?,
                })
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Err(Error::InvalidSetting(format!("no such review: {id}")));
    };
    let (_, fingerprint) = decode_mark(&row.ledger_mark)?;
    let body = match row.body {
        Some(text) => {
            Some(serde_json::from_str(&text).map_err(|err| Error::Json(err.to_string()))?)
        }
        None => None,
    };
    Ok(Review {
        id: row.id,
        scope: row.scope,
        task_id: row.task_id,
        report_id: row.report_id,
        run_id: row.run_id,
        branch: row.branch,
        reviewer: row.reviewer,
        reviewer_override: row.reviewer_override != 0,
        base_sha: row.base_sha,
        commit_sha: row.commit_sha,
        graph_sha: row.graph_sha,
        ledger_mark: row.ledger_mark,
        fingerprint,
        verdict: row.verdict,
        body,
        bytes: row.bytes,
        started_at: row.started_at,
        recorded_at: row.recorded_at,
    })
}

fn insert_graph_build(
    conn: &Connection,
    step: &str,
    task_id: Option<i64>,
    run_id: Option<i64>,
    commit_sha: &str,
    status: &str,
    detail: Option<&str>,
) -> Result<GraphBuild, Error> {
    let created_at = now(conn)?;
    conn.execute(
        "INSERT INTO graph_builds (
            step, task_id, run_id, commit_sha, status, detail, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            step, task_id, run_id, commit_sha, status, detail, created_at
        ],
    )?;
    load_graph_build(conn, conn.last_insert_rowid())
}

struct StoredClaimAnchor {
    base_sha: Option<String>,
    graph_build_id: Option<i64>,
}

/// `None` when this claim event has no anchor fields. Both fields are present
/// on a gated claim, including JSON null when that SHA or row does not exist.
fn parse_claim_anchor(payload: &str) -> Result<Option<StoredClaimAnchor>, Error> {
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|err| Error::Json(err.to_string()))?;
    match (value.get("base_sha"), value.get("graph_build_id")) {
        (None, None) => Ok(None),
        (None, Some(_)) | (Some(_), None) => Err(Error::InvalidSetting(
            "claim anchor is incomplete".to_string(),
        )),
        (Some(base_sha), Some(graph_build_id)) => Ok(Some(StoredClaimAnchor {
            base_sha: anchor_sha(base_sha)?,
            graph_build_id: anchor_id(graph_build_id)?,
        })),
    }
}

fn anchor_sha(value: &serde_json::Value) -> Result<Option<String>, Error> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(text) => Ok(Some(required_text("base_sha", text)?)),
        _ => Err(Error::InvalidSetting(
            "invalid claim anchor field: base_sha".to_string(),
        )),
    }
}

fn anchor_id(value: &serde_json::Value) -> Result<Option<i64>, Error> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(number) => match number.as_i64() {
            Some(id) => Ok(Some(id)),
            None => Err(Error::InvalidSetting(
                "invalid claim anchor field: graph_build_id".to_string(),
            )),
        },
        _ => Err(Error::InvalidSetting(
            "invalid claim anchor field: graph_build_id".to_string(),
        )),
    }
}

fn load_graph_build(conn: &Connection, id: i64) -> Result<GraphBuild, Error> {
    let row = conn
        .query_row(
            "SELECT id, step, task_id, run_id, commit_sha, status, detail, created_at
             FROM graph_builds WHERE id = ?1",
            params![id],
            |row| {
                Ok(GraphBuild {
                    id: row.get(0)?,
                    step: row.get(1)?,
                    task_id: row.get(2)?,
                    run_id: row.get(3)?,
                    commit_sha: row.get(4)?,
                    status: row.get(5)?,
                    detail: row.get(6)?,
                    created_at: row.get(7)?,
                })
            },
        )
        .optional()?;
    row.ok_or_else(|| Error::InvalidSetting(format!("no such graph build: {id}")))
}

fn load_authorization(conn: &Connection, id: i64) -> Result<ReleaseAuthorization, Error> {
    let row = conn
        .query_row(
            "SELECT id, run_id, branch, commit_sha, review_id, authorized_by, created_at
             FROM release_authorizations WHERE id = ?1",
            params![id],
            |row| {
                Ok(ReleaseAuthorization {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    branch: row.get(2)?,
                    commit_sha: row.get(3)?,
                    review_id: row.get(4)?,
                    authorized_by: row.get(5)?,
                    created_at: row.get(6)?,
                })
            },
        )
        .optional()?;
    row.ok_or_else(|| Error::InvalidSetting(format!("no such release authorization: {id}")))
}

fn constraint_message(err: &rusqlite::Error) -> Option<&str> {
    match err {
        rusqlite::Error::SqliteFailure(_, Some(message)) => Some(message.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::Ledger;
    use crate::model::{EventKind, Status};

    fn open_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = Ledger::initialize(&dir.path().join("ledger.db")).expect("init");
        (dir, ledger)
    }

    #[test]
    fn rejects_bad_slug_and_blank_title() {
        let (_dir, mut ledger) = open_ledger();
        assert_eq!(
            ledger
                .create_task("Auth", "Add auth")
                .expect_err("slug")
                .to_string(),
            "invalid slug: Auth"
        );
        assert_eq!(
            ledger
                .create_task("auth", "  ")
                .expect_err("title")
                .to_string(),
            "title is required"
        );
    }

    #[test]
    fn duplicate_slug_names_the_task() {
        let (_dir, mut ledger) = open_ledger();
        ledger.create_task("auth", "Add auth").expect("create");
        assert_eq!(
            ledger
                .create_task("auth", "Again")
                .expect_err("dup")
                .to_string(),
            "task already exists: auth"
        );
    }

    #[test]
    fn missing_claim_and_second_claim_use_distinct_errors() {
        let (_dir, mut ledger) = open_ledger();
        assert_eq!(
            ledger
                .claim_task("auth", "cursor")
                .expect_err("missing")
                .to_string(),
            "no such task: auth"
        );
        ledger.create_task("auth", "Add auth").expect("create");
        ledger.claim_task("auth", "cursor").expect("claim");
        assert_eq!(
            ledger
                .claim_task("auth", "other")
                .expect_err("taken")
                .to_string(),
            "already claimed"
        );
    }

    #[test]
    fn claim_anchor_ignores_later_implement_builds() {
        let (_dir, mut ledger) = open_ledger();
        ledger.create_task("plain", "Plain").expect("create");
        ledger.claim_task("plain", "cursor").expect("claim");
        assert!(ledger.claim_anchor("plain").expect("anchor").is_none());

        ledger.create_task("auth", "Add auth").expect("create");
        ledger
            .record_graph_build("implement", Some("auth"), None, "aaa", "built", None)
            .expect("earlier build");
        let claimed = ledger
            .claim_task_anchored("auth", "cursor", None, || Ok(Some("bbb".to_string())))
            .expect("anchored claim");
        assert_eq!(claimed.base_sha.as_deref(), Some("bbb"));
        let id = claimed.graph_build_id.expect("anchor row");
        let settled = ledger
            .set_graph_build_result(id, "built", None)
            .expect("settle");
        assert_eq!(settled.commit_sha, "bbb");
        assert_eq!(settled.status, "built");
        ledger
            .record_graph_build("implement", Some("auth"), None, "ccc", "built", None)
            .expect("later build");

        let anchor = ledger.claim_anchor("auth").expect("anchor").expect("some");
        assert_eq!(anchor.base_sha.as_deref(), Some("bbb"));
        assert_eq!(anchor.graph_sha.as_deref(), Some("bbb"));
        let replayed =
            crate::replay::replay(&ledger.context(Some("auth"), 20).expect("context").events)
                .expect("replay");
        assert_eq!(replayed[0].status, Status::Claimed);
        assert_eq!(replayed[0].claimed_by.as_deref(), Some("cursor"));
    }

    #[test]
    fn steps_start_at_one_and_unknown_positions_fail() {
        let (_dir, mut ledger) = open_ledger();
        ledger.create_task("auth", "Add auth").expect("create");
        let first = ledger.add_step("auth", "hash passwords").expect("step");
        let second = ledger.add_step("auth", "write tests").expect("step");
        assert_eq!(first.position, 1);
        assert_eq!(second.position, 2);
        assert!(!first.done);
        let checked = ledger.check_step("auth", 1).expect("check");
        assert!(checked.done);
        assert_eq!(
            ledger
                .check_step("auth", 9)
                .expect_err("missing")
                .to_string(),
            "no such step: 9"
        );
    }

    #[test]
    fn list_filters_status_and_done_is_idempotent() {
        let (_dir, mut ledger) = open_ledger();
        ledger.create_task("auth", "Add auth").expect("create");
        ledger
            .create_task("ledger", "Open the ledger")
            .expect("create");
        ledger.claim_task("auth", "cursor").expect("claim");
        let open = ledger
            .list_tasks(Some(Status::Open), false, true)
            .expect("list");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].slug, "ledger");
        ledger.complete_task("auth").expect("done");
        let before = ledger.context(Some("auth"), 20).expect("context");
        let status_events = before
            .events
            .iter()
            .filter(|event| event.kind == EventKind::StatusChanged)
            .count();
        ledger.complete_task("auth").expect("done again");
        let after = ledger.context(Some("auth"), 20).expect("context");
        assert_eq!(
            after
                .events
                .iter()
                .filter(|event| event.kind == EventKind::StatusChanged)
                .count(),
            status_events
        );
        assert_eq!(after.tasks[0].task.status, Status::Done);
    }

    #[test]
    fn v1_ledger_keeps_event_bytes_and_init_does_not_downgrade() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ledger.db");
        let payload = r#"{"slug":"auth","title":"Add auth"}"#;
        {
            let conn = rusqlite::Connection::open(&path).expect("open");
            conn.execute_batch(
                "
                CREATE TABLE tasks (
                    id INTEGER PRIMARY KEY,
                    slug TEXT NOT NULL UNIQUE,
                    title TEXT NOT NULL,
                    status TEXT NOT NULL,
                    claimed_by TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE events (
                    id INTEGER PRIMARY KEY,
                    ts TEXT NOT NULL,
                    kind TEXT NOT NULL CHECK (
                        kind IN ('task_created', 'claimed', 'progress', 'decision', 'status_changed')
                    ),
                    payload TEXT NOT NULL
                );
                ",
            )
            .expect("v1 schema");
            conn.pragma_update(None, "user_version", 1)
                .expect("version");
            conn.execute(
                "INSERT INTO events (id, ts, kind, payload) VALUES (7, '2026-01-01T00:00:00.000Z', 'task_created', ?1)",
                rusqlite::params![payload],
            )
            .expect("insert");
        }

        let ledger = Ledger::open(&path).expect("migrate");
        drop(ledger);
        let conn = rusqlite::Connection::open(&path).expect("reopen");
        let stored: (i64, String, String, String) = conn
            .query_row("SELECT id, ts, kind, payload FROM events", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("event");
        assert_eq!(stored.0, 7);
        assert_eq!(stored.1, "2026-01-01T00:00:00.000Z");
        assert_eq!(stored.2, "task_created");
        assert_eq!(stored.3, payload);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 7);
        let deps: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'task_deps'",
                [],
                |row| row.get(0),
            )
            .expect("task_deps");
        assert_eq!(deps, 1);

        drop(conn);
        Ledger::initialize(&path).expect("init existing");
        let conn = rusqlite::Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 7);
        let stored_again: String = conn
            .query_row("SELECT payload FROM events WHERE id = 7", [], |row| {
                row.get(0)
            })
            .expect("payload");
        assert_eq!(stored_again, payload);
    }

    #[test]
    fn v2_ledger_gains_reports_without_touching_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ledger.db");
        let payload = r#"{"slug":"auth","from":"open","to":"done"}"#;
        {
            let conn = rusqlite::Connection::open(&path).expect("open");
            conn.execute_batch(
                "
                CREATE TABLE events (
                    id INTEGER PRIMARY KEY,
                    ts TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    payload TEXT NOT NULL
                );
                ",
            )
            .expect("schema");
            conn.pragma_update(None, "user_version", 2)
                .expect("version");
            conn.execute(
                "INSERT INTO events (id, ts, kind, payload) VALUES (3, '2026-01-01T00:00:00.000Z', 'status_changed', ?1)",
                rusqlite::params![payload],
            )
            .expect("insert");
        }
        let ledger = Ledger::open(&path).expect("migrate");
        drop(ledger);
        let conn = rusqlite::Connection::open(&path).expect("reopen");
        let stored: String = conn
            .query_row("SELECT payload FROM events WHERE id = 3", [], |row| {
                row.get(0)
            })
            .expect("payload");
        assert_eq!(stored, payload);
        let reports: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'reports'",
                [],
                |row| row.get(0),
            )
            .expect("reports");
        assert_eq!(reports, 1);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 7);
    }

    #[test]
    fn v3_ledger_gains_settings_without_touching_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ledger.db");
        let payload = r#"{"slug":"auth","from":"open","to":"done"}"#;
        {
            let conn = rusqlite::Connection::open(&path).expect("open");
            conn.execute_batch(
                "
                CREATE TABLE events (
                    id INTEGER PRIMARY KEY,
                    ts TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    payload TEXT NOT NULL
                );
                CREATE TABLE reports (
                    id INTEGER PRIMARY KEY,
                    task_id INTEGER,
                    kind TEXT NOT NULL,
                    author TEXT NOT NULL,
                    body TEXT NOT NULL,
                    bytes INTEGER NOT NULL,
                    created_at TEXT NOT NULL
                );
                ",
            )
            .expect("schema");
            conn.pragma_update(None, "user_version", 3)
                .expect("version");
            conn.execute(
                "INSERT INTO events (id, ts, kind, payload) VALUES (4, '2026-01-01T00:00:00.000Z', 'status_changed', ?1)",
                rusqlite::params![payload],
            )
            .expect("insert");
        }
        let ledger = Ledger::open(&path).expect("migrate");
        let settings = ledger.settings().expect("settings");
        assert_eq!(settings.mode, "manual");
        assert_eq!(settings.max_agent_calls, 60);
        assert_eq!(settings.est_cost_cap_usd, None);
        drop(ledger);
        let conn = rusqlite::Connection::open(&path).expect("reopen");
        let stored: String = conn
            .query_row("SELECT payload FROM events WHERE id = 4", [], |row| {
                row.get(0)
            })
            .expect("payload");
        assert_eq!(stored, payload);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 7);
    }
}
