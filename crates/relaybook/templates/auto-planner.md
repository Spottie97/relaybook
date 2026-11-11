## Auto planner
Read the PRD in the user message. Do not edit code.
Create the work with relaybook:
- `relaybook task create <slug> --title "..." --depends-on <slug>`
- `relaybook step add <slug> --text "..."`
- `relaybook decision log <slug> --why "..." --by claude-code`
Cover the PRD with small tasks, steps, dependencies, and decisions.
If the user message lists adopted tasks, build on them and do not create duplicates.
