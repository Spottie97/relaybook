//! Relaybook ledger: tasks, steps, decisions, and an append-only event log.

mod deps;
mod error;
mod ledger;
pub mod legacy;
mod migrate;
mod model;
mod replay;
mod reports;
mod runs;
mod schema;
mod settings;

pub use error::Error;
pub use ledger::Ledger;
pub use legacy::{LegacyMove, migrate_legacy_ledger};
pub use model::{
    Context, Decision, Event, EventKind, RECENT_EVENT_LIMIT, Status, Step, Task, TaskDetail,
};
pub use replay::{ReplayedDecision, ReplayedTask, replay};
pub use reports::{InboxItem, Sections};
pub use runs::{
    Run, blocked_slug as blocked_run_slug, ready_slug as ready_run_slug,
    unfinished as unfinished_run,
};
pub use settings::{RunView, Snapshot as Settings, StatusView, format_config, format_status};
