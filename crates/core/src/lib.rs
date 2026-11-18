//! Relaybook ledger: tasks, steps, decisions, and an append-only event log.

mod deps;
mod error;
mod ledger;
pub mod legacy;
pub mod lifecycle;
mod migrate;
mod model;
mod replay;
mod reports;
mod runs;
mod schema;
mod settings;

pub use error::Error;
pub use ledger::{
    Absorb, AnchoredClaim, ClaimAnchor, GraphBuild, LEDGER_CHANGED_DURING_REVIEW, Ledger,
    OwnedReviewExpect, OwnedReviewSnapshot, OwnedRunAdvance, REPORT_CHANGED_DURING_REFRESH,
    ReleaseAuthorization, RemoteCheck, RemoteCheckDraft, Review, ReviewInvocationAction,
    ReviewInvocationOutcome, STALE_REVIEW_INVOCATION, ScopedTask, TaskReport,
    apply_review_invocation,
};
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
pub use settings::{
    DEFAULT_CODEX_MODEL, RunView, Snapshot as Settings, StatusView, configured_value,
    format_config, format_status, parse_codex_model,
};
