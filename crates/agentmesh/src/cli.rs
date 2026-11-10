use std::path::Path;
use std::process::ExitCode;

use agentmesh_core::{Error, Ledger, RECENT_EVENT_LIMIT, Status};
use clap::{Parser, Subcommand, ValueEnum};

use crate::gitinfo;
use crate::install;
use crate::paths;
use crate::render;

pub(crate) fn execute() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => err.exit(),
    };
    let json = cli.json;
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
    name = "agentmesh",
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
    Context { slug: Option<String> },
    #[command(subcommand)]
    Task(TaskCmd),
    #[command(subcommand)]
    Step(StepCmd),
    #[command(subcommand)]
    Progress(ProgressCmd),
    #[command(subcommand)]
    Decision(DecisionCmd),
    /// Append coordination instructions to CLAUDE.md and AGENTS.md.
    Install {
        /// Append the planner block to CLAUDE.md.
        #[arg(long)]
        claude: bool,
        /// Append the executor block to AGENTS.md.
        #[arg(long)]
        cursor: bool,
    },
}

#[derive(Subcommand)]
enum TaskCmd {
    /// Create an open task.
    Create {
        slug: String,
        #[arg(long)]
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
    /// Mark a task done.
    Done { slug: String },
    /// Mark an open or claimed task blocked.
    Block {
        slug: String,
        #[arg(long)]
        reason: String,
    },
    /// Change a task's title. The slug stays the same.
    Rename {
        slug: String,
        #[arg(long)]
        title: String,
    },
}

#[derive(Subcommand)]
enum StepCmd {
    /// Append a checklist step. Positions start at 1.
    Add { slug: String, description: String },
    /// Mark checklist step n done.
    Check { slug: String, n: i64 },
}

#[derive(Subcommand)]
enum ProgressCmd {
    /// Record progress once for the current git commit.
    Report {
        slug: String,
        #[arg(long)]
        note: String,
    },
}

#[derive(Subcommand)]
enum DecisionCmd {
    /// Record why a choice was made, and who made it.
    Log {
        slug: String,
        #[arg(long)]
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
            Self::NoLedger => formatter.write_str("no ledger found (run `agentmesh init`)"),
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

fn dispatch(cli: Cli) -> Result<Outcome, Fail> {
    let cwd = std::env::current_dir()?;
    match cli.command {
        Command::Init => init_ledger(&cwd),
        Command::Context { slug } => show_context(&cwd, slug.as_deref()),
        Command::Task(TaskCmd::Create {
            slug,
            title,
            depends_on,
        }) => create_task(&cwd, &slug, &title, &depends_on),
        Command::Task(TaskCmd::List { status, ready }) => {
            list_tasks(&cwd, status.map(StatusOpt::status), ready)
        }
        Command::Task(TaskCmd::Depend { slug, on }) => depend(&cwd, &slug, &on),
        Command::Task(TaskCmd::Undepend { slug, on }) => undepend(&cwd, &slug, &on),
        Command::Task(TaskCmd::Claim { slug, by }) => claim_task(&cwd, &slug, &by),
        Command::Task(TaskCmd::Done { slug }) => complete_task(&cwd, &slug),
        Command::Task(TaskCmd::Block { slug, reason }) => block_task(&cwd, &slug, &reason),
        Command::Task(TaskCmd::Rename { slug, title }) => rename_task(&cwd, &slug, &title),
        Command::Step(StepCmd::Add { slug, description }) => add_step(&cwd, &slug, &description),
        Command::Step(StepCmd::Check { slug, n }) => check_step(&cwd, &slug, n),
        Command::Progress(ProgressCmd::Report { slug, note }) => {
            report_progress(&cwd, &slug, &note)
        }
        Command::Decision(DecisionCmd::Log { slug, why, by }) => {
            log_decision(&cwd, &slug, &why, &by)
        }
        Command::Install { claude, cursor } => install_docs(&cwd, claude, cursor),
    }
}

fn init_ledger(cwd: &Path) -> Result<Outcome, Fail> {
    let path = paths::init_root(cwd).join(".agentmesh").join("ledger.db");
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
    let human = render::line(&format!("created {}: {}", task.slug, task.title));
    json_outcome(task, human)
}

fn depend(cwd: &Path, slug: &str, on: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.add_dependency(slug, on)?;
    let human = render::line(&format!("{slug} depends on {on}"));
    json_outcome(task, human)
}

fn undepend(cwd: &Path, slug: &str, on: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.remove_dependency(slug, on)?;
    let human = render::line(&format!("{slug} no longer depends on {on}"));
    json_outcome(task, human)
}

fn list_tasks(cwd: &Path, status: Option<Status>, ready: bool) -> Result<Outcome, Fail> {
    let ledger = open_ledger(cwd)?;
    let tasks = ledger.list_tasks(status, ready)?;
    let human = render::tasks(&tasks);
    json_outcome(tasks, human)
}

fn claim_task(cwd: &Path, slug: &str, by: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.claim_task(slug, by)?;
    let agent = task.claimed_by.clone().unwrap_or_default();
    let human = render::line(&format!("claimed {} by {agent}", task.slug));
    json_outcome(task, human)
}

fn complete_task(cwd: &Path, slug: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.complete_task(slug)?;
    let human = render::line(&format!("done {}", task.slug));
    json_outcome(task, human)
}

fn block_task(cwd: &Path, slug: &str, reason: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.block_task(slug, reason)?;
    let human = render::line(&format!("blocked {}: {reason}", task.slug));
    json_outcome(task, human)
}

fn rename_task(cwd: &Path, slug: &str, title: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let task = ledger.rename_task(slug, title)?;
    let human = render::line(&format!("renamed {}: {}", task.slug, task.title));
    json_outcome(task, human)
}

fn add_step(cwd: &Path, slug: &str, description: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let step = ledger.add_step(slug, description)?;
    let human = render::line(&format!(
        "added step {} on {slug}: {}",
        step.position, step.description
    ));
    json_outcome(step, human)
}

fn check_step(cwd: &Path, slug: &str, position: i64) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let step = ledger.check_step(slug, position)?;
    let human = render::line(&format!("checked step {} on {slug}", step.position));
    json_outcome(step, human)
}

fn report_progress(cwd: &Path, slug: &str, note: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let commit = gitinfo::head(cwd);
    let event = ledger.report_progress(slug, note, commit.as_deref())?;
    let human = render::line(&format!("recorded progress on {slug}"));
    json_outcome(event, human)
}

fn log_decision(cwd: &Path, slug: &str, why: &str, by: &str) -> Result<Outcome, Fail> {
    let mut ledger = open_ledger(cwd)?;
    let decision = ledger.log_decision(slug, why, by)?;
    let human = render::line(&format!("logged decision on {slug}"));
    json_outcome(decision, human)
}

fn show_context(cwd: &Path, slug: Option<&str>) -> Result<Outcome, Fail> {
    let ledger = open_ledger(cwd)?;
    let ctx = ledger.context(slug, RECENT_EVENT_LIMIT)?;
    let human = render::context(&ctx);
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

fn open_ledger(cwd: &Path) -> Result<Ledger, Fail> {
    let path = paths::ledger_path(cwd).ok_or(Fail::NoLedger)?;
    Ok(Ledger::open(&path)?)
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
