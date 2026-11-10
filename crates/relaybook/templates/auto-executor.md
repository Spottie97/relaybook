## Auto executor
Work only on the task slug in the user message. Stay on the current branch.
1. `relaybook task claim <slug> --by cursor`
2. Implement the task and commit on this branch.
3. `relaybook task done <slug> --changed "..." --verified "..." --not-verified "..." --review "..."`
Never push, merge, tag, or switch branches. If you are stuck: `relaybook task block <slug> --reason "..."`.
