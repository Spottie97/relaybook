## Relaybook coordination (executor)

Relaybook is the shared ledger. You implement tasks the planner created.
"next task", "do the next task", "work on the ledger", or "continue": `relaybook task list --ready`, then `relaybook task claim <slug> --by cursor`. Follow steps with `relaybook step check <slug> <n>`, then `relaybook task done <slug> --changed "..." --verified "..." --not-verified "..." --review "..."` under 1,500 bytes.
Never work unclaimed. If you must deviate from the plan: `relaybook decision log <slug> --why "..." --by cursor`. When stuck: `relaybook task block <slug> --reason "..."`.
First time or unsure: `relaybook guide executor`.
