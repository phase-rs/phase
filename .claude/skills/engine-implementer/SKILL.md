---
name: engine-implementer
description: "End-to-end phase.rs implementation pipeline: plan, review-plan, implement, review-impl, commit — each step run in a fresh spawned agent, with automatic phase decomposition for oversized workloads."
---

# Engine Implementer (Orchestrator)

This is the orchestrator for the phase.rs implementation pipeline. It runs as a **skill in the main thread** so it can spawn agents for every step that benefits from fresh context (plan review, surgical implementation, implementation review). Do not turn this into an agent — agents cannot spawn sub-agents, which is what made earlier versions silently degrade.

## Roles

| Step | Where it runs | Why |
|---|---|---|
| 1. Produce plan | **Spawned `general-purpose` agent** invoking `/engine-planner` | Fresh context = plan is shaped by the task, not by the conversation history that led here |
| 2. Review plan | **Spawned `general-purpose` agent** invoking `/review-engine-plan` | Fresh context = honest architectural review, independent of the planner |
| 3. Implement | **Spawned `engine-implementation-executor` agent** | Baseline measurement, surgical edits, and preparatory checks; never commits |
| 4. Checkpoint + measure | This thread, then a fresh measurement executor | Orchestrator creates the candidate commit; isolated executor measures that immutable candidate |
| 5. Complete verification | This thread | Verify the committed candidate, never an in-flight working tree |
| 6. Review implementation | **Spawned `general-purpose` agent** invoking `/review-impl` | Independent review of the immutable base-to-candidate diff |
| 7. Final acceptance | This thread | Accept only the exact reviewed checkpoint candidate |

**Runtimes without subagent spawning (contributor environments — Codex CLI, plain LLM sessions).** The pipeline's value comes from context isolation between author and reviewer, not from the spawning mechanism. If your runtime cannot spawn agents, do NOT silently degrade to reviewing your own work in the same context — that is the failure mode this skill exists to prevent. Instead: run each step against a fresh context (new session/conversation per step when your runtime supports it), and for every review step hand the reviewer ONLY the artifact under review (the full plan, or the unified diff), the original task description, `CLAUDE.md`, the relevant skill (`/review-engine-plan` or `/review-impl`), the brief scope/attempt history required below, and, in chartered runs, the charter, phase index, and deferral allowlist — never the conversation that produced it. If even that is impossible, say so explicitly in the final report and in the PR body under a "Validation Failures" heading; do not claim the review loop ran clean.

The orchestrator never authors content itself. Its only jobs are: spawn agents, route their output to the next step, apply the run limits while routing review steps, own the commit, and gracefully cull each spawned agent once its output is consumed (send a `shutdown_request` and wait for the `shutdown_response` ack — spawned agents now carry `SendMessage`, so they cull gracefully instead of being pane-killed). The structured report each agent returns stays the authoritative step handoff; SendMessage is an additive progress/acknowledgment channel, not a replacement.

## Run ownership and checkpoint identity

Before dispatching an executor, fix `BASE_SHA` and the in-scope paths for the run; in a chartered run each phase fixes its own `PHASE_BASE_SHA` at phase start. Every implementation or fix dispatch has a named `START_SHA` and `IMPLEMENTATION_WORKTREE` — the first round starts at `BASE_SHA`, a fix round at the prior reviewed `CANDIDATE_SHA`. Check `HEAD == START_SHA` and a clean tree before edits and again before the checkpoint, and stop if the diff has escaped the intended scope.

That is the whole provenance contract. **Do not build receipts, evidence records, manifests, digests, seals, ledgers, or any other artifact whose purpose is to prove to a later reader that these steps happened.** Git already records what changed and at which commit, and the reviewer reads the diff. Every step below is something you do and then act on, never something you notarize.

## Inputs

Either:

1. A task description (cards, CR rules, Oracle text patterns, affected subsystems, expected behavior), or
2. A pre-existing plan — treat as a draft unless it has already passed `/review-engine-plan` to clean.

Before Step 3, prepare and verify a clean `IMPLEMENTATION_WORKTREE` at `START_SHA`. After its checkpoint, prepare clean detached base and candidate projection worktrees at `BASE_SHA` and `CANDIDATE_SHA` (in a chartered phase these are `PHASE_BASE_SHA` and the phase's `CANDIDATE_SHA`), and a distinct clean detached `COMPLETION_WORKTREE` at `CANDIDATE_SHA`; no projection or completion worktree is used for implementation. Per `feedback_session_default_no_worktree`, do not re-ask about worktrees during an active pipeline session — use the session default.

Two build-economy rules govern measurement worktrees. **Build-once directories never pay for incremental state:** every measurement build — projection, completion, any target directory built once and never rebuilt — runs with `CARGO_INCREMENTAL=0` — incremental state on a directory that is never rebuilt is pure dead weight. **Completion allocation is per run-segment, not per candidate:** one completion worktree and one isolated completion target directory per phase (per run when unphased), reset to each round's `CANDIDATE_SHA`, instead of fresh ones per candidate — per-candidate allocation multiplies full cold builds for no isolation gain. Reuse is strictly sequential within the owning run — the metadata-hash collision hazard is concurrent writers on one target path, which sequential reuse never creates; never share a reused directory with any concurrently running agent or lane. A reused completion directory is no longer build-once, so the first rule's rationale does not carry to it automatically; it still runs `CARGO_INCREMENTAL=0`, since the disk cost is guaranteed and the warm-rebuild speedup is not. Re-measure if warm completion rebuilds start dominating wall clock.

**Sizing for pre-existing plans:** Step 1a requires a Sizing section regardless of how the plan arrived. A pre-existing plan lacking one — whether already `/review-engine-plan`-clean (which bypasses Step 1 entirely) or a draft (which reaches Step 1a before its first Step 2 round) — gets a **sizing addendum** from a spawned planner in `engine-planner` sizing-only mode, followed by a review loop in `review-engine-plan` sizing-audit mode: findings → a fresh planner revises the addendum → fresh sizing-audit re-review, subject to the shared [run limits](#run-limits) (T4's layer axis is undefined for a one-section artifact). Without this audit the addendum would be the only Sizing section no reviewer ever checks. The addendum, each round's result, and the adjudication are recorded in the phase-fit record. When Step 1a then fires multi-phase on an already-clean plan, the charter-mode planner partitions rather than re-plans (its review-clean input case).

## Task scope and verification work

Keep the original requested behavior and acceptance criteria in every handoff. Before a verification action, name the claim it answers and look for an existing test, fixture, command or supported tool API. Regression tests, fixtures built with existing helpers, and small probes using existing infrastructure are ordinary task work.

A card fix does not authorize a separate verification project: do not build a general-purpose browser driver, session manager, seeding service, cleanup protocol or framework just to validate it. Stop and report the missing evidence and the smallest existing-tool alternative. If the user explicitly requested that infrastructure, it is product work. Temporary, generated, untracked and outside-repository helpers remain subject to this boundary; exclusion from T2 is not permission to build them.

A test that reaches the product and reveals wrong behavior calls for a product fix. A helper or protocol that cannot produce trustworthy evidence calls for machinery recovery under the [run limits](#run-limits). Required evidence stays required when tooling blocks it: never drop the check or claim a clean/ready result to escape a limit.

## Run limits

Check these limits before every dispatch or verification-tool edit and after every result, including pre-measurement planning. They apply across all modes, phases and renamed successor work for the original task.

- **Review:** stop after **three unsuccessful review attempts** since the last accepted product candidate. Aggregate sizing, charter, plan, implementation and integration reviews. Findings requiring a revision count as an unsuccessful attempt; abandoning a dispatched review also counts. New findings count just as repeated ones do. Clean intermediate reviews consume no attempt and erase none. Charter corrections that require no re-review remain clean; review-only revisions requiring a fresh review count. Returning from implementation to planning carries the same history. Only accepting a candidate that implements part of the original requested product behavior resets this allowance; a checkpoint, clean plan, helper-only phase or new reviewer does not. If tooling itself is the user's requested product, its accepted implementation qualifies.
- **Verification machinery:** allow **one localized corrective attempt across the original task**, then rerun the blocked check or review the corrected design. This includes preparation and redesign before a helper first runs, as well as runtime repair. Before the correction, identify the missing evidence, concrete design/runtime defect, existing mechanism to repair, and check establishing recovery. If the correction fails, introduces another tooling defect, or later work needs another machinery correction, stop before further edits or dispatch. New helper names, phases, sessions and even accepted product candidates do not replenish this allowance. A proposed framework already exceeds the scope boundary above; the allowance does not authorize it.

Running documented setup, waiting for a legitimate build, adding ordinary product tests/fixtures and fixing product behavior do not consume the machinery allowance. Custom repair of broken setup does. A small probe may be prepared and run with existing infrastructure; once its own setup, protocol or cleanup needs correction, that work uses the same machinery allowance, even if described as another fixture or charter.

Every planner, reviewer and executor receives the original task, current scope and a brief attempt history, including the machinery correction already used. Carry this through handoffs and compaction; use a short summary in the existing phase-fit working note if persistence is needed. Do not create a new ledger, schema, tracking script or evidence-of-the-counter requirement. If prior history is unavailable, report the uncertainty and pause recovery rather than reset it.

**At a stop**, preserve commits and working changes. Report the original goal, completed product work, remaining findings/missing evidence, attempts made and smallest proposed next action. Do not launch another phase or charter to continue the same work. The user may explicitly authorize revised scope or another bounded attempt; a generic continuation or rename does not reset an exhausted allowance. Required checks and clean final review still govern acceptance. These stops take precedence over decomposition and surgical-fix routes below.

## Phase-fit gate and chartered runs

Oversized workloads make the review loops stop converging: each repair round is made against an artifact too large to hold, so repairs generate the next round's findings. The fix is shrinking the unit of review. This section defines when and how a run decomposes into sequential phases, each running the full plan → review → implement → review pipeline.

### Step 1a — the gate (after Step 1, before Step 2)

**Unit anchor:** one *unit* = one coherent mechanic/behavior implementable by a single skill-checklist pass (e.g. one `/add-engine-effect` traversal), regardless of how many lockstep layers that pass touches. A routine interactive effect wiring types/parser/resolver/frontend/AI is **one unit** — predictive triggers must not trip on it.

**The gate fires only on the conjunction T1 AND T2**, adjudicated against the plan's Sizing section (adjudication is measurement — the same category as the surgical-mode conditions, so it does not violate "the orchestrator never authors content"):

- **T1 — Unit count:** the plan contains ≥2 units.
- **T2 — Scope size:** expected scope-path count ≥13, counted mechanically with exclusion before grouping: test fixtures (regardless of authorship or commit status) and uncommitted/regenerated pipeline data are excluded outright — they inflate counts without adding review surface; then, among remaining files, a committed generated artifact groups with its source via a checked-in generator (committed `.d.ts` with source), and same-basename translation mirrors group with the authored file (the `en` locale counts; its six mirrors add nothing). Directory entries never count as one path — expand to expected changed files, then group. Each phase-fit record entry lists the groups used.
- **T3 — Dependency seams (seam-selection rule, not a trigger):** where one unit cannot be discriminating-tested until another lands (infrastructure→consumer), that edge is the preferred split point. Anything T3 would catch has ≥2 units and fires via T1∧T2.

Size without split structure (one large unit) and count without size (trivial multi-arm work) both stay single-phase; the conjunction confines false positives to the degenerate large-unit-plus-trivial-unit edge, where the cost is one tiny fast-converging phase.

**Re-adjudication:** the initial verdict is an estimate. Re-adjudicate (a) every time a fresh planner returns a revised plan during Step 2, and (b) at scope-freeze time against the materialized `SCOPE_PATHS` list (same counting rule), and again on every extension of it. Routes when a predictive firing occurs: **mid-Step-2 on a not-yet-clean draft** → charter-mode planner derives from the draft + accumulated findings, provided the run limits permit another dispatch. **At scope-freeze on a review-clean plan** → the charter-mode planner **partitions, not re-plans** — the charter carves converged content, each phase plan is a projection of reviewed material, so the split does not invalidate the reviewed artifact. **Inside a chartered phase before its executor dispatch** (its plan loop or its scope-freeze — zero landed candidates either way) → charter revision splitting that phase. A second-level predictive firing inside an already split phase stops for reassessment. With landed candidates requiring a new split, stop and propose a charter revision rather than automatically restarting or truncating the phase.

**Feasibility exit (the only single-phase path after a firing):** the charter-mode planner may report no green-tree seam exists — every candidate split point named and shown to leave the tree non-compiling or tests red. Record the named evidence in the phase-fit record and proceed single-phase.

### T4 — diagnosing broad non-convergence

T4 fires regardless of unit count — it overrides the one-unit anchor, because the anchor is a prediction while T4's three conditions together are an observation of non-convergence. T4 fires when rounds k−1 and k satisfy **all three**: (i) k ≥ 3 — never the first round pair; a fresh artifact's first review is routinely broad and breadth alone is not non-convergence; (ii) each round contains blocking findings classified into ≥3 distinct layers of the axis; (iii) round k's classified blocking count ≥ round k−1's — a shrinking count is a converging loop.

- **Axis:** the lockstep registration layer list — types / parser / resolver / targeting / frontend / AI / tests.
- **Severity mapping (exhaustive):** Step 2 rounds (`/review-engine-plan`: blockers and material gaps) — both count; Step 6 rounds (`/review-impl`: HIGH/MED/LOW) — HIGH and MED count, LOW does not, a checkpoint-mode clean verdict contributes zero; checkpoint mode's untagged gate "blocking findings" are process findings — always layer-unclassified, counting toward no layer.
- **Classification:** a finding is assigned to the layer(s) of the file(s)/plan-sections it names; multi-layer findings count toward each; findings naming nothing on the axis (process, CR-citation, cross-cutting) count toward none.
- **Spot-round exclusion (Step 2 loops only):** a round in which every finding is *spot* per the surgical-mode classification contributes to no T4 pair — spot findings are cheap check-and-replace and surgical mode takes precedence. No such exclusion in Step 6 loops, where surgical mode never operates; impl-loop spot-grade findings map to LOW, which already doesn't count.
T4 explains when decomposition may help; it does not authorize another dispatch. When the [run limits](#run-limits) stop the work, include this diagnosis and a proposed smaller scope in the handoff. Only explicit authorization to resume permits a new charter or restart. Unclassified process findings and spot findings still count toward the shared review limit even when they do not contribute to T4.

### Process records (append-only, by phase index only, never a commit SHA)

`<git-common-dir>/engine-implementer-runs/<run-id>/phase-fit` and `<run-root>/phase-charter`. The phase-fit record gets one numbered entry per adjudication — initial, each re-check, each T4 firing, each feasibility exit — carrying the Sizing values used, per-trigger measured results, the T2 groups used, the verdict, and for feasibility exits the named-seam evidence; and one numbered entry per review-only revision or correction, carrying its class, the before/after text, and the evidence that authorized it — the admitted path with its class evidence, or the replaced claim with its measurement — so the authorization survives the executor report that produced it. The no-SHA rule keeps both records inside the `surgical-mode-switch` carve-out. **There is no phase ledger** — a SHA-bearing acceptance record would be the prohibited parallel ledger; chain integrity is recomputed at run-level acceptance instead. In multi-phase runs, each `surgical-mode-switch` entry is additionally tagged with its phase index (index only), keeping interleaved entries from different phases' plan loops auditable.

### The charter

Authored by a freshly spawned planner in `engine-planner` **charter mode** (the orchestrator never authors), reviewed through `review-engine-plan` **charter mode** in its own loop. The loop follows the shared [run limits](#run-limits), including review-only revisions; T4's layer axis is undefined for charter-shaped findings. A charter-review round whose findings are all corrections (below) is a clean round: the orchestrator applies them and the charter freezes without a further round. A round carrying review-only findings is not clean until the orchestrator has applied them and a fresh charter-mode review of the whole charter returns none. Once clean, the charter's **decisions** are frozen: which phases exist, their order and seams, each phase's goal and acceptance rows, and what is deferred to which phase.

A charter is a contract about sequencing and acceptance, not a snapshot of the code. Two things never belong in it, because both change during implementation and each change would otherwise cost a planner round: a **file inventory** — per-phase scope is a scope rule (below), materialized into `SCOPE_PATHS` at scope-freeze — and **code-state assertions** — where a phase depends on how the code behaves today (a defect is live at base, a path is unreachable, a gate observes an outcome), the charter names the claim the phase must **establish** and the phase plan buys it by measurement; the charter never states the outcome as fact.

**Charter revision** has three classes, told apart by two questions: does the edit change a decision, and if not, could it change the acceptance basis or the authorization boundary — scope, a claim, or a measurement?

- **Decision revisions** may only add, split, merge, re-order, or re-scope **remaining** phases, or change another frozen decision of a remaining phase (its goal, an acceptance row, a seam, a deferral attribution), and run through the charter-mode planner and the charter review loop. Accepted phases are never reworked in place: a finding that invalidates accepted content becomes a **fix phase** — a later phase whose scope overlaps the earlier files.
- **Review-only revisions** change no decision but touch the acceptance basis or the authorization boundary: extending a phase's `SCOPE_PATHS` by a file in one of the scope rule's three standing classes, or replacing a code-state assertion with the claim the phase must establish, when the finding that names it supplies the replacement sentence and its measurement. For a scope-list addition, the orchestrator derives the updated list from the reported literal path and class evidence, verifies the path was absent before and occurs exactly once after, and preserves every existing entry; the executor need not supply replacement prose. For a prose revision, the orchestrator applies the supplied replacement itself as check-and-replace with two-sided verification (the old text present at the named coordinate before; the new text present exactly once after) and a sweep of the neighbours the edit breaks — the mechanics surgical-fix mode uses, borrowed without its entry conditions, which are defined over plan-review rounds and do not apply here. For either kind of review-only revision, the orchestrator then spawns a **fresh charter-mode review of the whole charter** before the charter freezes or any executor is dispatched against the extended list — whole-charter every time, never a delta read, so that a sequence of review-only revisions can never run indefinitely under a narrower review than the one the clean condition names. That reviewer checks what check-and-replace cannot: that each admitted path's evidence establishes its class, that each replaced claim's measurement buys it, and that no decision moved. No planner round is spent; a finding from that review that needs a decision is a decision revision.
- **Corrections** are mechanically verifiable and cannot alter scope, a claim, or a measurement: a figure, a citation, wording. The orchestrator applies them the same way and no re-review follows.

Both non-decision classes record the class and the before/after text in the phase-fit record, which is the audit trail until the next decision revision, whose charter review reads the accumulated edits as part of the artifact. A prose finding that supplies no replacement text, or any edit that would change a decision, is a decision revision whatever it is called; when in doubt, it is a decision, and a correction in doubt is a review-only revision.

### Per-phase identity and the substitution rule

Each phase k is a self-contained checkpoint pipeline with `PHASE_BASE_SHA` — phase 1: run-level `BASE_SHA`; phase k>1: phase k−1's accepted `CANDIDATE_SHA` — and its own scope, fixed at the phase's scope-freeze moment (after its plan loop, before executor dispatch; never before the phase plan exists). Per-phase scopes **may overlap** on shared registration files (`effects/mod.rs` and kin); sequential execution makes that safe — there is no global-partition requirement.

**Scope rule.** The `SCOPE_PATHS` list is materialized at scope-freeze by the orchestrator from the charter's phase entry and the phase plan's Sizing — materialization is measurement, the same category as the gate. Three standing classes are admitted to the list without any revision: files the compiler forces (an exhaustive `match` or constructor the phase's type change breaks), shared registration files (`effects/mod.rs` and kin, the integration-test `mod` list), and files in which no non-comment line is added or removed — together with any committed generated artifact T2 groups with such a file, since a doc comment on a binding-exported type regenerates its binding and CI byte-compares the pair; admit both or neither. A standing class never overrides an explicit out-of-bounds entry in the executor's spawn inputs: extension applies only to a standing-class path that was omitted from the materialized list, and a standing-class path that is explicitly out of bounds takes the ordinary stop-and-return decision path below, never re-dispatch. Every extension re-counts T2 over the extended list and re-evaluates the full T1 AND T2 conjunction; T2 alone never triggers a split. Only when the conjunction fires does the extension take the in-phase predictive route above, subject to its stop conditions and the shared run limits. An executor that meets one of these outside the list does not edit it — its authorized-path check is unchanged — but stops and returns the site as an admitted-class addition with its evidence (the compiler error, the registration site, the comment-only change it needs); the orchestrator extends `SCOPE_PATHS` as a review-only revision — its whole-charter review confirms the path's class against that evidence before anything else happens — then restores the implementation worktree to `START_SHA` and re-dispatches the same round with the extended list and the executor's report as constraints. No planner round; the reviewer round is the authorization for the broader write set. `RUST_PATHS` and every other list the executor derives from its authorized paths follow the extended list at re-dispatch. An out-of-list file of any other class is a stop-and-return of the ordinary kind: it is a decision, and it goes through charter revision. T2 re-adjudication at scope-freeze counts the materialized list. **Within a phase, every occurrence of `BASE_SHA` in the Inputs worktree preparation and Steps 3–7 (checkpoint delta, `scoped_diff_command`, projection worktrees, completion parser-gate range, Step 6 review span) means `PHASE_BASE_SHA`,** — the literal `"$BASE_SHA"` command templates stay byte-identical while the shell variable carries the phase base, so checkpoint-mode validation needs no changes. Run-level `BASE_SHA` remains available to seed phase 1's `PHASE_BASE_SHA` and for the final integration span; per-phase operations use `PHASE_BASE_SHA`.

**Per-phase spawn inputs:** All roles also receive the original task and shared scope/attempt history. Step 1 planners run in `engine-planner` **phase-plan mode** with the charter, the phase's entry, its deferral allowlist, and prior phases' accepted summaries (never their debates). Step 2 reviewers run in `review-engine-plan` **phase-plan mode** with the phase plan, the original task, the charter, the phase index, and the allowlist — and *all* Step 2 reviews in this pipeline, unphased and per-phase alike, declare the phase-fit context so the Sizing consistency check is blocking here. Step 3 executors run in the executor's **phase mode** with the charter, phase index, and allowlist, so the matrix and test map they author use the same `DEFERRED(phase n)` vocabulary their reviewers audit. Step 6 reviewers run in `/review-impl` **phase mode** with the charter, phase index, allowlist, and the phase's materialized `SCOPE_PATHS`.

### Run-level acceptance (after the last phase)

Per-phase acceptance is today's Step 7 applied to the phase — a clean review, completion checks passing at the candidate, and `rev-parse HEAD == CANDIDATE_SHA` — and **emits no Final Report snapshot and no PR-handoff block**; those are run-level only. Run-level final acceptance requires all of:

1. **Every phase accepted** on its own review and checks.
2. **The phases actually tile the run**, checked against git: each phase's accepted candidate is the next phase's base, and `git rev-list <prior accepted>..<phase accepted>` contains only that phase's own commits. Restarted and abandoned candidates legitimately sit outside those intervals — list them in the Final Report rather than forcing the intervals to match.
3. **The integration review returns zero findings**, run in `/review-impl` **integration mode** (findings-only; scoped to cross-phase seams and charter completeness). Reviewer inputs: the run-span `BASE_SHA..final CANDIDATE_SHA` diff, charter and shared scope/attempt history. Findings dispatch a fix phase via charter revision only while the run limits permit. **Bound:** at most one fix phase per integration round; findings still present after two fix phases → stop and surface to the user.

## Pipeline

### Step 1 — Produce the plan

Spawn a `general-purpose` agent and instruct it to invoke `/engine-planner`. The agent returns a plan with every mandatory architectural section.

**Spawn inputs:** original task and shared scope/attempt history; in-scope file/subsystem hints; any prior reviewer findings (none on first round); the requirement to emit the mandatory Sizing section (Step 1a adjudicates against it). In chartered runs, per-phase planners instead run in `engine-planner` phase-plan mode with the inputs listed under "Per-phase spawn inputs".

Do not author or edit the plan in this thread — surgical-fix mode (below) is the one exception, and only under its three measured conditions. If the returned plan is missing sections or is superficial, send the same inputs plus an explicit "missing sections" note to a **fresh** planning agent — do not patch it yourself.

### Step 1a — Phase-fit gate

Adjudicate the gate as defined in "Phase-fit gate and chartered runs", appending the phase-fit record entry. Single-phase verdict → the pipeline below proceeds unchanged (plus the stated re-adjudication points). Multi-phase verdict → spawn the charter-mode planner, run the charter review loop, then iterate phases — each phase runs Steps 1–7 with the per-phase spawn inputs and the `PHASE_BASE_SHA` substitution rule, followed by run-level acceptance.

### Step 2 — Review the plan within the run limits

Spawn a `general-purpose` agent and instruct it to invoke `/review-engine-plan` against the full plan.

**Reviewer spawn inputs:** the full plan; shared scope/attempt history; the original task description; the phase-fit context declaration (all Step 2 reviews in this pipeline declare it, so the Sizing consistency check is blocking here); in chartered runs additionally the charter, phase index, and deferral allowlist (phase-plan mode).

If the reviewer returns gaps and the run limits permit another attempt, spawn a **fresh** planning agent (Step 1 inputs plus the reviewer's findings as additional constraints) to produce a revised plan, then spawn a **fresh** reviewer agent against the revised plan.

A clean plan is required to proceed. Stop at the [run limits](#run-limits), for a human design decision the planner cannot resolve, for missing external access, or for an environment blocker that makes review impossible. A limit is a handoff, never permission to ship findings. T4 may inform the proposed next action but cannot bypass the stop.

Each review must run in a fresh agent context — never reuse the previous reviewer's context.

#### Surgical-fix mode — when the design is settled and the findings are spot drift

The loop above assumes findings move the **design**. Once they stop doing that, re-running it makes the artifact worse: a fresh planner rewrites prose to absorb each finding, prose is where spot findings live, so every round manufactures the next round's findings.

**When all three hold, switch modes** — measure them, do not judge them:

1. The design is unchanged for ≥2 consecutive rounds (compare the named entries themselves — which steps, sub-steps, enum variants, and call sites each round names, because a 1:1 substitution holds every count constant; **not** a count and **not** line count; an in-place rewrite that preserves every name survives this comparison and is caught only by the whole-artifact re-review below).
2. The last round's findings are all **spot** — a stale number, a stale coordinate, a claim contradicted by a neighbouring section, a missing restatement of a control the plan already specifies, a sentence never swept. None changes what the implementation does.
3. Each finding names a coordinate **and** its replacement text. If any finding requires *deciding* something, it is a design finding: stay in the loop.

**Do not add a fourth condition based on falling churn.** Round-over-round churn shrinks while a loop turns unproductive: smaller repairs to a growing record. It measures edit size, not convergence, and gating on it blocks the switch precisely when the switch is warranted.

**The corroborating signal, if you want one, is the fraction of a round's findings whose defect originated in the *previous* round's repairs.** It climbs as the loop starts feeding on itself, but not monotonically — so treat a high fraction as evidence for the switch, never as the trigger.

**In surgical-fix mode the orchestrator applies the findings itself**, as check-and-replace edits — the one narrow exception to "the orchestrator never authors content." It is *applying* adjudicated text, not authoring; the moment a fix needs a decision, dispatch a planner instead. Requirements:

- **Two-sided verification per edit:** before the edit, the quoted old string is present at the finding's named coordinate — a quote that is not there is a stale coordinate, not an applicable fix; after the edit, the text the replacement adds is present exactly once and sits where the old string was, and the old string is absent — except that when the replacement contains the old string, that string survives by construction and the added text is the sole gate; count occurrences, not matching lines, 1:1 per fragment, not a lucky aggregate.
- **State the sweep's boundary.** A changelog entry that quotes the struck text will match your own grep for it. Population, predicate, scan direction, and whether the matched line counts — write them down; every enumeration defect is an unstated predicate rather than a bad measurement.
- **Fix the neighbours the fix breaks.** A finding's repair frequently contradicts a section that classified the old form. Sweep by mechanism, not by coordinate.
- **Then re-review the WHOLE artifact**, fresh context — not just the repaired sections, per `$bug-triage`'s targeted-re-review rule. Repeat apply → whole-artifact re-review only while the run limits allow; any finding that requires *deciding* something ends surgical mode and returns to the bounded plan-review loop above. Surgical mode replaces the planner-rewrite rounds, never the final independent check.
- **Record the mode switch, its three measurements, the spot-vs-design classification of each round's findings, and why the mode ends** in `<git-common-dir>/engine-implementer-runs/<run-id>/surgical-mode-switch` (in multi-phase runs, tag each entry with its phase index — index only, no SHA), never in the plan text the fresh re-reviewer and the executor read — recording it there hands the one remaining independent check a prior verdict. It is a working note for this loop, not a provenance record. Append one numbered entry per round, never overwrite — ending surgical mode and re-entering it later continues the same numbered sequence; only a round that enters the mode records the switch and its three measurements, and only a round that ends the mode records why.

**This does not contradict `$bug-triage`'s fixpoint gate.** That gate requires whole-plan re-review because *"revisions routinely INTRODUCE new gaps in untouched-looking areas"* — planner **rewrites** do. A check-and-replace at a named coordinate does not rewrite, which is why it is the safe tool once the design has stopped moving. `$review-engine-plan` ends its loop with *"or the caller stops the process"* and states no criteria; this section is those criteria, and it lives here because the orchestrator is that caller.

This is not a licence for "two rounds and ship". The run limits apply in either mode; switching to surgical fixes does not reset attempts. The measured conditions above determine whether surgical fixes are appropriate. Surgical mode is scoped to this Step 2 plan-review loop only — Step 6's implementation-review loop never uses it, because there the artifact is a committed candidate that only an executor may edit under the `SCOPE_PATHS` contract.

### Step 3 — Dispatch implementation

Spawn the `engine-implementation-executor` agent.

**Spawn inputs:** mode `implementation/fix`; shared scope/attempt history; the reviewed clean plan in full; `BASE_SHA`; named `START_SHA`; the in-bounds / out-of-bounds path list; named `IMPLEMENTATION_WORKTREE`; any prior reviewer findings (none on first round); in chartered runs additionally the charter, phase index, and deferral allowlist (the executor's phase mode). First round: `START_SHA == BASE_SHA`. Fix round: `START_SHA` is the previously reviewed `CANDIDATE_SHA`, never a moving branch head.

The implementation executor edits only its `SCOPE_PATHS` list — an admitted-class site outside it is a stop-and-return the orchestrator answers with a review-only extension (one fresh charter-mode reviewer, no planner) and a re-dispatch — and runs **preparatory** checks. Preparatory success is not completion evidence. Its existing discriminating-test, selected-authority, coverage-honesty, maintainer-simulation, and CR-annotation gates remain the authoritative gates; do not restate or replace them here.

If the executor returns "stop and return" items (plan contradicts current code, ad hoc parser dispatch unavoidable, CR uncertain), do NOT improvise around them. Check the run limits and scope boundary first; machinery failures use the shared correction allowance. If further work is permitted, return to Step 1 with the executor's findings as constraints and re-run Steps 1–3 without resetting attempt history — in a chartered phase this resolves to the *phase's* plan step under phase-plan mode, never a fresh full-task Step 1.

**Large JSON fixture constraint.** Any repository-bound JSON fixture ≳100KB (test fixtures, game-state dumps, generated maps — not runtime/config JSON whose consumers read plain `.json`) gets `gzip -9 -n` (`-n` keeps the archive byte-reproducible) and loads via the established inflate pattern: `include_bytes!("….json.gz")` + a test-local `gunzip` helper using `flate2::read::GzDecoder` (examples: `tests/integration/combo_infinite_pile.rs`, `cr733_resolved_commands_p0.rs`). Never commit the uncompressed twin alongside the `.json.gz`. If a fixture is regenerated by a script, note in the reading test that regeneration requires re-gzipping.

### Step 4 — Checkpoint the candidate

The checkpoint is the candidate commit, and it is the orchestrator's to make — never the executor's. Stage each approved path by explicit pathspec: never `git add -A`, and never commit without a pathspec, because the shared index can sweep in another agent's staged files (`feedback_git_add_file_bundles_concurrent_work`, `feedback_shared_index_commit_pathspec`). Before staging, confirm no pre-existing change overlaps an approved path; if attribution is ambiguous, stop and return rather than unstage, sweep in, or overwrite another agent's work. Commit, then confirm `git -C "$IMPLEMENTATION_WORKTREE" rev-parse HEAD` equals the `CANDIDATE_SHA` you recorded, and that `START_SHA..CANDIDATE_SHA` contains only the intended paths. Never measure an uncommitted tree or use a moving `HEAD` as the candidate. Verify `HEAD` is attached before any explicitly requested push (`feedback_verify_head_attached_before_push`), never pipe `git push` into `tail`/`head` (`feedback_git_push_no_pipe`), and never push unless asked.

If the change touches the parser, find out whether it moves parser output: build the tooling from the base and the candidate, generate card data from each against the same pinned data root, and diff the two. Report what changed. `./scripts/gen-card-data.sh` and `cargo coverage` do not answer this question.

### Step 5 — Verify the committed candidate

Run the checks in a clean worktree at `CANDIDATE_SHA`, not in the implementation worktree — a check that passes against uncommitted edits has told you nothing about what you are shipping. Run every gate the changed surface calls for: formatting for any implementation change, the Rust/engine/parser block for Rust paths, the frontend block for frontend paths, the parser gate for parser paths. Markdown-only policy changes need scope and diff checks; do not run Cargo or Tilt for them.

The full suite is owed at the tree being shipped. An intermediate fix round may narrow to the touched surface — say so plainly when reporting it, since a narrowed run is not a suite pass — and re-run unfiltered before acceptance.

### Step 6 — Review the immutable candidate

Spawn a fresh `general-purpose` agent to invoke `/review-impl` against `BASE_SHA..CANDIDATE_SHA`, with the original task, reviewed plan, in-scope paths, prior findings and shared attempt history. It reviews the diff and the checks that were run; additional checks must answer a concrete unresolved claim within the task scope. Missing evidence returns to the orchestrator, not an independent tooling project. After the result, apply the [run limits](#run-limits) before any fix or return to planning. A clean review proceeds to Step 7; a failure there is not acceptance and does not reset the allowance.

### Step 7 — Final acceptance

Accept when the plan-review loop is clean, the review returns no findings, the completion checks pass at the candidate, and `rev-parse HEAD == CANDIDATE_SHA`. In a chartered run this is per-phase acceptance, with `PHASE_BASE_SHA` substituted; it emits no Final Report snapshot and no PR handoff, which are run-level only.


### Prepare the completed work for a PR

When the task includes opening a PR, perform this handoff after the pipeline completes and before the caller's final review and Gate A. Local-only implementation does not implicitly synchronize a remote. The rule is simple: finish the work, synchronize once, then test and review the code being submitted. A commit SHA identifies that code; it is not another artifact to create.

1. Confirm the actual PR target repository and base branch. Use `upstream` for the contributor fork workflow, or `origin` only when it points to the target repository. Fetch that branch once and retain the fetched commit as the comparison base. If the target is unclear or fetching fails, report the blocker rather than claiming the branch is synchronized.
2. With no active workers or checks, and a clean feature checkout containing the committed work, merge that fetched commit. Do not merge into a projection/completion worktree or mutate a candidate under active verification. If conflicts need edits, delegate them to a scoped general-purpose worker; this is PR preparation, not the checkpoint executor mode that requires a clean `START_SHA`. The orchestrator commits the resolution. Preserve unrelated work; never stash it. Keep the original task scope and [run limits](#run-limits); a conflict that invalidates the approved design returns to plan review.
3. Once the merge is complete and the checkout is clean, run the Step 5 checks required by the changed surface on the resulting committed branch. For parser changes, compare parser output against the fetched upstream commit using Step 4's measurement procedure. Use the full diff from that fetched commit to the branch head, so upstream changes are not attributed to this PR. Reuse prior evidence only when its committed head and comparison base both match; a pre-merge pass does not verify a later merge.
4. Hand the resulting commit and complete PR diff to the caller's ordinary final `/review-impl` and Gate A (§5–6 of [AI-CONTRIBUTOR.md](../../../docs/AI-CONTRIBUTOR.md#5-validate-the-review-actually-happened-and-was-addressed)). Do not substitute checkpoint-mode or phase-only review for this full-head review. Delegate any fixes to a scoped worker and commit them, then repeat the required checks and final review for the new commit against the same fetched base. Do not refetch in that loop: later upstream movement does not restart this preparation.

Keep earlier accepted checkpoints and phase-chain evidence intact. If synchronization changes the branch head, use the existing historical/current-head handoff below; do not relabel old results as verification of the new code or introduce more SHA fields. This can require repeating checks when synchronization changes code. A later deliberate merge or rebase invalidates the current-head evidence again.

### Post-acceptance PR handoff (non-gating)

Final acceptance emits an immutable Final Report snapshot: `Pipeline-reviewed head == Current branch head == accepted CANDIDATE_SHA`, `Pipeline status: current`, and `Current-head review: none`. Do not alter or replace that snapshot, the accepted candidate SHA after acceptance.

Copy the following mutable `PR handoff` block into the PR body beside the retained pipeline report:

```text
Pipeline-reviewed head: <accepted CANDIDATE_SHA>
Current branch head: <current branch SHA>
Pipeline status: current | historical — <reason>
Current-head review: none | clean at <SHA> | findings at <SHA>
```

Whenever the branch head changes after acceptance, including through a rebase, update `Current branch head`, set `Pipeline status` to `historical — <reason>`, and reset `Current-head review` to `none`. When current-head evidence is desired, run ordinary `/review-impl` against the complete current PR/head — not checkpoint mode and not an incremental-only diff — then record `clean at <SHA>` or `findings at <SHA>` for that exact SHA. Each future head change repeats the reset. `Pipeline status` remains historical unless the current head again equals the original accepted `CANDIDATE_SHA`, in which case set it to `current`.

If later work invalidates the approved plan or architecture, return to plan review. Otherwise, this is a concise reporting and navigation flow only: it is not a gate, GitHub automation, an executor change, or a PR-handler change.

## Final Report

Return after final acceptance:

1. Plan-review rounds (count), whether surgical-fix mode was used, and final clean result.
2. What changed, grouped by subsystem and file.
3. Key architectural decisions.
4. `BASE_SHA`, accepted `CANDIDATE_SHA`, the materialized `SCOPE_PATHS` (review-only extensions included), and run-artifact root.
5. The `START_SHA` each round began from, and what the parser measurement found when the change touched the parser.
6. Verification commands run and results, separated into preparatory and completion evidence.
7. Implementation-review rounds (count), reviewed SHA, and final clean result.
8. Checkpoint commit hash and staged file list.
9. Coverage impact for parser changes.
10. Deviations from the plan with reasons.
11. Self-flagged risks and judgment calls (yours + executor's).
12. Remaining items, if any, with reasons.
13. Phase-fit verdict and record path, unsuccessful review attempts, any machinery correction, and abandoned candidates from an explicitly authorized restart.
14. Chartered runs additionally: phase count; per-phase accepted `CANDIDATE_SHA`s; abandoned candidates from explicitly authorized restarts; the phase-charter record path; the chain-integrity result; and the integration-review result.
