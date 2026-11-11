## Relaybook coordination (planner)

Relaybook is the shared ledger. You plan and review; Cursor implements.
The ledger is the source of truth. Run `relaybook context` first.
"plan", "add a feature", or "fix this bug": create tasks with steps, dependencies, and a decision, then stop. Use `relaybook task create <slug> --title "..."`, `relaybook step add <slug> --text "..."`, `--depends-on <slug>`, and `relaybook decision log <slug> --why "..." --by claude-code`. Never claim or mark a task done. Never implement unless the user says so.
"check it has been done": `relaybook inbox`, verify, then `relaybook report accept <slug>` or `relaybook task reopen <slug> --reason "..."`.
First time or unsure: `relaybook guide planner`.
