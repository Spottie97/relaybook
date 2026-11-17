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
cleanup() {
  rm -rf "$work" "$graph" "$graph_home"
}
trap cleanup EXIT

relaybook_dir=$(dirname "$relaybook")
PATH="$relaybook_dir:$PATH"
export PATH

# Clear repository and index overrides before either throwaway repo.
# The graphify phase uses a temporary HOME. The agent phase keeps the
# caller's HOME. The optional build above writes under this checkout.
clear_git_repo_env

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
