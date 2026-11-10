# Relaybook

**Relaybook is a shared coordination layer for coding agents.** A single Rust binary
that keeps tasks, progress, and decisions in one SQLite ledger, so agents working in
the same repo stop stepping on each other.

Graphify maps the code — the "what is this and where" (the map). Relaybook tracks the
work — the "who is doing what and why" (the bus). Use Graphify for structure, Relaybook
for state.

## Quick start

```sh
curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh
```

Then, in the project:

```sh
relaybook setup
```

Upgrade an existing install with:

```sh
curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh -s -- --upgrade
```

Any agent that can run a shell command can join. No protocol, no server, no lock-in.

## Daily workflow

```text
relaybook task list --ready
relaybook task claim auth --by cursor
relaybook progress report auth --note "..."
relaybook task done auth
```

## Setup

`relaybook setup` creates the ledger when it is missing, appends the planner block to
`CLAUDE.md` and the executor block to `AGENTS.md`, then runs `graphify claude install`,
`graphify cursor install`, `graphify hook install`, and `graphify update .` when
`graphify` is on `PATH`. The update is AST-only and does not call an LLM. Run it again
whenever you want: existing instruction blocks and hooks are left as they are.

- `--no-graphify` skips every Graphify command and the pull hooks.
- `--no-hooks` skips `graphify hook install` and the post-merge / post-rewrite hooks.
  The other Graphify commands still run.
- `--ci` writes `.github/workflows/graphify-update.yml` and adds
  `graphify-out/graph.json merge=graphify` to `.gitattributes`. An existing workflow
  file is left untouched. The workflow opens a pull request instead of pushing to
  `main`. It runs `graphify extract` only when one of these GitHub secrets is set:
  `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`,
  `DEEPSEEK_API_KEY`, `MOONSHOT_API_KEY`, or `AZURE_OPENAI_API_KEY` (with
  `AZURE_OPENAI_ENDPOINT`). Extract sends repository text to that provider and spends
  API credits. With no secret, the job runs `graphify update`.

## How the graph stays current

Local hooks are the default. `graphify hook install` refreshes the graph after a
commit and a checkout. `relaybook setup` adds post-merge and post-rewrite hooks that
run `graphify update .` in the background, so `git pull` and `git pull --rebase`
return immediately. `relaybook setup --no-hooks` leaves those hooks out.

`relaybook setup --ci` is the opt-in GitHub Actions path for teams that want the graph
committed. Pushes that only change `graphify-out/` do not start another run. Delete
`.github/workflows/graphify-update.yml` to turn that job off.

The workflow opens a pull request, which needs Settings > Actions > General >
Allow GitHub Actions to create and approve pull requests. A pull request opened
with `GITHUB_TOKEN` does not trigger other workflows, so required checks will not
run on `graphify/update`. Set the `GRAPHIFY_PR_TOKEN` secret to a fine-grained PAT
or GitHub App token with contents and pull-requests write when those checks must
run. Without that secret the workflow uses `GITHUB_TOKEN`.

## Troubleshooting

- The installer supports linux x86_64 and macos aarch64. On another machine, use
  `cargo build --release`.
- If `~/.local/bin` is not on `PATH`, the installer still finishes, then prints the
  `export PATH=...` line to add to `~/.zshrc` or `~/.bashrc`. `RELAYBOOK_INSTALL_DIR`
  chooses a different directory. A plain re-run skips an existing binary. Update it
  with `--upgrade`.
- If Graphify is missing, install [uv](https://docs.astral.sh/uv/) and run
  `uv tool install graphifyy`. `pipx install graphifyy` is the fallback. Setup still
  finishes and skips the Graphify steps. Skip Graphify during install with
  `curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh -s -- --no-graphify`.
- If `core.hooksPath` is set, or the repo uses Husky or Lefthook, setup does not write
  `.git/hooks`. It prints where to add post-merge and post-rewrite commands that run
  `graphify update .` in the background.

## Build

```sh
cargo build --release
```

The binary is `target/release/relaybook`. It is a single static executable: SQLite is
compiled in via `rusqlite` with the `bundled` feature. There is no server and no daemon.

## Ledger

`relaybook init` creates `.relaybook/ledger.db` at the git repository root (or in the
current directory when you are not in a work tree). Later commands walk up from the
working directory until they find that file.

The database runs in WAL mode with a 5 second busy timeout. Tasks, steps, and decisions
are the current projection. The `events` table is append-only (`task_created`, `claimed`,
`progress`, `decision`, `status_changed`, `dependency_changed`, `task_renamed`); Relaybook
never updates or deletes a row there.
`relaybook task block <slug> --reason "..."` sets `status=blocked` on an open or claimed
task and records why. Blocking a task that is already blocked succeeds and writes no new
event. A done task cannot be blocked.

WAL sidecar files (`.db-wal`, `.db-shm`) are gitignored. The ledger file itself is safe to
commit after a command exits; each write checkpoints so the `.db` file holds the data.

## Commands

Every command prints plain text by default. Add `--json` for a single JSON envelope on
stdout. Exit `0` on success, `1` on an operational failure, and `2` on a usage error or
when the ledger is missing.

```text
relaybook init
relaybook context [slug]
relaybook task create <slug> --title "..." [--depends-on <slug>]
relaybook task list [--status open|claimed|done|blocked] [--ready]
relaybook task depend <slug> --on <slug>
relaybook task undepend <slug> --on <slug>
relaybook task claim <slug> --by <agent>
relaybook task block <slug> --reason "..."
relaybook task unblock <slug> [--note "..."]
relaybook task rename <slug> --title "..."
relaybook task done <slug>
relaybook step add <slug> "..."
relaybook step check <slug> <n>
relaybook progress report <slug> --note "..."
relaybook decision log <slug> --why "..." --by <agent>
relaybook install --claude --cursor
relaybook setup [--no-graphify] [--no-hooks] [--ci]
```

Slugs match `[a-z0-9]+(-[a-z0-9]+)*` and stay permanent. `task rename` changes the title
only, and writes a `task_renamed` event. The same title again succeeds and writes nothing.
A missing slug fails with `no such task: <slug>`. Checklist positions start at 1.
`install` appends the planner block to `CLAUDE.md` (`--claude`) and the executor block
to `AGENTS.md` (`--cursor`). A second run does not change a file that already has the
Relaybook markers or the coordination heading. Text outside the block is kept.

## JSON

Success and failure both use one envelope:

```json
{"ok": true, "data": {}, "error": null}
{"ok": false, "data": null, "error": "already claimed"}
```

`--json` writes that object to stdout and nothing else. Human-readable errors go to stderr.

## Claims and progress

`task claim` takes a write lock and runs `UPDATE ... WHERE status='open'`. One agent
wins. A missing slug fails with `no such task: <slug>`. Any other status, including a
claim that lost the race, fails with `already claimed` and exit code 1. A task whose
dependencies are not done fails with `waiting on: <slug>, <slug>` and stays open.
`blocked` is separate: it means someone marked the work stuck. A dependency clears
itself when the prerequisite is done.

`task list --ready` shows open tasks whose dependencies are all done. Human output
appends `needs: a, b` for unfinished dependencies. JSON tasks include a `depends_on`
array of every link. Adding a link that already exists, or removing one that does not,
succeeds and writes no event. A self-link or a cycle is rejected.

`progress report` reads `git rev-parse HEAD` when that succeeds and stores the commit
in the event payload. A second report for the same task and commit fails with
`already reported for <sha>`. Outside a git checkout the commit is null, and each report
is appended. `task done` on a task that is already done succeeds and does not write
another event.

## Context cost

`relaybook context` shows active tasks only. Checked steps collapse to `n/m done`,
older decisions stay behind `relaybook context <slug>`, and finished tasks stay behind
`relaybook context --all`. Events stay behind `relaybook context --events N`. On a
40-done fixture, context fell from 4567 bytes (~1141 tokens) to 1482 bytes (~370 tokens).
A planning burst of one create, five steps, and a decision fell from 232 bytes to 152.
Write commands answer with an id, slug, and status. Read commands still return the
full record.

Graphify answers where the code lives. Relaybook answers who is doing the work. Both
default to a small answer and name the command that restores the rest. The pull hooks
run `graphify update .` in the background and print nothing. Graphify's editor hook
does not rewrite `relaybook` commands.

`scripts/measure-context.sh --check` fails when fixture context exceeds 4000 bytes, a
write acknowledgement exceeds 120 bytes, or an instruction block exceeds 600 bytes.

## Library

`relaybook-core` owns the ledger. A future MCP adapter, or any other front end, should
call that crate. It should not invent a second protocol or a second store. The `relaybook`
binary is only the CLI over the same API.

## License

Apache-2.0. See [LICENSE](LICENSE).
