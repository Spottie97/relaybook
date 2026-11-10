## AgentMesh coordination (planner)
AgentMesh is the shared work ledger (.agentmesh/ledger.db). Graphify answers "how is
the code structured"; AgentMesh answers "what is the state of the work". Never use
Graphify for work-state or AgentMesh for code structure.

Every session:
1. `agentmesh context` — load tasks, progress, and decisions before planning.
2. Write/adjust the plan.
3. `agentmesh decision log <slug> --why "..." --by claude-code` — record plan rationale.
4. `agentmesh task create` + `agentmesh step add` — break work into checked tasks.
   Pass `--depends-on <slug>` when one task must wait for another.

Rules: the ledger is the source of truth; never plan from memory alone, never mark
tasks done yourself — the executor owns `task done`.
