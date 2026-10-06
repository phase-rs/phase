# Scout

Read this before [SKILL.md](SKILL.md) Step 1, and before Step 6 when its trigger applies. A scout is a cheap read-only agent that locates facts for a planner or reviewer: where the analogous feature lives, which helpers exist, who constructs and consumes a type, which tests reach it. Locating is most of what Steps 1, 2 and 6 spend before they can judge anything, and it needs no judgement.

A scout reports located facts only. It has no verdict authority, is not a reviewer, and never replaces a gate: the planner still verifies the premise, traces the analogous feature, reads every file it will touch and probes; the reviewers still run their own sweeps.

## What the pack is

The scout's prompt is [the scout contract](references/scout-contract.md) followed by a task file. Every fact quotes one verbatim line at a cited path and line. `scripts/scout.mjs` keeps a fact only when that quote is the whole line, ignoring indentation, at the cited line or within three lines of it, corrects the line number, and lists everything else under `rejected`. The result is one JSON object: `facts`, `rejected`, `unknowns` (things the scout looked for and did not find), and the run's model, duration and usage.

Running a scout is a documented command, so it is not [verification machinery](SKILL.md#task-scope-and-verification-work), is not a design round, and needs no expansion case. Do not extend the script or build tooling around it during a run.

## Task files

The orchestrator fills a template with the original task text and nothing else. It does not add hypotheses, suspected causes or a preferred design: a scout handed a hypothesis returns it confirmed.

**Precedent pack** — before Step 1, and in a chartered run before the charter and before each phase plan (use the phase's charter entry as the task text):

```text
Task: <the original task, verbatim, with the card's Oracle text when one is named>

Locate, for this task:
1. The existing feature most similar to it, and each site that feature touches across types, parser, resolver or effect handler, and tests.
2. For each piece of game state the behavior depends on, the site that writes it and every site that reads it, from the parser output through resolution to the layer or rule that applies it.
3. The declaration of every enum variant, struct and function the task names or would extend, and every construction and consumption site of each.
4. Existing helpers that already do part of the work, under crates/engine/src/parser/ and crates/engine/src/game/.
5. Existing tests and fixtures that exercise those declarations, in crates/engine/tests/integration/ and in inline test modules.
6. Sibling implementations of the same pattern, including the nearest one that already works.
7. Registration points the change would need: mod lists, dispatch tables, the integration-test mod list.

Cite a file only at a line that is itself relevant. Do not cite a file to say it was checked.
```

For work outside the engine crate (frontend, AI, server, card-data pipeline), replace the paths in items 4 and 5 with that surface's directories and keep the rest.

**Review pack** — before the first Step 6 round of a candidate, only when `BASE_SHA..CANDIDATE_SHA` adds a field or variant to an existing enum or struct, changes a serialized surface (`GameAction`, `WaitingFor`, a game-state field, the card-data export shape), or adds a parser arm beside existing arms. Fix rounds get a new pack only when the fix itself meets the trigger.

```text
Changed paths: <the candidate's changed paths, one per line>

Locate, for these changes:
1. Each declaration the change adds or alters, and every construction and consumption site of it across the workspace, including resume and continuation paths, batch handlers, and WASM, adapter and serialization payload constructors.
2. The tests that reach each changed function, and the production entry point each test drives.
3. Places that follow the same pattern as a changed site and are not in the changed paths.
4. Existing mechanisms and sibling implementations the change should match.
5. Registration points and generated or mirrored files that pair with a changed file.

Cite a file only at a line that is itself relevant. Do not cite a file to say it was checked.
```

## Launch

Run the scout against a tree at the commit the consumer will work from: for a precedent pack, the commit the run will start from, which the orchestrator then fixes as `BASE_SHA` (in a chartered phase, `PHASE_BASE_SHA`); for a review pack, `CANDIDATE_SHA`. A precedent pack runs before the implementation worktree exists, so use any clean checkout or detached worktree at that commit. A fresh worktree lacks the gitignored `docs/MagicCompRules.txt` and `data/engine-inventory.json`, so the scout reports those as unknowns; CR and inventory checks stay with the planner and reviewers.

Use the first launch that applies:

1. A Codex orchestrator spawns a native sub-agent with `gpt-6-luna` and `low` reasoning.
2. Any other orchestrator, when `codex` is installed, runs:

   ```bash
   node .claude/skills/engine-implementer/scripts/scout.mjs run --repo <worktree> --task-file <file> --label <step>
   ```

3. A Claude Code orchestrator without `codex` dispatches a native `Explore` agent with the `sonnet` model.
4. Otherwise, the same `run` command falls back to Sonnet through `claude -p`.

The `run` command gives the scout up to an hour by default (`--timeout SECONDS`), so that a slow scout is not cut short and a hung one still ends as a failure. Run it in the background or with a command timeout at least that long. It already prints the verified pack. A native scout (launches 1 and 3) returns only its raw report: save its final message to a file and check its quotes against the cited lines:

```bash
node .claude/skills/engine-implementer/scripts/scout.mjs verify --repo <worktree> --report-file <file> --label <step>
```

Keep task files, raw reports and packs under `<git-common-dir>/engine-implementer-runs/<run-id>/scout/`, never in a worktree.

## Handing the pack on

Give the pack's `facts` and `unknowns` to the consumer with this sentence: *the pack may be incomplete or misleading; verify what you rely on and explore beyond it; a missing fact is not evidence of absence.* A precedent pack goes to the Step 1 planner and the Step 2 reviewer of the same plan. A review pack goes to the Step 6 reviewer. Executors do not receive one: the reviewed plan is their authority.

Drop `rejected` entries; they are not leads.

If the scout fails, times out or returns no verified facts, note it once in the [phase-fit record](SKILL.md#phase-fit-record) and proceed without a pack. A missing pack never delays or stops a step.
