# Configuration

## Setup

`relaybook setup` creates the ledger when it is missing, appends the planner block to `CLAUDE.md` and the executor block to `AGENTS.md`, writes `/relaybook` for Claude Code and Cursor, then runs `graphify claude install`, `graphify cursor install`, `graphify hook install`, and `graphify update .` when `graphify` is on `PATH`. The update is AST-only and does not call an LLM. Run it again whenever you want: existing instruction blocks, command files that carry the `relaybook:managed` marker, and hooks are left as they are. A command file without that marker is skipped.

```sh
relaybook setup
relaybook setup --no-graphify
relaybook setup --no-hooks
relaybook setup --no-claude-hook
relaybook setup --no-commands
relaybook setup --ci
```

- `relaybook setup --no-graphify` skips every Graphify command and the pull hooks.
- `relaybook setup --no-hooks` skips `graphify hook install` and the post-merge / post-rewrite hooks.
- `relaybook setup --no-claude-hook` skips the project `UserPromptSubmit` hook that tells Claude to run `relaybook inbox`. The other Graphify commands still run.
- `relaybook setup --no-commands` skips `.claude/commands/relaybook.md` and `.cursor/commands/relaybook.md`.
- `relaybook setup --ci` writes `.github/workflows/graphify-update.yml` and adds `graphify-out/graph.json merge=graphify` to `.gitattributes`. An existing workflow file is left untouched. The workflow opens a pull request instead of pushing to `main`.

## How the graph stays current

Local hooks are the default. `graphify hook install` refreshes the graph after a commit and a checkout. `relaybook setup` adds post-merge and post-rewrite hooks that run `graphify update .` in the background, so `git pull` and `git pull --rebase` return immediately. `relaybook setup --no-hooks` leaves those hooks out.

`relaybook setup --ci` is the opt-in GitHub Actions path for teams that want the graph committed. Pushes that only change `graphify-out/` do not start another run. Delete `.github/workflows/graphify-update.yml` to turn that job off.

The workflow opens a pull request, which needs Settings > Actions > General > Allow GitHub Actions to create and approve pull requests. A pull request opened with `GITHUB_TOKEN` does not trigger other workflows, so required checks will not run on `graphify/update`. Set the `GRAPHIFY_PR_TOKEN` secret to a fine-grained PAT or GitHub App token with contents and pull-requests write when those checks must run. Without that secret the workflow uses `GITHUB_TOKEN`.

`relaybook setup --ci` runs `graphify extract` only when one of these GitHub secrets is set: `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, `MOONSHOT_API_KEY`, or `AZURE_OPENAI_API_KEY` with `AZURE_OPENAI_ENDPOINT`. Extract sends repository text to that provider and spends API credits. With no secret, the job runs `graphify update`.

## Settings

`relaybook config` prints the current values. `relaybook status` prints the mode, the limits, and the active run.

```sh
relaybook config
relaybook config set gates plan
relaybook config set max_cycles 20
relaybook config set max_reopens_per_task 2
relaybook config set max_agent_calls 60
relaybook config set cursor_timeout_min 30
relaybook config set est_cost_cap_usd 5
relaybook config set est_cost_cap_usd unset
relaybook status
relaybook mode
relaybook mode manual
relaybook mode auto
relaybook gates
relaybook gates none
relaybook gates plan
relaybook gates plan,review
```

| Key | Default | Values |
| --- | --- | --- |
| `mode` | `manual` | `manual` or `auto` |
| `gates` | `plan` | `none`, `plan`, or `plan,review` |
| `max_cycles` | 20 | integer, at least 1 |
| `max_reopens_per_task` | 2 | integer, at least 1 |
| `max_agent_calls` | 60 | integer, at least 1 |
| `cursor_timeout_min` | 30 | integer, at least 1 |
| `est_cost_cap_usd` | unset | a number greater than 0, or `unset` to clear it |

`relaybook mode` prints the current mode. `relaybook mode auto` sets it. `relaybook gates` prints the current checkpoints. `relaybook gates plan` sets them. The same words work from `/relaybook` in Claude Code and Cursor. `/relaybook` with no arguments shows status and a menu.

Dollars Claude reports are API-equivalent estimates. `est_cost_cap_usd` is optional and is mainly for API-key users. Hitting a plan usage limit pauses the run instead of failing it.

## Graphify and RTK

Graphify (Apache-2.0, installed by default) keeps code questions on the graph instead of a full-repo read. RTK (Apache-2.0, maintained separately) compacts git, test, build, and grep output. Relaybook compacts the work ledger.

```sh
curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh -s -- --with-rtk
curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh -s -- --no-graphify
```

`--yes` also runs `rtk init -g` without the y/N prompt. Without a terminal and without `--yes`, the installer prints `rtk init -g` instead of editing `~/.claude/settings.json`. `relaybook setup` never installs RTK and never edits that global file. The pull hooks run `graphify update .` in the background and print nothing. Graphify's editor hook does not rewrite `relaybook` commands.

`scripts/measure-context.sh --check` fails when fixture context exceeds 4000 bytes, a write acknowledgement exceeds 120 bytes, or an instruction block exceeds 600 bytes. Role prompts stay under 1200 bytes. Slash commands stay under 1500 bytes.
