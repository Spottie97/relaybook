use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use relaybook_core::{Ledger, Run};

use crate::agents::{self, AgentResult, Role};
use crate::paths;

pub(crate) struct Launch {
    pub prd: Option<PathBuf>,
    pub resume: bool,
    pub approve: bool,
    pub stop: bool,
    pub complete: bool,
    pub add: Vec<String>,
    pub new_only: bool,
    pub quiet: bool,
    pub watch: bool,
    pub max_cycles: Option<i64>,
    pub max_agent_calls: Option<i64>,
    pub max_reopens: Option<i64>,
    pub cursor_timeout_min: Option<i64>,
    pub est_cost_cap_usd: Option<String>,
    pub gates: Option<String>,
}

pub(crate) struct RunOutput {
    pub text: String,
    pub code: u8,
}

pub(crate) fn execute(cwd: &Path, launch: Launch) -> RunOutput {
    let Some(db) = paths::ledger_path(cwd) else {
        return fail("no ledger found (run `relaybook init`)");
    };
    if launch.complete {
        return complete_run(&db);
    }
    if launch.stop {
        return stop_requested(&db);
    }
    if launch.watch {
        return watch_run(cwd);
    }
    if !launch.add.is_empty() {
        return add_to_run(&db, &launch.add);
    }
    // Stop, complete, watch, and add returned above. They do not push.
    if let Err(err) = apply_overrides(&db, &launch) {
        return fail(&err);
    }
    let run_id = if launch.approve || launch.resume {
        match reopen_run(&db, cwd, launch.approve) {
            Ok(id) => id,
            Err(err) => return fail(&err),
        }
    } else {
        match begin_run(&db, cwd, launch.prd.as_deref(), launch.new_only) {
            Ok(id) => id,
            Err(err) => return fail(&err),
        }
    };
    drive(&db, cwd, launch.quiet, run_id)
}

fn complete_run(db: &Path) -> RunOutput {
    match Ledger::open(db).and_then(|mut ledger| ledger.request_run_complete()) {
        Ok(()) => ok("run complete requested"),
        Err(err) => fail(&err.to_string()),
    }
}

fn stop_requested(db: &Path) -> RunOutput {
    let Ok(mut ledger) = Ledger::open(db) else {
        return fail("no ledger found (run `relaybook init`)");
    };
    let found = match ledger.active_run() {
        Ok(Some(run)) => Some(run),
        Ok(None) => match ledger.resumable_run() {
            Ok(run) => run,
            Err(err) => return fail(&err.to_string()),
        },
        Err(err) => return fail(&err.to_string()),
    };
    let Some(mut run) = found else {
        return fail("no active run");
    };
    if run.stage == "publishing" {
        return fail("a publish claim is in progress; stop does not release it");
    }
    if run.status == "active"
        || run.status == "waiting"
        || run.status == "paused"
        || run.status == "stopped"
    {
        run.status = "stopped".to_string();
        run.stop_reason = Some("stopped by request".to_string());
        if let Err(err) = ledger.save_run(&mut run) {
            return fail(&err.to_string());
        }
        return fail("stopped: stopped by request; relaybook run --resume");
    }
    fail("no active run")
}

fn apply_overrides(db: &Path, launch: &Launch) -> Result<(), String> {
    let mut ledger = Ledger::open(db).map_err(|err| err.to_string())?;
    let pairs = [
        launch
            .max_cycles
            .map(|value| ("max_cycles", value.to_string())),
        launch
            .max_agent_calls
            .map(|value| ("max_agent_calls", value.to_string())),
        launch
            .max_reopens
            .map(|value| ("max_reopens_per_task", value.to_string())),
        launch
            .cursor_timeout_min
            .map(|value| ("cursor_timeout_min", value.to_string())),
        launch
            .est_cost_cap_usd
            .as_ref()
            .map(|value| ("est_cost_cap_usd", value.clone())),
        launch.gates.as_ref().map(|value| ("gates", value.clone())),
    ];
    for pair in pairs.into_iter().flatten() {
        ledger
            .set_setting(pair.0, &pair.1)
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn begin_run(db: &Path, cwd: &Path, prd: Option<&Path>, new_only: bool) -> Result<i64, String> {
    let accounts = agents::preflight()?;
    let mut ledger = Ledger::open(db).map_err(|err| err.to_string())?;
    let settings = ledger.settings().map_err(|err| err.to_string())?;
    if settings.mode != "auto" {
        return Err("mode is manual; run relaybook mode auto".to_string());
    }
    if ledger
        .active_run()
        .map_err(|err| err.to_string())?
        .is_some()
    {
        return Err("a run is already active".to_string());
    }
    if settings.workflow == "gated" {
        agents::preflight_gated()?;
    }
    if dirty(cwd)? {
        let status = git(cwd, &["status", "--porcelain"]).unwrap_or_default();
        return Err(format!("working tree is not clean: {status}"));
    }
    let Some(prd) = prd else {
        return Err("pass --prd or --resume".to_string());
    };
    let prd_path = prd.to_string_lossy().to_string();
    if !cwd.join(prd).is_file() && !prd.is_file() {
        return Err(format!("no such prd: {prd_path}"));
    }
    let full = if prd.is_absolute() {
        prd.to_path_buf()
    } else {
        cwd.join(prd)
    };
    let digest = file_sha256(&full)?;
    let branch = format!("relaybook/{}", stem(prd));
    if settings.workflow == "gated" {
        refuse_existing_run_branch(&mut ledger, cwd, &branch, &settings.remote_check)?;
    }
    sync_main(cwd)?;
    git(cwd, &["checkout", "-B", &branch, "main"])?;
    let run = ledger
        .start_run(
            &prd_path,
            &digest,
            &branch,
            Some(settings.workflow.as_str()),
        )
        .map_err(|err| err.to_string())?;
    let run_id = run.id;
    if !new_only {
        let adopted = match adopt_existing(&mut ledger, run.id) {
            Ok(adopted) => adopted,
            Err(err) => {
                if let Ok(Some(mut run)) = ledger.active_run() {
                    run.status = "stopped".to_string();
                    run.stop_reason = Some(err.clone());
                    let _ = ledger.save_run(&mut run);
                }
                return Err(err);
            }
        };
        if !adopted.is_empty() {
            println!(
                "adopted {} existing tasks: {}",
                adopted.len(),
                adopted.join(", ")
            );
        }
    }
    println!("{}", agents::account_line(&accounts));
    Ok(run_id)
}

fn adopt_existing(ledger: &mut Ledger, run_id: i64) -> Result<Vec<String>, String> {
    let mut adopted = ledger
        .adopt_backlog(run_id)
        .map_err(|err| err.to_string())?;
    match ledger
        .absorb_dependencies(run_id)
        .map_err(|err| err.to_string())?
    {
        relaybook_core::Absorb::Linked(extra) => {
            for slug in extra {
                if !adopted.contains(&slug) {
                    adopted.push(slug);
                }
            }
        }
        relaybook_core::Absorb::Waiting { task, dependency } => {
            return Err(format!(
                "{task} waits on {dependency}, which is not in this run; link it with relaybook run add {dependency}"
            ));
        }
    }
    Ok(adopted)
}

fn absorb_after_plan(cwd: &Path, ledger: &mut Ledger, run: &mut Run) -> Option<RunOutput> {
    match ledger.absorb_dependencies(run.id) {
        Ok(relaybook_core::Absorb::Linked(slugs)) if !slugs.is_empty() => {
            println!(
                "adopted {} existing tasks: {}",
                slugs.len(),
                slugs.join(", ")
            );
            None
        }
        Ok(relaybook_core::Absorb::Waiting { task, dependency }) => Some(halt(
            cwd,
            ledger,
            run,
            &format!(
                "{task} waits on {dependency}, which is not in this run; link it with relaybook run add {dependency}"
            ),
        )),
        Ok(_) => None,
        Err(err) => Some(fail(&err.to_string())),
    }
}

fn watch_run(cwd: &Path) -> RunOutput {
    let Some(db) = paths::ledger_path(cwd) else {
        return fail("no ledger found (run `relaybook init`)");
    };
    let run_id = {
        let Ok(ledger) = Ledger::open(&db) else {
            return fail("no ledger found (run `relaybook init`)");
        };
        match ledger.latest_run() {
            Ok(Some(run)) => run.id,
            Ok(None) => return fail("no run to watch"),
            Err(err) => return fail(&err.to_string()),
        }
    };
    let path = crate::live::log_path(cwd, run_id);
    match crate::live::follow(&path, || watch_message(&db, run_id)) {
        Ok(()) => ok(""),
        Err(err) => fail(&err.to_string()),
    }
}

fn watch_message(db: &Path, run_id: i64) -> Option<String> {
    let Ok(ledger) = Ledger::open(db) else {
        return Some("halted: no ledger".to_string());
    };
    let Ok(Some(run)) = ledger.latest_run() else {
        return Some("finished".to_string());
    };
    if run.id != run_id {
        return Some("finished".to_string());
    }
    match run.status.as_str() {
        "active" => None,
        "waiting" => Some("checkpoint: waiting for approval".to_string()),
        "paused" => Some(format!(
            "paused: {}",
            run.stop_reason.unwrap_or_else(|| "usage limit".to_string())
        )),
        "stopped" => Some(format!(
            "halted: {}",
            run.stop_reason.unwrap_or_else(|| "stopped".to_string())
        )),
        "complete" => Some("finished".to_string()),
        other => Some(format!("finished: {other}")),
    }
}

fn add_to_run(db: &Path, slugs: &[String]) -> RunOutput {
    let mut ledger = match Ledger::open(db) {
        Ok(ledger) => ledger,
        Err(err) => return fail(&err.to_string()),
    };
    let Some(run) = (match ledger.active_run() {
        Ok(run) => run,
        Err(err) => return fail(&err.to_string()),
    }) else {
        return fail("no active run");
    };
    let mut lines = Vec::new();
    for slug in slugs {
        match ledger.assign_run(slug, run.id) {
            Ok(true) => lines.push(format!("linked {slug}")),
            Ok(false) => lines.push(format!("{slug} already in this run")),
            Err(err) => return fail(&err.to_string()),
        }
    }
    ok(&lines.join("\n"))
}

fn reopen_run(db: &Path, cwd: &Path, approve: bool) -> Result<i64, String> {
    let accounts = agents::preflight()?;
    let mut ledger = Ledger::open(db).map_err(|err| err.to_string())?;
    let Some(mut run) = ledger.resumable_run().map_err(|err| err.to_string())? else {
        return Err("no run to resume".to_string());
    };
    if run.stage == "publishing"
        || ledger
            .publishing_run()
            .map_err(|err| err.to_string())?
            .is_some_and(|owner| owner.id != run.id)
    {
        return Err("a run is already publishing".to_string());
    }
    if approve
        && crate::gate::rereview_blocks_approve(&ledger, &run).map_err(|err| err.to_string())?
    {
        return Err("codex re-review pending".to_string());
    }
    if approve && run.status != "waiting" {
        return Err("run is not waiting".to_string());
    }
    let settings = ledger.settings().map_err(|err| err.to_string())?;
    if workflow_changed(&run, &settings) {
        let output = stop_workflow_mismatch(&mut ledger, &mut run);
        return Err(output.text.trim_end().to_string());
    }
    if is_gated(&run) {
        if let Some(message) = codex_resume_refusal(&ledger, &run).map_err(|err| err.to_string())? {
            return Err(message);
        }
        agents::require_codex()?;
    }
    if run.stage == "gate_release" {
        let tip = git(
            cwd,
            &["rev-parse", "--verify", "--end-of-options", &run.branch],
        )?;
        if !authorized_at(&ledger, &run, &tip)? {
            git(cwd, &["checkout", &run.branch])?;
            return Err(signoff_wait(cwd));
        }
        if crate::gate::rereview_blocks_publication(&ledger, run.id)
            .map_err(|err| err.to_string())?
        {
            return Err("codex re-review pending".to_string());
        }
        // One approve owns publication. The loser keeps the waiting row and
        // does not check out, screen, or push.
        run.stage = "publish".to_string();
        run.status = "active".to_string();
        if !approve {
            run.stop_reason = None;
        }
        ledger.claim_run(&mut run).map_err(|err| err.to_string())?;
        if let Err(err) = git(cwd, &["checkout", &run.branch]) {
            let output = halt(cwd, &mut ledger, &mut run, &err);
            return Err(output.text.trim_end().to_string());
        }
        let root = crate::gitinfo::toplevel(cwd).unwrap_or_else(|| cwd.to_path_buf());
        if let Err(err) = crate::gate::require_unpublished(&mut ledger, &root, "gate_release") {
            let output = halt(cwd, &mut ledger, &mut run, &err.to_string());
            return Err(output.text.trim_end().to_string());
        }
    } else {
        if approve && run.stage.starts_with("gate_review") {
            run.stage = "after_review".to_string();
        } else if approve && run.stage == "gate_plan" {
            run.stage = "execute".to_string();
        }
        run.status = "active".to_string();
        if !approve {
            run.stop_reason = None;
        }
        ledger.claim_run(&mut run).map_err(|err| err.to_string())?;
        git(cwd, &["checkout", &run.branch])?;
    }
    println!("{}", agents::account_line(&accounts));
    Ok(run.id)
}

fn drive(db: &Path, cwd: &Path, quiet: bool, run_id: i64) -> RunOutput {
    let mut transcript = 1_u32;
    let mut session = None;
    loop {
        let mut ledger = match Ledger::open(db) {
            Ok(ledger) => ledger,
            Err(err) => return fail(&err.to_string()),
        };
        let settings = match ledger.settings() {
            Ok(settings) => settings,
            Err(err) => return fail(&err.to_string()),
        };
        let mut run = match load_live(&ledger, run_id) {
            Ok(run) => run,
            Err(err) => return fail(&err),
        };
        if workflow_changed(&run, &settings) {
            return stop_workflow_mismatch(&mut ledger, &mut run);
        }
        if settings.mode != "auto" {
            return halt(cwd, &mut ledger, &mut run, "mode is manual");
        }
        if run.agent_calls >= settings.max_agent_calls {
            return halt(cwd, &mut ledger, &mut run, "max_agent_calls");
        }
        if let Some(cap) = settings.est_cost_cap_usd.as_deref()
            && let Ok(cap) = cap.parse::<f64>()
            && run.est_cost_usd >= cap
        {
            return halt(cwd, &mut ledger, &mut run, "est_cost_cap_usd");
        }
        if waiting_checkpoint(&run) {
            return ok(&waiting_checkpoint_message(cwd, &run));
        }
        if run.stage == "after_review" {
            match after_review(cwd, &mut ledger, &mut run, &settings) {
                Control::Stay => {}
                Control::Return(output) => return output,
            }
            continue;
        }
        if run.stage == "plan" {
            let prd_file = cwd.join(&run.prd_path);
            let prd = std::fs::read_to_string(&prd_file).unwrap_or_default();
            let adopted = ledger
                .run_tasks(run.id)
                .map(|tasks| {
                    tasks
                        .into_iter()
                        .map(|task| task.slug)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let mut prompt = if adopted.is_empty() {
                format!("PRD:\n{prd}")
            } else {
                format!(
                    "PRD:\n{prd}\nAdopted tasks: {adopted}\nBuild on these tasks. Do not create duplicates.\n"
                )
            };
            let budget = settings.est_cost_cap_usd.clone();
            let run_id = run.id;
            let gated = is_gated(&run);
            // Before the plan snapshot. A skipped or failed refresh stays on the prompt
            // as "no graph" and does not stop the run. Classic prompts are unchanged.
            if gated {
                match crate::gate::record_graph_refresh_for(
                    &mut ledger,
                    cwd,
                    "plan",
                    None,
                    Some(run_id),
                ) {
                    Ok(refresh) => append_graph_prompt(&mut prompt, &refresh),
                    Err(err) => return halt(cwd, &mut ledger, &mut run, &err.to_string()),
                }
            }
            run.agent_calls += 1;
            if let Err(err) = ledger.save_run(&mut run) {
                return fail(&err.to_string());
            }
            drop(ledger);
            let before = match snap(cwd, &run.branch, gated) {
                Ok(before) => before,
                Err(err) => return fail(&err),
            };
            let live = live_session(&mut session, cwd, run_id, quiet);
            live.begin("● Planning (claude)");
            let started = Instant::now();
            let result =
                agents::call_claude(cwd, Role::Planner, &prompt, budget.as_deref(), run_id, live);
            let elapsed = started.elapsed();
            let _ = agents::write_transcript(
                cwd,
                run_id,
                transcript,
                Role::Planner,
                &result_text(&result),
            );
            transcript += 1;
            if let Err(err) = guard_claude(cwd, &before, gated) {
                close_live(&mut session, cwd, run_id, quiet, elapsed, &result, "halted");
                return halt_db(db, cwd, run_id, &err);
            }
            if let Some(output) = take_agent(db, cwd, run_id, &result, "claude") {
                close_live(
                    &mut session,
                    cwd,
                    run_id,
                    quiet,
                    elapsed,
                    &result,
                    agent_outcome(&result),
                );
                return output;
            }
            if matches!(result, AgentResult::Failed { .. }) {
                close_live(&mut session, cwd, run_id, quiet, elapsed, &result, "failed");
                return bump_failure(db, cwd, quiet, run_id);
            }
            let mut ledger = match Ledger::open(db) {
                Ok(ledger) => ledger,
                Err(err) => return fail(&err.to_string()),
            };
            let mut run = match load_live(&ledger, run_id) {
                Ok(run) => run,
                Err(err) => return fail(&err),
            };
            note_cost(&mut run, &result);
            if let Some(output) = absorb_after_plan(cwd, &mut ledger, &mut run) {
                close_live(&mut session, cwd, run_id, quiet, elapsed, &result, "halted");
                return output;
            }
            let gates = ledger.settings().map(|item| item.gates).unwrap_or_default();
            run.stage = if gates == "plan" || gates == "plan,review" {
                "gate_plan".to_string()
            } else {
                "execute".to_string()
            };
            let outcome = if run.stage == "gate_plan" {
                "paused at checkpoint"
            } else {
                "planned"
            };
            close_live(&mut session, cwd, run_id, quiet, elapsed, &result, outcome);
            println!("{}", plan_line(&run));
            if run.stage == "gate_plan" {
                run.status = "waiting".to_string();
                if let Err(err) = ledger.save_run(&mut run) {
                    return fail(&err.to_string());
                }
                return ok("waiting for approval: relaybook approve");
            }
            if let Err(err) = ledger.save_run(&mut run) {
                return fail(&err.to_string());
            }
            continue;
        }
        if run.stage == "review" {
            if is_gated(&run) {
                let Some(slug) = run.current_slug.clone() else {
                    return halt(cwd, &mut ledger, &mut run, "no task to review");
                };
                drop(ledger);
                match review_gated(
                    db,
                    cwd,
                    quiet,
                    run_id,
                    &mut session,
                    &mut transcript,
                    CodexScope::Task(slug),
                ) {
                    Control::Stay => continue,
                    Control::Return(output) => return output,
                }
            }
            let Some(slug) = run.current_slug.clone() else {
                run.stage = "execute".to_string();
                if let Err(err) = ledger.save_run(&mut run) {
                    return fail(&err.to_string());
                }
                continue;
            };
            let prompt = format!("Review {slug}. Use relaybook inbox.");
            let budget = settings.est_cost_cap_usd.clone();
            let run_id = run.id;
            let branch = run.branch.clone();
            run.agent_calls += 1;
            if let Err(err) = ledger.save_run(&mut run) {
                return fail(&err.to_string());
            }
            drop(ledger);
            let before = match snap(cwd, &branch, false) {
                Ok(before) => before,
                Err(err) => return fail(&err),
            };
            let live = live_session(&mut session, cwd, run_id, quiet);
            live.begin(&format!("● Reviewing {slug} (claude)"));
            let started = Instant::now();
            let result = agents::call_claude(
                cwd,
                Role::Reviewer,
                &prompt,
                budget.as_deref(),
                run_id,
                live,
            );
            let elapsed = started.elapsed();
            let _ = agents::write_transcript(
                cwd,
                run_id,
                transcript,
                Role::Reviewer,
                &result_text(&result),
            );
            transcript += 1;
            if let Err(err) = guard_claude(cwd, &before, false) {
                close_live(&mut session, cwd, run_id, quiet, elapsed, &result, "halted");
                return halt_db(db, cwd, run_id, &err);
            }
            if let Some(output) = take_agent(db, cwd, run_id, &result, "claude") {
                close_live(
                    &mut session,
                    cwd,
                    run_id,
                    quiet,
                    elapsed,
                    &result,
                    agent_outcome(&result),
                );
                return output;
            }
            if matches!(result, AgentResult::Failed { .. }) {
                close_live(&mut session, cwd, run_id, quiet, elapsed, &result, "failed");
                return bump_failure(db, cwd, quiet, run_id);
            }
            let mut ledger = match Ledger::open(db) {
                Ok(ledger) => ledger,
                Err(err) => return fail(&err.to_string()),
            };
            let mut run = match load_live(&ledger, run_id) {
                Ok(run) => run,
                Err(err) => return fail(&err),
            };
            note_cost(&mut run, &result);
            let gates = ledger.settings().map(|item| item.gates).unwrap_or_default();
            let verdict = match ledger.latest_report_meta(&slug) {
                Ok(Some((_, Some(verdict)))) => verdict,
                _ => {
                    if let Err(err) = ledger.save_run(&mut run) {
                        return fail(&err.to_string());
                    }
                    close_live(&mut session, cwd, run_id, quiet, elapsed, &result, "failed");
                    return bump_failure(db, cwd, quiet, run_id);
                }
            };
            run.failure_streak = 0;
            let outcome = if verdict == "reopened" {
                "reopened"
            } else if gates == "plan,review" {
                "paused at checkpoint"
            } else {
                "accepted"
            };
            close_live(&mut session, cwd, run_id, quiet, elapsed, &result, outcome);
            println!("{}", review_line(&run, &slug, &verdict, &gates));
            run.stage = format!("gate_review:{verdict}");
            if gates == "plan,review" {
                run.status = "waiting".to_string();
                if let Err(err) = ledger.save_run(&mut run) {
                    return fail(&err.to_string());
                }
                return ok("waiting for approval: relaybook approve");
            }
            run.stage = "after_review".to_string();
            if let Err(err) = ledger.save_run(&mut run) {
                return fail(&err.to_string());
            }
            continue;
        }
        if run.stage == "release_review" {
            if !is_gated(&run) {
                return halt(cwd, &mut ledger, &mut run, "release review is unavailable");
            }
            drop(ledger);
            match review_gated(
                db,
                cwd,
                quiet,
                run_id,
                &mut session,
                &mut transcript,
                CodexScope::Release,
            ) {
                Control::Stay => continue,
                Control::Return(output) => return output,
            }
        }
        if run.stage == "publish" && is_gated(&run) {
            return publish(cwd, &mut ledger, &mut run);
        }
        if run.stage == "gate_release" && is_gated(&run) {
            run.status = "waiting".to_string();
            if let Err(err) = ledger.save_run(&mut run) {
                return fail(&err.to_string());
            }
            return ok(&signoff_wait(cwd));
        }
        // execute, or a gate that was already approved into execute
        let tasks = match ledger.run_tasks(run.id) {
            Ok(tasks) => tasks,
            Err(err) => return fail(&err.to_string()),
        };
        if run.complete_requested && !relaybook_core::unfinished_run(&tasks) {
            if is_gated(&run) {
                if let Err(err) = queue_release_review(&mut ledger, &mut run) {
                    return fail(&err);
                }
                continue;
            }
            return finish(cwd, &mut ledger, &mut run);
        }
        let slug = if let Some(slug) = run.current_slug.clone() {
            let still_open = tasks.iter().any(|task| {
                task.slug == slug
                    && matches!(
                        task.status,
                        relaybook_core::Status::Open | relaybook_core::Status::Claimed
                    )
            });
            if still_open {
                Some(slug)
            } else {
                relaybook_core::ready_run_slug(&tasks)
            }
        } else {
            relaybook_core::ready_run_slug(&tasks)
        };
        let Some(slug) = slug else {
            if let Some(blocked) = relaybook_core::blocked_run_slug(&tasks) {
                return halt(cwd, &mut ledger, &mut run, &format!("blocked: {blocked}"));
            }
            if tasks.is_empty() {
                return halt(cwd, &mut ledger, &mut run, "planner created no tasks");
            }
            if !relaybook_core::unfinished_run(&tasks) {
                if is_gated(&run) {
                    if let Err(err) = queue_release_review(&mut ledger, &mut run) {
                        return fail(&err);
                    }
                    continue;
                }
                return finish(cwd, &mut ledger, &mut run);
            }
            return halt(cwd, &mut ledger, &mut run, "no ready task");
        };
        let reopens = ledger.reopen_count(&slug).unwrap_or(0);
        if reopens > settings.max_reopens_per_task {
            return halt(
                cwd,
                &mut ledger,
                &mut run,
                &format!(
                    "reopened more than {} times: {slug}",
                    settings.max_reopens_per_task
                ),
            );
        }
        let new_task = run.current_slug.as_deref() != Some(slug.as_str());
        if new_task && run.cycle >= settings.max_cycles {
            return halt(cwd, &mut ledger, &mut run, "max_cycles");
        }
        let timeout = agents::cursor_timeout(settings.cursor_timeout_min);
        let mut prompt = format!("{}\nTask slug: {slug}\n", agents::executor_template());
        let run_id = run.id;
        let branch = run.branch.clone();
        let gated = is_gated(&run);
        // Before the executor snapshot. Claim and done still refresh on their own.
        if gated {
            match crate::gate::record_graph_refresh_for(
                &mut ledger,
                cwd,
                "implement",
                Some(&slug),
                Some(run_id),
            ) {
                Ok(refresh) => {
                    prompt.push_str(&format!("Base: {}\n", refresh.commit_sha));
                    append_graph_prompt(&mut prompt, &refresh);
                }
                Err(err) => return halt(cwd, &mut ledger, &mut run, &err.to_string()),
            }
        }
        if new_task {
            run.cycle += 1;
            run.current_slug = Some(slug.clone());
        }
        run.stage = "execute".to_string();
        run.agent_calls += 1;
        if let Err(err) = ledger.save_run(&mut run) {
            return fail(&err.to_string());
        }
        drop(ledger);
        let before = match snap(cwd, &branch, gated) {
            Ok(before) => before,
            Err(err) => return fail(&err),
        };
        let cycle = run.cycle;
        let live = live_session(&mut session, cwd, run_id, quiet);
        live.begin(&format!("● Task {slug} (cursor), cycle {cycle}"));
        let started = Instant::now();
        let result = agents::call_cursor(cwd, &prompt, timeout, run_id, live);
        let elapsed = started.elapsed();
        let _ = agents::write_transcript(
            cwd,
            run_id,
            transcript,
            Role::Executor,
            &result_text(&result),
        );
        transcript += 1;
        if let Err(err) = guard_cursor(cwd, &before, &branch, gated) {
            close_live(&mut session, cwd, run_id, quiet, elapsed, &result, "halted");
            return halt_db(db, cwd, run_id, &err);
        }
        if let Some(output) = take_agent(db, cwd, run_id, &result, "cursor") {
            close_live(
                &mut session,
                cwd,
                run_id,
                quiet,
                elapsed,
                &result,
                agent_outcome(&result),
            );
            return output;
        }
        let mut ledger = match Ledger::open(db) {
            Ok(ledger) => ledger,
            Err(err) => return fail(&err.to_string()),
        };
        let mut run = match load_live(&ledger, run_id) {
            Ok(run) => run,
            Err(err) => return fail(&err),
        };
        let tasks = ledger.run_tasks(run.id).unwrap_or_default();
        let task = tasks.iter().find(|task| task.slug == slug);
        let blocked = task.is_some_and(|task| task.status == relaybook_core::Status::Blocked);
        let done = task.is_some_and(|task| task.status == relaybook_core::Status::Done);
        let report_bytes = ledger
            .latest_report_meta(&slug)
            .ok()
            .flatten()
            .map(|(bytes, _)| bytes);
        if blocked {
            close_live(
                &mut session,
                cwd,
                run_id,
                quiet,
                elapsed,
                &result,
                "blocked",
            );
            println!(
                "{}",
                cursor_line(&run, &slug, "blocked", 0, &settings.est_cost_cap_usd)
            );
            return halt(cwd, &mut ledger, &mut run, &format!("blocked: {slug}"));
        }
        if !done || report_bytes.is_none() {
            close_live(&mut session, cwd, run_id, quiet, elapsed, &result, "failed");
            run.failure_streak += 1;
            if let Err(err) = ledger.save_run(&mut run) {
                return fail(&err.to_string());
            }
            if run.failure_streak >= 2 {
                return halt(cwd, &mut ledger, &mut run, "two consecutive agent failures");
            }
            continue;
        }
        run.failure_streak = 0;
        run.stage = "review".to_string();
        close_live(&mut session, cwd, run_id, quiet, elapsed, &result, "done");
        println!(
            "{}",
            cursor_line(
                &run,
                &slug,
                "done",
                report_bytes.unwrap_or(0),
                &settings.est_cost_cap_usd
            )
        );
        if let Err(err) = ledger.save_run(&mut run) {
            return fail(&err.to_string());
        }
    }
}

enum Control {
    Stay,
    Return(RunOutput),
}

fn after_review(
    cwd: &Path,
    ledger: &mut Ledger,
    run: &mut Run,
    settings: &relaybook_core::Settings,
) -> Control {
    let slug = run.current_slug.clone().unwrap_or_default();
    let verdict = run.stage.clone();
    let verdict = ledger
        .latest_report_meta(&slug)
        .ok()
        .flatten()
        .and_then(|(_, verdict)| verdict)
        .unwrap_or_else(|| {
            if verdict.contains("reopened") {
                "reopened".to_string()
            } else {
                "accepted".to_string()
            }
        });
    if verdict == "reopened" {
        let count = ledger.reopen_count(&slug).unwrap_or(0);
        if count > settings.max_reopens_per_task {
            return Control::Return(halt(
                cwd,
                ledger,
                run,
                &format!(
                    "reopened more than {} times: {slug}",
                    settings.max_reopens_per_task
                ),
            ));
        }
        run.current_slug = None;
        run.stage = "execute".to_string();
        if let Err(err) = ledger.save_run(run) {
            return Control::Return(fail(&err.to_string()));
        }
        return Control::Stay;
    }
    if !is_gated(run) {
        if let Some(output) = refuse_workflow_change(ledger, run) {
            return Control::Return(output);
        }
        if let Err(err) = push_branch(cwd, &run.branch) {
            return Control::Return(halt(cwd, ledger, run, &err));
        }
    }
    let tasks = ledger.run_tasks(run.id).unwrap_or_default();
    if run.complete_requested && !relaybook_core::unfinished_run(&tasks) {
        if is_gated(run) {
            if let Err(err) = queue_release_review(ledger, run) {
                return Control::Return(fail(&err));
            }
            return Control::Stay;
        }
        return Control::Return(finish(cwd, ledger, run));
    }
    run.current_slug = None;
    run.stage = "execute".to_string();
    if let Err(err) = ledger.save_run(run) {
        return Control::Return(fail(&err.to_string()));
    }
    Control::Stay
}

fn finish(cwd: &Path, ledger: &mut Ledger, run: &mut Run) -> RunOutput {
    if let Some(output) = refuse_workflow_change(ledger, run) {
        return output;
    }
    if let Err(err) = push_branch(cwd, &run.branch) {
        return halt(cwd, ledger, run, &err);
    }
    let body = report_body(ledger, run.id);
    let title = format!("Relaybook run {}", run.branch);
    match gh_pr(cwd, &run.branch, &title, &body) {
        Ok(url) => {
            run.status = "complete".to_string();
            run.stage = "done".to_string();
            run.stop_reason = Some(url.clone());
            if let Err(err) = ledger.save_run(run) {
                return fail(&err.to_string());
            }
            ok(&format!("opened {url}"))
        }
        Err(err) => halt(cwd, ledger, run, &err),
    }
}

fn halt(cwd: &Path, ledger: &mut Ledger, run: &mut Run, reason: &str) -> RunOutput {
    if let Some(output) = refuse_workflow_change(ledger, run) {
        return output;
    }
    if !is_gated(run) {
        let _ = push_branch(cwd, &run.branch);
    }
    stop_run(ledger, run, reason)
}

/// A workflow mismatch stops the run and does not push. A stored classic or
/// unset workflow would otherwise publish after the setting switched to gated.
fn stop_workflow_mismatch(ledger: &mut Ledger, run: &mut Run) -> RunOutput {
    stop_run(ledger, run, "workflow changed during run")
}

fn stop_run(ledger: &mut Ledger, run: &mut Run, reason: &str) -> RunOutput {
    if run.stage == "publishing" {
        run.stage = "publish".to_string();
    }
    run.status = "stopped".to_string();
    run.stop_reason = Some(reason.to_string());
    if let Err(err) = ledger.save_run(run) {
        return fail(&err.to_string());
    }
    fail(&format!("stopped: {reason}; relaybook run --resume"))
}

fn halt_db(db: &Path, cwd: &Path, run_id: i64, reason: &str) -> RunOutput {
    let Ok(mut ledger) = Ledger::open(db) else {
        return fail(reason);
    };
    let Ok(mut run) = ledger.run_by_id(run_id) else {
        return fail(reason);
    };
    if !run_is_live(&run) {
        return fail(reason);
    }
    halt(cwd, &mut ledger, &mut run, reason)
}

fn bump_failure(db: &Path, cwd: &Path, quiet: bool, run_id: i64) -> RunOutput {
    let Ok(mut ledger) = Ledger::open(db) else {
        return fail("no ledger");
    };
    let Ok(mut run) = ledger.run_by_id(run_id) else {
        return fail("no active run");
    };
    if !run_is_live(&run) {
        return fail("no active run");
    }
    if let Some(output) = refuse_workflow_change(&mut ledger, &mut run) {
        return output;
    }
    if waiting_checkpoint(&run) {
        return ok(&waiting_checkpoint_message(cwd, &run));
    }
    run.failure_streak += 1;
    if run.failure_streak >= 2 {
        run.status = "stopped".to_string();
        run.stop_reason = Some("two consecutive agent failures".to_string());
        if let Err(err) = ledger.save_run(&mut run) {
            return fail(&err.to_string());
        }
        return fail("stopped: two consecutive agent failures; relaybook run --resume");
    }
    if let Err(err) = ledger.save_run(&mut run) {
        return fail(&err.to_string());
    }
    drive(db, cwd, quiet, run_id)
}

fn take_agent(
    db: &Path,
    cwd: &Path,
    run_id: i64,
    result: &AgentResult,
    cli: &str,
) -> Option<RunOutput> {
    match result {
        AgentResult::UsageLimit { reset_at, .. } => {
            let Ok(mut ledger) = Ledger::open(db) else {
                return Some(fail("no ledger"));
            };
            let Ok(mut run) = ledger.run_by_id(run_id) else {
                return Some(fail("no active run"));
            };
            if !run_is_live(&run) {
                return Some(fail("no active run"));
            }
            if let Some(output) = refuse_workflow_change(&mut ledger, &mut run) {
                return Some(output);
            }
            run.status = "paused".to_string();
            run.failure_streak = 0;
            run.stop_reason = Some(match reset_at {
                Some(time) => format!("{cli}|{time}"),
                None => cli.to_string(),
            });
            if let Err(err) = ledger.save_run(&mut run) {
                return Some(fail(&err.to_string()));
            }
            let line = match reset_at {
                Some(time) => {
                    format!("usage limit reached on {cli}; relaybook run --resume after {time}")
                }
                None => format!("usage limit reached on {cli}; relaybook run --resume"),
            };
            Some(ok(&line))
        }
        AgentResult::TimedOut => Some(halt_db(db, cwd, run_id, "timeout")),
        AgentResult::Unavailable { cli } => {
            Some(halt_db(db, cwd, run_id, &format!("{cli} unavailable")))
        }
        AgentResult::Failed { .. } => None,
        AgentResult::Finished { .. } => None,
    }
}

fn note_cost(run: &mut Run, result: &AgentResult) {
    if let AgentResult::Finished { est_cost_usd, .. } = result {
        run.est_cost_usd += est_cost_usd;
    }
}

fn result_text(result: &AgentResult) -> String {
    match result {
        AgentResult::Finished { output, .. }
        | AgentResult::UsageLimit { output, .. }
        | AgentResult::Failed { output } => output.clone(),
        AgentResult::TimedOut => "timeout".to_string(),
        AgentResult::Unavailable { cli } => format!("{cli} unavailable"),
    }
}

fn live_session<'a>(
    slot: &'a mut Option<crate::live::Session>,
    cwd: &Path,
    run_id: i64,
    quiet: bool,
) -> &'a mut crate::live::Session {
    if slot.is_none() {
        *slot = Some(crate::live::Session::open(cwd, run_id, quiet));
    }
    slot.as_mut().expect("live session")
}

fn close_live(
    slot: &mut Option<crate::live::Session>,
    cwd: &Path,
    run_id: i64,
    quiet: bool,
    elapsed: Duration,
    result: &AgentResult,
    outcome: &str,
) {
    live_session(slot, cwd, run_id, quiet).end(&close_line(elapsed, result, outcome));
}

fn agent_outcome(result: &AgentResult) -> &'static str {
    match result {
        AgentResult::UsageLimit { .. } => "paused",
        AgentResult::TimedOut => "timed out",
        AgentResult::Unavailable { .. } => "unavailable",
        AgentResult::Failed { .. } => "failed",
        AgentResult::Finished { .. } => "done",
    }
}

fn close_line(elapsed: Duration, result: &AgentResult, outcome: &str) -> String {
    let cost = match result {
        AgentResult::Finished { est_cost_usd, .. } => *est_cost_usd,
        _ => 0.0,
    };
    format!("  {outcome} in {}s, est ${cost:.2}", elapsed.as_secs())
}

/// `Graph: <graph_sha | no graph> at <commit>`. `graph_sha` is set only when
/// the refresh status is `built`. A skipped or failed refresh says `no graph`.
fn graph_prompt_line(refresh: &crate::gate::GraphRefresh) -> String {
    let shown = refresh
        .graph_sha
        .as_deref()
        .unwrap_or(crate::graphify::NO_GRAPH);
    format!("Graph: {shown} at {}", refresh.commit_sha)
}

fn append_graph_prompt(prompt: &mut String, refresh: &crate::gate::GraphRefresh) {
    if !prompt.ends_with('\n') {
        prompt.push('\n');
    }
    prompt.push_str(&graph_prompt_line(refresh));
    prompt.push('\n');
}

fn plan_line(run: &Run) -> String {
    format!(
        "cycle {}: claude plan; calls {}/?",
        run.cycle, run.agent_calls
    )
}

fn review_line(run: &Run, slug: &str, verdict: &str, gates: &str) -> String {
    let mut line = format!(
        "cycle {}: claude review -> {slug} {verdict}; calls {}",
        run.cycle, run.agent_calls
    );
    if gates != "none" {
        line.push_str(&format!(" gates {gates}"));
    }
    line
}

fn cursor_line(run: &Run, slug: &str, state: &str, bytes: i64, cap: &Option<String>) -> String {
    let mut line = format!(
        "cycle {}: cursor -> {slug} ({state}, report {bytes} B); calls {}",
        run.cycle, run.agent_calls
    );
    if cap.is_some() {
        line.push_str(&format!(" est {:.2} USD (estimate)", run.est_cost_usd));
    }
    line
}

fn report_body(ledger: &Ledger, run_id: i64) -> String {
    let tasks = ledger.run_tasks(run_id).unwrap_or_default();
    let mut body = String::from("Accepted reports:\n");
    for task in tasks {
        if let Ok(Some((bytes, verdict))) = ledger.latest_report_meta(&task.slug) {
            let verdict = verdict.unwrap_or_else(|| "open".to_string());
            body.push_str(&format!("- {} {verdict} ({bytes} B)\n", task.slug));
        }
    }
    body
}

fn reviews_section(ledger: &Ledger, run_id: i64) -> String {
    let Ok(lines) = ledger.run_review_lines(run_id) else {
        return String::new();
    };
    if lines.is_empty() {
        return String::new();
    }
    let pending = match ledger.pending_rereviews(run_id) {
        Ok(rows) => rows
            .into_iter()
            .map(|row| row.review_id)
            .collect::<Vec<_>>(),
        Err(_) => lines
            .iter()
            .filter(|line| claude_review_awaits_coverage(line, &lines))
            .map(|line| line.id)
            .collect(),
    };
    let mut body = String::from("\n## Reviews\n");
    for line in &lines {
        let slug = line.slug.clone().unwrap_or_else(|| {
            if line.scope == "release" {
                "release".to_string()
            } else {
                format!("review {}", line.id)
            }
        });
        let model = line.reviewer_model.as_deref().unwrap_or("unset");
        let fallback = line.fallback_reason.as_deref().unwrap_or("none");
        let verdict = line.verdict.as_deref().unwrap_or("pending");
        let state = review_coverage_state(line, &lines, &pending);
        body.push_str(&format!(
            "- {slug}: {reviewer} {model}; fallback: {fallback}; {verdict}; {state}\n",
            reviewer = line.reviewer
        ));
    }
    body
}

fn claude_review_covered(
    line: &relaybook_core::RunReviewLine,
    lines: &[relaybook_core::RunReviewLine],
) -> bool {
    lines.iter().any(|other| {
        other.rereview_of == Some(line.id)
            && other.reviewer == "codex"
            && other.verdict.as_deref() == Some("approved")
    })
}

fn claude_review_awaits_coverage(
    line: &relaybook_core::RunReviewLine,
    lines: &[relaybook_core::RunReviewLine],
) -> bool {
    line.reviewer == "claude"
        && line.verdict.as_deref() == Some("approved")
        && !claude_review_covered(line, lines)
}

fn review_coverage_state(
    line: &relaybook_core::RunReviewLine,
    lines: &[relaybook_core::RunReviewLine],
    pending: &[i64],
) -> String {
    if let Some(id) = line.rereview_of {
        return format!("re-review of {id}");
    }
    if line.reviewer == "claude" && pending.contains(&line.id) {
        return "awaiting Codex re-review".to_string();
    }
    if line.reviewer == "claude" && claude_review_covered(line, lines) {
        return "Codex re-reviewed: approved".to_string();
    }
    if line.reviewer == "claude" {
        return "claude".to_string();
    }
    "codex".to_string()
}

struct Snap {
    head: String,
    branch: String,
    main: String,
    origin_main: Option<String>,
    origin_branch: Option<String>,
    tags: Vec<String>,
    tree: String,
    remote_refs: Vec<String>,
}

fn snap(cwd: &Path, branch: &str, gated: bool) -> Result<Snap, String> {
    Ok(Snap {
        head: git(cwd, &["rev-parse", "HEAD"])?,
        branch: git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?,
        main: git(cwd, &["rev-parse", "main"])?,
        origin_main: git(cwd, &["rev-parse", "origin/main"]).ok(),
        origin_branch: git(cwd, &["rev-parse", &format!("origin/{branch}")]).ok(),
        tags: tag_list(cwd),
        tree: crate::git::worktree_fingerprint(cwd)?,
        remote_refs: if gated {
            crate::remote::tracking_ref_lines(cwd)?
        } else {
            Vec::new()
        },
    })
}

fn guard_claude(cwd: &Path, before: &Snap, gated: bool) -> Result<(), String> {
    let after = snap(cwd, &before.branch, gated)?;
    if gated {
        shared_guard_gated(before, &after)?;
    } else {
        shared_guard(before, &after)?;
    }
    if after.head != before.head || after.tree != before.tree {
        return Err(format!(
            "claude edited the worktree: {} -> {}",
            before.head, after.head
        ));
    }
    Ok(())
}

fn guard_cursor(cwd: &Path, before: &Snap, branch: &str, gated: bool) -> Result<(), String> {
    let after = snap(cwd, branch, gated)?;
    if gated {
        shared_guard_gated(before, &after)?;
    } else {
        shared_guard(before, &after)?;
    }
    if after.branch != branch {
        return Err("switched branch".to_string());
    }
    Ok(())
}

fn shared_guard(before: &Snap, after: &Snap) -> Result<(), String> {
    if after.main != before.main {
        return Err("main moved".to_string());
    }
    if after.origin_main != before.origin_main {
        return Err("pushed main".to_string());
    }
    if after.origin_branch != before.origin_branch {
        return Err("pushed outside relaybook".to_string());
    }
    if after.tags != before.tags {
        return Err("created a tag".to_string());
    }
    Ok(())
}

fn shared_guard_gated(before: &Snap, after: &Snap) -> Result<(), String> {
    shared_guard(before, after)?;
    if before.remote_refs != after.remote_refs {
        return Err("pushed outside relaybook".to_string());
    }
    Ok(())
}

/// Worktree, refs, git config, and hooks around one Codex review.
///
/// `run_branch` is the run's branch, used for `origin/<branch>` even if the
/// reviewer switches HEAD. `snap.branch` is the abbrev-ref at snapshot time.
struct GatedSnap {
    run_branch: String,
    snap: Snap,
    refs: Vec<String>,
    config: Option<Vec<u8>>,
    hooks: String,
}

fn gated_snap(cwd: &Path, run_branch: &str) -> Result<GatedSnap, String> {
    Ok(GatedSnap {
        run_branch: run_branch.to_string(),
        snap: snap(cwd, run_branch, true)?,
        refs: all_ref_lines(cwd)?,
        config: git_path_bytes(cwd, "config")?,
        hooks: hooks_fingerprint(cwd)?,
    })
}

/// Halt when the reviewer changed the repo during the Codex call.
///
/// HEAD is compared before other refs: a commit moves both, and that halt
/// reason is "reviewer moved HEAD". Tags and remote-tracking refs keep the
/// reasons `shared_guard_gated` already uses. Any other ref change names the
/// first ref in sorted order.
fn guard_reviewer(cwd: &Path, before: &GatedSnap) -> Result<(), String> {
    let after = match gated_snap(cwd, &before.run_branch) {
        Ok(after) => after,
        // The before snapshot succeeded. A hooks read that fails only after
        // Codex ran is a change we will not approve.
        Err(err) if err.starts_with("hooks:") => {
            return Err("reviewer changed hooks".to_string());
        }
        Err(err) => return Err(err),
    };
    if after.snap.head != before.snap.head || after.snap.branch != before.snap.branch {
        return Err("reviewer moved HEAD".to_string());
    }
    if after.snap.tree != before.snap.tree {
        return Err("reviewer edited the worktree".to_string());
    }
    shared_guard_gated(&before.snap, &after.snap)?;
    if let Some(name) = changed_ref(&before.refs, &after.refs) {
        return Err(format!("reviewer changed refs: {name}"));
    }
    if after.config != before.config {
        return Err("reviewer changed git config".to_string());
    }
    if after.hooks != before.hooks {
        return Err("reviewer changed hooks".to_string());
    }
    Ok(())
}

fn all_ref_lines(cwd: &Path) -> Result<Vec<String>, String> {
    let text = git(
        cwd,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "--end-of-options",
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

fn changed_ref(before: &[String], after: &[String]) -> Option<String> {
    let before = ref_map(before);
    let after = ref_map(after);
    let mut names: Vec<&String> = before.keys().chain(after.keys()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .find(|name| before.get(*name) != after.get(*name))
        .cloned()
}

fn ref_map(lines: &[String]) -> BTreeMap<String, String> {
    let mut refs = BTreeMap::new();
    for line in lines {
        let Some((sha, name)) = line.split_once(' ') else {
            continue;
        };
        refs.insert(name.to_string(), sha.to_string());
    }
    refs
}

fn git_file(cwd: &Path, name: &str) -> Result<PathBuf, String> {
    let raw = git(cwd, &["rev-parse", "--git-path", name])?;
    let path = PathBuf::from(&raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(cwd.join(path))
    }
}

fn git_path_bytes(cwd: &Path, name: &str) -> Result<Option<Vec<u8>>, String> {
    let path = git_file(cwd, name)?;
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("read {}: {err}", path.display())),
    }
}

const HOOK_SYMLINK_HOPS: u32 = 8;
const HOOK_FILE_BYTES: u64 = 1_048_576;

struct HookChain {
    paths: Vec<PathBuf>,
    #[cfg(unix)]
    ids: Vec<(u64, u64)>,
}

fn hook_read(path: &Path, err: &std::io::Error) -> String {
    format!("hooks: read {}: {err}", path.display())
}

fn lexical_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn file_id(meta: &fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// Record `path` on `chain`. A repeated inode or lexical path is a cycle.
///
/// On Unix every successful call pushes one inode. [`pop_hook`] pops it.
fn note_hook(chain: &mut HookChain, meta: &fs::Metadata, path: &Path) -> Result<(), String> {
    let id = file_id(meta);
    #[cfg(unix)]
    if let Some(id) = id
        && chain.ids.contains(&id)
    {
        return Err("hooks: symlink cycle".to_string());
    }
    let key = lexical_path(path);
    if chain.paths.iter().any(|seen| seen == &key) {
        return Err("hooks: symlink cycle".to_string());
    }
    #[cfg(unix)]
    if let Some(id) = id {
        chain.ids.push(id);
    }
    #[cfg(not(unix))]
    {
        let _ = id;
    }
    chain.paths.push(key);
    Ok(())
}

fn pop_hook(chain: &mut HookChain) {
    chain.paths.pop();
    #[cfg(unix)]
    {
        chain.ids.pop();
    }
}

fn resolve_hook_link(link: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        match link.parent() {
            Some(parent) => parent.join(target),
            None => target.to_path_buf(),
        }
    }
}

fn hooks_fingerprint(cwd: &Path) -> Result<String, String> {
    let path = git_file(cwd, "hooks")?;
    let mut body = Vec::new();
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            // The hooks directory itself may be a link. Record that link, then
            // list the directory `read_dir` follows. Entry symlinks are separate.
            body.extend_from_slice(&permission_token(&meta).to_le_bytes());
            body.push(b'l');
            let target = fs::read_link(&path).map_err(|err| hook_read(&path, &err))?;
            body.extend_from_slice(target.to_string_lossy().as_bytes());
            body.push(0);
        }
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(hook_read(&path, &err)),
    }
    append_hooks(&path, &mut body, 0)?;
    Ok(fnv1a(&body))
}

fn append_hooks(dir: &Path, body: &mut Vec<u8>, depth: u32) -> Result<(), String> {
    if depth > 32 {
        return Err("hooks: directory is too deep".to_string());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            body.extend_from_slice(b"missing");
            return Ok(());
        }
        Err(err) => return Err(hook_read(dir, &err)),
    };
    body.extend_from_slice(b"present\0");
    let mut items = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| hook_read(dir, &err))?;
        items.push(entry);
    }
    items.sort_by_key(|entry| entry.file_name());
    for entry in items {
        let path = entry.path();
        let name = entry.file_name();
        body.extend_from_slice(name.to_string_lossy().as_bytes());
        body.push(0);
        let meta = fs::symlink_metadata(&path).map_err(|err| hook_read(&path, &err))?;
        body.extend_from_slice(&permission_token(&meta).to_le_bytes());
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            body.push(b'l');
            let target = fs::read_link(&path).map_err(|err| hook_read(&path, &err))?;
            body.extend_from_slice(target.to_string_lossy().as_bytes());
            let mut chain = HookChain {
                paths: Vec::new(),
                #[cfg(unix)]
                ids: Vec::new(),
            };
            note_hook(&mut chain, &meta, &path)?;
            let followed = follow_hook_target(&path, &target, body, 0, &mut chain);
            pop_hook(&mut chain);
            followed?;
        } else if file_type.is_dir() {
            body.push(b'd');
            append_hooks(&path, body, depth + 1)?;
        } else if file_type.is_file() {
            body.push(b'f');
            append_regular_hook(&path, &meta, body)?;
        } else {
            body.push(b'o');
        }
        body.push(0);
    }
    Ok(())
}

fn follow_hook_target(
    link: &Path,
    target: &Path,
    body: &mut Vec<u8>,
    hops: u32,
    chain: &mut HookChain,
) -> Result<(), String> {
    if hops >= HOOK_SYMLINK_HOPS {
        return Err("hooks: symlink chain is too long".to_string());
    }
    let next = resolve_hook_link(link, target);
    let meta = fs::symlink_metadata(&next).map_err(|err| hook_read(&next, &err))?;
    note_hook(chain, &meta, &next)?;
    let result = record_followed_hook(&next, &meta, body, hops, chain);
    pop_hook(chain);
    result
}

fn record_followed_hook(
    path: &Path,
    meta: &fs::Metadata,
    body: &mut Vec<u8>,
    hops: u32,
    chain: &mut HookChain,
) -> Result<(), String> {
    body.extend_from_slice(&permission_token(meta).to_le_bytes());
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        body.push(b'l');
        let nested = fs::read_link(path).map_err(|err| hook_read(path, &err))?;
        body.extend_from_slice(nested.to_string_lossy().as_bytes());
        follow_hook_target(path, &nested, body, hops + 1, chain)
    } else if file_type.is_file() {
        append_regular_hook(path, meta, body)
    } else if file_type.is_dir() {
        Err("hooks: symlink target is a directory".to_string())
    } else {
        // A fifo or device would block or never end. Do not read it.
        body.push(b'o');
        Ok(())
    }
}

fn append_regular_hook(path: &Path, meta: &fs::Metadata, body: &mut Vec<u8>) -> Result<(), String> {
    if meta.len() > HOOK_FILE_BYTES {
        return Err("hooks: target is too large".to_string());
    }
    let bytes = fs::read(path).map_err(|err| hook_read(path, &err))?;
    if bytes.len() as u64 > HOOK_FILE_BYTES {
        return Err("hooks: target is too large".to_string());
    }
    body.push(b'f');
    body.extend_from_slice(&bytes);
    Ok(())
}

fn permission_token(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode()
    }
    #[cfg(not(unix))]
    {
        u32::from(meta.permissions().readonly())
    }
}

fn fnv1a(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn dirty(cwd: &Path) -> Result<bool, String> {
    Ok(!crate::git::dirty_paths(cwd)?.is_empty())
}

fn frozen_workflow(run: &Run) -> &str {
    run.workflow.as_deref().unwrap_or("classic")
}

fn is_gated(run: &Run) -> bool {
    frozen_workflow(run) == "gated"
}

fn workflow_changed(run: &Run, settings: &relaybook_core::Settings) -> bool {
    frozen_workflow(run) != settings.workflow
}

fn run_is_live(run: &Run) -> bool {
    matches!(run.status.as_str(), "active" | "waiting" | "paused")
}

fn waiting_checkpoint(run: &Run) -> bool {
    run.status == "waiting"
        && (run.stage == "gate_plan"
            || run.stage == "gate_release"
            || run.stage.starts_with("gate_review"))
}

fn waiting_checkpoint_message(cwd: &Path, run: &Run) -> String {
    if run.stage == "gate_release" {
        signoff_wait(cwd)
    } else {
        "waiting for approval: relaybook approve".to_string()
    }
}

fn load_live(ledger: &Ledger, run_id: i64) -> Result<Run, String> {
    let run = ledger.run_by_id(run_id).map_err(|err| err.to_string())?;
    if !run_is_live(&run) {
        return Err("no active run".to_string());
    }
    Ok(run)
}

/// Stop without pushing when the live setting no longer matches the run.
/// A settings read failure also skips the push.
fn refuse_workflow_change(ledger: &mut Ledger, run: &mut Run) -> Option<RunOutput> {
    match ledger.settings() {
        Ok(settings) if workflow_changed(run, &settings) => {
            Some(stop_workflow_mismatch(ledger, run))
        }
        Ok(_) => None,
        Err(err) => Some(fail(&err.to_string())),
    }
}

fn signoff_wait(cwd: &Path) -> String {
    let head = git(cwd, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "HEAD".to_string());
    format!("waiting for sign-off: relaybook release authorize --sha {head}")
}

fn authorized_at(ledger: &Ledger, run: &Run, sha: &str) -> Result<bool, String> {
    match ledger
        .authorization_at(sha)
        .map_err(|err| err.to_string())?
    {
        Some(auth) if auth.branch == run.branch && auth.commit_sha == sha => Ok(true),
        _ => Ok(false),
    }
}

fn refuse_existing_run_branch(
    ledger: &mut Ledger,
    cwd: &Path,
    branch: &str,
    remote_check: &str,
) -> Result<(), String> {
    let root = crate::gitinfo::toplevel(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let view = crate::remote::screen_named_branch(&root, branch, remote_check)
        .map_err(|err| err.to_string())?;
    ledger
        .record_remote_check(&relaybook_core::RemoteCheckDraft {
            context: "begin_run".to_string(),
            branch: view.branch.clone(),
            head_sha: view.head_sha.clone(),
            remote: view.remote.clone(),
            status: view.status.to_string(),
            detail: view.detail.clone(),
            overridden: false,
        })
        .map_err(|err| err.to_string())?;
    if view.status == "present" || view.status == "unknown" {
        return Err(view
            .message
            .unwrap_or_else(|| "remote check failed".to_string()));
    }
    Ok(())
}

enum CodexScope {
    Task(String),
    Release,
}

enum ReviewerMode {
    Codex,
    Sonnet { until: String, version: String },
}

fn codex_resume_refusal(
    ledger: &Ledger,
    run: &Run,
) -> Result<Option<String>, relaybook_core::Error> {
    let limited = match ledger.codex_availability()? {
        relaybook_core::CodexAvailability::Limited {
            retry_after,
            due: false,
        } => Some(retry_after),
        _ => None,
    };
    let Some(retry_after) = limited else {
        return Ok(None);
    };
    let reason = run.stop_reason.as_deref().unwrap_or("");
    if reason.starts_with("codex|") || reason.starts_with("codex-rereview|") {
        return Ok(Some(format!("codex limited until {retry_after}")));
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)] // the drive loop's session and transcript stay with the caller
fn review_gated(
    db: &Path,
    cwd: &Path,
    quiet: bool,
    run_id: i64,
    session: &mut Option<crate::live::Session>,
    transcript: &mut u32,
    scope: CodexScope,
) -> Control {
    let mut ledger = match Ledger::open(db) {
        Ok(ledger) => ledger,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    if let Err(err) = load_live(&ledger, run_id) {
        return Control::Return(fail(&err));
    }
    let settings = match ledger.settings() {
        Ok(settings) => settings,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let availability = match ledger.codex_availability() {
        Ok(availability) => availability,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let release = matches!(scope, CodexScope::Release);
    match availability {
        relaybook_core::CodexAvailability::Limited {
            retry_after,
            due: false,
        } => {
            if release && settings.fallback_rereview != "off" {
                return pause_for_rereview(cwd, &mut ledger, run_id, &retry_after);
            }
            if settings.review_fallback == "sonnet" {
                let version = agents::codex_version();
                drop(ledger);
                return review_with_codex(
                    db,
                    cwd,
                    quiet,
                    run_id,
                    session,
                    transcript,
                    scope,
                    ReviewerMode::Sonnet {
                        until: retry_after,
                        version,
                    },
                );
            }
            pause_for_limit(cwd, &mut ledger, run_id, &retry_after)
        }
        relaybook_core::CodexAvailability::Limited { due: true, .. } => {
            drop(ledger);
            probe_codex(db, cwd, quiet, run_id, session)
        }
        relaybook_core::CodexAvailability::Available => {
            let pending = match ledger.pending_rereviews(run_id) {
                Ok(pending) => pending,
                Err(err) => return Control::Return(fail(&err.to_string())),
            };
            if let Some(original) = pending.into_iter().next() {
                drop(ledger);
                return review_rereview(
                    db,
                    cwd,
                    quiet,
                    run_id,
                    session,
                    transcript,
                    original,
                    settings.codex_model,
                    settings.codex_sandbox,
                );
            }
            drop(ledger);
            review_with_codex(
                db,
                cwd,
                quiet,
                run_id,
                session,
                transcript,
                scope,
                ReviewerMode::Codex,
            )
        }
    }
}

fn pause_for_limit(cwd: &Path, ledger: &mut Ledger, run_id: i64, retry_after: &str) -> Control {
    let mut run = match load_live(ledger, run_id) {
        Ok(run) => run,
        Err(err) => return Control::Return(fail(&err)),
    };
    run.status = "paused".to_string();
    run.failure_streak = 0;
    run.stop_reason = Some(format!("codex|{retry_after}"));
    if let Err(err) = ledger.save_run(&mut run) {
        return Control::Return(fail(&err.to_string()));
    }
    let _ = cwd;
    Control::Return(ok(&codex_pause_line(&run)))
}

fn pause_for_rereview(cwd: &Path, ledger: &mut Ledger, run_id: i64, retry_after: &str) -> Control {
    let pending = match ledger.pending_rereviews(run_id) {
        Ok(rows) => rows.len(),
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let mut run = match load_live(ledger, run_id) {
        Ok(run) => run,
        Err(err) => return Control::Return(fail(&err)),
    };
    run.status = "paused".to_string();
    run.failure_streak = 0;
    run.stop_reason = Some(format!("codex-rereview|{pending}|{retry_after}"));
    if let Err(err) = ledger.save_run(&mut run) {
        return Control::Return(fail(&err.to_string()));
    }
    let _ = cwd;
    Control::Return(ok(&codex_pause_line(&run)))
}

#[allow(clippy::too_many_arguments)] // the drive loop's session and transcript stay with the caller
fn review_with_codex(
    db: &Path,
    cwd: &Path,
    quiet: bool,
    run_id: i64,
    session: &mut Option<crate::live::Session>,
    transcript: &mut u32,
    scope: CodexScope,
    mode: ReviewerMode,
) -> Control {
    let mut ledger = match Ledger::open(db) {
        Ok(ledger) => ledger,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let mut run = match load_live(&ledger, run_id) {
        Ok(run) => run,
        Err(err) => return Control::Return(fail(&err)),
    };
    let settings = match ledger.settings() {
        Ok(settings) => settings,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let sandbox = if settings.codex_sandbox == "isolated" {
        agents::CodexSandbox::Isolated
    } else {
        agents::CodexSandbox::ReadOnly
    };
    let gates = settings.gates.clone();
    if let CodexScope::Task(slug) = &scope {
        match recover_accepted_task(cwd, &mut ledger, &mut run, slug, &gates) {
            Ok(Some(control)) => return control,
            Ok(None) => {}
            Err(err) => return Control::Return(halt(cwd, &mut ledger, &mut run, &err)),
        }
    }
    let choice = reviewer_choice(&settings, &mode);
    let prepared = match &scope {
        CodexScope::Task(slug) => {
            crate::gate::start_task_review(&mut ledger, cwd, &run, slug, &choice)
        }
        CodexScope::Release => crate::gate::start_release_review(&mut ledger, cwd, &run, &choice),
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(err) => {
            if err.to_string() == relaybook_core::STALE_REVIEW_INVOCATION {
                return Control::Return(fail(relaybook_core::STALE_REVIEW_INVOCATION));
            }
            return Control::Return(halt(cwd, &mut ledger, &mut run, &err.to_string()));
        }
    };
    let mut run = match load_live(&ledger, run_id) {
        Ok(run) => run,
        Err(err) => return Control::Return(fail(&err)),
    };
    if run.write_stamp != prepared.run_generation {
        return Control::Return(fail(relaybook_core::STALE_REVIEW_INVOCATION));
    }
    let packet = match review_packet(cwd, &prepared) {
        Ok(packet) => packet,
        Err(err) => return Control::Return(halt(cwd, &mut ledger, &mut run, &err)),
    };
    let prompt = match &scope {
        CodexScope::Task(slug) => {
            format!("Review task {slug}. Use only the packet on stdin. Return the JSON verdict. Body fields findings, security, tests_checked, and not_checked are arrays of strings.")
        }
        CodexScope::Release => {
            "Release review. Use only the packet on stdin. Return the JSON verdict. Body fields findings, security, tests_checked, not_checked, prd_covered, and gaps are arrays of strings.".to_string()
        }
    };
    if agents::review_input_too_large(&prompt, &packet) {
        return Control::Return(halt(
            cwd,
            &mut ledger,
            &mut run,
            "diff too large for auto review; review manually",
        ));
    }
    let release = matches!(scope, CodexScope::Release);
    let label = prepared
        .slug
        .clone()
        .unwrap_or_else(|| "release".to_string());
    let commit_sha = prepared.commit_sha.clone();
    let review_id = prepared.review_id;
    let generation = prepared.run_generation;
    let invocation_token = prepared.invocation_token.clone();
    let root = crate::gitinfo::toplevel(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let out = agents::codex_output_file(&root, run_id, review_id, generation);
    let review_branch = run.branch.clone();
    let before = match gated_snap(cwd, &review_branch) {
        Ok(before) => before,
        Err(err) => return Control::Return(halt(cwd, &mut ledger, &mut run, &err)),
    };
    drop(ledger);
    let live = live_session(session, cwd, run_id, quiet);
    live.begin(&review_live_line(&label, &mode));
    let started = Instant::now();
    let schema = if release {
        agents::release_codex_schema()
    } else {
        agents::task_codex_schema()
    };
    let result = match &mode {
        ReviewerMode::Codex => agents::call_codex(
            cwd,
            &prompt,
            &packet,
            &out,
            run_id,
            live,
            sandbox,
            schema,
            &settings.codex_model,
        ),
        ReviewerMode::Sonnet { .. } => {
            agents::call_claude_reviewer(cwd, &prompt, &packet, run_id, live)
        }
    };
    let elapsed = started.elapsed();
    let _ = agents::write_transcript(
        cwd,
        run_id,
        *transcript,
        Role::Reviewer,
        &result_text(&result),
    );
    *transcript += 1;
    let invocation = |action| {
        apply_codex_invocation(
            db,
            cwd,
            quiet,
            run_id,
            review_id,
            generation,
            &invocation_token,
            action,
        )
    };
    if let Err(reason) = guard_reviewer(cwd, &before) {
        close_live(session, cwd, run_id, quiet, elapsed, &result, "halted");
        return Control::Return(invocation(relaybook_core::ReviewInvocationAction::Halt {
            reason,
        }));
    }
    match &result {
        AgentResult::Unavailable { cli } => {
            close_live(session, cwd, run_id, quiet, elapsed, &result, "unavailable");
            return Control::Return(invocation(relaybook_core::ReviewInvocationAction::Halt {
                reason: format!("{cli} unavailable"),
            }));
        }
        AgentResult::TimedOut => {
            close_live(session, cwd, run_id, quiet, elapsed, &result, "timed out");
            return Control::Return(invocation(relaybook_core::ReviewInvocationAction::Halt {
                reason: "timeout".to_string(),
            }));
        }
        AgentResult::UsageLimit { .. } => {
            close_live(session, cwd, run_id, quiet, elapsed, &result, "paused");
            if !matches!(mode, ReviewerMode::Codex) {
                return Control::Return(invocation(
                    relaybook_core::ReviewInvocationAction::FailureStreak,
                ));
            }
            let Some(detail) = agents::codex_limit_detail(&result) else {
                return Control::Return(invocation(
                    relaybook_core::ReviewInvocationAction::FailureStreak,
                ));
            };
            let ledger = match Ledger::open(db) {
                Ok(ledger) => ledger,
                Err(err) => return Control::Return(fail(&err.to_string())),
            };
            let advisory = agents::parse_codex_retry_after(&detail.stderr);
            let (retry_after, source) =
                match ledger.resolve_retry_after(advisory.as_deref(), settings.codex_backoff_min) {
                    Ok(resolved) => resolved,
                    Err(err) => return Control::Return(fail(&err.to_string())),
                };
            let evidence = format!("{}: {}", detail.signature, detail.stderr);
            return Control::Return(invocation(
                relaybook_core::ReviewInvocationAction::CodexLimited {
                    retry_after,
                    source: source.to_string(),
                    evidence,
                    version: detail.version,
                },
            ));
        }
        AgentResult::Failed { .. } => {
            close_live(session, cwd, run_id, quiet, elapsed, &result, "failed");
            return Control::Return(invocation(
                relaybook_core::ReviewInvocationAction::FailureStreak,
            ));
        }
        AgentResult::Finished { .. } => {}
    }
    let (verdict, body) = match agents::parse_codex_verdict(&result_text(&result), release) {
        Ok(parsed) => parsed,
        Err(_) => {
            close_live(session, cwd, run_id, quiet, elapsed, &result, "failed");
            return Control::Return(invocation(
                relaybook_core::ReviewInvocationAction::FailureStreak,
            ));
        }
    };
    let mut ledger = match Ledger::open(db) {
        Ok(ledger) => ledger,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let run = match load_live(&ledger, run_id) {
        Ok(run) => run,
        Err(err) => return Control::Return(fail(&err)),
    };
    let advance = codex_run_advance(&run, release, &verdict, &gates);
    let recorded = match &scope {
        CodexScope::Task(slug) => crate::gate::record_owned_task_review(
            &mut ledger,
            cwd,
            slug,
            &commit_sha,
            &verdict,
            &body,
            review_id,
            &invocation_token,
            generation,
            &choice,
            advance.clone(),
        ),
        CodexScope::Release => crate::gate::record_owned_release_review(
            &mut ledger,
            cwd,
            &commit_sha,
            &verdict,
            &body,
            review_id,
            &invocation_token,
            generation,
            &choice,
            advance.clone(),
        ),
    };
    if let Err(err) = recorded {
        close_live(session, cwd, run_id, quiet, elapsed, &result, "failed");
        drop(ledger);
        let message = err.to_string();
        if message == relaybook_core::STALE_REVIEW_INVOCATION {
            return Control::Return(fail(relaybook_core::STALE_REVIEW_INVOCATION));
        }
        if message == relaybook_core::LEDGER_CHANGED_DURING_REVIEW {
            return Control::Return(invocation(relaybook_core::ReviewInvocationAction::Halt {
                reason: message,
            }));
        }
        return Control::Return(invocation(
            relaybook_core::ReviewInvocationAction::FailureStreak,
        ));
    }
    if advance.status == "stopped" {
        close_live(session, cwd, run_id, quiet, elapsed, &result, "halted");
        let reason = advance
            .stop_reason
            .as_deref()
            .unwrap_or("release review requested changes");
        return Control::Return(fail(&format!("stopped: {reason}; relaybook run --resume")));
    }
    let report_verdict = if release {
        "approved".to_string()
    } else if verdict == "approved" {
        "accepted".to_string()
    } else {
        "reopened".to_string()
    };
    let outcome = if report_verdict == "reopened" {
        "reopened"
    } else if advance.status == "waiting" && !release {
        "paused at checkpoint"
    } else if release {
        "waiting"
    } else {
        "accepted"
    };
    close_live(session, cwd, run_id, quiet, elapsed, &result, outcome);
    println!("{}", codex_review_line(&run, &label, &report_verdict));
    if advance.status == "waiting" {
        let message = if release {
            signoff_wait(cwd)
        } else {
            "waiting for approval: relaybook approve".to_string()
        };
        return Control::Return(ok(&message));
    }
    Control::Stay
}

fn reviewer_choice(
    settings: &relaybook_core::Settings,
    mode: &ReviewerMode,
) -> crate::gate::ReviewerChoice {
    match mode {
        ReviewerMode::Codex => crate::gate::ReviewerChoice::codex(&settings.codex_model),
        ReviewerMode::Sonnet { until, version } => crate::gate::ReviewerChoice {
            reviewer: "claude".to_string(),
            model: Some(relaybook_core::SONNET_REVIEW_MODEL.to_string()),
            fallback_reason: Some(format!("codex usage limit until {until}")),
            note_fallback: true,
            fallback_retry_after: Some(until.clone()),
            codex_version: Some(version.clone()),
            rereview_of: None,
            accept_approved_task: true,
            reopen_on_changes: true,
        },
    }
}

fn review_live_line(label: &str, mode: &ReviewerMode) -> String {
    match mode {
        ReviewerMode::Codex => format!("● Reviewing {label} (codex)"),
        ReviewerMode::Sonnet { until, .. } => format!(
            "● Reviewing {label} ({} — fallback: codex usage limit until {until})",
            relaybook_core::SONNET_REVIEW_MODEL
        ),
    }
}

fn probe_codex(
    db: &Path,
    cwd: &Path,
    quiet: bool,
    run_id: i64,
    session: &mut Option<crate::live::Session>,
) -> Control {
    let mut ledger = match Ledger::open(db) {
        Ok(ledger) => ledger,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let run = match load_live(&ledger, run_id) {
        Ok(run) => run,
        Err(err) => return Control::Return(fail(&err)),
    };
    let settings = match ledger.settings() {
        Ok(settings) => settings,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let sandbox = if settings.codex_sandbox == "isolated" {
        agents::CodexSandbox::Isolated
    } else {
        agents::CodexSandbox::ReadOnly
    };
    let claim = match ledger.begin_probe(run_id, run.write_stamp, &run.stage, run.agent_calls + 1) {
        Ok(claim) => claim,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let stage = run.stage.clone();
    let branch = run.branch.clone();
    let before = match gated_snap(cwd, &branch) {
        Ok(before) => before,
        Err(err) => {
            let _ = ledger.record_probe(
                run_id,
                claim.generation,
                &claim.token,
                &stage,
                &claim.ledger_mark,
                relaybook_core::ProbeOutcome::Halt {
                    reason: err.clone(),
                },
            );
            return Control::Return(fail(&err));
        }
    };
    drop(ledger);
    let root = crate::gitinfo::toplevel(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let out = agents::codex_output_file(&root, run_id, 0, claim.generation);
    let live = live_session(session, cwd, run_id, quiet);
    live.begin("● Probing Codex");
    let result = agents::call_codex_probe(cwd, &out, run_id, live, sandbox, &settings.codex_model);
    let outcome = if let Err(reason) = guard_reviewer(cwd, &before) {
        relaybook_core::ProbeOutcome::Halt { reason }
    } else {
        match &result {
            AgentResult::Finished { .. } => relaybook_core::ProbeOutcome::Ok {
                version: agents::codex_version(),
            },
            AgentResult::UsageLimit { .. } => {
                if let Some(detail) = agents::codex_limit_detail(&result) {
                    let ledger = match Ledger::open(db) {
                        Ok(ledger) => ledger,
                        Err(err) => return Control::Return(fail(&err.to_string())),
                    };
                    let (retry_after, source) = match ledger.resolve_retry_after(
                        agents::parse_codex_retry_after(&detail.stderr).as_deref(),
                        settings.codex_backoff_min,
                    ) {
                        Ok(resolved) => resolved,
                        Err(err) => return Control::Return(fail(&err.to_string())),
                    };
                    relaybook_core::ProbeOutcome::Limited {
                        retry_after,
                        source: source.to_string(),
                        evidence: format!("{}: {}", detail.signature, detail.stderr),
                        version: detail.version,
                    }
                } else {
                    relaybook_core::ProbeOutcome::Failed {
                        evidence: result_text(&result),
                        version: agents::codex_version(),
                    }
                }
            }
            AgentResult::Unavailable { cli } => relaybook_core::ProbeOutcome::Failed {
                evidence: format!("{cli} unavailable"),
                version: agents::codex_version(),
            },
            AgentResult::TimedOut => relaybook_core::ProbeOutcome::Failed {
                evidence: "timeout".to_string(),
                version: agents::codex_version(),
            },
            AgentResult::Failed { .. } => relaybook_core::ProbeOutcome::Failed {
                evidence: result_text(&result),
                version: agents::codex_version(),
            },
        }
    };
    let mut ledger = match Ledger::open(db) {
        Ok(ledger) => ledger,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let outcome = match ledger.record_probe(
        run_id,
        claim.generation,
        &claim.token,
        &stage,
        &claim.ledger_mark,
        outcome,
    ) {
        Ok(outcome) => outcome,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let limited_until = match &outcome {
        relaybook_core::ProbeOutcome::Limited { retry_after, .. } => Some(retry_after.clone()),
        _ => None,
    };
    let failed = matches!(
        outcome,
        relaybook_core::ProbeOutcome::Failed { .. } | relaybook_core::ProbeOutcome::Halt { .. }
    );
    if failed {
        let run = ledger.run_by_id(run_id).ok();
        let reason = run
            .and_then(|run| run.stop_reason)
            .unwrap_or_else(|| "codex probe failed (not a usage limit)".to_string());
        return Control::Return(fail(&format!("stopped: {reason}; relaybook run --resume")));
    }
    if let Some(until) = limited_until {
        println!("● Codex still limited until {until}");
        return Control::Stay;
    }
    println!("● Codex probe ok");
    println!("● Switched back to Codex");
    let _ = quiet;
    Control::Stay
}

#[allow(clippy::too_many_arguments)] // one re-review keeps the same session as the driver
fn review_rereview(
    db: &Path,
    cwd: &Path,
    quiet: bool,
    run_id: i64,
    session: &mut Option<crate::live::Session>,
    transcript: &mut u32,
    original: relaybook_core::PendingRereviewRow,
    model: String,
    sandbox_name: String,
) -> Control {
    let mut ledger = match Ledger::open(db) {
        Ok(ledger) => ledger,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let mut run = match load_live(&ledger, run_id) {
        Ok(run) => run,
        Err(err) => return Control::Return(fail(&err)),
    };
    let prepared = match crate::gate::start_rereview(&mut ledger, cwd, &run, &original, &model) {
        Ok(prepared) => prepared,
        Err(err) => {
            if err.to_string() == relaybook_core::STALE_REVIEW_INVOCATION {
                return Control::Return(fail(relaybook_core::STALE_REVIEW_INVOCATION));
            }
            return Control::Return(halt(cwd, &mut ledger, &mut run, &err.to_string()));
        }
    };
    let mut run = match load_live(&ledger, run_id) {
        Ok(run) => run,
        Err(err) => return Control::Return(fail(&err)),
    };
    if run.write_stamp != prepared.run_generation {
        return Control::Return(fail(relaybook_core::STALE_REVIEW_INVOCATION));
    }
    let packet = match review_packet(cwd, &prepared) {
        Ok(packet) => packet,
        Err(err) => return Control::Return(halt(cwd, &mut ledger, &mut run, &err)),
    };
    let release = original.scope == "release";
    let label = prepared
        .slug
        .clone()
        .unwrap_or_else(|| "release".to_string());
    let prompt = format!(
        "Re-review {label}. Use only the packet on stdin. Return the JSON verdict. Body fields are arrays of strings."
    );
    if agents::review_input_too_large(&prompt, &packet) {
        return Control::Return(halt(
            cwd,
            &mut ledger,
            &mut run,
            "diff too large for auto review; review manually",
        ));
    }
    let sandbox = if sandbox_name == "isolated" {
        agents::CodexSandbox::Isolated
    } else {
        agents::CodexSandbox::ReadOnly
    };
    let commit_sha = prepared.commit_sha.clone();
    let review_id = prepared.review_id;
    let generation = prepared.run_generation;
    let invocation_token = prepared.invocation_token.clone();
    let stage = run.stage.clone();
    let root = crate::gitinfo::toplevel(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let out = agents::codex_output_file(&root, run_id, review_id, generation);
    let before = match gated_snap(cwd, &run.branch) {
        Ok(before) => before,
        Err(err) => return Control::Return(halt(cwd, &mut ledger, &mut run, &err)),
    };
    drop(ledger);
    let live = live_session(session, cwd, run_id, quiet);
    live.begin(&format!("● Re-reviewing {label} (codex)"));
    let schema = if release {
        agents::release_codex_schema()
    } else {
        agents::task_codex_schema()
    };
    let result = agents::call_codex(
        cwd, &prompt, &packet, &out, run_id, live, sandbox, schema, &model,
    );
    let _ = agents::write_transcript(
        cwd,
        run_id,
        *transcript,
        Role::Reviewer,
        &result_text(&result),
    );
    *transcript += 1;
    let invocation = |action| {
        apply_codex_invocation(
            db,
            cwd,
            quiet,
            run_id,
            review_id,
            generation,
            &invocation_token,
            action,
        )
    };
    if let Err(reason) = guard_reviewer(cwd, &before) {
        return Control::Return(invocation(relaybook_core::ReviewInvocationAction::Halt {
            reason,
        }));
    }
    match &result {
        AgentResult::Unavailable { cli } => {
            return Control::Return(invocation(relaybook_core::ReviewInvocationAction::Halt {
                reason: format!("{cli} unavailable"),
            }));
        }
        AgentResult::TimedOut => {
            return Control::Return(invocation(relaybook_core::ReviewInvocationAction::Halt {
                reason: "timeout".to_string(),
            }));
        }
        AgentResult::UsageLimit { .. } => {
            let Some(detail) = agents::codex_limit_detail(&result) else {
                return Control::Return(invocation(
                    relaybook_core::ReviewInvocationAction::FailureStreak,
                ));
            };
            let ledger = match Ledger::open(db) {
                Ok(ledger) => ledger,
                Err(err) => return Control::Return(fail(&err.to_string())),
            };
            let backoff = ledger
                .settings()
                .map(|item| item.codex_backoff_min)
                .unwrap_or(relaybook_core::DEFAULT_CODEX_BACKOFF_MIN);
            let (retry_after, source) = match ledger.resolve_retry_after(None, backoff) {
                Ok(resolved) => resolved,
                Err(err) => return Control::Return(fail(&err.to_string())),
            };
            return Control::Return(invocation(
                relaybook_core::ReviewInvocationAction::CodexLimited {
                    retry_after,
                    source: source.to_string(),
                    evidence: format!("{}: {}", detail.signature, detail.stderr),
                    version: detail.version,
                },
            ));
        }
        AgentResult::Failed { .. } => {
            return Control::Return(invocation(
                relaybook_core::ReviewInvocationAction::FailureStreak,
            ));
        }
        AgentResult::Finished { .. } => {}
    }
    let (verdict, body) = match agents::parse_review_verdict(&result_text(&result), release) {
        Ok(parsed) => parsed,
        Err(_) => {
            return Control::Return(invocation(
                relaybook_core::ReviewInvocationAction::FailureStreak,
            ));
        }
    };
    let mut ledger = match Ledger::open(db) {
        Ok(ledger) => ledger,
        Err(err) => return Control::Return(fail(&err.to_string())),
    };
    let run = match load_live(&ledger, run_id) {
        Ok(run) => run,
        Err(err) => return Control::Return(fail(&err)),
    };
    let mut choice = crate::gate::ReviewerChoice::codex(&model);
    choice.accept_approved_task = false;
    choice.reopen_on_changes = false;
    choice.rereview_of = Some(original.review_id);
    let advance = if verdict == "approved" {
        relaybook_core::OwnedRunAdvance {
            stage: stage.clone(),
            status: "active".to_string(),
            failure_streak: 0,
            stop_reason: None,
        }
    } else {
        relaybook_core::OwnedRunAdvance {
            stage: stage.clone(),
            status: "stopped".to_string(),
            failure_streak: 0,
            stop_reason: Some(format!("codex re-review requested changes on {label}")),
        }
    };
    let recorded = if release {
        crate::gate::record_owned_release_review(
            &mut ledger,
            cwd,
            &commit_sha,
            &verdict,
            &body,
            review_id,
            &invocation_token,
            generation,
            &choice,
            advance.clone(),
        )
    } else {
        crate::gate::record_owned_task_review(
            &mut ledger,
            cwd,
            &label,
            &commit_sha,
            &verdict,
            &body,
            review_id,
            &invocation_token,
            generation,
            &choice,
            advance.clone(),
        )
    };
    if let Err(err) = recorded {
        let message = err.to_string();
        if message == relaybook_core::STALE_REVIEW_INVOCATION
            || message == relaybook_core::LEDGER_CHANGED_DURING_REVIEW
        {
            return Control::Return(fail(&message));
        }
        return Control::Return(invocation(
            relaybook_core::ReviewInvocationAction::FailureStreak,
        ));
    }
    if advance.status == "stopped" {
        let reason = advance.stop_reason.unwrap_or_else(|| "stopped".to_string());
        return Control::Return(fail(&format!("stopped: {reason}; relaybook run --resume")));
    }
    println!("{}", codex_review_line(&run, &label, "re-reviewed"));
    let _ = quiet;
    Control::Stay
}

fn codex_run_advance(
    run: &Run,
    release: bool,
    verdict: &str,
    gates: &str,
) -> relaybook_core::OwnedRunAdvance {
    if release && verdict != "approved" {
        return relaybook_core::OwnedRunAdvance {
            stage: run.stage.clone(),
            status: "stopped".to_string(),
            failure_streak: 0,
            stop_reason: Some("release review requested changes".to_string()),
        };
    }
    if release {
        return relaybook_core::OwnedRunAdvance {
            stage: "gate_release".to_string(),
            status: "waiting".to_string(),
            failure_streak: 0,
            stop_reason: run.stop_reason.clone(),
        };
    }
    let report_verdict = if verdict == "approved" {
        "accepted"
    } else {
        "reopened"
    };
    if gates == "plan,review" {
        return relaybook_core::OwnedRunAdvance {
            stage: format!("gate_review:{report_verdict}"),
            status: "waiting".to_string(),
            failure_streak: 0,
            stop_reason: run.stop_reason.clone(),
        };
    }
    relaybook_core::OwnedRunAdvance {
        stage: "after_review".to_string(),
        status: "active".to_string(),
        failure_streak: 0,
        stop_reason: run.stop_reason.clone(),
    }
}

/// An approved task review already accepted this report and committed that
/// fact, but the run was still at `review`. Continue from that result.
fn recover_accepted_task(
    cwd: &Path,
    ledger: &mut Ledger,
    run: &mut Run,
    slug: &str,
    gates: &str,
) -> Result<Option<Control>, String> {
    let Some((_, Some(verdict))) = ledger
        .latest_report_meta(slug)
        .map_err(|err| err.to_string())?
    else {
        return Ok(None);
    };
    if verdict != "accepted" {
        return Ok(None);
    }
    let Some(review) = ledger
        .latest_review("task", Some(slug))
        .map_err(|err| err.to_string())?
    else {
        return Ok(None);
    };
    if review.verdict.as_deref() != Some("approved") || review.run_id != Some(run.id) {
        return Ok(None);
    }
    let head = git(cwd, &["rev-parse", "HEAD"])?;
    if review.commit_sha != head {
        return Ok(None);
    }
    let Some(report) = ledger
        .latest_task_report(slug)
        .map_err(|err| err.to_string())?
    else {
        return Ok(None);
    };
    if report.commit_sha.as_deref() != Some(head.as_str()) {
        return Ok(None);
    }
    let advance = codex_run_advance(run, false, "approved", gates);
    run.stage = advance.stage;
    run.status = advance.status;
    run.failure_streak = advance.failure_streak;
    run.stop_reason = advance.stop_reason;
    ledger.save_run(run).map_err(|err| err.to_string())?;
    println!("{}", codex_review_line(run, slug, "accepted"));
    if run.status == "waiting" {
        return Ok(Some(Control::Return(ok(
            "waiting for approval: relaybook approve",
        ))));
    }
    Ok(Some(Control::Stay))
}

#[allow(clippy::too_many_arguments)] // invocation identity stays with the caller
fn apply_codex_invocation(
    db: &Path,
    cwd: &Path,
    quiet: bool,
    run_id: i64,
    review_id: i64,
    generation: i64,
    invocation_token: &str,
    action: relaybook_core::ReviewInvocationAction,
) -> RunOutput {
    let mut ledger = match Ledger::open(db) {
        Ok(ledger) => ledger,
        Err(err) => return fail(&err.to_string()),
    };
    match relaybook_core::apply_review_invocation(
        &mut ledger,
        review_id,
        invocation_token,
        generation,
        action,
    ) {
        Ok(relaybook_core::ReviewInvocationOutcome::Stale) => {
            fail(relaybook_core::STALE_REVIEW_INVOCATION)
        }
        Ok(relaybook_core::ReviewInvocationOutcome::WaitingCheckpoint) => {
            let stage = ledger
                .run_by_id(run_id)
                .map(|run| run.stage)
                .unwrap_or_default();
            if stage == "gate_release" {
                ok(&signoff_wait(cwd))
            } else {
                ok("waiting for approval: relaybook approve")
            }
        }
        Ok(relaybook_core::ReviewInvocationOutcome::Saved { drive: true, .. }) => {
            drop(ledger);
            drive(db, cwd, quiet, run_id)
        }
        Ok(relaybook_core::ReviewInvocationOutcome::Saved { drive: false, run }) => {
            if run.status == "paused" {
                ok(&codex_pause_line(&run))
            } else {
                let reason = run.stop_reason.unwrap_or_else(|| "stopped".to_string());
                fail(&format!("stopped: {reason}; relaybook run --resume"))
            }
        }
        Err(err) => fail(&err.to_string()),
    }
}

fn codex_pause_line(run: &Run) -> String {
    match run.stop_reason.as_deref() {
        Some(reason) if reason.starts_with("codex-rereview|") => {
            let mut parts = reason.splitn(3, '|');
            let _ = parts.next();
            let pending = parts.next().unwrap_or("0");
            let time = parts.next().unwrap_or("");
            format!("waiting for Codex re-review ({pending} pending, retry after {time})")
        }
        Some(reason) => match reason.split_once('|') {
            Some((cli, time)) if !time.is_empty() => {
                format!("usage limit reached on {cli}; relaybook run --resume after {time}")
            }
            Some((cli, _)) => format!("usage limit reached on {cli}; relaybook run --resume"),
            None => format!("usage limit reached on {reason}; relaybook run --resume"),
        },
        None => "usage limit reached on codex; relaybook run --resume".to_string(),
    }
}

fn review_packet(cwd: &Path, prepared: &crate::gate::PreparedReview) -> Result<String, String> {
    let guide = crate::guide::render(Some(crate::guide::Role::Reviewer));
    let handoff = serde_json::to_string_pretty(&prepared.handoff).map_err(|err| err.to_string())?;
    let spec = if let Some(spec) = &prepared.diff_spec {
        spec.clone()
    } else {
        match prepared.base_sha.as_deref() {
            Some(base) => format!("{base}..{}", prepared.commit_sha),
            None => format!("{sha}^..{sha}", sha = prepared.commit_sha),
        }
    };
    let diff = git(cwd, &["diff", "--end-of-options", &spec])?;
    Ok(format!("{guide}\n{handoff}\n{diff}"))
}

fn codex_review_line(run: &Run, slug: &str, verdict: &str) -> String {
    format!(
        "cycle {}: codex review -> {slug} {verdict}; calls {}",
        run.cycle, run.agent_calls
    )
}

fn queue_release_review(ledger: &mut Ledger, run: &mut Run) -> Result<(), String> {
    run.stage = "release_review".to_string();
    ledger.save_run(run).map_err(|err| err.to_string())
}

fn publish(cwd: &Path, ledger: &mut Ledger, run: &mut Run) -> RunOutput {
    if let Some(output) = refuse_workflow_change(ledger, run) {
        return output;
    }
    match crate::gate::rereview_blocks_publication(ledger, run.id) {
        Ok(true) => return fail("codex re-review pending"),
        Ok(false) => {}
        Err(err) => return fail(&err.to_string()),
    }
    // Take the run before any publish screen. A stale snapshot fails here
    // and must not push or save over the owner.
    run.stage = "publish".to_string();
    run.status = "active".to_string();
    if let Err(err) = ledger.claim_run(run) {
        return fail(&err.to_string());
    }
    let head = match git(cwd, &["rev-parse", "HEAD"]) {
        Ok(head) => head,
        Err(err) => return halt(cwd, ledger, run, &err),
    };
    let authorized = match authorized_at(ledger, run, &head) {
        Ok(authorized) => authorized,
        Err(err) => return halt(cwd, ledger, run, &err),
    };
    if !authorized {
        return halt(cwd, ledger, run, "HEAD moved after authorization");
    }
    if let Err(err) = crate::gate::ensure_gated_clean(cwd) {
        return halt(cwd, ledger, run, &err.to_string());
    }
    let root = crate::gitinfo::toplevel(cwd).unwrap_or_else(|| cwd.to_path_buf());
    if let Err(err) = crate::gate::require_unpublished(ledger, &root, "publish") {
        return halt(cwd, ledger, run, &err.to_string());
    }
    let origin = git(cwd, &["remote"])
        .unwrap_or_default()
        .lines()
        .any(|line| line == "origin");
    if !origin {
        return halt(cwd, ledger, run, "origin is missing");
    }
    if let Some(output) = refuse_workflow_change(ledger, run) {
        return output;
    }
    // Commit the claim before git push. `run stop` and `run --resume` do not
    // clear this stage; only this process does, when it finishes or stops.
    run.stage = "publishing".to_string();
    if let Err(err) = ledger.claim_run(run) {
        return fail(&err.to_string());
    }
    let spec = format!("{head}:refs/heads/{}", run.branch);
    if let Err(err) = git(cwd, &["push", "--no-follow-tags", "-u", "origin", &spec]) {
        return halt(cwd, ledger, run, &err);
    }
    let mut body = report_body(ledger, run.id);
    if is_gated(run) {
        body.push_str(&reviews_section(ledger, run.id));
    }
    let title = format!("Relaybook run {}", run.branch);
    match gh_pr(cwd, &run.branch, &title, &body) {
        Ok(url) => {
            run.status = "complete".to_string();
            run.stage = "done".to_string();
            run.stop_reason = Some(url.clone());
            if let Err(err) = ledger.save_run(run) {
                return fail(&err.to_string());
            }
            ok(&format!("opened {url}"))
        }
        Err(err) => halt(cwd, ledger, run, &err),
    }
}

fn sync_main(cwd: &Path) -> Result<(), String> {
    git(cwd, &["checkout", "main"])?;
    if git(cwd, &["remote"])
        .unwrap_or_default()
        .lines()
        .any(|line| line == "origin")
    {
        git(cwd, &["fetch", "origin", "main"])?;
        let local = git(cwd, &["rev-parse", "main"])?;
        let remote = git(cwd, &["rev-parse", "origin/main"])?;
        if local != remote {
            git(cwd, &["merge", "--ff-only", "origin/main"])?;
        }
    }
    Ok(())
}

fn push_branch(cwd: &Path, branch: &str) -> Result<(), String> {
    if git(cwd, &["remote"])
        .unwrap_or_default()
        .lines()
        .all(|line| line != "origin")
    {
        return Ok(());
    }
    git(cwd, &["push", "-u", "origin", &format!("HEAD:{branch}")])?;
    Ok(())
}

fn gh_pr(cwd: &Path, branch: &str, title: &str, body: &str) -> Result<String, String> {
    let output = Command::new("gh")
        .current_dir(cwd)
        .args([
            "pr", "create", "--base", "main", "--head", branch, "--title", title, "--body", body,
        ])
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "gh pr create failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() {
        Err("gh pr create returned no url".to_string())
    } else {
        Ok(url)
    }
}

fn tag_list(cwd: &Path) -> Vec<String> {
    git(cwd, &["tag", "--list"])
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn file_sha256(path: &Path) -> Result<String, String> {
    for (bin, flag) in [("shasum", Some("-a")), ("sha256sum", None)] {
        let mut command = Command::new(bin);
        if let Some(flag) = flag {
            command.arg(flag).arg("256");
        }
        command.arg(path);
        if let Ok(output) = command.output()
            && output.status.success()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            let Some(sum) = text.split_whitespace().next() else {
                continue;
            };
            return Ok(sum.to_string());
        }
    }
    Err("sha256 is not available".to_string())
}

fn stem(path: &Path) -> String {
    let raw = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("work");
    let mut slug = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "work".to_string()
    } else {
        slug.to_string()
    }
}

fn ok(text: &str) -> RunOutput {
    RunOutput {
        text: format!("{text}\n"),
        code: 0,
    }
}

fn fail(text: &str) -> RunOutput {
    RunOutput {
        text: format!("{text}\n"),
        code: 1,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::drive;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn bare_has(bare: &Path, rev: &str) -> bool {
        Command::new("git")
            .args([
                "--git-dir",
                bare.to_str().expect("utf8"),
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                rev,
            ])
            .status()
            .expect("rev-parse")
            .success()
    }

    #[test]
    fn drive_stops_classic_or_unset_workflow_without_pushing() {
        for workflow in [Some("classic"), None] {
            let root = tempfile::tempdir().expect("temp");
            let bare = root.path().join("origin.git");
            let work = root.path().join("work");
            fs::create_dir(&work).expect("work");
            git(
                root.path(),
                &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
            );
            git(&work, &["init", "-b", "main"]);
            git(&work, &["config", "user.email", "relaybook@example.com"]);
            git(&work, &["config", "user.name", "Relaybook"]);
            git(&work, &["config", "commit.gpgsign", "false"]);
            fs::write(work.join("README.md"), "seed\n").expect("write");
            git(&work, &["add", "README.md"]);
            git(&work, &["commit", "-m", "seed"]);
            git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
            git(&work, &["push", "-u", "origin", "main"]);
            fs::write(work.join("ship.txt"), "ship\n").expect("ship");
            git(&work, &["add", "ship.txt"]);
            git(&work, &["commit", "-m", "ship"]);
            let db = work.join(".relaybook").join("ledger.db");
            let mut ledger = relaybook_core::Ledger::initialize(&db).expect("init");
            ledger.set_setting("mode", "auto").expect("mode");
            let run = ledger
                .start_run("prd.md", "abc", "relaybook/prd", workflow)
                .expect("run");
            assert_eq!(run.status, "active");
            assert_eq!(run.workflow.as_deref(), workflow);
            let run_id = run.id;
            ledger.set_setting("reviewer", "codex").expect("reviewer");
            ledger.set_setting("workflow", "gated").expect("gated");
            drop(ledger);

            let output = drive(&db, &work, true, run_id);
            assert_eq!(output.code, 1, "{}", output.text);
            assert!(
                output.text.contains("workflow changed during run"),
                "{}",
                output.text
            );
            assert!(
                !bare_has(&bare, "refs/heads/relaybook/prd"),
                "workflow {workflow:?} was pushed"
            );
            let ledger = relaybook_core::Ledger::open(&db).expect("open");
            let stored = ledger.latest_run().expect("latest").expect("row");
            assert_eq!(stored.status, "stopped");
            assert_eq!(stored.workflow.as_deref(), workflow);
            assert_eq!(
                stored.stop_reason.as_deref(),
                Some("workflow changed during run")
            );
        }
    }

    #[test]
    fn halt_does_not_push_a_classic_run_after_the_workflow_changes() {
        let root = tempfile::tempdir().expect("temp");
        let bare = root.path().join("origin.git");
        let work = root.path().join("work");
        fs::create_dir(&work).expect("work");
        git(
            root.path(),
            &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "relaybook@example.com"]);
        git(&work, &["config", "user.name", "Relaybook"]);
        git(&work, &["config", "commit.gpgsign", "false"]);
        fs::write(work.join("README.md"), "seed\n").expect("write");
        git(&work, &["add", "README.md"]);
        git(&work, &["commit", "-m", "seed"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&work, &["push", "-u", "origin", "main"]);
        git(&work, &["checkout", "-b", "relaybook/prd"]);
        fs::write(work.join("ship.txt"), "ship\n").expect("ship");
        git(&work, &["add", "ship.txt"]);
        git(&work, &["commit", "-m", "ship"]);
        let db = work.join(".relaybook").join("ledger.db");
        let mut ledger = relaybook_core::Ledger::initialize(&db).expect("init");
        ledger.set_setting("mode", "auto").expect("mode");
        let mut run = ledger
            .start_run("prd.md", "abc", "relaybook/prd", Some("classic"))
            .expect("run");
        run.stage = "execute".to_string();
        ledger.save_run(&mut run).expect("save");
        ledger.set_setting("reviewer", "codex").expect("reviewer");
        ledger.set_setting("workflow", "gated").expect("gated");

        let output = super::halt(&work, &mut ledger, &mut run, "timeout");
        assert_eq!(output.code, 1, "{}", output.text);
        assert!(
            output.text.contains("workflow changed during run"),
            "{}",
            output.text
        );
        assert!(
            !output.text.contains("timeout"),
            "timeout halt published the classic reason: {}",
            output.text
        );
        assert!(!bare_has(&bare, "refs/heads/relaybook/prd"));
        let stored = ledger.latest_run().expect("latest").expect("row");
        assert_eq!(stored.status, "stopped");
        assert_eq!(stored.workflow.as_deref(), Some("classic"));
        assert_eq!(
            stored.stop_reason.as_deref(),
            Some("workflow changed during run")
        );
    }

    #[test]
    fn drive_follows_the_claimed_run_when_a_newer_run_is_publishing() {
        let root = tempfile::tempdir().expect("temp");
        let bare = root.path().join("origin.git");
        let work = root.path().join("work");
        fs::create_dir(&work).expect("work");
        git(
            root.path(),
            &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "relaybook@example.com"]);
        git(&work, &["config", "user.name", "Relaybook"]);
        git(&work, &["config", "commit.gpgsign", "false"]);
        fs::write(work.join("README.md"), "seed\n").expect("write");
        git(&work, &["add", "README.md"]);
        git(&work, &["commit", "-m", "seed"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&work, &["push", "-u", "origin", "main"]);
        git(&work, &["checkout", "-b", "relaybook/prd"]);
        fs::write(work.join("ship.txt"), "ship\n").expect("ship");
        git(&work, &["add", "ship.txt"]);
        git(&work, &["commit", "-m", "ship"]);
        let db = work.join(".relaybook").join("ledger.db");
        let mut ledger = relaybook_core::Ledger::initialize(&db).expect("init");
        ledger.set_setting("mode", "auto").expect("mode");
        ledger.set_setting("reviewer", "codex").expect("reviewer");
        ledger.set_setting("workflow", "gated").expect("gated");
        let mut older = ledger
            .start_run("prd.md", "abc", "relaybook/prd", Some("gated"))
            .expect("older");
        older.stage = "release_review".to_string();
        older.status = "stopped".to_string();
        ledger.save_run(&mut older).expect("stop older");
        // One live gated run. The older row stays stopped so the newer one
        // can hold publish. drive must follow the claimed id and not push.
        let mut newer = ledger
            .start_run("prd.md", "abc", "relaybook/prd", Some("gated"))
            .expect("newer");
        newer.stage = "publish".to_string();
        ledger.save_run(&mut newer).expect("newer stage");
        let older_id = older.id;
        let newer_id = newer.id;
        drop(ledger);

        let output = drive(&db, &work, true, older_id);
        assert_eq!(output.code, 1, "{}", output.text);
        assert!(output.text.contains("no active run"), "{}", output.text);
        assert!(!bare_has(&bare, "refs/heads/relaybook/prd"));
        let ledger = relaybook_core::Ledger::open(&db).expect("open");
        let older = ledger.run_by_id(older_id).expect("older");
        let newer = ledger.run_by_id(newer_id).expect("newer");
        assert_eq!(older.status, "stopped");
        assert_eq!(older.stage, "release_review");
        assert_eq!(newer.status, "active");
        assert_eq!(newer.stage, "publish");
    }

    #[cfg(unix)]
    fn init_hooks(work: &Path) -> std::path::PathBuf {
        git(work, &["init", "-b", "main"]);
        let hooks = work.join(".git/hooks");
        fs::create_dir_all(&hooks).expect("hooks");
        hooks
    }

    #[cfg(unix)]
    #[test]
    fn hooks_fingerprint_includes_symlink_target_bytes_and_mode() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir().expect("temp");
        let work = root.path();
        let hooks = init_hooks(work);
        let target = work.join(".git/shared-pre-commit");
        fs::write(&target, b"#!/bin/sh\necho one\n").expect("target");
        symlink("../shared-pre-commit", hooks.join("pre-commit")).expect("symlink");
        let before = super::hooks_fingerprint(work).expect("fingerprint");
        fs::write(&target, b"#!/bin/sh\necho two\n").expect("rewrite");
        let edited = super::hooks_fingerprint(work).expect("fingerprint");
        assert_ne!(
            before, edited,
            "target bytes did not change the fingerprint"
        );
        fs::write(&target, b"#!/bin/sh\necho one\n").expect("restore");
        let restored = super::hooks_fingerprint(work).expect("fingerprint");
        assert_eq!(before, restored);
        let mut permissions = fs::symlink_metadata(&target).expect("meta").permissions();
        permissions.set_mode(permissions.mode() ^ 0o111);
        fs::set_permissions(&target, permissions).expect("chmod");
        let chmodded = super::hooks_fingerprint(work).expect("fingerprint");
        assert_ne!(
            restored, chmodded,
            "target mode did not change the fingerprint"
        );
    }

    #[cfg(unix)]
    #[test]
    fn hooks_fingerprint_rejects_a_symlink_cycle() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("temp");
        let work = root.path();
        let hooks = init_hooks(work);
        symlink("b", hooks.join("a")).expect("a");
        symlink("a", hooks.join("b")).expect("b");
        let err = super::hooks_fingerprint(work).expect_err("cycle");
        assert!(err.starts_with("hooks:"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn hooks_fingerprint_rejects_a_directory_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("temp");
        let work = root.path();
        let hooks = init_hooks(work);
        let outside = work.join("hookdir");
        fs::create_dir_all(&outside).expect("dir");
        fs::write(outside.join("pre-commit"), b"#!/bin/sh\n").expect("script");
        symlink("../../hookdir", hooks.join("linked")).expect("symlink");
        let err = super::hooks_fingerprint(work).expect_err("directory symlink");
        assert!(
            err.starts_with("hooks:") && err.contains("directory"),
            "{err}"
        );
    }
}
