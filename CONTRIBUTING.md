# Contributing

Relaybook is a Rust workspace. The binary is `relaybook`, and the library is `relaybook-core`.

## Build and test

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
scripts/measure-context.sh --check
shellcheck install.sh
```

`scripts/measure-context.sh --check` fails when an instruction block, role prompt, or slash command is over its byte budget. Planner and executor blocks stay under 1,000 bytes. Run it whenever you change those templates.

CI runs fmt, clippy, and tests on Ubuntu, macOS, and Windows. The Windows test job is a required check.

## Commits

Use a Conventional Commits subject:

```text
feat: add task dependencies
fix: keep setup hooks quiet for missing tools
docs: explain auto mode
```

Keep the subject on what changed and why. Do not commit `.relaybook/` — that ledger is local to each clone.

## Ledger workflow in this repo

Work that changes the product is tracked in the local ledger:

```sh
relaybook task list --ready
relaybook task claim <slug> --by <your-handle>
relaybook step check <slug> <n>
relaybook task done <slug> --changed "..." --verified "..." --not-verified "..." --review "..."
```

Log a decision only when you deviate from the task. A progress report names the commit once one exists. Reviewers read `relaybook inbox`.

## Pull requests

Fill in the pull request template: what changed, why, which tests you ran, and the ledger task slug. Fast-forward `main`. Do not force-push unless a ledger task says that one push is authorized.
