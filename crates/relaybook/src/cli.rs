use std::path::Path;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use relaybook_core::{Error, Ledger, RECENT_EVENT_LIMIT, Status};

use crate::gitinfo;
use crate::install;
use crate::paths;
use crate::picker::{self, Pick};
use crate::render;
use crate::runloop;
use crate::setup;
use crate::uninstall;

pub(crate) fn execute() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => err.exit(),
    };
    let json = cli.json;
    if matches!(&cli.command, Command::Hook(HookCmd::Prompt)) {
        hook_prompt();
        return ExitCode::SUCCESS;
    }
    if let Err(err) = announce_legacy_move() {
        if json {
            print!("{}", envelope(false, None, Some(&err.to_string())));
        } else {
            eprintln!("{err}");
        }
        return ExitCode::from(err.code());
    }
    match dispatch(cli) {
        Ok(outcome) => {
            if json {
                print!(
                    "{}",
                    envelope(
                        outcome.code == 0,
                        Some(&outcome.data),
                        outcome.error.as_deref()
                    )
                );
            } else {
                print!("{}", outcome.human);
            }
            ExitCode::from(outcome.code)
        }
        Err(err) => {
            if json {
                print!("{}", envelope(false, None, Some(&err.to_string())));
            } else {
                eprintln!("{err}");
            }
            ExitCode::from(err.code())
        }
    }
}

#[derive(Parser)]
#[command(
    name = "relaybook",
    version,
    about = "Shared coordination ledger for coding agents",
    arg_required_else_help = true
)]
struct Cli {
    /// Print a JSON envelope on stdout.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the ledger at the repository root.
    Init,
    /// Print tasks, steps, decisions, and recent events.
    Context {
        slug: Option<String>,
        /// Include done tasks and the full history.
        #[arg(long)]
        all: bool,
        /// Append the last N events as one-line summaries.
        #[arg(long)]
        events: Option<usize>,
    },
    #[command(subcommand)]
    Task(TaskCmd),
    #[command(subcommand)]
    Step(StepCmd),
    #[command(subcommand)]
    Progress(ProgressCmd),
    #[command(subcommand)]
    Decision(DecisionCmd),
    #[command(subcommand)]
    Report(ReportCmd),
    /// List unreviewed reports.
    Inbox,
    /// Print or set manual/auto mode.
    Mode {
        /// `manual` or `auto`. Omit to print the current mode.
        value: Option<String>,
    },
    /// Print or set the auto-run checkpoints.
    Gates {
        /// `none`, `plan`, or `plan,review`. Omit to print the current value.
        value: Option<String>,
    },
    /// List or change auto-run settings.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Print mode, limits, and the active run.
    Status,
    /// Run the auto-mode loop, or stop and complete it.
    Run {
        /// PRD file to implement. Same as the positional path.
        #[arg(long)]
        prd: Option<std::path::PathBuf>,
        /// PRD file to implement.
        prd_path: Option<std::path::PathBuf>,
        /// Resume a stopped, paused, or waiting run.
        #[arg(long)]
        resume: bool,
        #[arg(long)]
        max_cycles: Option<i64>,
        #[arg(long)]
        max_agent_calls: Option<i64>,
        #[arg(long)]
        max_reopens: Option<i64>,
        #[arg(long)]
        cursor_timeout_min: Option<i64>,
        #[arg(long)]
        est_cost_cap_usd: Option<String>,
        #[arg(long)]
        gates: Option<String>,
        #[command(subcommand)]
        action: Option<RunAction>,
    },
    /// Resume a run that is waiting for approval.
    Approve,
    #[command(subcommand)]
    Hook(HookCmd),
    /// Append coordination instructions to CLAUDE.md and AGENTS.md.
    Install {
        /// Append the planner block to CLAUDE.md.
        #[arg(long)]
        claude: bool,
        /// Append the executor block to AGENTS.md.
        #[arg(long)]
        cursor: bool,
    },
    /// Create the ledger, install agent instructions, and set up Graphify.
    Setup {
        /// Skip every Graphify command and the pull hooks.
        #[arg(long)]
        no_graphify: bool,
        /// Skip git hook installation.
        #[arg(long)]
        no_hooks: bool,
        /// Skip the Claude Code UserPromptSubmit hook.
        #[arg(long)]
        no_claude_hook: bool,
        /// Skip the Claude and Cursor /relaybook commands.
        #[arg(long)]
        no_commands: bool,
        /// Write the opt-in GitHub Actions workflow and graph.json merge driver.
        #[arg(long)]
        ci: bool,
    },
    /// Remove what `relaybook setup` added to this project.
    Uninstall {
        /// Apply changes without asking.
        #[arg(long)]
        yes: bool,
        /// Print the changes and do not touch files.
        #[arg(long)]
        dry_run: bool,
        /// Also delete `.relaybook/` and `graphify-out/`.
        #[arg(long)]
        purge: bool,
        /// Also run Graphify's own project uninstall commands.
        #[arg(long)]
        graphify: bool,
    },
}

#[derive(Subcommand)]
enum TaskCmd {
    /// Create an open task.
    Create {
        slug: String,
        /// Title. A leading dash is part of the title.
        #[arg(long, allow_hyphen_values = true)]
        title: String,
        /// Slug this task must wait for. Repeat the flag for more than one.
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
    },
    /// List tasks, optionally filtered by status.
    List {
        #[arg(long)]
        status: Option<StatusOpt>,
        /// Open tasks whose dependencies are all done.
        #[arg(long)]
        ready: bool,
        /// Include done tasks.
        #[arg(long)]
        all: bool,
    },
    /// Record that a task waits for another task.
    Depend {
        slug: String,
        #[arg(long)]
        on: String,
    },
    /// Remove a dependency link.
    Undepend {
        slug: String,
        #[arg(long)]
        on: String,
    },
    /// Claim an open task. Fails when another agent already holds it.
    Claim {
        slug: String,
        #[arg(long)]
        by: String,
    },
    /// Send a done task back to the ready list.
    Reopen {
        slug: String,
        /// Why it is open again. A leading dash is part of the reason.
        #[arg(long, allow_hyphen_values = true)]
        reason: String,
    },
    /// Mark a claimed task done and attach a short report.
    Done {
        slug: String,
        /// What changed. Repeat for more than one line. At least one is required.
        #[arg(long, allow_hyphen_values = true)]
        changed: Vec<String>,
        /// What was verified.
        #[arg(long, allow_hyphen_values = true)]
        verified: Vec<String>,
        /// What was not verified.
        #[arg(long = "not-verified", allow_hyphen_values = true)]
        not_verified: Vec<String>,
        /// Where the work left the plan.
        #[arg(long, allow_hyphen_values = true)]
        deviations: Vec<String>,
        /// What the planner should look at.
        #[arg(long, allow_hyphen_values = true)]
        review: Vec<String>,
        /// Finish without section text. An empty report is still stored.
        #[arg(long)]
        no_report: bool,
    },
    /// Mark an open or claimed task blocked.
    Block {
        slug: String,
        /// Why the work is stuck. A leading dash is part of the reason.
        #[arg(long, allow_hyphen_values = true)]
        reason: String,
    },
    /// Return a blocked task to open, or to claimed when it still has an owner.
    Unblock {
        slug: String,
        /// Note stored with the unblock. A leading dash is part of the note.
        #[arg(long, allow_hyphen_values = true)]
        note: Option<String>,
    },
    /// Change a task's title. The slug stays the same.
    Rename {
        slug: String,
        /// New title. A leading dash is part of the title.
        #[arg(long, allow_hyphen_values = true)]
        title: String,
    },
}

#[derive(Subcommand)]
enum RunAction {
    /// End the active run.
    Stop,
    /// Mark the PRD covered. The loop opens the PR when nothing is open.
    Complete,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Change one setting.
    Set { key: String, value: String },
}

#[derive(Subcommand)]
enum HookCmd {
    /// Print one line when unreviewed reports are waiting. Always exits 0.
    Prompt,
}

#[derive(Subcommand)]
enum ReportCmd {
    /// Mark a task report reviewed.
    Accept {
        slug: Option<String>,
        /// Accept a session note by id.
        #[arg(long)]
        note: Option<i64>,
        /// Accept every unreviewed report.
        #[arg(long)]
        all: bool,
    },
    /// Record a short note that is not tied to one task.
    Note {
        #[arg(long)]
        by: String,
        /// Note text. Omit this to read the note from stdin. A leading dash is part of the text.
        #[arg(long, allow_hyphen_values = true)]
        text: Option<String>,
    },
}

#[derive(Subcommand)]
enum StepCmd {
    /// Append a checklist step. Positions start at 1.
    Add {
        slug: String,
        /// Step text. A leading dash is part of the text. Pass this or the positional, not both.
        #[arg(long, allow_hyphen_values = true)]
        text: Option<String>,
        /// Step text when it does not start with a dash.
        description: Option<String>,
    },
    /// Mark checklist step n done.
    Check { slug: String, n: i64 },
    /// Replace the text of checklist step n.
    Edit {
        slug: String,
        n: i64,
        /// New step text. A leading dash is part of the text.
        #[arg(long, allow_hyphen_values = true)]
        text: String,
    },
}

#[derive(Subcommand)]
enum ProgressCmd {
    /// Record progress once for the current git commit.
    Report {
        slug: String,
        /// Progress note. A leading dash is part of the note.
        #[arg(long, allow_hyphen_values = true)]
        note: String,
    },
}

#[derive(Subcommand)]
enum DecisionCmd {
    /// Record why a choice was made, and who made it.
    Log {
        slug: String,
        /// Why the choice was made. A leading dash is part of the text.
        #[arg(long, allow_hyphen_values = true)]
        why: String,
        #[arg(long)]
        by: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum StatusOpt {
    Open,
    Claimed,
    Done,
    Blocked,
}

impl StatusOpt {
    fn status(self) -> Status {
        match self {
            Self::Open => Status::Open,
            Self::Claimed => Status::Claimed,
            Self::Done => Status::Done,
            Self::Blocked => Status::Blocked,
        }
    }
}

struct Outcome {
    data: serde_json::Value,
    human: String,
    code: u8,
    error: Option<String>,
}

enum Fail {
    Core(Error),
    Io(std::io::Error),
    NoLedger,
    Usage(String),
}

impl Fail {
    fn code(&self) -> u8 {
        match self {
            Self::NoLedger | Self::Usage(_) => 2,
            Self::Core(_) | Self::Io(_) => 1,
        }
    }
}

impl std::fmt::Display for Fail {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core(err) => write!(formatter, "{err}"),
            Self::Io(err) => write!(formatter, "io: {err}"),
            Self::NoLedger => formatter.write_str("no ledger found (run `relaybook init`)"),
            Self::Usage(message) => formatter.write_str(message),
        }
    }
}

impl From<Error> for Fail {
    fn from(err: Error) -> Self {
        Self::Core(err)
    }
}

impl From<std::io::Error> for Fail {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<setup::Error> for Fail {
    fn from(err: setup::Error) -> Self {
        match err {
            setup::Error::Core(err) => Self::Core(err),
            setup::Error::Io(err) => Self::Io(err),
        }
    }
}

fn announce_legacy_move() -> Result<(), Fail> {
    let cwd = std::env::current_dir()?;
    if let Some(moved) = paths::migrate_legacy(&cwd)? {
        eprintln!(
            "moved ledger from {} to {}",
            moved.from.display(),
            moved.to.display()
        );
    }
    Ok(())
}

fn dispatch(cli: Cli) -> Result<Outcome, Fail> {
    let cwd = std::env::current_dir()?;
    match cli.command {
        Command::Init => init_ledger(&cwd),
        Command::Context { slug, all, events } => show_context(&cwd, slug.as_deref(), all, events),
        Command::Task(TaskCmd::Create {
            slug,
            title,
            depends_on,
        }) => create_task(&cwd, &slug, &title, &depends_on),
        Command::Task(TaskCmd::List { status, ready, all }) => {
            list_tasks(&cwd, status.map(StatusOpt::status), ready, all)
        }
        Command::Task(TaskCmd::Depend { slug, on }) => depend(&cwd, &slug, &on),
        Command::Task(TaskCmd::Undepend { slug, on }) => undepend(&cwd, &slug, &on),
        Command::Task(TaskCmd::Claim { slug, by }) => claim_task(&cwd, &slug, &by),
        Command::Task(TaskCmd::Reopen { slug, reason }) => reopen_task(&cwd, &slug, &reason),
        Command::Task(TaskCmd::Done {
            slug,
            changed,
            verified,
            not_verified,
            deviations,
            review,
            no_report,
        }) => {
            let sections = report_sections(
                &changed,
                &verified,
                &not_verified,
                &deviations,
                &review,
                no_report,
            )?;
            complete_task(&cwd, &slug, sections)
        }
        Command::Task(TaskCmd::Block { slug, reason }) => block_task(&cwd, &slug, &reason),
        Command::Task(TaskCmd::Unblock { slug, note }) => {
            unblock_task(&cwd, &slug, note.as_deref())
        }
        Command::Task(TaskCmd::Rename { slug, title }) => rename_task(&cwd, &slug, &title),
        Command::Step(StepCmd::Add {
            slug,
            description,
            text,
        }) => {
            let description = one_step_text(description, text)?;
            add_step(&cwd, &slug, &description)
        }
        Command::Step(StepCmd::Check { slug, n }) => check_step(&cwd, &slug, n),
        Command::Step(StepCmd::Edit { slug, n, text }) => edit_step(&cwd, &slug, n, &text),
        Command::Progress(ProgressCmd::Report { slug, note }) => {
            report_progress(&cwd, &slug, &note)
        }
        Command::Decision(DecisionCmd::Log { slug, why, by }) => {
            log_decision(&cwd, &slug, &why, &by)
        }
        Command::Report(ReportCmd::Accept { slug, note, all }) => {
            accept_report(&cwd, slug.as_deref(), note, all)
        }
        Command::Report(ReportCmd::Note { by, text }) => add_note(&cwd, &by, text),
        Command::Inbox => show_inbox(&cwd),
        Command::Mode { value } => match value {
            Some(value) => set_mode(&cwd, &value),
            None => choose_setting(&cwd, "mode", picker::MODES, set_mode, show_mode),
        },
        Command::Gates { value } => match value {
            Some(value) => set_gates(&cwd, &value),
            None => choose_setting(&cwd, "gates", picker::GATES, set_gates, show_gates),
        },
        Command::Config { action } => match action {
            Some(ConfigAction::Set { key, value }) => set_config(&cwd, &key, &value),
            None => show_config(&cwd),
        },
        Command::Status => show_status(&cwd),
        Command::Run {
            prd,
            prd_path,
            resume,
            max_cycles,
            max_agent_calls,
            max_reopens,
            cursor_timeout_min,
            est_cost_cap_usd,
            gates,
            action,
        } => {
            let prd = match (prd, prd_path) {
                (Some(_), Some(_)) => {
                    return Err(Fail::Usage("pass either a PRD path or --prd".to_string()));
                }
                (Some(path), None) | (None, Some(path)) => Some(path),
                (None, None) => None,
            };
            run_auto(
                &cwd,
                runloop::Launch {
                    prd,
                    resume,
                    approve: false,
                    stop: matches!(action, Some(RunAction::Stop)),
                    complete: matches!(action, Some(RunAction::Complete)),
                    max_cycles,
                    max_agent_calls,
                    max_reopens,
                    cursor_timeout_min,
                    est_cost_cap_usd,
                    gates,
                },
            )
        }
        Command::Approve => run_auto(
            &cwd,
            runloop::Launch {
                prd: None,
                resume: false,
                approve: true,
                stop: false,
                complete: false,
                max_cycles: None,
                max_agent_calls: None,
                max_reopens: None,
                cursor_timeout_min: None,
                est_cost_cap_usd: None,
                gates: None,
            },
        ),
        Command::Hook(HookCmd::Prompt) => Ok(Outcome {
            human: String::new(),
            data: serde_json::json!({}),
            error: None,
            code: 0,
        }),
        Command::Install { claude, cursor } => install_docs(&cwd, claude, cursor),
        Command::Setup {
            no_graphify,
            no_hooks,
            no_claude_hook,
            no_commands,
            ci,
        } => setup_project(
            &cwd,
            setup::Options {
                no_graphify,
                no_hooks,
                no_claude_hook,
                no_commands,
                ci,
            },
        ),
        Command::Uninstall {
            yes,
            dry_run,
            purge,
            graphify,
        } => uninstall_project(
            &cwd,
            uninstall::Options {
                yes,
                dry_run,
                purge,
                graphify,
            },
        ),
    }
}

fn init_ledger(cwd: &Path) -> Result<Outcome, Fail> {
    let path = paths::init_root(cwd).join(".relaybook").join("ledger.db");
    let created = !path.exists();
    let ledger = Ledger::initialize(&path)?;
    drop(ledger);
    let data = serde_json::json!({
        "path": path.display().to_string(),
        "created": created,
    });
    let human = if created {
        render::line(&format!("initialized {}", path.display()))
    } else {
        render::line(&format!("ledger already exists at {}", path.display()))
    };
    Ok(Outcome {
        data,
        human,
        code: 0,
        error: None,
    })
}

fn create_task(
    cwd: &Path,
    slug: &str,
    title: &str,
    depends_on: &[String],
) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.create_task_depending(slug, title, depends_on)?;
    if let Some(run_id) = std::env::var("RELAYBOOK_RUN_ID")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
    {
        ledger.assign_run(&task.slug, run_id)?;
    }
    let human = render::line(&format!("created {}", task.slug));
    json_outcome(task_ack(&task), human)
}

fn depend(cwd: &Path, slug: &str, on: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.add_dependency(slug, on)?;
    let human = render::line(&format!("{slug} depends on {on}"));
    json_outcome(task_ack(&task), human)
}

fn undepend(cwd: &Path, slug: &str, on: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.remove_dependency(slug, on)?;
    let human = render::line(&format!("{slug} no longer depends on {on}"));
    json_outcome(task_ack(&task), human)
}

fn list_tasks(cwd: &Path, status: Option<Status>, ready: bool, all: bool) -> Result<Outcome, Fail> {
    let ledger = open_ledger(cwd)?;
    let include_done = all || status.is_some() || ready;
    let tasks = ledger.list_tasks(status, ready, include_done)?;
    let hidden_done = if include_done {
        0
    } else {
        ledger.list_tasks(Some(Status::Done), false, true)?.len()
    };
    let mut human = render::tasks(&tasks);
    if hidden_done > 0 {
        human.push_str(&render::line(&format!(
            "{hidden_done} done tasks hidden (relaybook task list --all)"
        )));
    }
    json_outcome(
        serde_json::json!({
            "tasks": tasks,
            "hidden_done": hidden_done,
        }),
        human,
    )
}

fn claim_task(cwd: &Path, slug: &str, by: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.claim_task(slug, by)?;
    let agent = task.claimed_by.clone().unwrap_or_default();
    let human = render::line(&format!("claimed {} by {agent}", task.slug));
    json_outcome(task_ack(&task), human)
}

fn report_sections(
    changed: &[String],
    verified: &[String],
    not_verified: &[String],
    deviations: &[String],
    review: &[String],
    no_report: bool,
) -> Result<Option<relaybook_core::Sections>, Fail> {
    let sections = if no_report {
        if !changed.is_empty()
            || !verified.is_empty()
            || !not_verified.is_empty()
            || !deviations.is_empty()
            || !review.is_empty()
        {
            return Err(Fail::Core(relaybook_core::Error::ReportOrNone));
        }
        None
    } else {
        let changed = kept_lines(changed);
        if changed.is_empty() {
            return Err(Fail::Core(relaybook_core::Error::ChangedRequired));
        }
        Some(relaybook_core::Sections {
            changed,
            verified: kept_lines(verified),
            not_verified: kept_lines(not_verified),
            deviations: kept_lines(deviations),
            review: kept_lines(review),
        })
    };
    Ok(sections)
}

fn complete_task(
    cwd: &Path,
    slug: &str,
    sections: Option<relaybook_core::Sections>,
) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.finish_task(slug, sections)?;
    let human = render::line(&format!("done {}", task.slug));
    json_outcome(task_ack(&task), human)
}

fn reopen_task(cwd: &Path, slug: &str, reason: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.reopen_task(slug, reason)?;
    let human = render::line(&format!("reopened {}", task.slug));
    json_outcome(task_ack(&task), human)
}

fn show_inbox(cwd: &Path) -> Result<Outcome, Fail> {
    let ledger = open_ledger(cwd)?;
    let items = ledger.inbox()?;
    let human = render::inbox(&items);
    json_outcome(serde_json::json!({ "reports": items }), human)
}

fn accept_report(
    cwd: &Path,
    slug: Option<&str>,
    note: Option<i64>,
    all: bool,
) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    if all {
        let count = ledger.accept_all_reports()?;
        return json_outcome(
            serde_json::json!({ "accepted": count }),
            render::line(&format!("accepted {count}")),
        );
    }
    if let Some(id) = note {
        ledger.accept_note(id)?;
        return json_outcome(
            serde_json::json!({ "id": id, "verdict": "accepted" }),
            render::line(&format!("accepted note {id}")),
        );
    }
    let Some(slug) = slug else {
        return Err(Fail::Usage("pass a slug, --note, or --all".to_string()));
    };
    ledger.accept_report(slug)?;
    json_outcome(
        serde_json::json!({ "slug": slug, "verdict": "accepted" }),
        render::line(&format!("accepted {slug}")),
    )
}

fn hook_prompt() {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Some(path) = paths::ledger_path(&cwd) else {
        return;
    };
    let Ok(ledger) = Ledger::open_readonly(&path) else {
        return;
    };
    let Ok(count) = ledger.unreviewed_count() else {
        return;
    };
    if count > 0 {
        println!("{count} new Cursor reports: relaybook inbox");
    }
    if let Ok(Some(line)) = ledger.hook_wait_line() {
        println!("{line}");
    }
}

fn run_auto(cwd: &Path, launch: runloop::Launch) -> Result<Outcome, Fail> {
    let output = runloop::execute(cwd, launch);
    let trimmed = output.text.trim().to_string();
    Ok(Outcome {
        data: serde_json::json!({ "text": trimmed }),
        human: output.text,
        code: output.code,
        error: if output.code == 0 {
            None
        } else {
            Some(trimmed)
        },
    })
}

fn choose_setting(
    cwd: &Path,
    label: &str,
    options: &[&str],
    set: fn(&Path, &str) -> Result<Outcome, Fail>,
    show: fn(&Path) -> Result<Outcome, Fail>,
) -> Result<Outcome, Fail> {
    let ledger = open_ledger(cwd)?;
    let settings = ledger.settings()?;
    let current = match label {
        "mode" => settings.mode,
        _ => settings.gates,
    };
    match picker::pick(label, options, &current).map_err(Fail::Usage)? {
        Pick::UseExisting => show(cwd),
        Pick::Cancelled => json_outcome(serde_json::json!({ "unchanged": current }), String::new()),
        Pick::Selected(value) => set(cwd, &value),
    }
}

fn show_gates(cwd: &Path) -> Result<Outcome, Fail> {
    let ledger = open_ledger(cwd)?;
    let settings = ledger.settings()?;
    json_outcome(
        serde_json::json!({ "gates": settings.gates }),
        render::line(&settings.gates),
    )
}

fn set_gates(cwd: &Path, value: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let settings = ledger.set_setting("gates", value)?;
    json_outcome(
        serde_json::json!({ "gates": settings.gates }),
        render::line(&format!("gates {}", settings.gates)),
    )
}

fn show_mode(cwd: &Path) -> Result<Outcome, Fail> {
    let ledger = open_ledger(cwd)?;
    let settings = ledger.settings()?;
    json_outcome(
        serde_json::json!({ "mode": settings.mode }),
        render::line(&settings.mode),
    )
}

fn set_mode(cwd: &Path, value: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let settings = ledger.set_setting("mode", value)?;
    json_outcome(
        serde_json::json!({ "mode": settings.mode }),
        render::line(&format!("mode {}", settings.mode)),
    )
}

fn show_config(cwd: &Path) -> Result<Outcome, Fail> {
    let ledger = open_ledger(cwd)?;
    let settings = ledger.settings()?;
    json_outcome(settings.clone(), relaybook_core::format_config(&settings))
}

fn set_config(cwd: &Path, key: &str, value: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let settings = ledger.set_setting(key, value)?;
    let shown = match key {
        "est_cost_cap_usd" => settings
            .est_cost_cap_usd
            .clone()
            .unwrap_or_else(|| "unset".to_string()),
        "mode" => settings.mode.clone(),
        "gates" => settings.gates.clone(),
        "max_cycles" => settings.max_cycles.to_string(),
        "max_reopens_per_task" => settings.max_reopens_per_task.to_string(),
        "max_agent_calls" => settings.max_agent_calls.to_string(),
        "cursor_timeout_min" => settings.cursor_timeout_min.to_string(),
        _ => value.to_string(),
    };
    json_outcome(settings, render::line(&format!("{key} {shown}")))
}

fn show_status(cwd: &Path) -> Result<Outcome, Fail> {
    let ledger = open_ledger(cwd)?;
    let view = ledger.status_view()?;
    json_outcome(view.clone(), relaybook_core::format_status(&view))
}

fn kept_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn add_note(cwd: &Path, author: &str, text: Option<String>) -> Result<Outcome, Fail> {
    let text = match text {
        Some(text) => text,
        None => {
            let mut buffer = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer)?;
            buffer
        }
    };
    let mut ledger = open_ledger(cwd)?;
    let id = ledger.add_note(author, &text)?;
    let human = render::line(&format!("noted {id}"));
    json_outcome(serde_json::json!({ "id": id, "kind": "note" }), human)
}

fn block_task(cwd: &Path, slug: &str, reason: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.block_task(slug, reason)?;
    let human = render::line(&format!("blocked {}", task.slug));
    json_outcome(task_ack(&task), human)
}

fn unblock_task(cwd: &Path, slug: &str, note: Option<&str>) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.unblock_task(slug, note)?;
    let human = render::line(&format!("unblocked {}", task.slug));
    json_outcome(task_ack(&task), human)
}

fn rename_task(cwd: &Path, slug: &str, title: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.rename_task(slug, title)?;
    let human = render::line(&format!("renamed {}", task.slug));
    json_outcome(task_ack(&task), human)
}

fn one_step_text(positional: Option<String>, flag: Option<String>) -> Result<String, Fail> {
    match (positional, flag) {
        (Some(text), None) | (None, Some(text)) => Ok(text),
        (None, None) => Err(Fail::Usage(
            "step text is required: pass it as an argument or with --text".to_string(),
        )),
        (Some(_), Some(_)) => Err(Fail::Usage(
            "pass step text as an argument or with --text, not both".to_string(),
        )),
    }
}

fn add_step(cwd: &Path, slug: &str, description: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let step = ledger.add_step(slug, description)?;
    let human = render::line(&format!("added step {} to {slug}", step.position));
    json_outcome(
        serde_json::json!({
            "id": step.id,
            "slug": slug,
            "position": step.position,
        }),
        human,
    )
}

fn check_step(cwd: &Path, slug: &str, position: i64) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let step = ledger.check_step(slug, position)?;
    let human = render::line(&format!("checked step {} on {slug}", step.position));
    json_outcome(step, human)
}

fn edit_step(cwd: &Path, slug: &str, position: i64, text: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let step = ledger.edit_step(slug, position, text)?;
    let human = render::line(&format!("edited step {position} on {slug}"));
    let status = if step.done { "done" } else { "open" };
    json_outcome(
        serde_json::json!({
            "slug": slug,
            "position": step.position,
            "status": status,
        }),
        human,
    )
}

fn report_progress(cwd: &Path, slug: &str, note: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let commit = gitinfo::head(cwd);
    let event = ledger.report_progress(slug, note, commit.as_deref())?;
    let human = render::line(&format!("recorded progress on {slug}"));
    json_outcome(serde_json::json!({ "id": event.id, "slug": slug }), human)
}

fn log_decision(cwd: &Path, slug: &str, why: &str, by: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let decision = ledger.log_decision(slug, why, by)?;
    let human = render::line(&format!("logged decision {} on {slug}", decision.id));
    json_outcome(
        serde_json::json!({ "id": decision.id, "slug": slug }),
        human,
    )
}

fn show_context(
    cwd: &Path,
    slug: Option<&str>,
    all: bool,
    events: Option<usize>,
) -> Result<Outcome, Fail> {
    let ledger = open_ledger(cwd)?;
    let full = all || slug.is_some();
    let event_limit = if let Some(limit) = events {
        Some(limit)
    } else if full {
        Some(RECENT_EVENT_LIMIT)
    } else {
        None
    };
    let ctx = ledger.context_view(slug, full, event_limit)?;
    let human = render::context(&ctx, !full, events.is_some());
    json_outcome(ctx, human)
}

fn install_docs(cwd: &Path, claude: bool, cursor: bool) -> Result<Outcome, Fail> {
    if !claude && !cursor {
        return Err(Fail::Usage("pass --claude and/or --cursor".to_string()));
    }
    let outcome = install::apply(&paths::install_root(cwd), claude, cursor)?;
    let mut data = serde_json::Map::new();
    let mut human = String::new();
    if let Some(state) = outcome.claude {
        data.insert(
            "claude".to_string(),
            serde_json::Value::String(state.as_str().to_string()),
        );
        human.push_str(&render::line(&format!("{} CLAUDE.md", state.as_str())));
    }
    if let Some(state) = outcome.cursor {
        data.insert(
            "cursor".to_string(),
            serde_json::Value::String(state.as_str().to_string()),
        );
        human.push_str(&render::line(&format!("{} AGENTS.md", state.as_str())));
    }
    Ok(Outcome {
        data: serde_json::Value::Object(data),
        human,
        code: 0,
        error: None,
    })
}

fn uninstall_project(cwd: &Path, options: uninstall::Options) -> Result<Outcome, Fail> {
    let report = uninstall::run(cwd, options).map_err(Fail::Usage)?;
    let human = uninstall::render(&report);
    Ok(Outcome {
        data: serde_json::json!({ "changes": report.changes }),
        human,
        code: u8::from(report.error.is_some()),
        error: report.error,
    })
}

fn setup_project(cwd: &Path, options: setup::Options) -> Result<Outcome, Fail> {
    let report = setup::run(cwd, options)?;
    let data = serde_json::json!({ "steps": &report.steps });
    let human = setup::render(&report);
    Ok(Outcome {
        data,
        human,
        code: u8::from(report.error.is_some()),
        error: report.error,
    })
}

fn open_ledger(cwd: &Path) -> Result<Ledger, Fail> {
    let path = paths::ledger_path(cwd).ok_or(Fail::NoLedger)?;
    Ok(Ledger::open(&path)?)
}

fn task_ack(task: &relaybook_core::Task) -> serde_json::Value {
    serde_json::json!({
        "id": task.id,
        "slug": task.slug,
        "status": task.status,
    })
}

fn json_outcome(value: impl serde::Serialize, human: String) -> Result<Outcome, Fail> {
    let data =
        serde_json::to_value(value).map_err(|err| Fail::Core(Error::Json(err.to_string())))?;
    Ok(Outcome {
        data,
        human,
        code: 0,
        error: None,
    })
}

fn envelope(ok: bool, data: Option<&serde_json::Value>, error: Option<&str>) -> String {
    let body = serde_json::json!({
        "ok": ok,
        "data": data,
        "error": error,
    });
    format!("{body}\n")
}
