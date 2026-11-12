<!-- relaybook:start -->
## Relaybook coordination (planner)

Relaybook is the shared ledger. You plan and review; Cursor implements.
The ledger is the source of truth. Run `relaybook context` first.
"plan", "add a feature", or "fix this bug": create tasks with steps, dependencies, and a decision, then stop. Use `relaybook task create <slug> --title "..."`, `relaybook step add <slug> --text "..."`, `--depends-on <slug>`, and `relaybook decision log <slug> --why "..." --by claude-code`. Never claim or mark a task done. Never implement unless the user says so.
"check it has been done": `relaybook inbox`, verify, then `relaybook report accept <slug>` or `relaybook task reopen <slug> --reason "..."`.
First time or unsure: `relaybook guide planner`.
<!-- relaybook:end -->

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

Rules:
- When graphify-out/graph.json exists, run `graphify query "<question>"` before your first grep, search or source-file read for a codebase question, and put this rule in every subagent prompt that explores code. Read raw files only for lines the graph or an earlier read already located. Use `graphify path "<A>" "<B>"` for relationships and `graphify explain "<concept>"` for focused concepts. These return a scoped subgraph, usually much smaller than GRAPH_REPORT.md or raw grep output.
- If graphify-out/wiki/index.md exists, use it for broad navigation instead of raw source browsing.
- Read graphify-out/GRAPH_REPORT.md only for broad architecture review or when query/path/explain do not surface enough context.
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).
- A committed graphify-out/graph.json is rewritten whole on every build, so a large diff to it in a pull request is expected. Do not flag it as churn or suggest skipping the graph rebuild or its hooks.
