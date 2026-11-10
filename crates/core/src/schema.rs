/// Current ledger schema. `initialize` applies this to a new database.
///
/// `events_progress_once` rejects a second progress event for the same task
/// and commit. A null commit (no git HEAD) is not covered, so those reports append.
pub(crate) const SCHEMA_VERSION: i64 = 5;

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
            'task_renamed'
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
    review_note TEXT
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
    complete_requested INTEGER NOT NULL DEFAULT 0
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
