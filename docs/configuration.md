# Configuration

## Setup

`relaybook setup` creates the ledger when it is missing, writes the planner block to `CLAUDE.md` and the executor block to `AGENTS.md`, writes `/relaybook` for Claude Code and Cursor, writes `.claude/skills/relaybook/SKILL.md`, then runs `graphify claude install`, `graphify cursor install`, `graphify hook install`, and `graphify update .` when `graphify` is on `PATH`. The update is AST-only and does not call an LLM. Run it again whenever you want. Instruction blocks between the `relaybook` markers are replaced, and setup reports `updated agent instructions`. Command files and the skill that carry the `relaybook:managed` marker are refreshed. A file without that marker is skipped. Hooks that are already installed stay as they are.

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
- `relaybook setup --no-claude-hook` skips the project `UserPromptSubmit` hook and the `SessionStart` hook. The other Graphify commands still run.
- `relaybook setup --no-commands` skips `.claude/commands/relaybook.md` and `.cursor/commands/relaybook.md`.
- `relaybook setup --ci` writes `.github/workflows/graphify-update.yml` and adds `graphify-out/graph.json merge=graphify` to `.gitattributes`. An existing workflow file is left untouched. The workflow opens a pull request instead of pushing to `main`. `graphify-out/` stays tracked.

`relaybook setup` writes a `# relaybook:start` / `# relaybook:end` block in `.gitignore` and, with `--ci`, in `.gitattributes`. The entries sit on their own lines. A pattern you already ignore is left where it is. The ignore block covers Graphify backups, `graphify-out/` (unless you passed `--ci`), `.relaybook/*.db-wal`, `.relaybook/*.db-shm`, and `.relaybook/runs/`. `.relaybook/ledger.db` stays committable.

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

`scripts/measure-context.sh --check` fails when fixture context exceeds 4000 bytes, a write acknowledgement exceeds 120 bytes, or an instruction block exceeds 1000 bytes. Role prompts stay under 1200 bytes. Slash commands stay under 1500 bytes. `relaybook guide` stays under 4000 bytes per role.

## Uninstall

`relaybook uninstall` reverses what `relaybook setup` added. It prints one line per item, then asks `y/N`. `--yes` skips the question. `--dry-run` prints the list and changes nothing. A file setup created is `removed`. A file that already existed is `restored`. `--dry-run` says `would remove`, `would restore`, or `would keep`. An empty folder that setup created is removed when nothing else is left in it. A folder that existed before setup stays.

```sh
relaybook uninstall --dry-run
relaybook uninstall --yes
relaybook uninstall --yes --purge
relaybook uninstall --yes --graphify
```

| Flag | Effect |
| --- | --- |
| `--dry-run` | Show what would change. Write nothing |
| `--yes` | Apply without asking |
| `--purge` | Also delete `.relaybook/` and `graphify-out/`. Asks `y/N` unless `--yes` is set |
| `--graphify` | Also run `graphify claude uninstall`, `graphify cursor uninstall`, and `graphify hook uninstall` |

What it removes, and only inside Relaybook's markers or the setup manifest:

- The Relaybook blocks in `CLAUDE.md` and `AGENTS.md`. A file that becomes empty is deleted. Other text stays.
- `.claude/commands/relaybook.md` and `.cursor/commands/relaybook.md` when they still contain `relaybook:managed`. An edited command file is kept.
- The `UserPromptSubmit` hook. Setup writes `command -v relaybook >/dev/null 2>&1 && relaybook hook prompt || true`, so the hook stays silent when `relaybook` is not on PATH. On Windows, when Git Bash `bash.exe` is not on PATH (the `System32` WSL stub does not count), setup writes `if (Get-Command relaybook -ErrorAction SilentlyContinue) { relaybook hook prompt }` with `"shell": "powershell"`. A bare `relaybook hook prompt` is rewritten to the guarded POSIX command. Uninstall removes the guarded command, the bare command, and the PowerShell command.
- The `SessionStart` hook. It uses the same guarded POSIX command and PowerShell command, with `relaybook hook session`. It prints one line when ready tasks or unreviewed reports are waiting, and nothing otherwise. Uninstall removes all three forms.
- `.claude/skills/relaybook/SKILL.md` when it still contains `relaybook:managed`. The skill tells Claude to run `relaybook guide planner` and `relaybook context`. Its `allowed-tools` list pre-approves the safe commands for the turn that opens the skill. An edited skill without that marker is kept.
- The safe `permissions.allow` rules in `.claude/settings.json`. A skill grant clears on the next message, so setup writes the same commands into the project permissions for the rest of the session. Those commands are `relaybook context`, `inbox`, `guide`, `status`, `task create`, `task list`, `task reopen`, `task depend`, `step add`, `step edit`, `decision log`, `report accept`, and `report note`. Claim, done, run, approve, setup, uninstall, mode, and gates still ask, because they execute work or change the project. Uninstall removes only those Relaybook rules and leaves every other allow rule in place.
- The `command -v graphify` guards wrapped around Graphify's `PreToolUse` hooks. Graphify's own hooks stay.
- The post-merge and post-rewrite blocks between the relaybook hook markers.
- The `# relaybook:start` / `# relaybook:end` block in `.gitignore` and `.gitattributes`, including an older `pattern # relaybook` line and a lone `# relaybook` line directly above a pattern Relaybook owns. Other lines stay.
- `.github/workflows/graphify-update.yml` when it still matches the template, the graphify merge attribute, and `merge.graphify.name` / `merge.graphify.driver`.

What it keeps unless you pass `--purge`:

- `.relaybook/`, including the ledger. That is project data.
- `graphify-out/`.
- Graphify and RTK themselves. `--graphify` only calls Graphify's project uninstall commands.

Uninstall refuses while an auto run is active. Stop it with `relaybook run stop` first. Running uninstall again is safe.

The machine installer is separate:

```sh
curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh -s -- --uninstall
```

Windows PowerShell:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/Spottie97/relaybook/main/install.ps1))) -Uninstall
```

That removes the `relaybook` binary the installer wrote. Graphify and RTK are removed only when the installer added them, and only after a `y/N` for each tool (`--yes` skips the questions). Tools that were already installed are kept. If another `relaybook` remains on `PATH` from `~/.cargo/bin`, the installer prints `cargo uninstall relaybook` instead of deleting it. Run `relaybook uninstall` in each project before removing the binary.
