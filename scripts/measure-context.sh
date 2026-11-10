#!/bin/sh
# Measure Relaybook context cost against a fixture ledger. Never reads the real ledger.
set -eu

check=0
over=0
if [ "${1:-}" = "--check" ]; then
  check=1
fi

root=$(cd "$(dirname "$0")/.." && pwd)
bin=${RELAYBOOK_BIN:-$root/target/debug/relaybook}
if [ ! -x "$bin" ]; then
  echo "building $bin" >&2
  (cd "$root" && cargo build -q --bin relaybook)
fi

work=$(mktemp -d)
cleanup() {
  rm -rf "$work"
}
trap cleanup EXIT

cd "$work"
"$bin" init >/dev/null

i=1
while [ "$i" -le 40 ]; do
  "$bin" task create "done-$i" --title "Done task $i" >/dev/null
  # shellcheck disable=SC1010
  "$bin" task done "done-$i" >/dev/null
  i=$((i + 1))
done

"$bin" task create alpha --title "Alpha open" >/dev/null
"$bin" task create beta --title "Beta claimed" >/dev/null
"$bin" task create gamma --title "Gamma blocked" >/dev/null
"$bin" task create delta --title "Delta open" --depends-on alpha >/dev/null
"$bin" task create epsilon --title "Epsilon claimed" --depends-on done-1 >/dev/null
"$bin" task claim beta --by cursor >/dev/null
"$bin" task claim epsilon --by cursor >/dev/null
"$bin" task block gamma --reason "stuck on a decision" >/dev/null

for slug in alpha beta gamma delta epsilon; do
  n=1
  while [ "$n" -le 5 ]; do
    "$bin" step add "$slug" "step $n for $slug" >/dev/null
    n=$((n + 1))
  done
  "$bin" step check "$slug" 1 >/dev/null
  "$bin" step check "$slug" 2 >/dev/null
  "$bin" decision log "$slug" --why "first decision for $slug" --by claude-code >/dev/null
  "$bin" decision log "$slug" --why "second decision for $slug with a longer rationale that should still be counted" --by cursor >/dev/null
done

measure() {
  label=$1
  shift
  output=$("$@" 2>/dev/null || true)
  bytes=$(printf '%s' "$output" | wc -c | tr -d ' ')
  tokens=$((bytes / 4))
  if [ "$check" -eq 1 ]; then
    limit=0
    case $label in
      context) limit=4000 ;;
      write-*) limit=120 ;;
    esac
    if [ "$limit" -gt 0 ] && [ "$bytes" -gt "$limit" ]; then
      echo "over budget: $label is $bytes bytes (limit $limit)" >&2
      over=1
    fi
  fi
  exact=""
  if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
    exact=$(count_tokens "$output")
  fi
  if [ -n "$exact" ]; then
    printf '%-32s %8s bytes %8s est %8s api\n' "$label" "$bytes" "$tokens" "$exact"
  else
    printf '%-32s %8s bytes %8s est\n' "$label" "$bytes" "$tokens"
  fi
}

count_tokens() {
  text=$1
  model=${ANTHROPIC_MODEL:-claude-sonnet-4-5}
  body=$(printf '%s' "$text" | python3 -c 'import json,sys; print(json.dumps({"model": sys.argv[1], "messages": [{"role": "user", "content": sys.stdin.read()}]}))' "$model")
  response=$(curl -fsS https://api.anthropic.com/v1/messages/count_tokens \
    -H "x-api-key: $ANTHROPIC_API_KEY" \
    -H "anthropic-version: 2023-06-01" \
    -H "content-type: application/json" \
    -d "$body" 2>/dev/null || true)
  printf '%s' "$response" | python3 -c 'import json,sys
try:
    print(json.load(sys.stdin).get("input_tokens", ""))
except Exception:
    print("")'
}

echo "fixture ledger in $work"
measure context "$bin" context
measure context-json "$bin" context --json
measure context-slug "$bin" context alpha
measure task-list "$bin" task list
measure task-list-ready "$bin" task list --ready
measure write-create "$bin" task create sample --title "Sample task"
measure write-step "$bin" step add sample "write a test"
measure write-edit "$bin" step edit sample 1 --text "rewrite the test"
measure write-claim "$bin" task claim sample --by cursor
measure write-progress "$bin" progress report sample --note "started"
measure write-decision "$bin" decision log sample --why "keep the ack short" --by cursor
measure write-depend "$bin" task depend sample --on alpha
measure write-undepend "$bin" task undepend sample --on alpha
measure write-rename "$bin" task rename sample --title "Sample renamed"
measure write-block "$bin" task block sample --reason "paused"
measure write-unblock "$bin" task unblock sample --note "resumed"
# shellcheck disable=SC1010
measure write-done "$bin" task done sample

planner=$(wc -c < "$root/crates/relaybook/templates/claude.md" | tr -d ' ')
executor=$(wc -c < "$root/crates/relaybook/templates/agents.md" | tr -d ' ')
printf '%-32s %8s bytes %8s est\n' planner-block "$planner" "$((planner / 4))"
printf '%-32s %8s bytes %8s est\n' executor-block "$executor" "$((executor / 4))"

if [ "$check" -eq 1 ]; then
  if [ "$planner" -gt 600 ] || [ "$executor" -gt 600 ]; then
    echo "over budget: an instruction block exceeds 600 bytes" >&2
    over=1
  fi
  if command -v rtk >/dev/null 2>&1; then
    rtk_out="$work/rtk-out"
    if rtk rewrite "relaybook context" >"$rtk_out" 2>/dev/null; then
      echo "rtk rewrote relaybook context" >&2
      over=1
    elif [ -s "$rtk_out" ]; then
      echo "rtk printed a rewrite for relaybook context" >&2
      over=1
    fi
  fi
  exit "$over"
fi
