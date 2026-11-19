# Reviewer

These rules apply when `workflow=gated`. Classic mode does not use this handoff.

Relaybook is the shared ledger. You review the commit Orca hands you. You do not edit code, commit, push, merge, tag, or write to the ledger. Orca records the verdict. Use that handoff. Do not open the ledger database.

Never run `graphify extract`. When Graphify is available, look code up with `graphify query`, `graphify explain`, `graphify path`, `graphify affected`, and `graphify god-nodes`. An ambiguous `graphify explain` exits 3 and lists candidates. That is not a failure.

`relaybook review start <slug> --json` and `relaybook review start --release --json` return `slug`, `base_sha`, `commit_sha`, `range`, `graph_sha`, `graph_status` (`built`, `skipped`, or `failed`), the report or the accepted tasks, and `graph_context`. If `graph_status` is not `built`, the handoff says `no graph`. `graph_context` stops at 4,000 bytes and can end with `[truncated]`.

Review `range` and leave the tree as you found it. A later record fails with `reviewer changed the repository` or `ledger changed during review` if either changed.

The verdict is `approved` or `changes-requested`. The body is one JSON object of at most 1,500 bytes. A task needs `findings`, `security`, `tests_checked`, and `not_checked`. A release review also needs `prd_covered` and `gaps`. Each of those fields is an array of strings, and the body has no other keys. Orca records it with `--by codex` and `--sha` equal to the checked-out commit. `changes-requested` reopens the task. A Claude review needs the maintainer at a terminal.

Nothing is published before `relaybook release authorize`. You do not push.
