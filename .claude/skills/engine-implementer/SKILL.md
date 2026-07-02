---
name: engine-implementer
description: "End-to-end phase.rs implementation pipeline: plan, review-plan, implement, review-impl, commit — each step run in a fresh spawned agent."
---

# Engine Implementer (Orchestrator)

This is the orchestrator for the phase.rs implementation pipeline. It runs as a **skill in the main thread** so it can spawn agents for every step that benefits from fresh context (plan review, surgical implementation, implementation review). Do not turn this into an agent — agents cannot spawn sub-agents, which is what made earlier versions silently degrade.

> **⚠️ `mtgish` is dormant — DO NOT route implementation work through it.** `mtgish/`, `crates/mtgish-import/`, and `data/mtgish-*` are NOT live consumers of the engine, parser, or card data. Reject any plan section, executor edit, or review fix that touches mtgish files; surface it to the user instead of silently shipping it. PRs that only modify mtgish are rejected on sight.

## Roles

| Step | Where it runs | Why |
|---|---|---|
| 1. Produce plan | **Spawned `general-purpose` agent** invoking `/engine-planner` | Fresh context = plan is shaped by the task, not by the conversation history that led here |
| 2. Review plan | **Spawned `general-purpose` agent** invoking `/review-engine-plan` | Fresh context = honest architectural review, independent of the planner |
| 3. Implement | **Spawned `engine-implementation-executor` agent** | Surgical edits + Tilt verification; worktree-isolatable |
| 4. Spot-check verification | This thread | Re-run anything the executor skipped; confirm formatting |
| 5. Review implementation | **Spawned `general-purpose` agent** invoking `/review-impl` | Independent reviewer, not the implementer |
| 6. Commit | This thread | Owner of the working tree decides what gets staged |

**Runtimes without subagent spawning (contributor environments — Codex CLI, plain LLM sessions).** The pipeline's value comes from context isolation between author and reviewer, not from the spawning mechanism. If your runtime cannot spawn agents, do NOT silently degrade to reviewing your own work in the same context — that is the failure mode this skill exists to prevent. Instead: run each step against a fresh context (new session/conversation per step when your runtime supports it), and for every review step hand the reviewer ONLY the artifact under review (the full plan, or the unified diff), the original task description, `CLAUDE.md`, and the relevant skill (`/review-engine-plan` or `/review-impl`) — never the conversation that produced it. If even that is impossible, say so explicitly in the final report and in the PR body under a "Validation Failures" heading; do not claim the review loop ran clean.

The orchestrator never authors content itself. Its only jobs are: spawn agents, route their output to the next step, loop review steps until clean, own the commit, and gracefully cull each spawned agent once its output is consumed (send a `shutdown_request` and wait for the `shutdown_response` ack — spawned agents now carry `SendMessage`, so they cull gracefully instead of being pane-killed). The structured report each agent returns stays the authoritative step handoff; SendMessage is an additive progress/acknowledgment channel, not a replacement.

## Inputs

Either:

1. A task description (cards, CR rules, Oracle text patterns, affected subsystems, expected behavior), or
2. A pre-existing plan — treat as a draft unless it has already passed `/review-engine-plan` to clean.

If running in worktree-isolation mode, prepare the worktree before Step 3 and pass its path to the executor agent. Per `feedback_session_default_no_worktree`, do not re-ask about worktrees during an active pipeline session — use the session default.

## Pipeline

### Step 1 — Produce the plan

Spawn a `general-purpose` agent and instruct it to invoke `/engine-planner`. The agent returns a plan with every mandatory architectural section.

**Spawn inputs:** task description; in-scope file/subsystem hints; any prior reviewer findings (none on first round).

Do not author or edit the plan in this thread. If the returned plan is missing sections or is superficial, send the same inputs plus an explicit "missing sections" note to a **fresh** planning agent — do not patch it yourself.

### Step 2 — Review the plan until clean (unbounded loop)

Spawn a `general-purpose` agent and instruct it to invoke `/review-engine-plan` against the full plan.

**Reviewer spawn inputs:** the full plan; the original task description.

If the reviewer returns gaps, spawn a **fresh** planning agent (Step 1 inputs plus the reviewer's findings as additional constraints) to produce a revised plan, then spawn a **fresh** reviewer agent against the revised plan.

**Repeat until a full review round returns zero gaps.** There is no iteration cap — "two rounds and ship" is not acceptable. Stop only for:

- a true human design decision the planner cannot resolve,
- missing external access (CR text unavailable, file inaccessible), or
- an environment blocker that makes review impossible.

Each review must run in a fresh agent context — never reuse the previous reviewer's context.

### Step 3 — Dispatch implementation

Spawn the `engine-implementation-executor` agent.

**Spawn inputs:** the reviewed clean plan in full; in-bounds / out-of-bounds file scope; worktree path if applicable; any prior reviewer findings (none on first round).

The executor edits files, runs Tilt-first verification, runs the parser diff gate if any parser file changed, and returns a structured report (diff summary, verification results, judgement calls, stop-and-return items, CR annotations verified, deviations, risks).

If the executor returns "stop and return" items (plan contradicts current code, ad hoc parser dispatch unavoidable, CR uncertain), do NOT improvise around them. Loop back to Step 1, feed the executor's findings into `/engine-planner` as new constraints, and re-run Steps 1–3.

### Step 4 — Spot-check verification

The executor already ran the appropriate Tilt block. Re-run only what the executor skipped or what changed because of intervening commits from other agents. Always confirm formatting:

```bash
cargo fmt --all
```

After a non-zero `tilt-wait.sh`, fetch details with `tilt logs <resource> --tail 50 --since 2m`. Distinguish your diff's errors from concurrent-agent errors per CLAUDE.md's "Defer to other active agents" guidance.

Confirm the executor's pre-commit artifacts came back complete: the **discriminating-test gate** (a complete production-path coverage map for every behavioral claim — changed seam/function, production entry point, test name, revert-failing assertion, and sibling/negative cases), the **maintainer-simulation matrix** (selected authority, binding time, storage, consuming function, invalidation behavior, hostile fixture rows, and serialized-surface impact for each claim/seam), and the **CR-annotation diff gate** (every added/changed `CR <n>` resolves in `docs/MagicCompRules.txt`). Do not accept generic "gate: pass" summaries. If any changed seam is unmapped, any maintainer-simulation row is missing or superficial, the executor shipped only shape tests for runtime semantics or coverage-support claims, parser work accepts Oracle text while dropping semantics without preserving an honest `Unimplemented`/coverage gap, a rules-bearing "this way" / "that source" / "chosen" / "cast using" / "from among them" / duration-bound "you" path relies on unproven global rescanning, or any CR annotation came back `UNVERIFIED`, loop back to Step 3 with that as a fix constraint — do not commit.

### Step 5 — Review implementation until clean (unbounded loop)

Spawn a `general-purpose` agent and instruct it to invoke `/review-impl` against the implementation diff. The reviewer MUST also verify the originally reported bug or requirement is actually fixed via a discriminating runtime test — not just that the code looks clean (`feedback_review_impl_verify_bug_fixed`). The reviewer MUST audit the executor's production-path coverage map, maintainer-simulation matrix, and parser coverage-honesty statement; a clean review is invalid unless it explicitly confirms those artifacts are complete or returns findings.

**Reviewer spawn inputs:** `git diff` of the in-flight branch against its base; the original task description; the reviewed plan; the executor's discriminating-test map and maintainer-simulation matrix.

If the reviewer returns findings, spawn a **fresh** `engine-implementation-executor` agent to apply fixes:

**Fix-round executor spawn inputs:** the reviewed plan; current `git diff HEAD` of the in-flight branch; the reviewer findings as the fix constraints; same scope and worktree as the original Step 3 spawn.

Then spawn a **fresh** review agent against the new diff. Repeat until a full review round returns zero findings. Per `feedback_engine_implementer_runs_review`, never self-review — always spawn an isolated reviewer.

**Repeat until a full review round returns zero findings.** No iteration cap. Per `feedback_engine_implementer_runs_review`, never self-review — always spawn an isolated reviewer.

### Step 6 — Commit

Commit only after:

- Step 2 plan-review loop is clean,
- Step 4 verification passes (or unrelated failures are clearly isolated to other agents),
- Step 5 implementation-review loop is clean.

Stage by pathspec — never `git add -A` and never `git commit` without a pathspec, because the shared index can sweep in other agents' staged files (`feedback_git_add_file_bundles_concurrent_work`, `feedback_shared_index_commit_pathspec`):

```bash
git status --short
git diff --stat
git diff --cached <paths>                 # confirm nothing unrelated is staged
git commit <paths> -m "<type>: <summary>"
```

Verify HEAD is on a branch before any push (`feedback_verify_head_attached_before_push`). Never pipe `git push` into `tail`/`head` (`feedback_git_push_no_pipe`). Do not push unless explicitly requested.

## Final Report

Return after the commit:

1. Plan-review rounds (count) and final clean result.
2. What changed, grouped by subsystem and file.
3. Key architectural decisions.
4. Verification commands run and results (executor's + your spot-checks).
5. Implementation-review rounds (count) and final clean result.
6. Commit hash and staged file list.
7. Coverage impact for parser changes.
8. Deviations from the plan with reasons.
9. Self-flagged risks and judgment calls (yours + executor's).
10. Remaining items, if any, with reasons.
