# Scout

Read this before [SKILL.md](SKILL.md) Step 1, and before Step 6 when its trigger applies. A scout is a cheap read-only agent that locates facts for a planner or reviewer: where the analogous feature lives, which helpers exist, who constructs and consumes a type, which tests reach it. Locating is most of what Steps 1, 2 and 6 spend before they can judge anything, and it needs no judgement.

A scout reports located facts only. It has no verdict authority, is not a reviewer, and never replaces a gate: the planner still verifies the premise, traces the analogous feature, reads every file it will touch and probes; the reviewers still run their own sweeps.

## What the pack is

The scout's prompt is [the scout contract](references/scout-contract.md) followed by a task file. Every fact quotes one verbatim line at a cited path and line. `scripts/scout.mjs` keeps a fact only when that quote is the whole line, ignoring indentation, at the cited line or within three lines of it, corrects the line number, and lists everything else under `rejected`. The result is one JSON object: `facts`, `rejected`, `unknowns` (things the scout looked for and did not find), and the run's model, duration and usage.

Running a scout is a documented command, so it is not [verification machinery](SKILL.md#task-scope-and-verification-work), is not a design round, and needs no expansion case. Do not extend the script or build tooling around it during a run.

## Task files

The orchestrator fills a template with the original task text and nothing else. It does not add hypotheses, suspected causes or a preferred design: a scout handed a hypothesis returns it confirmed.

Each pack is split into two halves, each its own task file for its own scout: **reach** enumerates every site the change touches, and **resemble** finds the existing code it should match.

**Precedent pack** — before Step 1, and in a chartered run before the charter and before each phase plan (use the phase's charter entry as the task text):

```text
Task: <the original task, verbatim, with the card's Oracle text when one is named>

Locate, for this task:
1. For each piece of game state the behavior depends on, the site that writes it and every site that reads it, from the parser output through resolution to the layer or rule that applies it.
2. The declaration of every enum variant, struct and function the task names or would extend, and every construction and consumption site of each.
3. Existing tests and fixtures that exercise those declarations, in crates/engine/tests/integration/ and in inline test modules.
4. Registration points the change would need: mod lists, dispatch tables, the integration-test mod list.

Cite a file only at a line that is itself relevant. Do not cite a file to say it was checked.
```

```text
Task: <the original task, verbatim, with the card's Oracle text when one is named>

Locate, for this task:
1. The existing feature most similar to it, and each site that feature touches across types, parser, resolver or effect handler, and tests.
2. Existing helpers that already do part of the work, under crates/engine/src/parser/ and crates/engine/src/game/.
3. Sibling implementations of the same pattern, including the nearest one that already works.

Cite a file only at a line that is itself relevant. Do not cite a file to say it was checked.
```

The first is reach, the second resemble. For work outside the engine crate (frontend, AI, server, card-data pipeline), replace the paths in reach item 3 and resemble item 2 with that surface's directories and keep the rest.

**Review pack** — before the first Step 6 round of a candidate, only when `BASE_SHA..CANDIDATE_SHA` adds a field or variant to an existing enum or struct, changes a serialized surface (`GameAction`, `WaitingFor`, a game-state field, the card-data export shape), or adds a parser arm beside existing arms. Fix rounds get a new pack only when the fix itself meets the trigger.

```text
Changed paths: <the candidate's changed paths, one per line>

Locate, for these changes:
1. Each declaration the change adds or alters, and every construction and consumption site of it across the workspace, including resume and continuation paths, batch handlers, and WASM, adapter and serialization payload constructors.
2. The tests that reach each changed function, and the production entry point each test drives.
3. Registration points and generated or mirrored files that pair with a changed file.

Cite a file only at a line that is itself relevant. Do not cite a file to say it was checked.
```

```text
Changed paths: <the candidate's changed paths, one per line>

Locate, for these changes:
1. Places that follow the same pattern as a changed site and are not in the changed paths.
2. Existing mechanisms and sibling implementations the change should match.

Cite a file only at a line that is itself relevant. Do not cite a file to say it was checked.
```

The first is reach, the second resemble.

## Launch

Run the scout against a tree at the commit the consumer will work from: for a precedent pack, the commit the run will start from, which the orchestrator then fixes as `BASE_SHA` (in a chartered phase, `PHASE_BASE_SHA`); for a review pack, `CANDIDATE_SHA`. A precedent pack runs before the implementation worktree exists, so use any clean checkout or detached worktree at that commit. A fresh worktree lacks the gitignored `docs/MagicCompRules.txt` and `data/engine-inventory.json`, so the scout reports those as unknowns; CR and inventory checks stay with the planner and reviewers.

Launch the two halves concurrently, each at `low` reasoning: GPT-6 Luna takes reach and Haiku 5.5 takes resemble. When only one model can be launched (a Claude Code orchestrator without `codex`), it takes both halves as two concurrent scouts. Each model runs as a native sub-agent when the orchestrator has one for it, and otherwise through the `run` command:

| Orchestrator | GPT-6 Luna | Haiku 5.5 |
|---|---|---|
| Codex | native sub-agent with `gpt-6-luna` | `run --backend claude` |
| Claude Code | `run --backend codex`, when `codex` is installed | native `Explore` agent with the `haiku` model |
| Any other | `run --backend codex`, when `codex` is installed | `run --backend claude` |

```bash
node .claude/skills/engine-implementer/scripts/scout.mjs run --backend codex|claude --repo <worktree> --task-file <file> --label <step>-<half>
```

The `run` command gives the scout up to an hour by default (`--timeout SECONDS`), so that a slow scout is not cut short and a hung one still ends as a failure. Run it in the background or with a command timeout at least that long. It already prints the verified pack. A native scout returns only its raw report: save its final message to a file and check its quotes against the cited lines:

```bash
node .claude/skills/engine-implementer/scripts/scout.mjs verify --repo <worktree> --report-file <file> --label <step>-<half>
```

If a scout fails, times out or returns no verified facts, the other half's model picks up its half: relaunch the failed half once on that model as soon as the failure is known, without waiting for the other scout to finish. A pickup is not itself picked up.

Keep task files, raw reports and packs under `<git-common-dir>/engine-implementer-runs/<run-id>/scout/`, never in a worktree.

## Handing the pack on

Give each half's `facts` and `unknowns` to the consumer, labelled by half, with this sentence: *the packs may be incomplete or misleading; verify what you rely on and explore beyond them; a missing fact is not evidence of absence.* Precedent packs go to the Step 1 planner and the Step 2 reviewer of the same plan. Review packs go to the Step 6 reviewer. Executors do not receive them: the reviewed plan is their authority.

Drop `rejected` entries; they are not leads.

Note each failed scout once in the [phase-fit record](SKILL.md#phase-fit-record). The consumer waits for a pickup; when the pickup also fails, proceed without that half. Beyond that one pickup, a missing half never delays or stops a step.
