#!/bin/sh
# Manual smoke test. Not part of CI: it needs a signed-in claude and Cursor CLI.
# One run: a fresh repo, then the three plain prompts from agent-onboarding-eval.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
relaybook=${RELAYBOOK_BIN:-$root/target/debug/relaybook}
if [ ! -x "$relaybook" ]; then
  echo "building $relaybook" >&2
  (cd "$root" && cargo build -q --bin relaybook)
fi

if ! command -v claude >/dev/null 2>&1; then
  echo "claude is not on PATH" >&2
  exit 1
fi
cursor=$(command -v cursor-agent || true)
if [ -z "$cursor" ]; then
  cursor=$(command -v agent || true)
fi
if [ -z "$cursor" ]; then
  echo "cursor-agent and agent are not on PATH" >&2
  exit 1
fi

work=$(mktemp -d)
cleanup() {
  rm -rf "$work"
}
trap cleanup EXIT

cd "$work"
git init -q -b main
git config user.name "Relaybook"
git config user.email "relaybook@example.com"
git config commit.gpgsign false
mkdir -p src
cat > src/main.rs <<'EOF'
fn main() {
    println!("hello");
}
EOF
git add src/main.rs
git commit -q -m "tiny app"
base=$(git rev-parse HEAD)
code_paths() {
  git diff --name-only "$base" -- src Cargo.toml Cargo.lock
  git ls-files --others --exclude-standard -- src Cargo.toml Cargo.lock
}

export PATH="$(dirname "$relaybook"):$PATH"
"$relaybook" setup --no-graphify >/dev/null

claude -p "Plan adding a /health endpoint" --dangerously-skip-permissions

python3 - "$work/.relaybook/ledger.db" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
tasks = db.execute("select count(*) from tasks").fetchone()[0]
steps = db.execute("select count(*) from steps").fetchone()[0]
decisions = db.execute("select count(*) from decisions").fetchone()[0]
if tasks < 1 or steps < 1 or decisions < 1:
    raise SystemExit(f"plan incomplete: tasks={tasks} steps={steps} decisions={decisions}")
print(f"plan: tasks={tasks} steps={steps} decisions={decisions}")
PY

code=$(code_paths)
if [ -n "$code" ]; then
  echo "plan changed code: $code" >&2
  exit 1
fi
echo "plan: code unchanged"

"$cursor" -p --force "Do the next task"

python3 - "$work/.relaybook/ledger.db" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
held = db.execute(
    "select count(*) from tasks where status in ('claimed', 'done')"
).fetchone()[0]
reports = db.execute("select count(*) from reports where kind = 'task'").fetchone()[0]
if held < 1 or reports < 1:
    raise SystemExit(f"execute incomplete: held={held} reports={reports}")
print(f"execute: held={held} reports={reports}")
PY

code=$(code_paths)
if [ -z "$code" ]; then
  echo "execute did not change code" >&2
  exit 1
fi
echo "execute: code changed ($code)"

claude -p "Check that it has been done" --dangerously-skip-permissions

python3 - "$work/.relaybook/ledger.db" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
row = db.execute(
    """
    select verdict, coalesce(review_note, '')
    from reports
    where kind = 'task' and verdict in ('accepted', 'reopened')
    order by id desc
    limit 1
    """
).fetchone()
if row is None:
    raise SystemExit("review did not accept or reopen the report")
verdict, note = row
if verdict == "reopened" and not note.strip():
    raise SystemExit("reopen has no reason")
print(f"review: {verdict}")
PY

planner=$(wc -c < "$root/crates/relaybook/templates/claude.md" | tr -d ' ')
executor=$(wc -c < "$root/crates/relaybook/templates/agents.md" | tr -d ' ')
echo "blocks: planner=$planner executor=$executor"
echo "smoke: ok"
