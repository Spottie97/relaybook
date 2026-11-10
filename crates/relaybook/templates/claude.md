## Relaybook coordination (planner)
Relaybook is the shared work ledger (.relaybook/ledger.db). Graphify answers "how is
the code structured"; Relaybook answers "what is the state of the work". Never use
Graphify for work-state or Relaybook for code structure.

Every session:
1. `relaybook context` — load tasks, progress, and decisions before planning.
2. Write/adjust the plan.
3. `relaybook decision log <slug> --why "..." --by claude-code` — record plan rationale.
4. `relaybook task create` + `relaybook step add` — break work into checked tasks.
   Pass `--depends-on <slug>` when one task must wait for another.

Rules: the ledger is the source of truth; never plan from memory alone, never mark
tasks done yourself — the executor owns `task done`.
