use std::path::{Path, PathBuf};
use std::process::Command;

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
    if !launch.add.is_empty() {
        return add_to_run(&db, &launch.add);
    }
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
    drive(&db, cwd)
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
        if let Err(err) = ledger.save_run(&run) {
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
    sync_main(cwd)?;
    git(cwd, &["checkout", "-B", &branch, "main"])?;
    let run = ledger
        .start_run(&prd_path, &digest, &branch)
        .map_err(|err| err.to_string())?;
    if !new_only {
        let adopted = match adopt_existing(&mut ledger, run.id) {
            Ok(adopted) => adopted,
            Err(err) => {
                if let Ok(Some(mut run)) = ledger.active_run() {
                    run.status = "stopped".to_string();
                    run.stop_reason = Some(err.clone());
                    let _ = ledger.save_run(&run);
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
    if approve && run.stage.starts_with("gate_review") {
        run.stage = "after_review".to_string();
    } else if approve && run.stage == "gate_plan" {
        run.stage = "execute".to_string();
    }
    run.status = "active".to_string();
    if !approve {
        run.stop_reason = None;
    }
    ledger.save_run(&run).map_err(|err| err.to_string())?;
    git(cwd, &["checkout", &run.branch])?;
    println!("{}", agents::account_line(&accounts));
    Ok(())
}

fn drive(db: &Path, cwd: &Path) -> RunOutput {
    let mut transcript = 1_u32;
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
            run.agent_calls += 1;
            if let Err(err) = ledger.save_run(&run) {
                return fail(&err.to_string());
            }
            drop(ledger);
            let before = match snap(cwd, &run.branch) {
                Ok(before) => before,
                Err(err) => return fail(&err),
            };
            let result =
                agents::call_claude(cwd, Role::Planner, &prompt, budget.as_deref(), run_id);
            let _ = agents::write_transcript(
                cwd,
                run_id,
                transcript,
                Role::Planner,
                &result_text(&result),
            );
            transcript += 1;
            if let Err(err) = guard_claude(cwd, &before) {
                return halt_db(db, cwd, &err);
            }
            if let Some(output) = take_agent(db, cwd, &result, "claude") {
                return output;
            }
            if matches!(result, AgentResult::Failed { .. }) {
                return bump_failure(db, cwd);
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
                return output;
            }
            let gates = ledger.settings().map(|item| item.gates).unwrap_or_default();
            run.stage = if gates == "plan" || gates == "plan,review" {
                "gate_plan".to_string()
            } else {
                "execute".to_string()
            };
            println!("{}", plan_line(&run));
            if run.stage == "gate_plan" {
                run.status = "waiting".to_string();
                if let Err(err) = ledger.save_run(&run) {
                    return fail(&err.to_string());
                }
                return ok("waiting for approval: relaybook approve");
            }
            if let Err(err) = ledger.save_run(&run) {
                return fail(&err.to_string());
            }
            continue;
        }
        if run.stage == "review" {
            let Some(slug) = run.current_slug.clone() else {
                run.stage = "execute".to_string();
                if let Err(err) = ledger.save_run(&run) {
                    return fail(&err.to_string());
                }
                continue;
            };
            let prompt = format!("Review {slug}. Use relaybook inbox.");
            let budget = settings.est_cost_cap_usd.clone();
            let run_id = run.id;
            let branch = run.branch.clone();
            run.agent_calls += 1;
            if let Err(err) = ledger.save_run(&run) {
                return fail(&err.to_string());
            }
            drop(ledger);
            let before = match snap(cwd, &branch) {
                Ok(before) => before,
                Err(err) => return fail(&err),
            };
            let result =
                agents::call_claude(cwd, Role::Reviewer, &prompt, budget.as_deref(), run_id);
            let _ = agents::write_transcript(
                cwd,
                run_id,
                transcript,
                Role::Reviewer,
                &result_text(&result),
            );
            transcript += 1;
            if let Err(err) = guard_claude(cwd, &before) {
                return halt_db(db, cwd, &err);
            }
            if let Some(output) = take_agent(db, cwd, &result, "claude") {
                return output;
            }
            if matches!(result, AgentResult::Failed { .. }) {
                return bump_failure(db, cwd);
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
                    if let Err(err) = ledger.save_run(&run) {
                        return fail(&err.to_string());
                    }
                    return bump_failure(db, cwd);
                }
            };
            run.failure_streak = 0;
            println!("{}", review_line(&run, &slug, &verdict, &gates));
            run.stage = format!("gate_review:{verdict}");
            if gates == "plan,review" {
                run.status = "waiting".to_string();
                if let Err(err) = ledger.save_run(&run) {
                    return fail(&err.to_string());
                }
                return ok("waiting for approval: relaybook approve");
            }
            run.stage = "after_review".to_string();
            if let Err(err) = ledger.save_run(&run) {
                return fail(&err.to_string());
            }
            continue;
        }
        // execute, or a gate that was already approved into execute
        let tasks = match ledger.run_tasks(run.id) {
            Ok(tasks) => tasks,
            Err(err) => return fail(&err.to_string()),
        };
        if run.complete_requested && !relaybook_core::unfinished_run(&tasks) {
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
        if let Err(err) = ledger.save_run(&run) {
            return fail(&err.to_string());
        }
        drop(ledger);
        let before = match snap(cwd, &branch) {
            Ok(before) => before,
            Err(err) => return fail(&err),
        };
        let result = agents::call_cursor(cwd, &prompt, timeout, run_id);
        let _ = agents::write_transcript(
            cwd,
            run_id,
            transcript,
            Role::Executor,
            &result_text(&result),
        );
        transcript += 1;
        if let Err(err) = guard_cursor(cwd, &before, &branch) {
            return halt_db(db, cwd, &err);
        }
        if let Some(output) = take_agent(db, cwd, &result, "cursor") {
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
            println!(
                "{}",
                cursor_line(&run, &slug, "blocked", 0, &settings.est_cost_cap_usd)
            );
            return halt(cwd, &mut ledger, &mut run, &format!("blocked: {slug}"));
        }
        if !done || report_bytes.is_none() {
            run.failure_streak += 1;
            if let Err(err) = ledger.save_run(&run) {
                return fail(&err.to_string());
            }
            if run.failure_streak >= 2 {
                return halt(cwd, &mut ledger, &mut run, "two consecutive agent failures");
            }
            continue;
        }
        run.failure_streak = 0;
        run.stage = "review".to_string();
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
        if let Err(err) = ledger.save_run(&run) {
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
    if let Err(err) = push_branch(cwd, &run.branch) {
        return Control::Return(halt(cwd, ledger, run, &err));
    }
    let tasks = ledger.run_tasks(run.id).unwrap_or_default();
    if run.complete_requested && !relaybook_core::unfinished_run(&tasks) {
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
    let _ = push_branch(cwd, &run.branch);
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

fn bump_failure(db: &Path, cwd: &Path) -> RunOutput {
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
        if let Err(err) = ledger.save_run(&run) {
            return fail(&err.to_string());
        }
        return fail("stopped: two consecutive agent failures; relaybook run --resume");
    }
    if let Err(err) = ledger.save_run(&run) {
        return fail(&err.to_string());
    }
    drive(db, cwd)
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
            if let Err(err) = ledger.save_run(&run) {
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
}

fn snap(cwd: &Path, branch: &str) -> Result<Snap, String> {
    Ok(Snap {
        head: git(cwd, &["rev-parse", "HEAD"])?,
        branch: git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?,
        main: git(cwd, &["rev-parse", "main"])?,
        origin_main: git(cwd, &["rev-parse", "origin/main"]).ok(),
        origin_branch: git(cwd, &["rev-parse", &format!("origin/{branch}")]).ok(),
        tags: tag_list(cwd),
        tree: worktree_fingerprint(cwd)?,
    })
}

fn guard_claude(cwd: &Path, before: &Snap) -> Result<(), String> {
    let after = snap(cwd, &before.branch)?;
    shared_guard(before, &after)?;
    if after.head != before.head || after.tree != before.tree {
        return Err(format!(
            "claude edited the worktree: {} -> {}",
            before.head, after.head
        ));
    }
    Ok(())
}

fn guard_cursor(cwd: &Path, before: &Snap, branch: &str) -> Result<(), String> {
    let after = snap(cwd, branch)?;
    shared_guard(before, &after)?;
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

fn dirty(cwd: &Path) -> Result<bool, String> {
    Ok(!dirty_paths(cwd)?.is_empty())
}

fn worktree_fingerprint(cwd: &Path) -> Result<String, String> {
    let diff = git(cwd, &["diff", "HEAD", "--", ".", ":(exclude).relaybook"])?;
    let untracked = git(cwd, &["ls-files", "--others", "--exclude-standard"])?;
    let mut body = diff;
    body.push('\n');
    for path in untracked.lines() {
        let path = git_path(path);
        if path == ".relaybook" || path.starts_with(".relaybook/") {
            continue;
        }
        body.push_str(&path);
        body.push('\n');
        let bytes = std::fs::read(cwd.join(&path)).unwrap_or_default();
        body.push_str(&content_hash(&bytes));
        body.push('\n');
    }
    Ok(content_hash(body.as_bytes()))
}

fn git_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn content_hash(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn dirty_paths(cwd: &Path) -> Result<Vec<String>, String> {
    let text = git(cwd, &["status", "--porcelain"])?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let path = line.get(3..).unwrap_or(line).trim().trim_matches('"');
            let path = path.rsplit(" -> ").next().unwrap_or(path);
            let path = git_path(path);
            if path == ".relaybook" || path.starts_with(".relaybook/") {
                None
            } else {
                Some(path)
            }
        })
        .collect())
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
