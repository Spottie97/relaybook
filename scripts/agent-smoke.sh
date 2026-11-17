#!/bin/sh
# Manual smoke test. Not part of CI.
# Needs a signed-in claude, the Cursor CLI, and a real graphify on PATH.
# One run: real-graphify checks in a throwaway repo, then the three plain
# prompts from agent-onboarding-eval. The unit tests use a stand-in graphify.
#
# The graphify phase clears GIT_DIR, GIT_WORK_TREE, and GIT_INDEX_FILE and
# uses a temporary HOME, so it does not operate on another repository or
# rewrite the caller's Claude or Cursor config. The agent phase keeps the
# caller's HOME. The optional relaybook build writes under this checkout.
# RELAYBOOK_SMOKE_LIB=1 loads the helpers and returns before that work.
set -eu

# git -C does not override these. Drop them before either throwaway repo.
clear_git_repo_env() {
  unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
}

# Run a command in a repository. A non-empty home isolates HOME and XDG.
run_in_repo() {
  _repo=$1
  _home=$2
  shift 2
  (
    clear_git_repo_env
    cd "$_repo" || exit 1
    if [ -n "$_home" ]; then
      HOME="$_home"
      XDG_CONFIG_HOME="$_home/config"
      XDG_DATA_HOME="$_home/data"
      export HOME XDG_CONFIG_HOME XDG_DATA_HOME
    fi
    "$@"
  )
}

init_throwaway() {
  _init_repo=$1
  _init_home=$2
  run_in_repo "$_init_repo" "$_init_home" git init -q -b main
  run_in_repo "$_init_repo" "$_init_home" git config user.name "Relaybook"
  run_in_repo "$_init_repo" "$_init_home" git config user.email "relaybook@example.com"
  run_in_repo "$_init_repo" "$_init_home" git config commit.gpgsign false
}

run_graphify() {
  run_in_repo "$graph" "$graph_home" "$@"
}

# Build 0.3.3 and check schema 8 guards. Not part of CI.
# Classic run --prd is capped with timeout: it must get past the insert and
# must not print the guard. It does not have to finish the agent loop.
# Classic run --resume stays in manual mode, so 0.3.3 saves the run and then
# stops with "mode is manual". Gated run commands must print the guard.
# Reaching a run statement needs the signed-in claude and Cursor CLI that the
# rest of this smoke already requires, because 0.3.3 preflight runs first.
smoke_schema_v8() {
  _root=$1
  _new=$2
  _guard="workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)"
  if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "sqlite3 is not on PATH" >&2
    exit 1
  fi
  if ! command -v timeout >/dev/null 2>&1; then
    echo "timeout is not on PATH" >&2
    exit 1
  fi
  _sqlver=$(sqlite3 -version | awk '{print $1}')
  if [ "$_sqlver" != "3.45.1" ]; then
    echo "sqlite3 is $_sqlver, want 3.45.1" >&2
    exit 1
  fi
  oldsrc=$(mktemp -d)
  rmdir "$oldsrc"
  echo "schema v8: building 0.3.3 at c25c29f"
  git -C "$_root" worktree add --detach "$oldsrc" c25c29f
  (
    cd "$oldsrc" || exit 1
    CARGO_TARGET_DIR="$oldsrc/target" cargo build -q --bin relaybook
  )
  _old="$oldsrc/target/debug/relaybook"
  _gated=$(mktemp -d)
  _classic=$(mktemp -d)
  _out=$(mktemp)
  _err=$(mktemp)
  init_throwaway "$_gated" ""
  init_throwaway "$_classic" ""
  printf '# prd\n' > "$_gated/prd.md"
  printf '# prd\n' > "$_classic/prd.md"
  run_in_repo "$_gated" "" git add prd.md
  run_in_repo "$_gated" "" git commit -q -m "prd"
  run_in_repo "$_classic" "" git add prd.md
  run_in_repo "$_classic" "" git commit -q -m "prd"

  run_in_repo "$_gated" "" "$_new" init
  run_in_repo "$_gated" "" "$_new" config set reviewer codex
  run_in_repo "$_gated" "" "$_new" config set workflow gated
  _db="$_gated/.relaybook/ledger.db"
  _triggers=$(sqlite3 "$_db" "SELECT count(*) FROM sqlite_master WHERE type='trigger';")
  if [ "$_triggers" != "5" ]; then
    echo "sqlite3 3.45.1 saw $_triggers triggers, want 5" >&2
    exit 1
  fi
  sqlite3 "$_db" "SELECT count(*) FROM tasks;" >/dev/null
  run_in_repo "$_gated" "" "$_old" task create auth --title "Add auth"
  sqlite3 "$_db" "UPDATE tasks SET status='claimed', claimed_by='cursor' WHERE slug='auth';"
  _rc=0
  run_in_repo "$_gated" "" "$_old" task done auth --no-report >"$_out" 2>"$_err" || _rc=$?
  if [ "$_rc" -eq 0 ] || { ! grep -F -q "$_guard" "$_out" && ! grep -F -q "$_guard" "$_err"; }; then
    echo "gated task done did not fail with the guard" >&2
    cat "$_out" "$_err" >&2
    exit 1
  fi
  _status=$(sqlite3 "$_db" "SELECT status FROM tasks WHERE slug='auth';")
  if [ "$_status" != "claimed" ]; then
    echo "gated task done changed status to $_status" >&2
    exit 1
  fi
  sqlite3 "$_db" "INSERT INTO reports (task_id, kind, author, body, bytes, created_at, commit_sha) SELECT id, 'task', 'cursor', '{}', 2, '2026-01-01T00:00:00.000Z', 'abc' FROM tasks WHERE slug='auth';"
  _rc=0
  run_in_repo "$_gated" "" "$_old" report accept auth >"$_out" 2>"$_err" || _rc=$?
  if [ "$_rc" -eq 0 ] || { ! grep -F -q "$_guard" "$_out" && ! grep -F -q "$_guard" "$_err"; }; then
    echo "gated report accept did not fail with the guard" >&2
    cat "$_out" "$_err" >&2
    exit 1
  fi
  _verdict=$(sqlite3 "$_db" "SELECT coalesce(verdict, '') FROM reports WHERE kind='task' ORDER BY id DESC LIMIT 1;")
  if [ -n "$_verdict" ]; then
    echo "gated report accept stored verdict $_verdict" >&2
    exit 1
  fi
  run_in_repo "$_gated" "" "$_new" mode auto
  _rc=0
  run_in_repo "$_gated" "" timeout 90 "$_old" run --prd prd.md >"$_out" 2>"$_err" || _rc=$?
  if [ "$_rc" -eq 0 ] || { ! grep -F -q "$_guard" "$_out" && ! grep -F -q "$_guard" "$_err"; }; then
    echo "gated run --prd did not fail with the guard" >&2
    cat "$_out" "$_err" >&2
    exit 1
  fi
  _runs=$(sqlite3 "$_db" "SELECT count(*) FROM runs;")
  if [ "$_runs" != "0" ]; then
    echo "gated run --prd left $_runs run rows" >&2
    exit 1
  fi
  sqlite3 "$_db" "INSERT INTO runs (prd_path, prd_sha256, branch, stage, cycle, agent_calls, est_cost_usd, status, started_at, failure_streak, complete_requested, workflow, write_stamp) VALUES ('prd.md', 'abc', 'main', 'plan', 0, 0, 0, 'paused', '2026-01-01T00:00:00.000Z', 0, 0, 'gated', 0);"
  _rc=0
  run_in_repo "$_gated" "" timeout 90 "$_old" run --resume >"$_out" 2>"$_err" || _rc=$?
  if [ "$_rc" -eq 0 ] || { ! grep -F -q "$_guard" "$_out" && ! grep -F -q "$_guard" "$_err"; }; then
    echo "gated run --resume did not fail with the guard" >&2
    cat "$_out" "$_err" >&2
    exit 1
  fi
  _status=$(sqlite3 "$_db" "SELECT status FROM runs ORDER BY id DESC LIMIT 1;")
  if [ "$_status" != "paused" ]; then
    echo "gated run --resume changed status to $_status" >&2
    exit 1
  fi
  sqlite3 "$_db" "SELECT count(*) FROM tasks;" >/dev/null

  run_in_repo "$_classic" "" "$_new" init
  run_in_repo "$_classic" "" "$_old" task create auth --title "Add auth"
  run_in_repo "$_classic" "" "$_old" task claim auth --by cursor
  _rc=0
  run_in_repo "$_classic" "" "$_old" task done auth --no-report >"$_out" 2>"$_err" || _rc=$?
  if [ "$_rc" -ne 0 ] || grep -F -q "$_guard" "$_out" || grep -F -q "$_guard" "$_err"; then
    echo "classic task done failed" >&2
    cat "$_out" "$_err" >&2
    exit 1
  fi
  _rc=0
  run_in_repo "$_classic" "" "$_old" report accept auth >"$_out" 2>"$_err" || _rc=$?
  if [ "$_rc" -ne 0 ] || grep -F -q "$_guard" "$_out" || grep -F -q "$_guard" "$_err"; then
    echo "classic report accept failed" >&2
    cat "$_out" "$_err" >&2
    exit 1
  fi
  run_in_repo "$_classic" "" "$_new" mode auto
  _cdb="$_classic/.relaybook/ledger.db"
  _rc=0
  run_in_repo "$_classic" "" timeout 45 "$_old" run --prd prd.md >"$_out" 2>"$_err" || _rc=$?
  if grep -F -q "$_guard" "$_out" || grep -F -q "$_guard" "$_err"; then
    echo "classic run --prd hit the guard" >&2
    cat "$_out" "$_err" >&2
    exit 1
  fi
  _runs=$(sqlite3 "$_cdb" "SELECT count(*) FROM runs;")
  if [ "$_runs" = "0" ]; then
    echo "classic run --prd did not insert a run (rc $_rc)" >&2
    cat "$_out" "$_err" >&2
    exit 1
  fi
  sqlite3 "$_cdb" "UPDATE runs SET status='paused', stop_reason=NULL WHERE id = (SELECT id FROM runs ORDER BY id DESC LIMIT 1);"
  run_in_repo "$_classic" "" "$_new" mode manual
  _rc=0
  run_in_repo "$_classic" "" timeout 90 "$_old" run --resume >"$_out" 2>"$_err" || _rc=$?
  if grep -F -q "$_guard" "$_out" || grep -F -q "$_guard" "$_err"; then
    echo "classic run --resume hit the guard" >&2
    cat "$_out" "$_err" >&2
    exit 1
  fi
  if ! grep -F -q "mode is manual" "$_out" && ! grep -F -q "mode is manual" "$_err"; then
    echo "classic run --resume did not reach the manual stop (rc $_rc)" >&2
    cat "$_out" "$_err" >&2
    exit 1
  fi
  _status=$(sqlite3 "$_cdb" "SELECT status FROM runs ORDER BY id DESC LIMIT 1;")
  if [ "$_status" = "paused" ]; then
    echo "classic run --resume left the run paused" >&2
    exit 1
  fi
  git -C "$_root" worktree remove --force "$oldsrc"
  oldsrc=
  rm -rf "$_gated" "$_classic"
  rm -f "$_out" "$_err"
  echo "schema v8: 0.3.3 guards and sqlite3 3.45.1 ok"
}

if [ "${RELAYBOOK_SMOKE_LIB:-}" = 1 ]; then
  return 0 2>/dev/null || exit 0
fi

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
if ! command -v graphify >/dev/null 2>&1; then
  echo "graphify is not on PATH" >&2
  exit 1
fi

work=$(mktemp -d)
graph=$(mktemp -d)
graph_home=$(mktemp -d)
oldsrc=
cleanup() {
  rm -rf "$work" "$graph" "$graph_home" "$oldsrc" ${_gated:+"$_gated"} ${_classic:+"$_classic"}
  rm -f ${_out:+"$_out"} ${_err:+"$_err"}
}
trap cleanup EXIT

relaybook_dir=$(dirname "$relaybook")
PATH="$relaybook_dir:$PATH"
export PATH

# Clear repository and index overrides before either throwaway repo.
# The graphify phase uses a temporary HOME. The agent phase keeps the
# caller's HOME. The optional build above writes under this checkout.
clear_git_repo_env

cleanup() {
  if [ -n "${oldsrc:-}" ]; then
    git -C "$root" worktree remove --force "$oldsrc" >/dev/null 2>&1 || true
  fi
  rm -rf "$work" "$graph" "$graph_home" "$oldsrc" ${_gated:+"$_gated"} ${_classic:+"$_classic"}
  rm -f ${_out:+"$_out"} ${_err:+"$_err"}
}
trap cleanup EXIT

init_throwaway "$graph" "$graph_home"
mkdir -p "$graph/src"
printf 'fn main() {}\n' > "$graph/src/main.rs"
run_in_repo "$graph" "$graph_home" git add src/main.rs
run_in_repo "$graph" "$graph_home" git commit -q -m "tiny app"

if ! run_graphify graphify hook install >"$graph/hook-install.out" 2>&1; then
  echo "graphify hook install failed" >&2
  cat "$graph/hook-install.out" >&2
  exit 1
fi
before=$(run_in_repo "$graph" "$graph_home" git config --local --get merge.graphify.driver || true)
if [ -z "$before" ]; then
  echo "graphify hook install did not set merge.graphify.driver" >&2
  cat "$graph/hook-install.out" >&2
  exit 1
fi
if ! run_graphify "$relaybook" setup --ci >"$graph/setup.out" 2>&1; then
  echo "relaybook setup --ci failed" >&2
  cat "$graph/setup.out" >&2
  exit 1
fi
after=$(run_in_repo "$graph" "$graph_home" git config --local --get merge.graphify.driver || true)
if [ "$before" != "$after" ]; then
  echo "setup replaced merge.graphify.driver" >&2
  printf 'before: %s\n' "$before" >&2
  printf 'after: %s\n' "$after" >&2
  exit 1
fi
if ! grep -q 'CLAUDE.md: warn (graphify claude install changed CLAUDE.md)' "$graph/setup.out"; then
  echo "setup did not warn that graphify changed CLAUDE.md" >&2
  cat "$graph/setup.out" >&2
  exit 1
fi
rules="$graph/.cursor/rules/graphify.mdc"
had_rules=0
if [ -f "$rules" ]; then
  had_rules=1
fi
if ! run_graphify "$relaybook" uninstall --dry-run --graphify >"$graph/dry.out" 2>&1; then
  echo "relaybook uninstall --dry-run --graphify failed" >&2
  cat "$graph/dry.out" >&2
  exit 1
fi
dry_has() {
  if ! grep -F -q "$1" "$graph/dry.out"; then
    echo "dry-run missing: $1" >&2
    cat "$graph/dry.out" >&2
    exit 1
  fi
}
dry_has "would remove graphify claude uninstall"
dry_has "would remove graphify cursor uninstall"
dry_has "would remove graphify hook uninstall"
dry_has "deletes .cursor/rules/graphify.mdc"
dry_has "graphify-out/graph.json merge=graphify"
dry_has "merge.graphify.*"
if [ ! -f "$graph/CLAUDE.md" ]; then
  echo "dry-run removed CLAUDE.md" >&2
  exit 1
fi
if [ "$had_rules" -eq 1 ] && [ ! -f "$rules" ]; then
  echo "dry-run removed .cursor/rules/graphify.mdc" >&2
  exit 1
fi
still=$(run_in_repo "$graph" "$graph_home" git config --local --get merge.graphify.driver || true)
if [ "$still" != "$before" ]; then
  echo "dry-run changed merge.graphify.driver" >&2
  exit 1
fi
echo "graphify: driver kept, CLAUDE.md warned, dry-run listed side effects"

smoke_schema_v8 "$root" "$relaybook"

init_throwaway "$work" ""
cd "$work" || exit 1
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

"$relaybook" setup --no-graphify >/dev/null

allow="Bash(relaybook context:*),Bash(relaybook inbox:*),Bash(relaybook guide:*),Bash(relaybook status:*),Bash(relaybook task create:*),Bash(relaybook task list:*),Bash(relaybook task reopen:*),Bash(relaybook task depend:*),Bash(relaybook step add:*),Bash(relaybook step edit:*),Bash(relaybook decision log:*),Bash(relaybook report accept:*),Bash(relaybook report note:*)"

claude -p "Plan adding a /health endpoint" \
  --permission-mode default \
  --allowedTools "$allow"

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

claude -p "Check that it has been done" \
  --permission-mode default \
  --allowedTools "$allow"

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
