/// Current ledger schema. `initialize` applies this to a new database.
///
/// `events_progress_once` rejects a second progress event for the same task
/// and commit. A null commit (no git HEAD) is not covered, so those reports append.
pub(crate) const SCHEMA_VERSION: i64 = 8;

/// Text of every gated write guard. 0.3.3 sees it as `sqlite: <message>`.
pub(crate) const GUARD_MESSAGE: &str =
    "workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)";

pub(crate) const GUARD_NAMES: &[&str] = &[
    "relaybook_guard_task_report_insert",
    "relaybook_guard_task_accept",
    "relaybook_guard_claim_base_sha",
    "relaybook_guard_run_insert",
    "relaybook_guard_run_update",
];

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tasks (
    id INTEGER PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('open', 'claimed', 'done', 'blocked')),
    claimed_by TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    run_id INTEGER
);

CREATE TABLE IF NOT EXISTS steps (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks(id),
    position INTEGER NOT NULL,
    description TEXT NOT NULL,
    done INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1)),
    UNIQUE (task_id, position)
);

CREATE TABLE IF NOT EXISTS decisions (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks(id),
    rationale TEXT NOT NULL,
    provenance TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS task_deps (
    task_id INTEGER NOT NULL REFERENCES tasks(id),
    depends_on_id INTEGER NOT NULL REFERENCES tasks(id),
    created_at TEXT NOT NULL,
    PRIMARY KEY (task_id, depends_on_id)
);

CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY,
    ts TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (
        kind IN (
            'task_created',
            'claimed',
            'progress',
            'decision',
            'status_changed',
            'dependency_changed',
            'task_renamed',
            'run_linked'
        )
    ),
    payload TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS events_progress_once
ON events (
    json_extract(payload, '$.slug'),
    json_extract(payload, '$.commit')
)
WHERE kind = 'progress'
  AND json_extract(payload, '$.commit') IS NOT NULL;

CREATE TABLE IF NOT EXISTS reports (
    id INTEGER PRIMARY KEY,
    task_id INTEGER REFERENCES tasks(id),
    kind TEXT NOT NULL CHECK (kind IN ('task', 'note')),
    author TEXT NOT NULL,
    body TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    reviewed_at TEXT,
    verdict TEXT CHECK (verdict IS NULL OR verdict IN ('accepted', 'reopened')),
    review_note TEXT,
    commit_sha TEXT,
    base_sha TEXT,
    graph_sha TEXT
);

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY,
    prd_path TEXT NOT NULL,
    prd_sha256 TEXT NOT NULL,
    branch TEXT NOT NULL,
    stage TEXT NOT NULL,
    cycle INTEGER NOT NULL,
    agent_calls INTEGER NOT NULL,
    est_cost_usd REAL NOT NULL DEFAULT 0,
    status TEXT NOT NULL CHECK (status IN ('active', 'waiting', 'paused', 'stopped', 'complete')),
    stop_reason TEXT,
    started_at TEXT NOT NULL,
    failure_streak INTEGER NOT NULL DEFAULT 0,
    current_slug TEXT,
    complete_requested INTEGER NOT NULL DEFAULT 0,
    workflow TEXT,
    write_stamp INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS reviews (
    id INTEGER PRIMARY KEY,
    scope TEXT NOT NULL CHECK (scope IN ('task', 'release')),
    task_id INTEGER REFERENCES tasks(id),
    report_id INTEGER REFERENCES reports(id),
    run_id INTEGER REFERENCES runs(id),
    branch TEXT NOT NULL,
    reviewer TEXT NOT NULL,
    reviewer_override INTEGER NOT NULL DEFAULT 0 CHECK (reviewer_override IN (0, 1)),
    base_sha TEXT,
    commit_sha TEXT NOT NULL,
    graph_sha TEXT,
    ledger_mark TEXT NOT NULL,
    verdict TEXT CHECK (verdict IN ('approved', 'changes_requested')),
    body TEXT,
    bytes INTEGER,
    started_at TEXT NOT NULL,
    recorded_at TEXT,
    CHECK ((scope = 'task') = (task_id IS NOT NULL))
);

CREATE TABLE IF NOT EXISTS graph_builds (
    id INTEGER PRIMARY KEY,
    step TEXT NOT NULL,
    task_id INTEGER REFERENCES tasks(id),
    run_id INTEGER REFERENCES runs(id),
    commit_sha TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('built', 'skipped', 'failed')),
    detail TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS release_authorizations (
    id INTEGER PRIMARY KEY,
    run_id INTEGER REFERENCES runs(id),
    branch TEXT NOT NULL,
    commit_sha TEXT NOT NULL,
    review_id INTEGER NOT NULL REFERENCES reviews(id),
    authorized_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    method TEXT,
    evidence TEXT
);

CREATE TABLE IF NOT EXISTS remote_checks (
    id INTEGER PRIMARY KEY,
    context TEXT NOT NULL,
    branch TEXT NOT NULL,
    head_sha TEXT NOT NULL,
    remote TEXT,
    status TEXT NOT NULL CHECK (status IN ('absent', 'present', 'unknown', 'local')),
    detail TEXT,
    override INTEGER NOT NULL DEFAULT 0 CHECK (override IN (0, 1)),
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS publish_claims (
    run_id INTEGER PRIMARY KEY,
    pid INTEGER NOT NULL,
    host TEXT NOT NULL,
    process_start TEXT NOT NULL DEFAULT '',
    heartbeat_at TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS runs_one_live_gated
ON runs (workflow)
WHERE workflow = 'gated'
  AND (
    status IN ('active', 'waiting', 'paused')
    OR stage = 'publishing'
  );
";

/// One live gated run. `publishing` is the stage set once the push has started.
/// A read-write open installs this on an existing schema 8 ledger.
pub(crate) const ONE_LIVE_GATED_INDEX: &str = "
CREATE UNIQUE INDEX IF NOT EXISTS runs_one_live_gated
ON runs (workflow)
WHERE workflow = 'gated'
  AND (
    status IN ('active', 'waiting', 'paused')
    OR stage = 'publishing'
  );
";

/// Publisher pid, host, and heartbeat for a gated run at stage `publishing`.
/// A read-write open installs this on an existing schema 8 ledger.
/// `user_version` stays 8.
pub(crate) const PUBLISH_CLAIMS: &str = "
CREATE TABLE IF NOT EXISTS publish_claims (
    run_id INTEGER PRIMARY KEY,
    pid INTEGER NOT NULL,
    host TEXT NOT NULL,
    process_start TEXT NOT NULL DEFAULT '',
    heartbeat_at TEXT NOT NULL
);
";

/// Triggers installed for schema 8. They only `RAISE`; they do not write.
pub(crate) const GUARDS: &str = "
CREATE TRIGGER IF NOT EXISTS relaybook_guard_task_report_insert
BEFORE INSERT ON reports
WHEN NEW.kind = 'task'
 AND NEW.commit_sha IS NULL
 AND (SELECT value FROM settings WHERE key = 'workflow') = 'gated'
BEGIN
  SELECT RAISE(ABORT, 'workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)');
END;

CREATE TRIGGER IF NOT EXISTS relaybook_guard_task_accept
BEFORE UPDATE OF verdict ON reports
WHEN NEW.kind = 'task'
 AND NEW.verdict = 'accepted'
 AND OLD.verdict IS NOT 'accepted'
 AND (SELECT value FROM settings WHERE key = 'workflow') = 'gated'
 AND NOT EXISTS (
    SELECT 1 FROM reviews
    WHERE id = (
        SELECT MAX(id) FROM reviews
        WHERE scope = 'task' AND task_id = NEW.task_id
    )
    AND verdict = 'approved'
    AND commit_sha = (
        SELECT commit_sha FROM reports
        WHERE task_id = NEW.task_id AND kind = 'task'
        ORDER BY id DESC
        LIMIT 1
    )
 )
BEGIN
  SELECT RAISE(ABORT, 'workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)');
END;

CREATE TRIGGER IF NOT EXISTS relaybook_guard_claim_base_sha
BEFORE INSERT ON events
WHEN NEW.kind = 'claimed'
 AND json_type(NEW.payload, '$.base_sha') IS NULL
 AND (SELECT value FROM settings WHERE key = 'workflow') = 'gated'
BEGIN
  SELECT RAISE(ABORT, 'workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)');
END;

CREATE TRIGGER IF NOT EXISTS relaybook_guard_run_insert
BEFORE INSERT ON runs
WHEN (SELECT value FROM settings WHERE key = 'workflow') = 'gated'
 AND NEW.workflow IS NOT 'gated'
BEGIN
  SELECT RAISE(ABORT, 'workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)');
END;

CREATE TRIGGER IF NOT EXISTS relaybook_guard_run_update
BEFORE UPDATE ON runs
WHEN (OLD.workflow = 'gated' OR (SELECT value FROM settings WHERE key = 'workflow') = 'gated')
 AND NEW.write_stamp IS OLD.write_stamp
BEGIN
  SELECT RAISE(ABORT, 'workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)');
END;
";

/// Additive v8 objects. Column alters are guarded in `migrate`. A v6 fixture
/// can lack `settings` or `runs`; the triggers read and target those tables,
/// so both are created when missing. Triggers come from [`GUARDS`].
/// [`ONE_LIVE_GATED_INDEX`] is the same partial index, installed again on open.
pub(crate) const MIGRATE_V7_TO_V8: &str = "
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY,
    prd_path TEXT NOT NULL,
    prd_sha256 TEXT NOT NULL,
    branch TEXT NOT NULL,
    stage TEXT NOT NULL,
    cycle INTEGER NOT NULL,
    agent_calls INTEGER NOT NULL,
    est_cost_usd REAL NOT NULL DEFAULT 0,
    status TEXT NOT NULL CHECK (status IN ('active', 'waiting', 'paused', 'stopped', 'complete')),
    stop_reason TEXT,
    started_at TEXT NOT NULL,
    failure_streak INTEGER NOT NULL DEFAULT 0,
    current_slug TEXT,
    complete_requested INTEGER NOT NULL DEFAULT 0,
    workflow TEXT,
    write_stamp INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS remote_checks (
    id INTEGER PRIMARY KEY,
    context TEXT NOT NULL,
    branch TEXT NOT NULL,
    head_sha TEXT NOT NULL,
    remote TEXT,
    status TEXT NOT NULL CHECK (status IN ('absent', 'present', 'unknown', 'local')),
    detail TEXT,
    override INTEGER NOT NULL DEFAULT 0 CHECK (override IN (0, 1)),
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS publish_claims (
    run_id INTEGER PRIMARY KEY,
    pid INTEGER NOT NULL,
    host TEXT NOT NULL,
    process_start TEXT NOT NULL DEFAULT '',
    heartbeat_at TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS runs_one_live_gated
ON runs (workflow)
WHERE workflow = 'gated'
  AND (
    status IN ('active', 'waiting', 'paused')
    OR stage = 'publishing'
  );
";

pub(crate) const MIGRATE_V3_TO_V4: &str = "
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY,
    prd_path TEXT NOT NULL,
    prd_sha256 TEXT NOT NULL,
    branch TEXT NOT NULL,
    stage TEXT NOT NULL,
    cycle INTEGER NOT NULL,
    agent_calls INTEGER NOT NULL,
    est_cost_usd REAL NOT NULL DEFAULT 0,
    status TEXT NOT NULL CHECK (status IN ('active', 'waiting', 'paused', 'stopped', 'complete')),
    stop_reason TEXT,
    started_at TEXT NOT NULL
);
";

pub(crate) const MIGRATE_V2_TO_V3: &str = "
CREATE TABLE IF NOT EXISTS reports (
    id INTEGER PRIMARY KEY,
    task_id INTEGER REFERENCES tasks(id),
    kind TEXT NOT NULL CHECK (kind IN ('task', 'note')),
    author TEXT NOT NULL,
    body TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    reviewed_at TEXT,
    verdict TEXT CHECK (verdict IS NULL OR verdict IN ('accepted', 'reopened')),
    review_note TEXT
);
";

/// Rebuild used when opening a v1 ledger. Copies every event row unchanged.
pub(crate) const MIGRATE_V1_TO_V2: &str = "
CREATE TABLE IF NOT EXISTS task_deps (
    task_id INTEGER NOT NULL REFERENCES tasks(id),
    depends_on_id INTEGER NOT NULL REFERENCES tasks(id),
    created_at TEXT NOT NULL,
    PRIMARY KEY (task_id, depends_on_id)
);

CREATE TABLE events_v2 (
    id INTEGER PRIMARY KEY,
    ts TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (
        kind IN (
            'task_created',
            'claimed',
            'progress',
            'decision',
            'status_changed',
            'dependency_changed',
            'task_renamed'
        )
    ),
    payload TEXT NOT NULL
);

INSERT INTO events_v2 (id, ts, kind, payload)
SELECT id, ts, kind, payload FROM events;

DROP TABLE events;

ALTER TABLE events_v2 RENAME TO events;

CREATE UNIQUE INDEX events_progress_once
ON events (
    json_extract(payload, '$.slug'),
    json_extract(payload, '$.commit')
)
WHERE kind = 'progress'
  AND json_extract(payload, '$.commit') IS NOT NULL;
";

pub(crate) const MIGRATE_V5_TO_V6: &str = "
CREATE TABLE events_v6 (
    id INTEGER PRIMARY KEY,
    ts TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (
        kind IN (
            'task_created',
            'claimed',
            'progress',
            'decision',
            'status_changed',
            'dependency_changed',
            'task_renamed',
            'run_linked'
        )
    ),
    payload TEXT NOT NULL
);

INSERT INTO events_v6 (id, ts, kind, payload)
SELECT id, ts, kind, payload FROM events;

DROP TABLE events;

ALTER TABLE events_v6 RENAME TO events;

CREATE UNIQUE INDEX IF NOT EXISTS events_progress_once
ON events (
    json_extract(payload, '$.slug'),
    json_extract(payload, '$.commit')
)
WHERE kind = 'progress'
  AND json_extract(payload, '$.commit') IS NOT NULL;
";

/// Additive v7 tables. Report SHA columns are added with `column_exists` guards.
pub(crate) const MIGRATE_V6_TO_V7: &str = "
CREATE TABLE IF NOT EXISTS reviews (
    id INTEGER PRIMARY KEY,
    scope TEXT NOT NULL CHECK (scope IN ('task', 'release')),
    task_id INTEGER REFERENCES tasks(id),
    report_id INTEGER REFERENCES reports(id),
    run_id INTEGER REFERENCES runs(id),
    branch TEXT NOT NULL,
    reviewer TEXT NOT NULL,
    reviewer_override INTEGER NOT NULL DEFAULT 0 CHECK (reviewer_override IN (0, 1)),
    base_sha TEXT,
    commit_sha TEXT NOT NULL,
    graph_sha TEXT,
    ledger_mark TEXT NOT NULL,
    verdict TEXT CHECK (verdict IN ('approved', 'changes_requested')),
    body TEXT,
    bytes INTEGER,
    started_at TEXT NOT NULL,
    recorded_at TEXT,
    CHECK ((scope = 'task') = (task_id IS NOT NULL))
);

CREATE TABLE IF NOT EXISTS graph_builds (
    id INTEGER PRIMARY KEY,
    step TEXT NOT NULL,
    task_id INTEGER REFERENCES tasks(id),
    run_id INTEGER REFERENCES runs(id),
    commit_sha TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('built', 'skipped', 'failed')),
    detail TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS release_authorizations (
    id INTEGER PRIMARY KEY,
    run_id INTEGER REFERENCES runs(id),
    branch TEXT NOT NULL,
    commit_sha TEXT NOT NULL,
    review_id INTEGER NOT NULL REFERENCES reviews(id),
    authorized_by TEXT NOT NULL,
    created_at TEXT NOT NULL
);
";

#[cfg(test)]
mod guard_sql {
    use super::{
        GUARD_MESSAGE, GUARD_NAMES, GUARDS, MIGRATE_V7_TO_V8, ONE_LIVE_GATED_INDEX, SCHEMA,
    };

    #[test]
    fn guard_text_is_the_raised_message() {
        assert_eq!(GUARDS.matches(GUARD_MESSAGE).count(), GUARD_NAMES.len());
        for name in GUARD_NAMES {
            assert!(GUARDS.contains(name), "{name}");
        }
        assert!(SCHEMA.contains("CREATE TABLE IF NOT EXISTS remote_checks"));
        assert!(SCHEMA.contains("write_stamp INTEGER NOT NULL DEFAULT 0"));
        assert!(SCHEMA.contains("method TEXT"));
        assert!(MIGRATE_V7_TO_V8.contains("CREATE TABLE IF NOT EXISTS remote_checks"));
        assert!(SCHEMA.contains("runs_one_live_gated"));
        assert!(MIGRATE_V7_TO_V8.contains("runs_one_live_gated"));
        assert!(
            ONE_LIVE_GATED_INDEX.contains("CREATE UNIQUE INDEX IF NOT EXISTS runs_one_live_gated")
        );
        assert!(SCHEMA.contains(ONE_LIVE_GATED_INDEX.trim()));
        assert!(MIGRATE_V7_TO_V8.contains(ONE_LIVE_GATED_INDEX.trim()));
    }
}
