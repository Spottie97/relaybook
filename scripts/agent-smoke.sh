#!/bin/sh
# Manual smoke test. Not part of CI.
# Needs a signed-in claude, the Cursor CLI, a signed-in codex, and a real
# graphify on PATH. Also needs git, python3, and sqlite3 3.45.1.
# A deadline uses timeout if it is on PATH, otherwise gtimeout, otherwise python3.
# One run: real-graphify checks in a throwaway repo, schema 8 against a
# 0.3.3 build, real git ls-remote, a real read-only Codex review (exit 0
# and a schema-valid verdict) plus a separate write-denial probe, then the
# three plain prompts from agent-onboarding-eval. The unit tests use a
# stand-in graphify and do not run the full script.
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

# True when run_timeout can enforce a deadline.
have_deadline() {
  command -v timeout >/dev/null 2>&1 \
    || command -v gtimeout >/dev/null 2>&1 \
    || command -v python3 >/dev/null 2>&1
}

require_deadline() {
  if have_deadline; then
    return 0
  fi
  echo "timeout, gtimeout, and python3 are not on PATH" >&2
  return 1
}

# Stop a command after N seconds. A deadline exits 124, matching GNU timeout.
# Prefer timeout, then gtimeout. python3 kills the command's process group.
run_timeout() {
  _deadline_secs=$1
  shift
  if command -v timeout >/dev/null 2>&1; then
    timeout "$_deadline_secs" "$@" || return $?
    return 0
  fi
  if command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$_deadline_secs" "$@" || return $?
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    echo "timeout, gtimeout, and python3 are not on PATH" >&2
    return 127
  fi
  python3 -c '
import os
import signal
import subprocess
import sys

seconds = float(sys.argv[1])
command = sys.argv[2:]
if not command:
    sys.stderr.write("run_timeout: missing command\n")
    raise SystemExit(127)
try:
    child = subprocess.Popen(command, start_new_session=True)
except OSError:
    raise SystemExit(127)
try:
    code = child.wait(timeout=seconds)
except subprocess.TimeoutExpired:
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except OSError:
        pass
    try:
        child.wait(timeout=1)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except OSError:
            pass
        child.wait()
    raise SystemExit(124)
if code < 0:
    raise SystemExit(128 - code)
raise SystemExit(code)
' "$_deadline_secs" "$@" || return $?
  return 0
}

# Build 0.3.3 and check schema 8 guards. Not part of CI.
# Classic run --prd is capped with run_timeout: it must get past the insert and
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
  if ! require_deadline; then
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
  # schema.rs: five 0.3.3 guards plus the two re-review triggers. Sorted.
  _want=$(printf '%s\n' \
    relaybook_guard_rereview_authorize \
    relaybook_guard_rereview_publish \
    relaybook_guard_task_report_insert \
    relaybook_guard_task_accept \
    relaybook_guard_claim_base_sha \
    relaybook_guard_run_insert \
    relaybook_guard_run_update | LC_ALL=C sort)
  _got=$(sqlite3 "$_db" "SELECT name FROM sqlite_master WHERE type='trigger' ORDER BY name;")
  if [ "$_got" != "$_want" ]; then
    echo "sqlite3 3.45.1 trigger names differ" >&2
    echo "saw:" >&2
    printf '%s\n' "$_got" >&2
    echo "want:" >&2
    printf '%s\n' "$_want" >&2
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
  run_in_repo "$_gated" "" run_timeout 90 "$_old" run --prd prd.md >"$_out" 2>"$_err" || _rc=$?
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
  run_in_repo "$_gated" "" run_timeout 90 "$_old" run --resume >"$_out" 2>"$_err" || _rc=$?
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
  run_in_repo "$_classic" "" run_timeout 45 "$_old" run --prd prd.md >"$_out" 2>"$_err" || _rc=$?
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
  run_in_repo "$_classic" "" run_timeout 90 "$_old" run --resume >"$_out" 2>"$_err" || _rc=$?
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

# relaybook in a throwaway repo. A non-zero status prints the captured output.
relay_ok() {
  _relay_repo=$1
  _relay_home=$2
  _relay_bin=$3
  shift 3
  if ! run_in_repo "$_relay_repo" "$_relay_home" "$_relay_bin" "$@" >"$smoke_tmp/relay.out" 2>"$smoke_tmp/relay.err"; then
    echo "relaybook $* failed" >&2
    cat "$smoke_tmp/relay.out" "$smoke_tmp/relay.err" >&2
    exit 1
  fi
}

# Gated task report at HEAD, with review start already recorded.
prepare_gated_review_repo() {
  _prep_repo=$1
  _prep_home=$2
  _prep_bin=$3
  init_throwaway "$_prep_repo" "$_prep_home"
  relay_ok "$_prep_repo" "$_prep_home" "$_prep_bin" init
  relay_ok "$_prep_repo" "$_prep_home" "$_prep_bin" task create auth --title "Add auth"
  relay_ok "$_prep_repo" "$_prep_home" "$_prep_bin" config set reviewer codex
  relay_ok "$_prep_repo" "$_prep_home" "$_prep_bin" config set workflow gated
  printf '%s\n' '.relaybook/' 'graphify-out/' > "$_prep_repo/.gitignore"
  run_in_repo "$_prep_repo" "$_prep_home" git add .gitignore
  run_in_repo "$_prep_repo" "$_prep_home" git commit -q -m "ignore"
  relay_ok "$_prep_repo" "$_prep_home" "$_prep_bin" task claim auth --by cursor
  printf 'work\n' > "$_prep_repo/work.txt"
  run_in_repo "$_prep_repo" "$_prep_home" git add work.txt
  run_in_repo "$_prep_repo" "$_prep_home" git commit -q -m "do the work"
  relay_ok "$_prep_repo" "$_prep_home" "$_prep_bin" task done auth --changed shipped
  review_head=$(run_in_repo "$_prep_repo" "$_prep_home" git rev-parse HEAD)
  if ! printf '%s\n' "$review_head" | grep -E -q '^[0-9a-f]{40}$'; then
    echo "HEAD is not a commit sha: $review_head" >&2
    exit 1
  fi
  relay_ok "$_prep_repo" "$_prep_home" "$_prep_bin" review start auth
}

# Real git ls-remote, including a push URL that must not hide the fetch URL.
# Relaybook's review record then refuses the published commit and accepts one
# that is absent. Tracking refs are removed so the refusal comes from ls-remote.
smoke_ls_remote() {
  relaybook_bin=$1
  review_text='{"findings":"none","security":"ok","tests_checked":"cargo test","not_checked":"none"}'
  if ! command -v git >/dev/null 2>&1; then
    echo "git is not on PATH" >&2
    exit 1
  fi
  if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "sqlite3 is not on PATH" >&2
    exit 1
  fi
  smoke_tmp=$(mktemp -d)
  ls_home=$smoke_tmp/home
  present_repo=$smoke_tmp/present
  present_bare=$smoke_tmp/present.git
  absent_repo=$smoke_tmp/absent
  absent_bare=$smoke_tmp/absent.git
  mkdir -p "$ls_home" "$present_repo" "$absent_repo" "$present_bare" "$absent_bare"
  run_in_repo "$present_bare" "$ls_home" git init -q --bare -b main
  run_in_repo "$absent_bare" "$ls_home" git init -q --bare -b main

  prepare_gated_review_repo "$present_repo" "$ls_home" "$relaybook_bin"
  present_head=$review_head
  run_in_repo "$present_repo" "$ls_home" git remote add origin "$present_bare"
  ls_rc=0
  run_in_repo "$present_repo" "$ls_home" git ls-remote --exit-code --quiet --end-of-options origin refs/heads/other >/dev/null 2>&1 || ls_rc=$?
  if [ "$ls_rc" -ne 2 ]; then
    echo "ls-remote missing ref exited $ls_rc, want 2" >&2
    exit 1
  fi
  run_in_repo "$present_repo" "$ls_home" git push origin "HEAD:refs/heads/other"
  run_in_repo "$present_repo" "$ls_home" git for-each-ref --format='%(refname)' refs/remotes >"$smoke_tmp/tracking"
  while IFS= read -r refname; do
    if [ -n "$refname" ]; then
      run_in_repo "$present_repo" "$ls_home" git update-ref -d "$refname"
    fi
  done <"$smoke_tmp/tracking"
  left=$(run_in_repo "$present_repo" "$ls_home" git for-each-ref refs/remotes)
  if [ -n "$left" ]; then
    echo "tracking refs remained after the push" >&2
    printf '%s\n' "$left" >&2
    exit 1
  fi
  run_in_repo "$present_repo" "$ls_home" git config remote.origin.pushurl /nonexistent/push-disabled-by-smoke
  run_in_repo "$present_repo" "$ls_home" git config "url./nonexistent/push-instead.pushInsteadOf" "$present_bare"
  listed=$(run_in_repo "$present_repo" "$ls_home" git ls-remote --quiet --end-of-options origin)
  if ! printf '%s\n' "$listed" | grep -F -q "$present_head"; then
    echo "ls-remote lost the fetch URL after the push rewrite" >&2
    printf '%s\n' "$listed" >&2
    exit 1
  fi
  if ! printf '%s\n' "$listed" | grep -F -q "refs/heads/other"; then
    echo "ls-remote did not list refs/heads/other" >&2
    printf '%s\n' "$listed" >&2
    exit 1
  fi
  run_in_repo "$present_repo" "$ls_home" git ls-remote --exit-code --quiet --end-of-options origin refs/heads/other >/dev/null
  ls_rc=0
  run_in_repo "$present_repo" "$ls_home" git push origin "HEAD:refs/heads/elsewhere" >"$smoke_tmp/push.out" 2>"$smoke_tmp/push.err" || ls_rc=$?
  if [ "$ls_rc" -eq 0 ]; then
    echo "git push used the fetch URL despite pushurl" >&2
    exit 1
  fi
  ls_rc=0
  run_in_repo "$present_repo" "$ls_home" "$relaybook_bin" review record auth \
    --by codex --sha "$present_head" --verdict approved --text "$review_text" \
    >"$smoke_tmp/record.out" 2>"$smoke_tmp/record.err" || ls_rc=$?
  if [ "$ls_rc" -eq 0 ] || { ! grep -F -q "refs/heads/other" "$smoke_tmp/record.out" && ! grep -F -q "refs/heads/other" "$smoke_tmp/record.err"; }; then
    echo "published review record did not fail from ls-remote" >&2
    cat "$smoke_tmp/record.out" "$smoke_tmp/record.err" >&2
    exit 1
  fi
  if ! grep -F -q "on origin" "$smoke_tmp/record.out" && ! grep -F -q "on origin" "$smoke_tmp/record.err"; then
    echo "published review record was not refused by the online check" >&2
    cat "$smoke_tmp/record.out" "$smoke_tmp/record.err" >&2
    exit 1
  fi
  verdict=$(sqlite3 "$present_repo/.relaybook/ledger.db" "SELECT coalesce(verdict, '') FROM reviews WHERE scope = 'task' ORDER BY id DESC LIMIT 1;")
  if [ -n "$verdict" ]; then
    echo "published review stored verdict $verdict" >&2
    exit 1
  fi
  status=$(sqlite3 "$present_repo/.relaybook/ledger.db" "SELECT status FROM remote_checks ORDER BY id DESC LIMIT 1;")
  if [ "$status" != "present" ]; then
    echo "remote check status was $status, want present" >&2
    exit 1
  fi
  detail=$(sqlite3 "$present_repo/.relaybook/ledger.db" "SELECT detail FROM remote_checks ORDER BY id DESC LIMIT 1;")
  if ! printf '%s\n' "$detail" | grep -F -q "push_url_differs=true"; then
    echo "remote check did not record push_url_differs=true" >&2
    printf '%s\n' "$detail" >&2
    exit 1
  fi

  prepare_gated_review_repo "$absent_repo" "$ls_home" "$relaybook_bin"
  absent_head=$review_head
  run_in_repo "$absent_repo" "$ls_home" git remote add origin "$absent_bare"
  ls_rc=0
  run_in_repo "$absent_repo" "$ls_home" git ls-remote --exit-code --quiet --end-of-options origin refs/heads/other >/dev/null 2>&1 || ls_rc=$?
  if [ "$ls_rc" -ne 2 ]; then
    echo "empty remote ls-remote exited $ls_rc, want 2" >&2
    exit 1
  fi
  ls_rc=0
  run_in_repo "$absent_repo" "$ls_home" "$relaybook_bin" review record auth \
    --by codex --sha "$absent_head" --verdict approved --text "$review_text" \
    >"$smoke_tmp/absent.out" 2>"$smoke_tmp/absent.err" || ls_rc=$?
  if [ "$ls_rc" -ne 0 ]; then
    echo "absent remote review record failed" >&2
    cat "$smoke_tmp/absent.out" "$smoke_tmp/absent.err" >&2
    exit 1
  fi
  verdict=$(sqlite3 "$absent_repo/.relaybook/ledger.db" "SELECT coalesce(verdict, '') FROM reviews WHERE scope = 'task' ORDER BY id DESC LIMIT 1;")
  if [ "$verdict" != "approved" ]; then
    echo "absent remote stored verdict $verdict" >&2
    exit 1
  fi
  status=$(sqlite3 "$absent_repo/.relaybook/ledger.db" "SELECT status FROM remote_checks ORDER BY id DESC LIMIT 1;")
  if [ "$status" != "absent" ]; then
    echo "remote check status was $status, want absent" >&2
    exit 1
  fi
  rm -rf "$smoke_tmp"
  smoke_tmp=
  echo "ls-remote: real git, push URL rewrite, and relaybook publish check ok"
}

# Real codex exec with the read-only argv Relaybook uses.
# The review exec must exit 0 and leave one nonempty schema-valid verdict.
# The write denial is a second exec: it must exit 0, must not create
# codex-wrote.txt, and must leave the worktree unchanged. A non-zero exit,
# a missing or invalid verdict, a timeout, or a new file fails. Diagnostics
# are printed before the temporary directory is removed.
smoke_codex() {
  if ! command -v codex >/dev/null 2>&1; then
    echo "codex: not on PATH" >&2
    exit 1
  fi
  if ! require_deadline; then
    exit 1
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    echo "python3 is not on PATH" >&2
    exit 1
  fi
  if ! codex login status </dev/null >/dev/null 2>&1; then
    echo "codex: not signed in; run codex login" >&2
    exit 1
  fi
  codex_help=$(codex exec --help 2>&1 || true)
  for flag in --ephemeral --output-schema --color --sandbox read-only -m; do
    if ! printf '%s\n' "$codex_help" | grep -F -q -- "$flag"; then
      echo "codex exec --help is missing $flag" >&2
      exit 1
    fi
  done
  smoke_tmp=$(mktemp -d)
  codex_repo=$smoke_tmp/repo
  codex_schema=$smoke_tmp/codex-schema.json
  codex_out=$smoke_tmp/codex-last.md
  probe_out=$smoke_tmp/codex-probe.md
  mkdir -p "$codex_repo"
  init_throwaway "$codex_repo" ""
  printf 'fn main() {}\n' > "$codex_repo/src-main.rs"
  run_in_repo "$codex_repo" "" git add src-main.rs
  run_in_repo "$codex_repo" "" git commit -q -m "tiny app"
  before=$(run_in_repo "$codex_repo" "" git status --porcelain)
  codex_model=gpt-6.1-sol
  cat >"$codex_schema" <<'CODEX_TASK_SCHEMA'
{
  "type": "object",
  "additionalProperties": false,
  "required": ["verdict", "body"],
  "properties": {
    "verdict": { "type": "string", "enum": ["approved", "changes-requested"] },
    "body": {
      "type": "object",
      "additionalProperties": false,
      "required": ["findings", "security", "tests_checked", "not_checked"],
      "properties": {
        "findings": { "type": "array", "items": { "type": "string" } },
        "security": { "type": "array", "items": { "type": "string" } },
        "tests_checked": { "type": "array", "items": { "type": "string" } },
        "not_checked": { "type": "array", "items": { "type": "string" } }
      }
    }
  }
}
CODEX_TASK_SCHEMA
  cat >"$smoke_tmp/check_verdict.py" <<'PY'
import json
import sys

verdict_path, schema_path = sys.argv[1], sys.argv[2]

def fail():
    raise SystemExit(1)

def check(schema, value):
    if not isinstance(schema, dict):
        fail()
    if "enum" in schema and value not in schema["enum"]:
        fail()
    kind = schema.get("type")
    if kind == "object":
        if schema.get("additionalProperties") is not False:
            fail()
        if not isinstance(value, dict):
            fail()
        props = schema.get("properties")
        required = schema.get("required")
        if not isinstance(props, dict) or not isinstance(required, list):
            fail()
        if set(props) != set(required) or set(value) != set(required):
            fail()
        for key, child in props.items():
            check(child, value[key])
        return
    if kind == "string":
        if not isinstance(value, str):
            fail()
        return
    if kind == "array":
        if not isinstance(value, list):
            fail()
        items = schema.get("items")
        if not isinstance(items, dict):
            fail()
        for item in value:
            check(items, item)
        return
    if "enum" in schema:
        return
    fail()

try:
    raw = open(verdict_path, encoding="utf-8").read()
    schema = json.load(open(schema_path, encoding="utf-8"))
except OSError:
    raise SystemExit(1)
text = raw.strip()
if not text:
    raise SystemExit(1)
try:
    value = json.loads(text)
except json.JSONDecodeError:
    raise SystemExit(1)
check(schema, value)
PY
  printf '%s\n' "packet: leave the repository unchanged" >"$smoke_tmp/packet.txt"
  codex_prompt="Answer with the output schema only: verdict approved, and body findings, security, tests_checked, and not_checked each an empty array of strings. Leave the repository unchanged."
  probe_prompt="Create a file named codex-wrote.txt in the working directory containing the word written."
  codex_rc=0
  (
    clear_git_repo_env
    cd "$codex_repo" || exit 1
    run_timeout 180 codex exec -s read-only --ephemeral --color never -C "$codex_repo" -m "$codex_model" --output-schema "$codex_schema" -o "$codex_out" "$codex_prompt" <"$smoke_tmp/packet.txt"
  ) >"$smoke_tmp/codex.out" 2>"$smoke_tmp/codex.err" || codex_rc=$?
  if [ "$codex_rc" -ne 0 ]; then
    if [ "$codex_rc" -eq 124 ]; then
      codex_fail "codex review exec timed out" "$codex_out"
    fi
    if grep -F -q "unexpected argument" "$smoke_tmp/codex.err" || grep -F -q "unexpected argument" "$smoke_tmp/codex.out" || grep -F -q "unrecognized" "$smoke_tmp/codex.err" || grep -F -q "invalid value" "$smoke_tmp/codex.err"; then
      codex_fail "codex rejected the review argv" "$codex_out"
    fi
    codex_fail "codex review exec failed (rc $codex_rc)" "$codex_out"
  fi
  if [ ! -f "$codex_out" ]; then
    codex_fail "codex review verdict is missing" "$codex_out"
  fi
  verdict_rc=0
  python3 "$smoke_tmp/check_verdict.py" "$codex_out" "$codex_schema" || verdict_rc=$?
  if [ "$verdict_rc" -ne 0 ]; then
    codex_fail "codex review verdict is not schema-valid" "$codex_out"
  fi
  codex_assert_clean "$codex_out"
  codex_rc=0
  (
    clear_git_repo_env
    cd "$codex_repo" || exit 1
    run_timeout 180 codex exec -s read-only --ephemeral --color never -C "$codex_repo" -m "$codex_model" -o "$probe_out" "$probe_prompt" </dev/null
  ) >"$smoke_tmp/codex.out" 2>"$smoke_tmp/codex.err" || codex_rc=$?
  if [ "$codex_rc" -ne 0 ]; then
    if [ "$codex_rc" -eq 124 ]; then
      codex_fail "codex sandbox probe timed out" "$probe_out"
    fi
    codex_fail "codex sandbox probe failed (rc $codex_rc)" "$probe_out"
  fi
  codex_assert_clean "$probe_out"
  codex_version=$(codex --version 2>&1 | awk 'NR==1 { print; exit }')
  rm -rf "$smoke_tmp"
  smoke_tmp=
  echo "codex: $codex_version read-only left the repository unchanged and recorded a schema-valid verdict"
}

codex_fail() {
  echo "$1" >&2
  if [ -f "$smoke_tmp/codex.out" ]; then
    cat "$smoke_tmp/codex.out" >&2
  fi
  if [ -f "$smoke_tmp/codex.err" ]; then
    cat "$smoke_tmp/codex.err" >&2
  fi
  if [ -n "${2-}" ]; then
    if [ -f "$2" ]; then
      echo "--- verdict ---" >&2
      cat "$2" >&2
    else
      echo "verdict file missing: $2" >&2
    fi
  fi
  rm -rf "$smoke_tmp"
  smoke_tmp=
  exit 1
}

codex_assert_clean() {
  if [ -e "$codex_repo/codex-wrote.txt" ]; then
    codex_fail "codex read-only wrote codex-wrote.txt" "$1"
  fi
  after=$(run_in_repo "$codex_repo" "" git status --porcelain)
  if [ "$before" != "$after" ]; then
    printf 'before: %s\n' "$before" >&2
    printf 'after: %s\n' "$after" >&2
    codex_fail "codex read-only changed the worktree" "$1"
  fi
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
  rm -rf "$work" "$graph" "$graph_home" "$oldsrc" ${smoke_tmp:+"$smoke_tmp"} ${_gated:+"$_gated"} ${_classic:+"$_classic"}
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
  rm -rf "$work" "$graph" "$graph_home" "$oldsrc" ${smoke_tmp:+"$smoke_tmp"} ${_gated:+"$_gated"} ${_classic:+"$_classic"}
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
smoke_ls_remote "$relaybook"
smoke_codex

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
