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

**Runtimes without subagent spawning (contributor environments — Codex CLI, plain LLM sessions).** The pipeline's value comes from context isolation between author and reviewer, not from the spawning mechanism. If your runtime cannot spawn agents, do NOT silently degrade to reviewing your own work in the same context — that is the failure mode this skill exists to prevent. Instead: run each step against a fresh context (new session/conversation per step when your runtime supports it), and for every review step hand the reviewer ONLY the artifact under review (the full plan, or the unified diff), the original task description, `CLAUDE.md`, the relevant skill (`/review-engine-plan` or `/review-impl`), and, in chartered runs, the charter, phase index, and deferral allowlist — never the conversation that produced it. If even that is impossible, say so explicitly in the final report and in the PR body under a "Validation Failures" heading; do not claim the review loop ran clean.

The orchestrator never authors content itself. Its only jobs are: spawn agents, route their output to the next step, loop review steps until clean, own the commit, and gracefully cull each spawned agent once its output is consumed (send a `shutdown_request` and wait for the `shutdown_response` ack — spawned agents now carry `SendMessage`, so they cull gracefully instead of being pane-killed). The structured report each agent returns stays the authoritative step handoff; SendMessage is an additive progress/acknowledgment channel, not a replacement.

## Run ownership and checkpoint identity

Before dispatching an executor, fix `BASE_SHA` and the in-scope paths for the run; in a chartered run each phase fixes its own `PHASE_BASE_SHA` at phase start. Every implementation or fix dispatch has a named `START_SHA` and `IMPLEMENTATION_WORKTREE` — the first round starts at `BASE_SHA`, a fix round at the prior reviewed `CANDIDATE_SHA`. Check `HEAD == START_SHA` and a clean tree before edits and again before the checkpoint, and stop if the diff has escaped the intended scope.

That is the whole provenance contract. **Do not build receipts, evidence records, manifests, digests, seals, ledgers, or any other artifact whose purpose is to prove to a later reader that these steps happened.** Git already records what changed and at which commit, and the reviewer reads the diff. Every step below is something you do and then act on, never something you notarize.

## Inputs

Either:

1. A task description (cards, CR rules, Oracle text patterns, affected subsystems, expected behavior), or
2. A pre-existing plan — treat as a draft unless it has already passed `/review-engine-plan` to clean.

Before Step 3, prepare and verify a clean `IMPLEMENTATION_WORKTREE` at `START_SHA`. After its checkpoint, prepare clean detached base and candidate projection worktrees at `BASE_SHA` and `CANDIDATE_SHA` (in a chartered phase these are `PHASE_BASE_SHA` and the phase's `CANDIDATE_SHA`), and a distinct clean detached `COMPLETION_WORKTREE` at `CANDIDATE_SHA`; no projection or completion worktree is used for implementation. Per `feedback_session_default_no_worktree`, do not re-ask about worktrees during an active pipeline session — use the session default.

Two build-economy rules govern measurement worktrees. **Build-once directories never pay for incremental state:** every measurement build — projection, completion, any target directory built once and never rebuilt — runs with `CARGO_INCREMENTAL=0` — incremental state on a directory that is never rebuilt is pure dead weight. **Completion allocation is per run-segment, not per candidate:** one completion worktree and one isolated completion target directory per phase (per run when unphased), reset to each round's `CANDIDATE_SHA`, instead of fresh ones per candidate — per-candidate allocation multiplies full cold builds for no isolation gain. Reuse is strictly sequential within the owning run — the metadata-hash collision hazard is concurrent writers on one target path, which sequential reuse never creates; never share a reused directory with any concurrently running agent or lane. A reused completion directory is no longer build-once, so the first rule's rationale does not carry to it automatically; it still runs `CARGO_INCREMENTAL=0`, since the disk cost is guaranteed and the warm-rebuild speedup is not. Re-measure if warm completion rebuilds start dominating wall clock.

**Sizing for pre-existing plans:** Step 1a requires a Sizing section regardless of how the plan arrived. A pre-existing plan lacking one — whether already `/review-engine-plan`-clean (which bypasses Step 1 entirely) or a draft (which reaches Step 1a before its first Step 2 round) — gets a **sizing addendum** from a spawned planner in `engine-planner` sizing-only mode, followed by a review loop in `review-engine-plan` sizing-audit mode: findings → a fresh planner revises the addendum → fresh sizing-audit re-review, inheriting Step 2's stop conditions plus the 4-round charter-loop backstop (T4 is inactive here; its axis is undefined for a one-section artifact). Without this audit the addendum would be the only Sizing section no reviewer ever checks. The addendum, each round's result, and the adjudication are recorded in the phase-fit record. When Step 1a then fires multi-phase on an already-clean plan, the charter-mode planner partitions rather than re-plans (its review-clean input case).

## Phase-fit gate and chartered runs

Oversized workloads make the review loops stop converging: each repair round is made against an artifact too large to hold, so repairs generate the next round's findings. The fix is shrinking the unit of review. This section defines when and how a run decomposes into sequential phases, each running the full plan → review → implement → review pipeline.

### Step 1a — the gate (after Step 1, before Step 2)

**Unit anchor:** one *unit* = one coherent mechanic/behavior implementable by a single skill-checklist pass (e.g. one `/add-engine-effect` traversal), regardless of how many lockstep layers that pass touches. A routine interactive effect wiring types/parser/resolver/frontend/AI is **one unit** — predictive triggers must not trip on it.

**The gate fires only on the conjunction T1 AND T2**, adjudicated against the plan's Sizing section (adjudication is measurement — the same category as the surgical-mode conditions, so it does not violate "the orchestrator never authors content"):

- **T1 — Unit count:** the plan contains ≥2 units.
- **T2 — Scope size:** expected scope-path count ≥13, counted mechanically with exclusion before grouping: test fixtures (regardless of authorship or commit status) and uncommitted/regenerated pipeline data are excluded outright — they inflate counts without adding review surface; then, among remaining files, a committed generated artifact groups with its source via a checked-in generator (committed `.d.ts` with source), and same-basename translation mirrors group with the authored file (the `en` locale counts; its six mirrors add nothing). Directory entries never count as one path — expand to expected changed files, then group. Each phase-fit record entry lists the groups used.
- **T3 — Dependency seams (seam-selection rule, not a trigger):** where one unit cannot be discriminating-tested until another lands (infrastructure→consumer), that edge is the preferred split point. Anything T3 would catch has ≥2 units and fires via T1∧T2.

Size without split structure (one large unit) and count without size (trivial multi-arm work) both stay single-phase; the conjunction confines false positives to the degenerate large-unit-plus-trivial-unit edge, where the cost is one tiny fast-converging phase.

**Re-adjudication:** the initial verdict is an estimate. Re-adjudicate (a) every time a fresh planner returns a revised plan during Step 2, and (b) at scope-freeze time against the actual frozen list (same counting rule). Routes when a predictive firing occurs: **mid-Step-2 on a not-yet-clean draft** → same route as plan-loop T4 below (charter-mode planner derives from the draft + accumulated findings). **At scope-freeze on a review-clean plan** → the charter-mode planner **partitions, not re-plans** — the charter carves converged content, each phase plan is a projection of reviewed material, so the split does not invalidate the reviewed artifact. **Inside a chartered phase before its executor dispatch** (its plan loop or its scope-freeze — zero landed candidates either way) → charter revision splitting that phase.

**Feasibility exit (the only single-phase path after a firing):** the charter-mode planner may report no green-tree seam exists — every candidate split point named and shown to leave the tree non-compiling or tests red. Record the named evidence in the phase-fit record and proceed single-phase. After a **T4** firing this combination is instead a **terminal stop** (below).

### T4 — the retroactive trigger (observed non-convergence, both loops)

T4 fires regardless of unit count — it overrides the one-unit anchor, because the anchor is a prediction while T4's three conditions together are an observation of non-convergence. T4 fires when rounds k−1 and k satisfy **all three**: (i) k ≥ 3 — never the first round pair; a fresh artifact's first review is routinely broad and breadth alone is not non-convergence; (ii) each round contains blocking findings classified into ≥3 distinct layers of the axis; (iii) round k's classified blocking count ≥ round k−1's — a shrinking count is a converging loop.

- **Axis:** the lockstep registration layer list — types / parser / resolver / targeting / frontend / AI / tests.
- **Severity mapping (exhaustive):** Step 2 rounds (`/review-engine-plan`: blockers and material gaps) — both count; Step 6 rounds (`/review-impl`: HIGH/MED/LOW) — HIGH and MED count, LOW does not, a checkpoint-mode clean verdict contributes zero; checkpoint mode's untagged gate "blocking findings" are process findings — always layer-unclassified, counting toward no layer.
- **Classification:** a finding is assigned to the layer(s) of the file(s)/plan-sections it names; multi-layer findings count toward each; findings naming nothing on the axis (process, CR-citation, cross-cutting) count toward none.
- **Spot-round exclusion (Step 2 loops only):** a round in which every finding is *spot* per the surgical-mode classification contributes to no T4 pair — spot findings are cheap check-and-replace and surgical mode takes precedence. No such exclusion in Step 6 loops, where surgical mode never operates; impl-loop spot-grade findings map to LOW, which already doesn't count.
- **At most once per run:** a T4 firing when the phase-fit record already contains any prior T4 firing or feasibility exit **stops the run and surfaces to the user**. After a branch-(b) return to Step 1, the fresh Step 1a re-measures freely (a redesigned plan may honestly size single-phase), but the persisted T4 entry makes a second observed non-convergence terminal.
- **T4 + infeasible decomposition is a terminal stop** — proceeding single-phase would re-enter the very loop whose non-convergence was just measured.

These two terminal stops join the unbounded loops' enumerated stop-condition list alongside the existing three.

**Routes.** *Step 2 loop, unphased run:* exit the loop, spawn a charter-mode planner with the current draft plan and accumulated findings, run the charter review loop, proceed per-phase. *Step 6 loop, unphased run:* two branches, and in both the charter exists **before** any acceptance — (a) if the candidate can plausibly be stabilized green-and-coherent: first spawn the charter-mode planner (phase 1 = the stabilized current candidate + its deferral list; phases 2..n = the remainder — the planner's third input case), run the charter review loop, then one stabilization fix round, then the normal checkpoint → measure → completion → review sequence under `/review-impl` phase mode with phase 1's allowlist; zero findings → accept as phase 1; findings → resume normal fix rounds within phase 1; (b) if it cannot be made coherent, return to Step 1 with a decomposition directive — the abandoned candidates stay outside every accepted interval and are listed in the Final Report. *Inside a chartered phase:* charter revision — before executor dispatch, split the phase; in the impl loop, the truncate/restart branches below. *Second-level firing of either kind* — T4 or predictive, inside a phase that a charter revision or T4 stabilization produced — stops the run and surfaces to the user.

### Process records (append-only, by phase index only, never a commit SHA)

`<git-common-dir>/engine-implementer-runs/<run-id>/phase-fit` and `<run-root>/phase-charter`. The phase-fit record gets one numbered entry per adjudication — initial, each re-check, each T4 firing, each feasibility exit — carrying the Sizing values used, per-trigger measured results, the T2 groups used, the verdict, and for feasibility exits the named-seam evidence. The no-SHA rule keeps both records inside the `surgical-mode-switch` carve-out. **There is no phase ledger** — a SHA-bearing acceptance record would be the prohibited parallel ledger; chain integrity is recomputed at run-level acceptance instead. In multi-phase runs, each `surgical-mode-switch` entry is additionally tagged with its phase index (index only), keeping interleaved entries from different phases' plan loops auditable.

### The charter

Authored by a freshly spawned planner in `engine-planner` **charter mode** (the orchestrator never authors), reviewed through `review-engine-plan` **charter mode** in its own loop. The loop inherits Step 2's enumerated stop conditions plus a dedicated backstop: T4's axis is undefined for charter-shaped findings (T4 inactive, recorded as such), so a charter-review or sizing-audit loop exceeding **4 rounds** without converging stops and surfaces to the user. Once clean, the charter is frozen.

**Charter revision** may only add, split, merge, or re-scope **remaining** phases. Accepted phases are never reworked in place: a finding that invalidates accepted content becomes a **fix phase** — a later phase whose scope overlaps the earlier files. In a chartered phase's impl loop, T4 runs two branches executed as charter revision: *(a) truncate* — the revision truncates phase k (its deferral list grows by the split-off remainder, attributed to new successor phases) and passes charter review; then one stabilization fix round, then the normal checkpoint → measure → completion → review sequence under phase mode with the truncated allowlist; zero findings → accepted (successors base on the truncated phase's accepted candidate); findings → resume normal fix rounds. Already-landed candidates remain the phase's rounds. *(b) restart* — restart phase k from its own `PHASE_BASE_SHA` in a fresh implementation worktree under the revised charter; the abandoned candidates fork off the accepted chain, never appear in any chain-integrity interval, and are listed as abandoned in the Final Report.

### Per-phase identity and the substitution rule

Each phase k is a self-contained checkpoint pipeline with `PHASE_BASE_SHA` — phase 1: run-level `BASE_SHA`; phase k>1: phase k−1's accepted `CANDIDATE_SHA` — and its own frozen scope, frozen at the phase's scope-freeze moment (after its plan loop, before executor dispatch; never before the phase plan exists). Per-phase scopes **may overlap** on shared registration files (`effects/mod.rs` and kin); sequential execution makes that safe — there is no global-partition requirement. **Within a phase, every occurrence of `BASE_SHA` in the Inputs worktree preparation and Steps 3–7 (checkpoint delta, `scoped_diff_command`, projection worktrees, completion parser-gate range, Step 6 review span) means `PHASE_BASE_SHA`,** — the literal `"$BASE_SHA"` command templates stay byte-identical while the shell variable carries the phase base, so checkpoint-mode validation needs no changes. Run-level `BASE_SHA` is retained for the final integration span only.

**Per-phase spawn inputs:** Step 1 planners run in `engine-planner` **phase-plan mode** with the charter, the phase's entry, its deferral allowlist, and prior phases' accepted summaries (never their debates). Step 2 reviewers run in `review-engine-plan` **phase-plan mode** with the phase plan, the original task, the charter, the phase index, and the allowlist — and *all* Step 2 reviews in this pipeline, unphased and per-phase alike, declare the phase-fit context so the Sizing consistency check is blocking here. Step 3 executors run in the executor's **phase mode** with the charter, phase index, and allowlist, so the matrix and test map they author use the same `DEFERRED(phase n)` vocabulary their reviewers audit. Step 6 reviewers run in `/review-impl` **phase mode** with the charter, phase index, allowlist, and the phase's frozen scope.

### Run-level acceptance (after the last phase)

Per-phase acceptance is today's Step 7 applied to the phase — a clean review, completion checks passing at the candidate, and `rev-parse HEAD == CANDIDATE_SHA` — and **emits no Final Report snapshot and no PR-handoff block**; those are run-level only. Run-level final acceptance requires all of:

1. **Every phase accepted** on its own review and checks.
2. **The phases actually tile the run**, checked against git: each phase's accepted candidate is the next phase's base, and `git rev-list <prior accepted>..<phase accepted>` contains only that phase's own commits. Restarted and abandoned candidates legitimately sit outside those intervals — list them in the Final Report rather than forcing the intervals to match.
3. **The integration review returns zero findings**, run in `/review-impl` **integration mode** (findings-only; scoped to cross-phase seams and charter completeness). Reviewer inputs: the run-span `BASE_SHA..final CANDIDATE_SHA` diff and the charter. Findings dispatch a fix phase via charter revision. **Bound:** at most one fix phase per integration round; findings still present after two fix phases → stop and surface to the user.

## Pipeline

### Step 1 — Produce the plan

Spawn a `general-purpose` agent and instruct it to invoke `/engine-planner`. The agent returns a plan with every mandatory architectural section.

**Spawn inputs:** task description; in-scope file/subsystem hints; any prior reviewer findings (none on first round); the requirement to emit the mandatory Sizing section (Step 1a adjudicates against it). In chartered runs, per-phase planners instead run in `engine-planner` phase-plan mode with the inputs listed under "Per-phase spawn inputs".

Do not author or edit the plan in this thread — surgical-fix mode (below) is the one exception, and only under its three measured conditions. If the returned plan is missing sections or is superficial, send the same inputs plus an explicit "missing sections" note to a **fresh** planning agent — do not patch it yourself.

### Step 1a — Phase-fit gate

Adjudicate the gate as defined in "Phase-fit gate and chartered runs", appending the phase-fit record entry. Single-phase verdict → the pipeline below proceeds unchanged (plus the stated re-adjudication points). Multi-phase verdict → spawn the charter-mode planner, run the charter review loop, then iterate phases — each phase runs Steps 1–7 with the per-phase spawn inputs and the `PHASE_BASE_SHA` substitution rule, followed by run-level acceptance.

### Step 2 — Review the plan until clean (unbounded loop)

Spawn a `general-purpose` agent and instruct it to invoke `/review-engine-plan` against the full plan.

**Reviewer spawn inputs:** the full plan; the original task description; the phase-fit context declaration (all Step 2 reviews in this pipeline declare it, so the Sizing consistency check is blocking here); in chartered runs additionally the charter, phase index, and deferral allowlist (phase-plan mode).

If the reviewer returns gaps, spawn a **fresh** planning agent (Step 1 inputs plus the reviewer's findings as additional constraints) to produce a revised plan, then spawn a **fresh** reviewer agent against the revised plan.

**Repeat until a full review round returns zero gaps.** There is no iteration cap — "two rounds and ship" is not acceptable. Stop only for:

- a true human design decision the planner cannot resolve,
- missing external access (CR text unavailable, file inaccessible),
- an environment blocker that makes review impossible,
- T4 fired and the charter-mode planner returned a feasibility exit (no green seam), or
- T4 fired with a prior T4 firing or feasibility exit already in the phase-fit record.

The last two stop the run and surface to the user with the phase-fit record. A T4 firing without those conditions exits this loop into decomposition per "Phase-fit gate and chartered runs" — that is a route, not a stop.

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
- **Then re-review the WHOLE artifact**, fresh context — not just the repaired sections, per `$bug-triage`'s targeted-re-review rule. Repeat apply → whole-artifact re-review until a round returns zero gaps; any finding that requires *deciding* something ends surgical mode and returns to the unbounded loop above. Surgical mode replaces the planner-rewrite rounds, never the final independent check.
- **Record the mode switch, its three measurements, the spot-vs-design classification of each round's findings, and why the mode ends** in `<git-common-dir>/engine-implementer-runs/<run-id>/surgical-mode-switch` (in multi-phase runs, tag each entry with its phase index — index only, no SHA), never in the plan text the fresh re-reviewer and the executor read — recording it there hands the one remaining independent check a prior verdict. It is a working note for this loop, not a provenance record. Append one numbered entry per round, never overwrite — ending surgical mode and re-entering it later continues the same numbered sequence; only a round that enters the mode records the switch and its three measurements, and only a round that ends the mode records why.

**This does not contradict `$bug-triage`'s fixpoint gate.** That gate requires whole-plan re-review because *"revisions routinely INTRODUCE new gaps in untouched-looking areas"* — planner **rewrites** do. A check-and-replace at a named coordinate does not rewrite, which is why it is the safe tool once the design has stopped moving. `$review-engine-plan` ends its loop with *"or the caller stops the process"* and states no criteria; this section is those criteria, and it lives here because the orchestrator is that caller.

This is not a licence for "two rounds and ship". The unbounded loop remains the default and the burden of proof is on leaving it: no measurement, no switch. Surgical mode is scoped to this Step 2 plan-review loop only — Step 6's implementation-review loop never uses it, because there the artifact is a committed candidate that only an executor may edit under the frozen-scope contract.

### Step 3 — Dispatch implementation

Spawn the `engine-implementation-executor` agent.

**Spawn inputs:** mode `implementation/fix`; the reviewed clean plan in full; `BASE_SHA`; named `START_SHA`; the in-bounds / out-of-bounds path list; named `IMPLEMENTATION_WORKTREE`; any prior reviewer findings (none on first round); in chartered runs additionally the charter, phase index, and deferral allowlist (the executor's phase mode). First round: `START_SHA == BASE_SHA`. Fix round: `START_SHA` is the previously reviewed `CANDIDATE_SHA`, never a moving branch head.

The implementation executor edits only its frozen scope and runs **preparatory** checks. Preparatory success is not completion evidence. Its existing discriminating-test, selected-authority, coverage-honesty, maintainer-simulation, and CR-annotation gates remain the authoritative gates; do not restate or replace them here.

If the executor returns "stop and return" items (plan contradicts current code, ad hoc parser dispatch unavoidable, CR uncertain), do NOT improvise around them. Loop back to Step 1, feed the executor's findings into `/engine-planner` as new constraints, and re-run Steps 1–3 — in a chartered phase this resolves to the *phase's* plan step under phase-plan mode, never a fresh full-task Step 1.

**Large JSON fixture constraint.** Any repository-bound JSON fixture ≳100KB (test fixtures, game-state dumps, generated maps — not runtime/config JSON whose consumers read plain `.json`) gets `gzip -9 -n` (`-n` keeps the archive byte-reproducible) and loads via the established inflate pattern: `include_bytes!("….json.gz")` + a test-local `gunzip` helper using `flate2::read::GzDecoder` (examples: `tests/integration/combo_infinite_pile.rs`, `cr733_resolved_commands_p0.rs`). Never commit the uncompressed twin alongside the `.json.gz`. If a fixture is regenerated by a script, note in the reading test that regeneration requires re-gzipping.

### Step 4 — Checkpoint the candidate

The checkpoint is the candidate commit, and it is the orchestrator's to make — never the executor's. Stage each approved path by explicit pathspec: never `git add -A`, and never commit without a pathspec, because the shared index can sweep in another agent's staged files (`feedback_git_add_file_bundles_concurrent_work`, `feedback_shared_index_commit_pathspec`). Before staging, confirm no pre-existing change overlaps an approved path; if attribution is ambiguous, stop and return rather than unstage, sweep in, or overwrite another agent's work. Commit, then confirm `git -C "$IMPLEMENTATION_WORKTREE" rev-parse HEAD` equals the `CANDIDATE_SHA` you recorded, and that `START_SHA..CANDIDATE_SHA` contains only the intended paths. Never measure an uncommitted tree or use a moving `HEAD` as the candidate. Verify `HEAD` is attached before any explicitly requested push (`feedback_verify_head_attached_before_push`), never pipe `git push` into `tail`/`head` (`feedback_git_push_no_pipe`), and never push unless asked.

If the change touches the parser, find out whether it moves parser output: build the tooling from the base and the candidate, generate card data from each against the same pinned data root, and diff the two. Report what changed. `./scripts/gen-card-data.sh` and `cargo coverage` do not answer this question.

### Step 5 — Verify the committed candidate

Run the checks in a clean worktree at `CANDIDATE_SHA`, not in the implementation worktree — a check that passes against uncommitted edits has told you nothing about what you are shipping. Run every gate the changed surface calls for: formatting for any implementation change, the Rust/engine/parser block for Rust paths, the frontend block for frontend paths, the parser gate for parser paths. Markdown-only policy changes need scope and diff checks; do not run Cargo or Tilt for them.

The full suite is owed at the tree being shipped. An intermediate fix round may narrow to the touched surface — say so plainly when reporting it, since a narrowed run is not a suite pass — and re-run unfiltered before acceptance.

### Step 6 — Review the immutable candidate

Spawn a fresh `general-purpose` agent to invoke `/review-impl` against `BASE_SHA..CANDIDATE_SHA`, with the original task, the reviewed plan, the in-scope paths, and any prior findings. It reviews the diff and the checks that were run; it re-runs whatever it needs to trust. Findings dispatch a fix round.

### Step 7 — Final acceptance

Accept when the plan-review loop is clean, the review returns no findings, the completion checks pass at the candidate, and `rev-parse HEAD == CANDIDATE_SHA`. In a chartered run this is per-phase acceptance, with `PHASE_BASE_SHA` substituted; it emits no Final Report snapshot and no PR handoff, which are run-level only.


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
4. `BASE_SHA`, accepted `CANDIDATE_SHA`, frozen scope paths, and run-artifact root.
5. The `START_SHA` each round began from, and what the parser measurement found when the change touched the parser.
6. Verification commands run and results, separated into preparatory and completion evidence.
7. Implementation-review rounds (count), reviewed SHA, and final clean result.
8. Checkpoint commit hash and staged file list.
9. Coverage impact for parser changes.
10. Deviations from the plan with reasons.
11. Self-flagged risks and judgment calls (yours + executor's).
12. Remaining items, if any, with reasons.
13. Phase-fit verdict and record path. Single-phase runs report these plus any abandoned candidates whenever a T4 branch-(b) redo preceded the verdict — the Final Report is their only durable listing under the no-SHA process-record rule.
14. Chartered runs additionally: phase count; per-phase accepted `CANDIDATE_SHA`s; abandoned candidates from either source (restarts and branch-(b) returns); the phase-charter record path; the chain-integrity result; and the integration-review result.
