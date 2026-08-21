---
name: review-engine-plan
description: Review phase.rs engine, parser, AI, frontend, or rules implementation plans before code is written. Use when Codex needs an architectural gate for plans involving parser changes, engine mechanics, MTG Comprehensive Rules behavior, new variants, targeting, replacement effects, stack/casting flow, AI policy, or frontend GameAction workflows.
---

# Review Engine Plan

Review the plan as an architectural gate. Reject the plan if any required dimension is missing, superficial, or contradicted by code evidence.

## Probe policy — you MAY and SHOULD run code

**You are not a read-only reviewer.** Building and running throwaway probes is an expected part of this
review, and it is the only instrument that can refute a plan whose prose is internally consistent but
whose runtime behaviour differs. Static review structurally cannot catch a predicate that reads
correctly and answers wrongly on a real board.

Use an isolated `CARGO_TARGET_DIR` and the worktree's absolute path; never build in a checkout another
process (e.g. Tilt) owns; serialize probe activity behind any active implementation executor. That
isolation is why no brief needs to withhold builds from you — if one says "do not run cargo", use your
own target dir and note it in your report.

## Required Checks

0. **Probe the plan, don't just read it**
   - The plan's central premise is yours to test, not merely to assess. Probe it against a real
     committed fixture or dump where one exists — synthetic state proves a predicate reads a field, a
     real board proves what it answers in production. A premise that only ever held on synthetic input
     is the highest-value refutation available to you.
   - The assertions worth probing are the ones whose falsity would change the design: a predicate's
     runtime verdict, which route or branch is actually taken, an observed count or delta, "X never
     happens", "this conjunct is what refuses". Where the plan asserts one of these from reading alone
     and you doubt it, go measure it — the finding is the wrong assertion, never the missing paperwork.
   - **Believe a probe only if it reached the code under test** — the plan's or your own. A zero with no
     positive control showing the instrument fires is not a negative result, and a run that didn't exit
     cleanly didn't measure anything. This is Check 9's paired positive reach-guard applied to the
     plan's evidence rather than to its tests; the two recurring shapes are a census that reports zero
     because the instrument never fired, and a discriminator whose verdict is really decided by an
     upstream conjunct that dominates it.
   - **Verify every board-census premise against a hostile fixture.** The shape that survives static
     review is a census predicate that ignores an applicability/filter field and so matches objects
     with nothing to do with the phenomenon. Run the census on a real fixture that *contains irrelevant
     objects* and confirm the predicate actually consults the applicability field. A census matching
     "almost everything" is a defect signature, not a result: **reject the premise** until that field
     check is shown. A plausible positive result on friendly input does not discharge this.
   - **A falsified snapshot claim is repaired by reformulation, not refresh.** When an edit falsifies a
     recorded count, coordinate, or cardinality ("four sites", "two red flags", a per-section tally),
     say so *and* require the durable form — a symbol name, "every X in §Y", or the command that
     regenerates the figure. Asking only for a corrected number re-arms the same defect for the next
     edit, which is how one stale claim becomes a multi-round loop.

1. **Class vs card**
   - Identify how many cards or patterns the plan covers.
   - Reject one-card plans unless the card is only the validating consumer of a reusable building block.

2. **Building-block reuse**
   - Confirm the plan consulted relevant existing modules from the CLAUDE.md building-block table.
   - Reject duplicated logic already covered by `parser/oracle_nom/`, `parser/oracle_util.rs`, `game/filter.rs`, `game/quantity.rs`, `game/ability_utils.rs`, `game/keywords.rs`, or nearby helpers.
   - Require justification for every new helper.

3. **Trace verification**
   - The plan must name an analogous existing feature and list the file path trace followed end to end.
   - Reject plans that did not trace an existing feature.

4. **Abstraction layer correctness**
   - Parser logic belongs in `parser/`.
   - Runtime rules belong in `game/` or `game/effects/`.
   - Types belong in `types/`.
   - Game logic must not leak into frontend or WASM bridge.
   - Display formatting must not leak into the engine.
   - **i18n boundary:** if the plan adds frontend UI or log text, it must route frontend-authored strings through `t()` (react-i18next, keys in `client/src/i18n/locales/en/<ns>.json`) and leave engine/card pass-through raw. Reject plans that hardcode user-facing chrome strings or that wrap card/Oracle/enum text in `t()`. See `client/src/i18n/README.md`.

5. **Idiomatic Rust**
   - Prefer typed enums such as `ControllerRef`, `Comparator`, and `Option<T>` over bool fields.
   - Prefer exhaustive matches over wildcard catch-alls when the type set is known.
   - Prefer existing `strip_prefix`/parser helpers over `format!()` plus matching.

6. **Nom compliance for parser plans**
   - If any parser file changes, the plan must specify exact `nom` combinators or existing parser functions for every detection, dispatch, or classification step.
   - Reject plans using `contains()`, `starts_with()`, `ends_with()`, `find()`, or heuristics for Oracle parsing.
   - The parser is the detector; try the real parser rather than duplicating detection logic.

7. **CR verification**
   - Every referenced CR number must be verified against `docs/MagicCompRules.txt`.
   - If CR comments are added or changed, the plan must say how they will be verified.

8. **Skill checklist adherence**
   - Identify applicable skills, such as `$add-engine-effect`, `$oracle-parser`, `$add-keyword`, `$add-trigger`, `$add-static-ability`, `$add-replacement-effect`, `$add-interactive-effect`, or `$casting-stack-conditions`.
   - Reject plans that omit required checklist steps from applicable skills.

9. **Verification matrix**
   - Reject plans without a claim-to-test map for every behavioral claim.
   - Each map entry must name the changed seam/function, production entry point, runtime test, revert-failing assertion, sibling/negative cases, and coverage status impact.
   - Reject helper-only tests for changes whose production path goes through `apply()`, `WaitingFor`/`GameAction`, casting/stack, combat declaration, replacement handling, or the scenario runner.
   - Parser shape tests do not satisfy runtime semantics or coverage-support claims. Parser-only shape tests are acceptable only when unsupported semantics remain honest via `Effect::unimplemented`, an equivalent strict-failure marker, or unchanged red coverage.
   - Reject parser plans that accept full Oracle text while dropping semantics unless the plan explicitly preserves an `Unimplemented`/coverage gap.
   - Reject negative-only test plans without a paired positive reach-guard: a planned assertion like `!detector(...)` or "X is NOT applied" must also prove the input got past upstream short-circuits (parse succeeded, zero `Effect::Unimplemented`), or an early-return makes it pass vacuously.
   - Cast-pipeline runtime tests must be planned via the `/card-test` recipe with the card's verbatim Oracle text, not a paraphrase.
   - If the plan adds a field to an existing enum variant or struct, require an enumeration of every construction/consumption site of that variant and how each threads the new field (resume/continuation paths, single-vs-multi-pick branches, and adapter payload constructors are the recurring drop points).

10. **Identity / provenance contract**
   - For any "this way", "that source", "chosen", "cast using", "from among them", selected target/mode, duration-bound effect, replacement predicate, or controller/owner-relative text, require the plan to name the source phrase/rules concept, selected authority type and id/value, binding time/event, live vs snapshotted/latched semantics, storage location, consumption point, invalidation/expiration behavior, and a multi-authority hostile fixture.
   - Reject plans that rely on rescanning matching permissions, sources, costs, replacements, tracked sets, controllers, owners, or choices at consumption time unless they prove the rescan is equivalent for a multi-authority fixture.

11. **Scope matrix**
   - For target, player, combat, controller, owner, protector, or defender changes, require the plan to enumerate the variants/scopes reachable at the touched production boundary.
   - Include permissions, costs, choice provenance, tracked sets, duration snapshots, source/controller/owner shifts, and serialization/protocol/card-data boundaries when those are touched.
   - Require negative tests for semantically adjacent sibling variants that are plausibly affected, or a concrete explanation for why a sibling is unreachable/out of scope.

12. **Sizing section** (severity depends on context)
   - Verify a Sizing section exists and is consistent with the plan body: a plan whose body names N independently tested behaviors must not report fewer units.
   - Blocking **only when the review's spawn inputs declare a phase-fit context** (the `/engine-implementer` and `/implement-task` pipelines). For any other consumer of this skill — standalone planner runs, `/batch-mechanics`, contributor plan reviews — a missing Sizing section is a non-blocking note, never a rejection; the "reject if any required dimension is missing" rule is not extended to Sizing outside phase-fit context.

## Modes

Ordinary mode is everything above. Three additional modes activate when the caller's spawn inputs name them; each scopes this file's mandatory language here, in this file — a spawn prompt never overrides this text.

### Charter mode (spawn inputs: a phase charter + the originating plan/task)

Review the decomposition, not per-phase detail. Checklist:

- **Seam green-tree safety** — every phase boundary leaves the tree compiling and tests green (strict-failure tags are the sanctioned way to hold coverage waiting between phases).
- **Each phase independently reviewable and shippable against its charter entry** — this does not mean every phase carries a full end-to-end test: a phase verification plan written `DEFERRED(phase n)` with a named landing phase is accepted; a deferred verification with no named landing phase is rejected.
- **Deferral lists complete and phase-attributed** — everything the full task needs that a phase omits names the phase that lands it.
- **Linear ordering respects dependencies** — infrastructure before consumer.
- **Recursive gate check** — no individual phase itself trips the T1∧T2 conjunction defined in `/engine-implementer`.
- **Premise verification present** — charter mode preserves engine-planner Step 0.
- **Scope entries are literal paths or directories, no globs** — the orchestrator's frozen `SCOPE_PATHS` representation and T2 directory expansion consume concrete paths (as does `/implement-task`'s snapshot pathspec machinery).

Checks that do not apply to a charter: 6 (nom compliance), 9 (verification matrix), 11 (scope matrix), and check 3's full end-to-end trace requirement — those apply later, to each phase plan under phase-plan mode. A charter's feasibility exit (a report that no green-tree seam exists) is reviewed on its named evidence: every candidate split point named, each shown to leave the tree non-compiling or tests red.

### Phase-plan mode (spawn inputs: charter + phase index + that phase's deferral allowlist)

Review one phase's plan given the charter. All ordinary checks apply to the phase's own claims, with this scoping: deferral-listed items are exempt unless the phase plan forecloses or contradicts them — **deferred ≠ gap; foreclosure = gap**.

- **Row-level rule for check 9:** a Verification Matrix row written `DEFERRED(phase n)` with a named landing phase is accepted; a deferred row with no named landing phase is rejected.
- **Pattern-coverage scoping for check 1:** "reject one-card plans" is assessed against the **charter's** class attribution, not the phase's own diff — a dependency-seam infrastructure phase covers zero cards by itself by construction; its class coverage lives in the charter and lands with the consumer phase.
- **Check 12 is inherent and blocking in this mode:** a phase plan without a Sizing section, or one inconsistent with its body, is a blocking finding — the section is the measured input for the orchestrator's per-phase re-adjudication.

### Sizing-audit mode (spawn input: a plan + its sizing addendum)

Check **only** Sizing consistency against the plan body (check 12's substance, nothing else). Used by `/engine-implementer` for pre-existing plans on both of its input paths — for an already review-clean plan because re-running the other checks would be redundant, and for a pre-existing draft because the addendum must be audited before the phase-fit gate adjudicates, which is earlier than any full review round.

## Review Loop

Return every gap to the planner. Require a revised full plan, then re-review the entire revised plan with fresh context. Repeat until a full round returns clean or the caller stops the process.

## Output

Lead with blockers and material gaps. For each issue, include evidence and the required revision. If the plan is clean, say that no blocking gaps were found and name any residual assumptions.
