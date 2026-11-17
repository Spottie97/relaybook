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
    if launch.approve || launch.resume {
        if let Err(err) = reopen_run(&db, cwd, launch.approve) {
            return fail(&err);
        }
    } else if let Err(err) = begin_run(&db, cwd, launch.prd.as_deref(), launch.new_only) {
        return fail(&err);
    }
    drive(&db, cwd, launch.quiet)
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

fn begin_run(db: &Path, cwd: &Path, prd: Option<&Path>, new_only: bool) -> Result<(), String> {
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
    Ok(())
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

fn reopen_run(db: &Path, cwd: &Path, approve: bool) -> Result<(), String> {
    let accounts = agents::preflight()?;
    let mut ledger = Ledger::open(db).map_err(|err| err.to_string())?;
    let Some(mut run) = ledger.resumable_run().map_err(|err| err.to_string())? else {
        return Err("no run to resume".to_string());
    };
    if approve && run.status != "waiting" {
        return Err("run is not waiting".to_string());
    }
    let settings = ledger.settings().map_err(|err| err.to_string())?;
    if workflow_changed(&run, &settings) {
        let output = stop_workflow_mismatch(&mut ledger, &mut run);
        return Err(output.text.trim_end().to_string());
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
        // One approve owns publication. The loser keeps the waiting row and
        // does not check out, screen, or push.
        run.stage = "publish".to_string();
        run.status = "active".to_string();
        if !approve {
            run.stop_reason = None;
        }
        ledger.save_run(&mut run).map_err(|err| err.to_string())?;
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
        ledger.save_run(&mut run).map_err(|err| err.to_string())?;
        git(cwd, &["checkout", &run.branch])?;
    }
    println!("{}", agents::account_line(&accounts));
    Ok(())
}

fn drive(db: &Path, cwd: &Path, quiet: bool) -> RunOutput {
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
        let Some(mut run) = (match ledger.active_run() {
            Ok(run) => run,
            Err(err) => return fail(&err.to_string()),
        }) else {
            return fail("no active run");
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
            let prompt = if adopted.is_empty() {
                format!("PRD:\n{prd}")
            } else {
                format!(
                    "PRD:\n{prd}\nAdopted tasks: {adopted}\nBuild on these tasks. Do not create duplicates.\n"
                )
            };
            let budget = settings.est_cost_cap_usd.clone();
            let run_id = run.id;
            let gated = is_gated(&run);
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
                return halt_db(db, cwd, &err);
            }
            if let Some(output) = take_agent(db, cwd, &result, "claude") {
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
                return bump_failure(db, cwd, quiet);
            }
            let mut ledger = match Ledger::open(db) {
                Ok(ledger) => ledger,
                Err(err) => return fail(&err.to_string()),
            };
            let Some(mut run) = (match ledger.active_run() {
                Ok(run) => run,
                Err(err) => return fail(&err.to_string()),
            }) else {
                return fail("no active run");
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
                return halt(cwd, &mut ledger, &mut run, "codex routing not built");
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
                return halt_db(db, cwd, &err);
            }
            if let Some(output) = take_agent(db, cwd, &result, "claude") {
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
                return bump_failure(db, cwd, quiet);
            }
            let mut ledger = match Ledger::open(db) {
                Ok(ledger) => ledger,
                Err(err) => return fail(&err.to_string()),
            };
            let Some(mut run) = (match ledger.active_run() {
                Ok(run) => run,
                Err(err) => return fail(&err.to_string()),
            }) else {
                return fail("no active run");
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
                    return bump_failure(db, cwd, quiet);
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
            return halt(cwd, &mut ledger, &mut run, "codex routing not built");
        }
        if run.stage == "publish" && is_gated(&run) {
            return publish(cwd, &mut ledger, &mut run);
        }
        if run.stage == "gate_release" && is_gated(&run) {
            return halt(cwd, &mut ledger, &mut run, &signoff_wait(cwd));
        }
        // execute, or a gate that was already approved into execute
        let tasks = match ledger.run_tasks(run.id) {
            Ok(tasks) => tasks,
            Err(err) => return fail(&err.to_string()),
        };
        if run.complete_requested && !relaybook_core::unfinished_run(&tasks) {
            if is_gated(&run) {
                return enter_release_review(cwd, &mut ledger, &mut run);
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
                    return enter_release_review(cwd, &mut ledger, &mut run);
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
        if new_task {
            run.cycle += 1;
            run.current_slug = Some(slug.clone());
        }
        run.stage = "execute".to_string();
        run.agent_calls += 1;
        let timeout = agents::cursor_timeout(settings.cursor_timeout_min);
        let prompt = format!("{}\nTask slug: {slug}\n", agents::executor_template());
        let run_id = run.id;
        let branch = run.branch.clone();
        let gated = is_gated(&run);
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
            return halt_db(db, cwd, &err);
        }
        if let Some(output) = take_agent(db, cwd, &result, "cursor") {
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
        let Some(mut run) = (match ledger.active_run() {
            Ok(run) => run,
            Err(err) => return fail(&err.to_string()),
        }) else {
            return fail("no active run");
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
    if !is_gated(run)
        && let Err(err) = push_branch(cwd, &run.branch)
    {
        return Control::Return(halt(cwd, ledger, run, &err));
    }
    let tasks = ledger.run_tasks(run.id).unwrap_or_default();
    if run.complete_requested && !relaybook_core::unfinished_run(&tasks) {
        if is_gated(run) {
            return Control::Return(enter_release_review(cwd, ledger, run));
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
    if !is_gated(run) {
        let _ = push_branch(cwd, &run.branch);
    }
    stop_run(ledger, run, reason)
}

/// A workflow mismatch stops the run and does not push. `halt` would push
/// when the stored workflow is classic or unset, which would publish a run
/// after the ledger had switched to gated.
fn stop_workflow_mismatch(ledger: &mut Ledger, run: &mut Run) -> RunOutput {
    stop_run(ledger, run, "workflow changed during run")
}

fn stop_run(ledger: &mut Ledger, run: &mut Run, reason: &str) -> RunOutput {
    run.status = "stopped".to_string();
    run.stop_reason = Some(reason.to_string());
    if let Err(err) = ledger.save_run(run) {
        return fail(&err.to_string());
    }
    fail(&format!("stopped: {reason}; relaybook run --resume"))
}

fn halt_db(db: &Path, cwd: &Path, reason: &str) -> RunOutput {
    let Ok(mut ledger) = Ledger::open(db) else {
        return fail(reason);
    };
    let Ok(Some(mut run)) = ledger.active_run() else {
        return fail(reason);
    };
    halt(cwd, &mut ledger, &mut run, reason)
}

fn bump_failure(db: &Path, cwd: &Path, quiet: bool) -> RunOutput {
    let Ok(mut ledger) = Ledger::open(db) else {
        return fail("no ledger");
    };
    let Ok(Some(mut run)) = ledger.active_run() else {
        return fail("no active run");
    };
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
    drive(db, cwd, quiet)
}

fn take_agent(db: &Path, cwd: &Path, result: &AgentResult, cli: &str) -> Option<RunOutput> {
    match result {
        AgentResult::UsageLimit { reset_at, .. } => {
            let Ok(mut ledger) = Ledger::open(db) else {
                return Some(fail("no ledger"));
            };
            let Ok(Some(mut run)) = ledger.active_run() else {
                return Some(fail("no active run"));
            };
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
        AgentResult::TimedOut => Some(halt_db(db, cwd, "timeout")),
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

fn enter_release_review(cwd: &Path, ledger: &mut Ledger, run: &mut Run) -> RunOutput {
    run.stage = "release_review".to_string();
    if let Err(err) = ledger.save_run(run) {
        return fail(&err.to_string());
    }
    halt(cwd, ledger, run, "codex routing not built")
}

fn publish(cwd: &Path, ledger: &mut Ledger, run: &mut Run) -> RunOutput {
    // Take the run before any publish screen. A stale snapshot fails here
    // and must not push or save over the owner.
    run.stage = "publish".to_string();
    run.status = "active".to_string();
    if let Err(err) = ledger.save_run(run) {
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
    let spec = format!("{head}:refs/heads/{}", run.branch);
    if let Err(err) = git(cwd, &["push", "--no-follow-tags", "-u", "origin", &spec]) {
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
            ledger.set_setting("reviewer", "codex").expect("reviewer");
            ledger.set_setting("workflow", "gated").expect("gated");
            drop(ledger);

            let output = drive(&db, &work, true);
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
}
