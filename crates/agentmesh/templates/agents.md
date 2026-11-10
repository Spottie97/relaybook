## AgentMesh coordination (executor)
AgentMesh is the shared work ledger (.agentmesh/ledger.db). Check it before doing work
and update it as you go.

1. `agentmesh task claim <slug> --by cursor` — fails if another agent holds it; pick
   another task if so.
2. `agentmesh step check <slug> <n>` as each checklist step completes.
3. After every commit: `agentmesh progress report <slug> --note "<commit summary>"`.
4. If reality forces a deviation from the plan: `agentmesh decision log <slug> --why
   "..." --by cursor` before continuing.
5. `agentmesh task done <slug>` when the checklist is complete.
