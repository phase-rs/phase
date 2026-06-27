---
name: batch-mechanics
description: Use when implementing multiple unimplemented mechanics in batch. Creates an agent team that groups items by relevance, plans each group with review loops, then executes sequentially with post-implementation verification. Append the list of mechanics to address after invoking.
---

# Batch Mechanics Implementation

Orchestrate an agent team to plan and implement multiple mechanics from UNIMPLEMENTED-MECHANICS.md.

**Prerequisites:**
- Agent Teams must be enabled (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` in settings)
- The lead session should be running Opus (it handles grouping and coordination decisions)

---

## Instructions for the Team Lead

You are orchestrating a multi-phase workflow. Follow each phase exactly. If any phase isn't possible, **stop and discuss with the user** before proceeding.

**Small batch shortcut:** If the user has only 1-2 items, skip the agent team — implement them directly without the team overhead.

### Phase 1 — Group & Classify

1. Read `.planning/rules-audit/UNIMPLEMENTED-MECHANICS.md`
2. Review the items the user has appended below
3. Group items by relevance to each other. Consider:
   - Items that touch the same files or subsystems (e.g., two SBA items → group together)
   - Items that share a pattern (e.g., multiple keyword implementations)
   - Items that have dependencies (e.g., damage prevention must exist before protection prevention pipeline)
4. Balance group sizes — fold small items into related groups rather than having a teammate do only 1-2 trivial tasks
5. For each group, identify which existing skill(s) apply. A mechanic may need multiple skills:
   - `/add-engine-effect` — new effects or stub completions
   - `/add-keyword` — keyword abilities
   - `/add-trigger` — triggered abilities
   - `/add-static-ability` — static/continuous effects
   - `/add-replacement-effect` — replacement effects
   - `/add-interactive-effect` — effects requiring player choices (WaitingFor/GameAction round-trip)
   - `/oracle-parser` — parser-only changes
   - `/casting-stack-conditions` — casting flow or stack changes
6. Order groups by dependency — if Group A must be done before Group B can start, note this
7. Present the groupings to the user for approval before spawning teammates

### Phase 2 — Plan (Parallel Teammates)

For each group, spawn a planning teammate (model: opus) with these instructions. **Replace all bracketed placeholders** with concrete values before spawning:

> **Your task:** Create an implementation plan for the following mechanics: {list the specific items}.
>
> **How to plan:**
> 1. Read the relevant skill(s): {list the specific skill names, e.g., `/add-engine-effect`}
> 2. For each mechanic, look up the CR rule in `docs/MagicCompRules.txt`
> 3. Trace how an existing analogous mechanic works end-to-end (the skill tells you which to trace)
> 4. Read the current state of all files you'll need to modify
> 5. Create a detailed implementation plan covering every file change, using the skill's checklist as your guide
>
> **Require plan approval before making any changes.**

**Plan review loop (lead-orchestrated — teammates cannot spawn subagents):**

When a teammate finishes their plan:
1. The teammate messages the lead with their plan
2. **You (the lead) spawn a review subagent** (model: opus) with the teammate's plan, asking it to check for: missed files from the skill checklist, incorrect CR references, building-block violations, composability issues, and CLAUDE.md adherence
3. Send the review feedback back to the teammate and direct them to address all gaps
4. When the teammate finishes revisions, **you spawn another review subagent** (model: opus) with the revised plan
5. Repeat until the reviewer finds no gaps (max 3 rounds)
6. Once clean, approve the plan

### Phase 3 — Execute (Sequential by Default)

**Important:** Execute groups **one at a time** unless the user explicitly requests parallel execution AND the groups touch completely different files.

For each approved plan, direct the teammate:

> "Before implementing, re-read all files listed in your plan to ensure you have their current state — a previous group may have modified shared files like `types/ability.rs` or `effects/mod.rs`.
>
> Then implement your plan. Follow the skill checklist exactly. After each file change, verify it compiles. Do not skip any checklist step."

### Phase 4 — Post-Implementation Verification

When a teammate finishes implementing, direct them through these steps **in order:**

**Step 1 — Lead-orchestrated review (teammates cannot spawn subagents):**
1. The teammate messages the lead summarizing what they implemented
2. **You (the lead) spawn a review subagent** (model: opus) asking it to review the teammate's changes for: logic errors, missed checklist steps, missing CR annotations, missing tests, and building-block violations
3. Send the review feedback back to the teammate and direct them to address all gaps
4. When the teammate finishes fixes, **you spawn another review subagent** (model: opus)
5. Repeat until the reviewer finds no gaps (max 3 rounds)

**Step 2 — Run verification (prefer Tilt; fall back to direct cargo when Tilt is down — see CLAUDE.md § 'Canonical verification pattern'):**
> "Run these commands and fix any failures:
> 1. `cargo fmt --all` (always direct)
> 2. Verify clippy + tests:
>    ```bash
>    if tilt get uiresource clippy >/dev/null 2>&1; then
>      ./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data
>    else
>      cargo clippy --all-targets -- -D warnings
>      cargo test -p engine
>      ./scripts/gen-card-data.sh
>    fi
>    ```
> 3. If you added or changed parser output, accept new snapshots: `cargo insta accept`
> 4. Run coverage (one-shot, always direct): `cargo coverage`"

**Step 3 — Update tracking:**
> "Edit `.planning/rules-audit/UNIMPLEMENTED-MECHANICS.md` — remove every item you've successfully implemented. Update the Summary Statistics table."

**Step 4 — Commit:**
> "Commit your work with a descriptive message. Stage only the files you changed."

### Phase 5 — Cleanup

After all groups are complete:
1. Verify the workspace:
   ```bash
   if tilt get uiresource clippy >/dev/null 2>&1; then
     ./scripts/tilt-wait.sh --timeout 300 clippy test-engine card-data
   else
     cargo test --all
     ./scripts/gen-card-data.sh
   fi
   ```
2. Run `cargo coverage` (one-shot binary, always direct) to verify reduced Unimplemented count
3. Report results to the user: which mechanics were implemented, coverage delta, any items that couldn't be completed
4. Clean up the team — gracefully cull each teammate by sending a `shutdown_request` and waiting for its `shutdown_response` acknowledgment (teammates now carry `SendMessage`, so they can ack a graceful shutdown instead of being tmux-pane-killed, which left zombie roster entries). Only force-terminate a teammate that fails to acknowledge.

---

## Constraints

- **Max 4 teammates** at once — more creates diminishing returns and coordination overhead
- **Sequential execution** is the default — parallel only when groups have zero file overlap
- **Teammates must use existing skills** — no ad-hoc implementation approaches
- **Every CR annotation must be verified** against `docs/MagicCompRules.txt`
- **No mega-effects** — if a teammate creates an Effect variant that does multiple things, send them back to decompose it
- **Plan review max 3 rounds** — if still finding gaps after 3 rounds, escalate to the user
- **Re-read before executing** — teammates must re-read shared files before implementing, since a prior group may have modified them
- **Stuck teammates** — if a teammate hits a wall (can't resolve review feedback, encounters a conflict, or the problem is more complex than expected), escalate to the user with context rather than spinning

---

## Items to Implement

<!-- The user appends their list below this line -->
