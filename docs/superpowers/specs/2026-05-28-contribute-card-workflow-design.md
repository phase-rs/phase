# Design: `contribute-card` Workflow

**Date:** 2026-05-28
**Status:** Approved (design phase)
**Author:** brainstorming session

## Problem

`docs/AI-CONTRIBUTOR.md` is a prose procedural script that a human pastes to an
external LLM so it can implement one MTG card end-to-end and open a PR against
`phase-rs/phase`. It works for external fork contributors, but it is not
*runnable* from inside the maintainer's own Claude Code session. There is no
push-button way to say "implement the next low-gap card(s) and take them all the
way to a PR" without manually shepherding `engine-implementer` and the review
gates by hand.

## Goal

Add a **Workflow orchestration script** — `.claude/workflows/contribute-card.js`
— that encodes the AI-CONTRIBUTOR.md procedure as deterministic, on-demand
multi-agent automation invokable via the Workflow tool. It selects card(s),
drives the full plan → review → implement → review → cross-check → verify →
PR pipeline, and loops over a batch.

`docs/AI-CONTRIBUTOR.md` is **left unchanged** — it still serves external fork
contributors. This workflow is the maintainer-facing in-repo counterpart of the
same procedure.

## Decisions (from brainstorming)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Form | Workflow orchestration script (`.claude/workflows/`) | User wants push-button automation runnable inside Claude Code. |
| Orchestration approach | **A — the workflow is the orchestrator** | Calls leaf skills (`engine-planner`, `review-engine-plan`, `review-impl`) directly as its own `agent()` steps. Avoids nested agent-spawning (a Workflow `agent()` cannot reliably spawn sub-agents, which is what wrapping `engine-implementer` would require). Deterministic control flow lives in the script — exactly what the Workflow tool is for. |
| End state | Go all the way to PR | Mirrors AI-CONTRIBUTOR.md §7: commit → push → `gh pr create`. |
| Batch size | Batch — loop N cards | Accept a count; process cards sequentially. |
| Card source | Either — arg or auto-pick | If a card name is passed via `args`, use it verbatim; otherwise fetch coverage data and auto-pick low-gap unsupported cards (§3). |

### Approach A vs B (the orchestration fork)

- **A (chosen):** The script orchestrates the individual steps. Each leaf skill
  (`engine-planner`, `review-engine-plan`, `review-impl`) is invoked as its own
  `agent()` call. The workflow plays the role `engine-implementer`'s own
  orchestration plays, reusing the same leaf skills. Mild duplication of the
  step sequence is the only cost.
- **B (rejected):** Wrap `engine-implementer` in a single `agent()`. Thinner
  script, but relies on nested agent-spawning the harness restricts, and cedes
  loop control to the skill — making the batch loop and the §5 independent
  cross-check hard to insert cleanly.

## Architecture

### Entry point & arguments

`.claude/workflows/contribute-card.js`, `meta.name: "contribute-card"`.

`args` accepts either:
- a bare string — treated as a single card name, or
- an object `{ card?: string, count?: number }`
  - `card` — explicit card name (verbatim). When present, `count` is ignored
    (single-card run).
  - `count` — number of cards to auto-pick and process (default `1`). Only used
    when `card` is absent.

Argument normalization happens in one small helper at the top of the script.

### Phase 0 — Select (runs once)

- If a card name is supplied → work-list is `[name]`.
- Else → `WebFetch` the published coverage endpoint
  `https://pub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev/staging/coverage-data.json`
  (via an `agent()`, since the script itself has no WebFetch), and select the
  `count` cards where `supported == false`, `gap_count` is smallest (prefer
  1–3), excluding cards with known deferred-infrastructure dependencies (Rooms,
  Enchant Player, Suspend Aggression, etc.). Returns an ordered work-list of
  card names.
- `log()` the selected work-list so the user sees what will be built.

This phase produces `string[]` card names. It is the only place a barrier-style
single agent runs before the per-card loop.

### Phases 1..N — Per-card pipeline (sequential)

Cards are processed **sequentially**, not in parallel. Justification: the parser
touches shared files (`oracle.rs`, `oracle_static.rs`, effect modules), there is
a single working tree, card-data regeneration is global, and the model is
one-branch-per-PR. Parallel mutation would collide. A plain `for...of` loop with
`await` per card; a thrown error for one card is caught, logged, and the loop
continues to the next card.

Each card runs this chain (each step is one `agent()` call unless noted):

1. **Branch.** Collision-guarded branch creation (`card/<slug>`, append `-2`,
   `-3`, … if taken locally or on `origin`), per AI-CONTRIBUTOR.md §4. Run by a
   small bash agent (or folded into the implement agent's first action).
2. **Plan.** `engine-planner` skill → architecturally idiomatic plan for the
   card. Structured output: the plan text.
3. **Review plan.** `review-engine-plan` skill loop (max 3 rounds). If findings,
   feed back to a re-plan agent; stop when clean or max rounds hit.
4. **Implement.** Agent invokes the implementation using the AI-CONTRIBUTOR.md
   §4 prompt **verbatim** (build for the class, nom combinators first pass, CR
   annotations verified against `docs/MagicCompRules.txt` citing the
   *authorizing* rule, idiomatic Rust, engine owns logic, frontend display-only,
   reuse building blocks, proceed on scope expansion and note it).
5. **Review impl.** `review-impl` skill loop (max 3 rounds) against the
   uncommitted diff. Findings classified as defect/gap/missing-case must be
   addressed with code before proceeding.
6. **§5 independent cross-check.** A **fresh-context** reviewer agent that
   receives *only* the unified diff, `CLAUDE.md`, and the relevant skills — no
   prior conversation. It explicitly checks: (a) nom-mandate compliance
   (no `match` over stringified parser text, no chained `if let Ok = tag(..)`,
   no `.contains`/`.find`/`.split_once` dispatch); (b) CR-citation completeness
   (authorizing rule cited, not just layering rule); (c) pattern coverage
   (≥10 cards, not one); (d) logic placement (engine vs frontend); (e)
   building-block reuse; (f) bool-flag avoidance (typed enum over raw `bool`).
   Findings → feed back into the implement/review loop (bounded retries).
7. **Verify** (Developer track — maintainer has full toolchain). One agent runs,
   in order:
   - `cargo fmt --all` (always direct)
   - `./scripts/check-parser-combinators.sh` (Gate A; one-shot, direct)
   - Tilt-aware gate: if `tilt get uiresource clippy` succeeds →
     `./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data`; else
     `cargo clippy-strict && cargo test -p engine && ./scripts/gen-card-data.sh`
   - `cargo coverage` (confirm the card is now `supported: true`,
     `gap_count: 0`)
   - `cargo semantic-audit` (confirm zero findings for the card)
   - On failure: fix in-loop (max 2 retries), then record under "CI Failures"
     and continue — do not abort the card.
8. **Open PR.** Commit (`Add <Card Name>`), push the branch, `gh pr create`
   with the §7 body template. Body includes the canonical `Tier: Frontier` line,
   `Model:`/`Thinking:` lines, the `## Verification` checklist, and `None.`
   defaults for Scope Expansion / Validation Failures / CI Failures unless the
   run logged content. Title `Add <Card Name>`, or `Partial: <Card Name>` if
   validation/CI failures were unresolved. No `--label` flag (upstream
   auto-labeler handles it).

### Tier handling

The maintainer runs Opus (Frontier). The script assumes **Frontier** tier and
writes `Tier: Frontier` to the PR body, but still always runs Gate A
(`check-parser-combinators.sh`) in the verify step because it is cheap and
high-value. The script does not attempt model self-detection — the Frontier
assumption is recorded as a single constant with a comment.

### Return value

The workflow returns an array, one entry per card:

```js
{ card: string, branch: string, prUrl: string | null, status: "success" | "partial" | "aborted" }
```

and `log()`s a final one-line-per-card summary.

## Components & boundaries

| Unit | Purpose | Depends on |
|------|---------|-----------|
| `normalizeArgs(args)` | Resolve `args` (string \| object) → `{ explicitCard, count }` | none |
| Phase 0 select agent | Coverage fetch + low-gap pick | R2 coverage endpoint |
| `pickCardSchema` | Structured output for the work-list | — |
| per-card loop body | Branch → plan → review → implement → review → cross-check → verify → PR | leaf skills, bash, gh |
| `crossCheckSchema` | Structured findings from §5 reviewer | — |
| `verifySchema` | Structured pass/fail + per-command status | — |
| final summary | Aggregate `{card,branch,prUrl,status}[]` | — |

## Error handling

- **Per-card isolation:** the loop body is wrapped so one card's failure
  (planning dead-end, unrecoverable verify failure, gh error) is caught, the
  card is marked `partial` or `aborted` with the reason, and the loop proceeds.
- **Bounded review loops:** plan-review, impl-review, and cross-check each cap
  retries (max 3 / 3 / fold-into-impl). Hitting the cap without clean →
  the card is marked `partial` and its PR (if opened) uses the `Partial:` title
  with the failure recorded in the body, mirroring AI-CONTRIBUTOR.md §5/§7.
- **Verify failures:** max 2 in-loop fix retries, then recorded under "CI
  Failures" and the card still proceeds to PR (matching §6's "continue, do not
  abort").

## Testing / validation

This is an orchestration script, not engine code, so validation is behavioral:
- **Dry structural check:** invoke with an explicit, already-supported card name
  and a `count` of 1 to confirm phase wiring, schema validation, and the summary
  shape without depending on auto-pick.
- **Single auto-pick run:** `count: 1`, no card — confirm Phase 0 selection,
  one full pipeline, and a real PR (or `partial` with recorded reason).
- The script's own correctness (control flow, arg parsing) is verified by
  reading the persisted script + one real invocation; there is no unit-test
  harness for `.claude/workflows/` scripts in this repo.

## Out of scope

- Modifying `docs/AI-CONTRIBUTOR.md` (left as the external-contributor script).
- Parallel multi-card execution (rejected — shared files / single tree).
- Non-developer track (the maintainer has a full toolchain; Developer-track
  verification always runs).
- `mtgish/` paths (dormant — never touched, per AI-CONTRIBUTOR.md §0.5).
- Model self-detection / Standard-tier honesty-clause branching (Frontier
  assumed; Gate A runs unconditionally).
