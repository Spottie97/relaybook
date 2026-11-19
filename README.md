<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
    <img alt="Relaybook" src="assets/logo-light.svg" width="420">
  </picture>
</h1>

<p align="center">Shared work ledger for coding agents.</p>

<p align="center">
  <a href="https://github.com/Spottie97/relaybook/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/Spottie97/relaybook/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/Spottie97/relaybook/releases"><img alt="Latest release" src="https://img.shields.io/github/v/release/Spottie97/relaybook"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <img alt="Platforms: macOS arm64, Linux x86_64, Windows x64, and Windows ARM64" src="https://img.shields.io/badge/platforms-macOS%20arm64%20%7C%20Linux%20x86__64%20%7C%20Windows%20x64%20%7C%20Windows%20ARM64-lightgrey">
</p>

<p align="center">
  <a href="#quick-start">Install</a>
  · <a href="#how-it-works">How it works</a>
  · <a href="#auto-mode">Auto mode</a>
  · <a href="#gated-workflow">Gated workflow</a>
  · <a href="#docs">Docs</a>
  · <a href="CONTRIBUTING.md">Contributing</a>
</p>

## What it is

Relaybook is a shared coordination layer for coding agents. One Rust binary keeps tasks, progress, and decisions in a SQLite ledger, so agents in the same repo stop stepping on each other.

Graphify maps the code. Relaybook tracks the work. Use Graphify for structure and Relaybook for state.

## Why

Agents step on the same task, re-explain state every session, and pay context cost for work that is already finished. The ledger is the hand-off. One agent plans, another executes, and the report comes back through an inbox. Finished work stays behind a command.

## Features

- Tasks, an atomic claim, dependencies, and an append-only event log
- A short report on `relaybook task done`, and `relaybook inbox` for the planner
- Auto mode: Claude plans, Cursor implements, one task at a time. Claude reviews by default, and Codex reviews with `workflow=gated`
- `/relaybook` in Claude Code and Cursor
- Compact `relaybook context` by default, with the full history one flag away
- Setup teaches each agent its role: Claude plans, Cursor implements, and `relaybook guide` holds the planner, executor, and reviewer workflows. The reviewer guide is for Codex in gated mode
- Gated workflow: Codex reviews each task and the branch at `HEAD`, the maintainer signs off the commit, and nothing is published before that

## Quick start

macOS, Linux, and Windows Git Bash:

```sh
curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/Spottie97/relaybook/main/install.ps1 | iex
```

The same `curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh` line works in Git Bash. Open a new terminal on Windows after the installer changes PATH.

The installers download the latest [release](https://github.com/Spottie97/relaybook/releases): `relaybook-linux-x86_64.tar.gz`, `relaybook-macos-aarch64.tar.gz`, `relaybook-windows-x86_64.zip`, or `relaybook-windows-aarch64.zip`. Each archive holds the one binary.

Then, in the project:

```sh
relaybook setup
```

Setup takes `--no-graphify`, `--no-hooks`, `--no-claude-hook`, `--no-commands`, and `--ci`. See [Setup](docs/configuration.md#setup).

Upgrade an existing install with:

```sh
curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh -s -- --upgrade
```

Windows PowerShell passes flags through a scriptblock, because a script piped into `iex` does not receive them:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/Spottie97/relaybook/main/install.ps1))) -Upgrade
```

After an upgrade, run `relaybook setup` again in each project to refresh the agent instructions.

Any agent that can run a shell command can join. There is no server and no protocol.

## Uninstall

In a project, `relaybook uninstall` removes the files and hooks setup added. The ledger stays. `relaybook uninstall --dry-run` lists the changes first. `relaybook uninstall --purge` also deletes `.relaybook/` and `graphify-out/`. `relaybook uninstall --graphify` also runs Graphify's own uninstall commands. See [Uninstall](docs/configuration.md#uninstall).

On a machine, macOS, Linux, and Windows Git Bash:

```sh
curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh -s -- --uninstall
```

Windows PowerShell:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/Spottie97/relaybook/main/install.ps1))) -Uninstall
```

<img alt="Demo: setup, claim a task, file a report, and read the inbox" src="docs/demo.gif" width="720">

## How it works

```mermaid
flowchart LR
  planner[Planner] --> ledger[Ledger]
  ledger --> executor[Executor]
  executor --> report[Report]
  report --> inbox[Inbox]
  inbox --> review[Review]
```

```sh
relaybook task list --ready
relaybook task claim greet --by cursor
relaybook task done greet --changed "said hello" --verified "cargo test"
relaybook inbox
```

The executor claims a ready task and finishes with a report under 1,500 bytes. The planner runs `relaybook inbox`, then `relaybook report accept greet` or `relaybook task reopen greet --reason "needs a test"`. A reopened task returns to `relaybook task list --ready`.

## Auto mode

Manual mode is the default. `/relaybook` shows the current status and a menu. `/relaybook mode` prints the current mode. `/relaybook mode manual` and `/relaybook mode auto` set it. `/relaybook gates` prints the checkpoints. `/relaybook gates none`, `/relaybook gates plan`, and `/relaybook gates plan,review` set them. The terminal commands do the same: `relaybook mode` and `relaybook gates` print the current value, and a value sets it.

| Action | Slash command | Terminal |
| --- | --- | --- |
| Status and the menu | `/relaybook` | `relaybook status` |
| Mode | `/relaybook mode` or `/relaybook mode auto` | `relaybook mode` or `relaybook mode auto` |
| Checkpoints | `/relaybook gates` or `/relaybook gates plan` | `relaybook gates` or `relaybook gates plan` |
| Start a run | `/relaybook run docs/prd.md` | `relaybook run docs/prd.md` |
| Follow a run | `/relaybook run watch` | `relaybook run watch` |
| Approve a checkpoint | `/relaybook approve` | `relaybook approve` |
| Inbox | `/relaybook inbox` | `relaybook inbox` |

```sh
relaybook mode auto
relaybook gates plan
relaybook run docs/prd.md
```

`plan` pauses after planning. Continue with `relaybook approve`. The terminal shows each phase as it happens, and `relaybook run watch` follows a run that was started in the background. `relaybook run --quiet docs/prd.md` prints only the phase headers and the closing summary. With the default `workflow=classic`, a run pushes its branch to `origin` and opens the pull request without a sign-off. Limits, safety checks, and a PRD template are in [docs/auto-mode.md](docs/auto-mode.md). For Codex review and a sign-off before anything is pushed, see [Gated workflow](#gated-workflow).

## Gated workflow

`workflow=classic` is the default. `workflow=gated` needs Relaybook 0.4.0 or later and splits the roles: Claude plans, Cursor implements, and Codex reviews. Codex does not edit code or write to the ledger. Set the reviewer first:

```sh
relaybook config set reviewer codex
relaybook config set workflow gated
```

In gated mode, claim and done need a clean tree, and `relaybook task done` records the commit. `relaybook review start greet` hands a done task to the reviewer, and `relaybook review record` stores the verdict for `HEAD`. `changes-requested` reopens the task. `relaybook report accept greet` then needs an approved review at `HEAD`. `relaybook review start --release` covers the branch.

The maintainer signs off with `relaybook release authorize` at a terminal, after an approved release review at that commit. The prompt asks for `authorize` and the first 12 hex digits of the commit. `y` does not confirm. That is a deliberate typed confirmation, not proof of a human. `relaybook release status` shows whether `HEAD` is signed off. See [Sign-off](docs/orca-workflow.md#sign-off).

In gated mode, Relaybook's own commands publish nothing before sign-off. With the default `remote_check=online`, review and sign-off check every configured remote with `git ls-remote` and refuse a commit that is already on one. `relaybook config set remote_check local` checks only local tracking refs, and when `relaybook release authorize` cannot verify the remotes, it can go ahead after a typed `offline` line. The check does not see a push to a different push URL, a repository that is not a configured remote, or another clone, and anyone can still run `git push` outside Relaybook. Manual commands never run `git push` or `gh`. A gated auto run needs the Codex CLI signed in, and `relaybook approve` pushes only the authorized commit, then opens one pull request. See [Nothing is published before sign-off](docs/orca-workflow.md#nothing-is-published-before-sign-off) and [Gated workflow](docs/auto-mode.md#gated-workflow).

In gated mode, claim, done, review, `relaybook graph refresh`, and the auto run call `graphify update .` and record the commit the graph was built from. A missing or failed graph records `no graph` and does not stop the work. Relaybook never runs `graphify extract`. `graphify-out/` must stay gitignored. `relaybook setup --ci` removes that ignore rule, and its workflow opens pull requests that commit the graph. When Graphify is installed and the worktree is clean, a gated graph refresh can then record `failed` and `no graph: graphify-out/ is not gitignored`. Once graph output is tracked, it records `no graph: graphify-out/ is tracked`. The work continues without a graph.

`codex_model` picks the Codex model (default `gpt-6.1-sol`). `review_fallback` is `off` by default. With `sonnet`, a gated run reviews with Claude `claude-sonnet-5-5` after a Codex usage limit. Turning it on needs the typed confirmation. By default (`fallback_rereview=before-release`), Codex re-reviews each approved Sonnet review before sign-off and publishing. `relaybook config set fallback_rereview off` drops that requirement and also needs the typed confirmation. The fallback has not yet been verified against a real Codex usage limit or a real Sonnet response. See [Reviewer fallback](docs/auto-mode.md#reviewer-fallback), [Known limits](docs/auto-mode.md#known-limits), and [Settings](docs/configuration.md#settings).

## Token savings

On this ledger, `relaybook context --all` is 102,072 bytes (about 25,518 tokens) and the default `relaybook context` is 6,386 bytes (about 1,597 tokens), about 94% less. Those counts are bytes/4 estimates. Done tasks stay behind `relaybook context --all`. On a 40-done fixture, context fell from 4,567 bytes (about 1,141 tokens) to 1,482 bytes (about 370 tokens).

Graphify answers where the code lives. RTK compacts command output. Relaybook compacts the work ledger. The always-loaded planner and executor blocks are 828 and 788 bytes. `scripts/measure-context.sh --check` guards those budgets. After `relaybook setup`, Claude Code and Cursor already know when to plan, claim, and review. `relaybook guide` prints the worked examples.

## FAQ

**Where does the data live?** In `.relaybook/ledger.db` at the git root. SQLite is compiled into the binary. There is no daemon.

**What if two agents claim the same task?** One `relaybook task claim` wins. The other exits 1 with `already claimed`.

**Which machines can install a release?** macOS arm64, Linux x86_64, Windows x64, and Windows ARM64. On another machine, build with Rust 1.89 or later: `cargo build --release`.

**What about older ledgers and older binaries?** Relaybook 0.4.0 uses ledger schema 8 and migrates an older ledger in place when it opens it. A ledger newer than the binary is refused with `this ledger needs a newer relaybook`. While `workflow=gated`, five triggers refuse the 0.3.3 writes for a task report with no `commit_sha`, accepting a task without an approved review at its commit, a claim with no `base_sha` key, a run insert whose `workflow` is not `gated`, and a run update that does not advance `write_stamp`, with `workflow=gated: this ledger needs relaybook 0.4.0 or later (schema 8)`. That guards against an old binary by accident. It is not a security boundary. See [Migrations](docs/ledger.md#migrations).

## Docs

- [Commands](docs/commands.md)
- [Configuration](docs/configuration.md)
- [Auto mode](docs/auto-mode.md)
- [Orca workflow](docs/orca-workflow.md)
- [Ledger](docs/ledger.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Changelog](CHANGELOG.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
