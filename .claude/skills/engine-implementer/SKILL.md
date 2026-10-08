---
name: engine-implementer
description: "End-to-end phase.rs implementation pipeline: plan, review-engine-plan, implement, review-engine-impl, commit — each step run in a fresh agent, with review loops bounded by finding class and a contributor budget, and phase decomposition for oversized workloads."
---

# Engine Implementer (Orchestrator)

This is the orchestrator for the phase.rs implementation pipeline. It runs as a **skill in the main thread** so it can spawn a fresh agent for every step that benefits from fresh context (planning, plan review, implementation, implementation review). Do not turn this into an agent — agents cannot spawn sub-agents, which is what made earlier versions silently degrade.

## Roles

| Step | Where it runs | Why |
|---|---|---|
| 1. Produce plan | **Fresh agent** invoking `engine-planner` | The plan is shaped by the task, not by the conversation history that led here |
| 2. Review plan | **Fresh agent** invoking `review-engine-plan` | Honest architectural review, independent of the planner |
| 3. Implement | **Fresh agent** following [executor.md](executor.md) | Surgical edits and preparatory checks; never commits |
| 4. Checkpoint + measure | This thread, then a fresh executor in measurement-only mode | The orchestrator creates the candidate commit; an isolated executor measures that immutable candidate |
| 5. Complete verification | This thread | Verify the committed candidate, never an in-flight working tree |
| 6. Review implementation | **Fresh agent** invoking `review-engine-impl` | Independent review of the immutable base-to-candidate diff |
| 7. Final acceptance | This thread | Accept only the exact reviewed checkpoint candidate or its Step 6 comment-only correction commit |

**Spawning by runtime.** Skills are invoked as `/name` under Claude Code and `$name` under Codex; both read `.claude/skills/<name>/SKILL.md` (`.agents/skills` and `.codex/skills` link there). Under Claude Code, spawn `general-purpose` agents for Steps 1, 2 and 6 and the `engine-implementation-executor` agent type for Steps 3 and 4, which loads `executor.md`. Under Codex, spawn a worker agent per step and tell it which skill or file to read first — for Steps 3 and 4, `.claude/skills/engine-implementer/executor.md`. A runtime that cannot spawn agents runs each step in a fresh session and hands every reviewer ONLY the artifact under review (the full plan, or the unified diff), the original task, `CLAUDE.md`, the relevant review skill, the attempt history below, and in chartered runs the charter, phase index and deferral allowlist — never the conversation that produced the artifact. Never degrade to reviewing your own work in the same context; that is the failure this skill exists to prevent. If even a fresh session is impossible, say so in the final report and in the PR body under "Validation Failures"; do not claim the review loop ran clean.

Before Step 1, and before Step 6 when its trigger applies, a read-only [scout](scout.md) locates facts for the planner and reviewers. It is not a step and holds no gate.

The orchestrator never authors content itself. Its only jobs are: spawn agents, route their output to the next step, apply the run limits, own the commit, and close each spawned agent once its output is consumed (under Claude Code, send a `shutdown_request` and wait for the `shutdown_response` ack). The structured report each agent returns is the authoritative step handoff; progress messages are additive.

## Run ownership and checkpoint identity

Before dispatching an executor, fix `BASE_SHA` and the in-scope paths for the run; in a chartered run each phase fixes its own `PHASE_BASE_SHA` at phase start. Every implementation or fix dispatch has a named `START_SHA` and `IMPLEMENTATION_WORKTREE` — the first round starts at `BASE_SHA`, a fix round at the prior reviewed `CANDIDATE_SHA`. Check `HEAD == START_SHA` and a clean tree before edits and again before the checkpoint, and stop if the diff has escaped the intended scope.

**Baseline reachability.** Before the first executor dispatch, check that the branch the work will land on can reach the base: `git merge-base --is-ancestor "$BASE_SHA" <landing target>` (the fetched PR target branch, or the local branch the work lands on). If it fails, list `git log --oneline <landing target>.."$BASE_SHA"`, and proceed only when those commits belong to this task or will land first; otherwise stop and report. Review verifies a plan against its base and never questions the base itself.

That is the whole provenance contract. **Do not build receipts, evidence records, manifests, digests, seals, ledgers, or any other artifact whose purpose is to prove to a later reader that these steps happened.** Git already records what changed and at which commit, and the reviewer reads the diff. Every step below is something you do and then act on, never something you notarize.

## Inputs

Either:

1. A task description (cards, CR rules, Oracle text patterns, affected subsystems, expected behavior), or
2. A pre-existing plan — treat as a draft unless it has already passed `review-engine-plan` to clean.

The invocation may also name a budget (see [Run limits](#run-limits)).

Before Step 3, prepare and verify a clean `IMPLEMENTATION_WORKTREE` at `START_SHA`. After its checkpoint, prepare clean detached base and candidate projection worktrees at `BASE_SHA` and `CANDIDATE_SHA`, and a distinct clean detached `COMPLETION_WORKTREE` at `CANDIDATE_SHA`; no projection or completion worktree is used for implementation. During an active pipeline session, do not re-ask about worktrees; use the session default.

Two build-economy rules govern measurement worktrees. **Build-once directories never pay for incremental state:** every measurement build — projection, completion, any target directory built once and never rebuilt — runs with `CARGO_INCREMENTAL=0`. **Completion allocation is per run-segment, not per candidate:** one completion worktree and one isolated completion target directory per phase (per run when unphased), reset to each round's `CANDIDATE_SHA` — per-candidate allocation multiplies full cold builds for no isolation gain. Reuse is strictly sequential within the owning run; never share a reused directory with any concurrently running agent or lane. A reused completion directory still runs `CARGO_INCREMENTAL=0`, since the disk cost is guaranteed and the warm-rebuild speedup is not. Re-measure if warm completion rebuilds start dominating wall clock.

**Sizing for pre-existing plans:** Step 1a needs a Sizing section however the plan arrived. A pre-existing plan lacking one — review-clean (which bypasses Step 1) or a draft — gets a **sizing addendum** from a fresh planner in `engine-planner` sizing-only mode, reviewed in `review-engine-plan` sizing-audit mode under the run limits, with the addendum and each round recorded in the phase-fit record. When Step 1a then fires on an already-clean plan, the charter-mode planner partitions rather than re-plans.

## Task scope and verification work

Keep the original requested behavior and acceptance criteria in every handoff. Before a verification action, name the claim it answers and look for an existing test, fixture, command or supported tool API. Regression tests, fixtures built with existing helpers, and small probes using existing infrastructure are ordinary task work.

**Verification machinery** is code or protocol that produces evidence about the candidate and is neither a committed product test run by the standard runners nor a documented command or supported tool API. Adding or repairing it needs an accepted [expansion case](#run-limits) naming the claim no existing test or command answers. Documented setup, waiting for a legitimate build, ordinary product tests and fixtures, and substituting an existing gate (Step 5's checks, an existing test) need none. A card fix never justifies a general-purpose browser driver, session manager, seeding service, cleanup protocol or framework unless the user asked for that product. Temporary, generated, untracked and outside-repository helpers are machinery too; exclusion from T2 is not permission to build them.

A test that reaches the product and reveals wrong behavior calls for a product fix. Required evidence stays required when tooling blocks it: never drop the check or claim a clean/ready result to escape a limit.

## Run limits

Apply these before every dispatch or verification-tool edit and after every result, across all modes, phases and renamed successor work for the original task.

**Attempt history.** Every planner, reviewer and executor receives the original task and acceptance criteria, current scope, the budget, and a brief attempt history: each review loop's rounds with their finding tags, and every expansion case with its outcome. Keep it in the [phase-fit record](#phase-fit-record); create no other ledger, schema or tracking script. If prior history is lost, report that and stop rather than reset it.

**Finding classes.** Reviewers tag every blocking finding — every plan-review blocker or material gap, and every HIGH or MED implementation-review finding (a LOW finding that reports no wrong behavior rides along with a design round or ships as a residual; any finding reporting wrong behavior is blocking):

- `behavior` — closing it changes product code or tests: a design choice, a missing site or path, a wrong or vacuous test, a false CR claim.
- `text` — it quotes a coordinate and supplies the replacement text (or the repair is deleting the sentence), and closing it changes nothing the code does.
- `machinery` — it concerns the artifact's own probes, censuses or verification protocol.

When in doubt, it is `behavior`.

**Pre-existing reports.** A reviewer's Pre-existing list (defects that reproduce at base and that the change does not depend on) is not a finding. It opens no round, counts toward no loop and never blocks. The orchestrator triages each item once, on receipt, reading the card before the code:

- **Fix it in the change** when the change's own claims are false with the defect present: a test or acceptance row it asserts cannot pass, or a card, board or row it names as delivered behaves wrongly because of it. The fix rides the phase's next executor round as a constraint, with a test red at base, recorded as an addendum line in a chartered run, as in the [defective-reference route](#defective-reference-route).
- **File an issue** on phase-rs/phase, with the Oracle text and the measured evidence, after a duplicate search, when every case the change claims still works. Filing closes the item.
- **Drop it**, recording the reason, when no printed card's text reaches it.

When the orchestrator cannot reach a verdict, it asks whoever dispatched the run, who decides; only a fix out of proportion to the task goes to the user. The verdict is final for the run unless a test the change asserts fails because of the defect. A filed or dropped item never becomes a phase, a scope extension or a dispatch of its own. A small fix in a file the work already edits may ride an already-scheduled executor round, recorded as an addendum line in a chartered run.

**Design rounds and the loop limit.** A round with a `behavior` finding is a design round: fix it through a fresh planner (Step 2) or fix executor (Step 6), then review afresh. A round without one is closed as Steps 2 and 6 describe. Each return to Step 1 from an executor stop-and-return, and each abandoned or failed review dispatch, also counts as a design round of its loop; these rounds count toward the fifth-round limit only, and the lowering comparison skips them. Each review loop — the sizing audit, the charter, each plan loop, each implementation-review loop, the integration review, and the final PR review ([pr-handoff.md](pr-handoff.md)) — stops at its **fifth design round**, or sooner when **two consecutive design rounds** (counting design rounds only) **fail to lower the behavior-finding count**. A loop restarted, renamed or re-phased for the same work keeps its count, and returning from implementation to planning resumes that plan loop's count. Only accepting a candidate that implements part of the requested behavior resets the counts; a checkpoint, clean plan, helper-only phase or new reviewer does not. If tooling is itself the requested product, its accepted implementation qualifies.

**Budget.** The invocation may name `budget=tight|standard|until-clean`; the default is `standard`. The budget decides who approves spending past a limit — a design round past a loop limit, verification machinery, or a new phase or restart proposed at a stop:

- `tight` approves nothing and takes the decline path.
- `standard` asks the user with an expansion case, and takes the decline path when nobody can answer (an autonomous run).
- `until-clean` approves its own complete cases within the authorized limits. Non-verification expansion cases still stop after two consecutive failed predictions, ignoring intervening verification outcomes. Predictions about verification/diagnostic results use the separate recovery procedure below, including when a failed check requires a product/test fix; that fix still counts in the normal design loop. Review-round behavior-count predictions always remain design predictions. Verification successes cannot reset a design loop or its failed-prediction streak. The design-round and behavior-count limits above still apply; approved extensions never exceed a user ceiling or, with no design-round ceiling named, a loop's tenth design round.

A budget or approval covers one original task. A generic "continue" approves nothing.

**Expansion case.** Before any such spend, record this in the phase-fit record and, when asking, pose it as the question:

1. **Open item** — the finding's ID, severity and tag, or the missing evidence, quoted.
2. **What the spend buys** — the next action and the check that closes the item.
3. **Cost** — dispatches and cold builds, as counts; include the maximum job/matrix fan-out and separately triggered verification runs for remote work.
4. **If declined** — this item's decline-path outcome.
5. **Prediction** — the checkable result that shows the spend worked (for a review round, the next round's behavior-finding count as a number lower than the current one), graded at the next result, listed with this loop's earlier predictions and their outcomes. Grade the whole prediction: a failed prediction remains `FAIL` even when it yields useful partial evidence. Record failure cause and evidence progress separately; append corrections to a diagnosis rather than rewriting earlier outcomes.

Argue from the open item and the prediction record only. Work already spent is not a reason. Claim no severity or convergence the finding list does not show, and never offer to drop a required check.

**Decline path.** Preserve commits and working changes. Before implementation, a declined case stops the run unless every open finding is `text`, which the orchestrator applies before continuing. After implementation, these may ship as residuals, each listed under the PR's Validation Failures with its evidence and a follow-up: `text` findings, LOW findings that report no wrong behavior, and observations outside the acceptance criteria. A final review whose only findings are listed residuals then counts as clean. Any HIGH or MED finding tagged `behavior` or `machinery`, wrong behavior for the card class, a false CR claim, or missing required evidence stops with no PR.

**At a stop**, report the original goal, completed product work, open items, each loop's round tags and prediction record, and the smallest next action as an expansion case. When behavior findings stayed at or above their prior count across three or more of the layers types / parser / resolver / targeting / frontend / AI / tests, say so and propose a smaller scope or a decomposition ([chartered.md](chartered.md)). No new phase or charter continues the same work without an accepted case. Required checks and clean final review still govern acceptance, and stops take precedence over the decomposition and text-round routes below.

### Verification recovery (until-clean only)

This is standing authority to diagnose and correct ordinary verification failures through the existing pipeline, not permission to bypass a design stop. `standard` and `tight` keep their approval and decline rules above; this subsection grants them no additional authority. An explicit user ceiling or instruction to stop overrides it. Proposed policy edits do not authorize their own contribution or resume a stopped task.

**Resource ceiling.** An invocation may set `remote-dispatch-limit=N` (a nonnegative integer); for `until-clean` the default is **12 remote verification/generation runs for the original task**, initial runs included. Explicit user ceilings replace the default within their stated scope; retain all applicable ceilings and use the most restrictive where they overlap. Count each submitted workflow run, including push-triggered verification, manual dispatches, reruns, separately triggered downstream verification runs, and submissions rejected before jobs start. Cancellations and infrastructure failures consume their slot too. Jobs/matrix entries within one run are not extra dispatches, but their maximum count and cold-build cost belong in the case before submission. Reading or waiting on an existing run consumes no new slot. Reserve the proposed run count before the triggering action; if the remaining allowance cannot cover it, stop. A new head, phase, worktree, restart, successor task name or accepted candidate never replenishes this task-wide allowance. Do not publish diagnostic commits or open a PR early to obtain verification outside the allowance.

Apply this procedure after every verification failure and before every recovery expenditure:

1. **Diagnose from actual evidence.** Inspect the failing command, logs and tested source/input identity; distinguish a command printed in a script from one executed. Record each independent blocker in the existing phase-fit record. An exit code, cancellation, OOM or `jobs: []` alone proves neither a product defect nor an infrastructure cause. If the cause is unknown, authorize only a bounded diagnostic action, not a presumed infrastructure fix.
2. **Route the cause.** Workflow/context errors, runner/service outages, transport failures and missing prerequisites may use scoped verification recovery when the logs establish that cause. A failing product assertion, compiler error in product code, wrong/vacuous test, lost semantics or new actionable product/test/design defect is `behavior` and returns to the normal scoped fix/checkpoint/verify/review pipeline, with its existing round counts and convergence limits. Review findings retain the existing `behavior`/`text`/`machinery` taxonomy. A supposedly incorrect assertion requires a derived expected result and a discriminating replacement through that pipeline; never delete, skip or weaken a test or validation requirement to get green. Step 5's existing semantics-neutral lint route remains unchanged. Mixed failures take both routes; an infrastructure error does not erase a product blocker.
3. **Choose a defensible next action.** Name the specific changed correction or diagnostic question, its evidence, and the check that will resolve it. Before each additional recovery dispatch, tool edit, diagnostic or corrective attempt, record and self-approve the complete five-part expansion case, including bounded cost and a checkable prediction. For a transient failure, another run is justified only by concrete evidence that the prerequisite/service/runner condition has changed; elapsed time or a different run number is insufficient. Do not repeat an unchanged failed workflow without such evidence. Keep machinery limited to the missing claim and existing task scope.
4. **Grade and measure progress.** Keep every historical prediction, including `FAIL`, and separately record what required evidence was actually obtained for the intended immutable candidate and inputs. Progress means closing the relevant blocker, completing a previously unavailable required check/artifact, or reaching a later required check after the formerly failing prerequisite succeeds. A new diagnosis, renamed failure, scheduled job, green unrelated check or old-head pass alone is not progress. A partial result cannot satisfy a full-suite or current-head gate.
5. **Continue or stop.** A failed verification prediction does not itself trigger the two-failed-predictions stop for non-verification cases. Instead, stop after **two corrective/diagnostic recovery attempts for the same unresolved blocker produce no relevant evidence progress**; the initial failure is not a recovery attempt. Count these attempts across intervening work on other blockers, and group by failing requirement and underlying cause, not message spelling, job name or head SHA. Only relevant evidence progress resets that blocker's no-progress count; it never resets history, design counters or resource consumption. Also stop immediately when there is no defensible next action, the authorized resource allowance is exhausted, authoritative history is lost, protected architecture lacks authorization, required evidence is impossible, a genuine human decision is needed, or behavior/design convergence limits require a stop.

For example, rejection of a temporary workflow before any jobs start is a failed prediction and a consumed dispatch; a diagnosed invalid job context can justify a scoped repair. If the repaired run passes those prerequisites and then fails a real product assertion, retain both outcomes: verification advanced, and the assertion now goes to a fix executor and independent review. Neither failure alone warrants another approval under `until-clean` while these limits permit the next case; two no-progress recovery attempts or an explicit ceiling still stop it.

Recovery preserves candidate generation, complete pinned-input parser measurement, all applicable Step 5 checks, fresh independent reviews/cross-checks, Gate A/B, scope checks, PR requirements and the once-fetched handoff base. A product/test edit creates a new immutable candidate and invalidates SHA-dependent evidence as the existing steps require. All accounting stays in the original append-only phase-fit; no new ledger, framework or persistent verification subsystem is authorized.

### Process-effort check

The run limits count rounds. They do not see a run that converges slowly, where planning and review take most of the time and little product work lands. This check is how the orchestrator notices that. It is for self-reflection: it adds no gate, delays no dispatch, and never stops a run. Stops come only from the limits above.

**Timings.** Note the start and end of every dispatch and check in the phase-fit record with `date +%s`, in one of four kinds: `planning` (planner and charter dispatches), `review` (every reviewer), `implementation` (implementation and fix executors), `verification` (Step 4 measurement and Step 5 checks). Scout runs are not timed here; each pack carries its own duration. If a time cannot be recorded, note that once and continue. Do not reconstruct times for work already done.

**When to look.** At every review result and every accepted candidate, before choosing the next dispatch. Progress means an accepted candidate, or `behavior` findings retired. Process effort is dominant when planning and review time keeps growing without that progress. Signs include:

- several review results in a row with no newly accepted candidate;
- design rounds that surface new `behavior` findings instead of converging;
- review rounds spent on `text` or `machinery` findings;
- planning plus review time several times the implementation time, with open findings not falling.

A high planning-and-review share is a reason to look and proves nothing by itself. Heavy review that keeps retiring real `behavior` findings and keeps candidates landing is justified. Compare against implementation time; `verification` is its own kind because a cold build is a wait and does not count as process effort. Do not let each round's local justification hide the cumulative cost.

**When it is dominant.** Tell the user in the next progress update, before they ask, with the four totals, the rounds and candidates behind them, and the cause. Then make the cuts this skill already allows:

- close a round without `behavior` findings as Steps 2 and 6 describe, with no further review;
- narrow intermediate Step 5 runs to the touched surface, and stop repeating green checks absent new evidence;
- keep fix rounds to the findings supplied, and do not tighten acceptance mid-loop;
- pass a reviewer's pattern finding, with its predicate and site list verbatim, to later planners and phases as a constraint, so the next reviewer does not find it again;
- name a smaller scope or a decomposition ([chartered.md](chartered.md)) as an option in the progress update. The check itself files no expansion case.

If the required gates are themselves the floor, say so and ask the user once, in the progress update: a lower budget, a smaller scope, or carry on. Do not wait for the answer: continue with the next dispatch and apply an answer when it arrives. An autonomous run records the note. Record one short evidence-and-action note in the phase-fit record.

### Defective-reference route

A parity or preservation row takes its expected value from another reading: the prompted route, base, or a sibling route. When that reference is wrong for the card, the defect exists before this work. It can surface in the planner's Reference Readings, in plan review, in an executor stop-and-return, or in any implementation-review finding (codex and CodeRabbit included). The route is fixed, so it is **not a stop and needs no expansion case under any budget**. Shipping the dependent work on the defective reading would be wrong behavior for the card class, which the decline path already refuses. Asking the user would only offer a choice between stopping and this route.

1. **Record** the card, the reading derived from its Oracle text and the CR, the measured reference, and the affected rows in the phase-fit record.
2. **Fix the reference inside the dependent work**, as the work encounters it.
   - Cover the defect's class (every route that drops the same value), not only the row that exposed it, with tests red at base.
   - In a chartered run, record it as one addendum line in the phase's addenda file ([chartered.md](chartered.md#the-charter)).
3. **Assert the derived reading** in the dependent rows, in the same work.

A fix that breaks a decision the design rests on, or sizes above one unit (Sizing T1), goes back for design revision: a re-charter in a chartered run, a fresh planner otherwise. Short of that, it never inserts a phase.

## Phase-fit gate (Step 1a)

**Unit anchor:** one *unit* = one coherent mechanic/behavior implementable by a single skill-checklist pass (e.g. one `/add-engine-effect` traversal), regardless of how many lockstep layers that pass touches. A routine interactive effect wiring types/parser/resolver/frontend/AI is **one unit** — the gate must not trip on it.

**The gate fires only on T1 AND T2**, adjudicated against the plan's Sizing section (adjudication is measurement, so it is not authoring):

- **T1 — Unit count:** the plan contains ≥2 units.
- **T2 — Scope size:** expected scope-path count ≥13, counted mechanically with exclusion before grouping: test fixtures (regardless of authorship or commit status) and uncommitted/regenerated pipeline data are excluded outright; then, among remaining files, a committed generated artifact groups with its source via a checked-in generator (committed `.d.ts` with source), and same-basename translation mirrors group with the authored file (the `en` locale counts; its mirrors add nothing). Directory entries never count as one path — expand to expected changed files, then group.

A single-phase verdict proceeds through the pipeline below. Re-adjudicate every time a fresh planner returns a revised plan, and at scope-freeze against the materialized `SCOPE_PATHS` list. When the gate fires, read [chartered.md](chartered.md) and follow it for the rest of the run: each phase runs Steps 1–7 with its per-phase inputs, followed by run-level acceptance.

### Phase-fit record

`<git-common-dir>/engine-implementer-runs/<run-id>/phase-fit`, append-only and numbered, carrying phase indexes only, never a commit SHA. It gets one entry per adjudication (the Sizing values used, per-trigger results, the T2 groups, the verdict), per review round (its finding tags and counts, and for a round closed without a design change, the edits applied with their before/after text), per expansion case and its outcome, per revision or correction (class, before/after text, and the evidence that authorized it), per dispatch or check (its kind and its start and end time, for the [process-effort check](#process-effort-check)), and per failed [scout](scout.md) run (one line). Keep it out of the plan text reviewers and executors read: recording a verdict there hands the next independent check a prior verdict. It is a working note, not a provenance record.

## Pipeline

### Step 1 — Produce the plan

Spawn a fresh agent and instruct it to invoke `engine-planner`. The agent returns a plan with every mandatory architectural section.

**Spawn inputs:** original task and attempt history; in-scope file/subsystem hints; the precedent pack from the [scout](scout.md), when one was produced; any prior reviewer findings as constraints (none on first round); the requirement to emit the mandatory Sizing section. In chartered runs, per-phase planners run in phase-plan mode with the inputs [chartered.md](chartered.md) lists.

Do not author or edit the plan in this thread; applying `text` findings in Step 2 is the one exception. If the returned plan is missing sections or is superficial, send the same inputs plus an explicit "missing sections" note to a **fresh** planning agent — do not patch it yourself.

### Step 2 — Review the plan

Spawn a fresh agent and instruct it to invoke `review-engine-plan` against the full plan. Each review runs in a fresh context — never reuse the previous reviewer's.

**Reviewer spawn inputs:** the full plan; attempt history; the original task description; the same precedent pack the planner received, with the [scout](scout.md)'s hand-on sentence; the phase-fit context declaration (all Step 2 reviews in this pipeline declare it, so the Sizing consistency check is blocking); in chartered runs, the phase-plan inputs [chartered.md](chartered.md) lists.

- **A design round** (any `behavior` finding): when the run limits allow, a fresh planner revises the plan with the findings as constraints, then a fresh reviewer reviews the whole revised plan.
- **A round without `behavior` findings closes the loop:** the orchestrator applies each `text` finding itself (below), closes each `machinery` finding by substituting an existing gate or through an expansion case, and the plan is clean. No review follows. Closing a `text` finding changes nothing the plan decides, and re-reviewing applied wording only mints more wording findings. A review follows a behavior change, never an edit to text.

**Applying a `text` finding** is applying adjudicated text, not authoring:

- **Two-sided verification:** before the edit, the quoted old string is present at the finding's named coordinate — a quote that is not there is a stale coordinate, not an applicable fix; after the edit, the text the replacement adds is present exactly once where the old string was, and the old string is absent — except when the replacement contains it, where the added text is the sole gate. Count occurrences per fragment, not matching lines.
- **State the sweep's boundary:** population, predicate, scan direction, and whether the matched line counts. Every enumeration defect is an unstated predicate.
- **Fix the neighbours the edit breaks.** A repair frequently contradicts a section that classified the old form; sweep by mechanism, not by coordinate.
- Record the edits in the phase-fit record, never in the plan text.

A `text` finding that turns out to need a decision is `behavior`: dispatch a planner.

**Small-change lane.** When the Sizing shows one unit, at most four counted scope paths, and no new enum variant, serialized surface, or `WaitingFor`/`GameAction` change, the first review can close the loop by itself: if no `behavior` finding changes a seam, layer or type, and each supplies a concrete fix, apply the `text` findings, pass the `behavior` findings to the executor as constraints with no planner revision or re-review, and give them to the Step 6 reviewer to confirm each is closed. Otherwise the normal loop applies. Record `small-change lane` and the round's tags in the phase-fit record so the lane's outcomes can be compared with the full loop's.

A clean plan, or one closed by the small-change lane, is required to proceed. Stop at the run limits, for a human design decision the planner cannot resolve, for missing external access, or for an environment blocker that makes review impossible.

### Step 3 — Dispatch implementation

Spawn a fresh executor (see **Spawning by runtime**).

**Spawn inputs:** mode `implementation/fix`; attempt history; the reviewed clean plan in full; `BASE_SHA`; named `START_SHA`; the in-bounds / out-of-bounds path list; named `IMPLEMENTATION_WORKTREE`; any prior reviewer findings (none on first round); in chartered runs additionally the charter, phase index, and deferral allowlist. First round: `START_SHA == BASE_SHA`. Fix round: `START_SHA` is the previously reviewed `CANDIDATE_SHA`, never a moving branch head.

The executor edits only its `SCOPE_PATHS` list and runs **preparatory** checks. Preparatory success is not completion evidence. Its discriminating-test, selected-authority, coverage-honesty, maintainer-simulation, and CR-annotation gates remain the authoritative gates; do not restate or replace them here. A site outside the list in one of the scope rule's standing classes ([chartered.md](chartered.md#per-phase-identity-and-the-substitution-rule): compiler-forced, shared registration, comment-only) is a stop-and-return: in a chartered run the scope rule answers it; in an unphased run the orchestrator adds the path, records its class evidence in the phase-fit record, re-dispatches the round, and gives that evidence to the Step 6 reviewer to confirm. Any other out-of-list site returns to Step 1. Every re-dispatch after the first counts as a design round of the implementation-review loop.

If the executor returns "stop and return" items (plan contradicts current code, ad hoc parser dispatch unavoidable, CR uncertain, machinery needed), do NOT improvise around them. Check the run limits and scope boundary first; machinery needs an expansion case. If further work is permitted, return to Step 1 with the executor's findings as constraints and re-run Steps 1–3 without resetting attempt history — in a chartered phase this resolves to the *phase's* plan step, never a fresh full-task Step 1.

### Step 4 — Checkpoint the candidate

The checkpoint is the candidate commit, and it is the orchestrator's to make — never the executor's. Stage each approved path by explicit pathspec: never `git add -A`, and never commit without a pathspec, because the shared index can sweep in another agent's staged files. Before staging, confirm no pre-existing change overlaps an approved path; if attribution is ambiguous, stop and return rather than unstage, sweep in, or overwrite another agent's work. Commit, then confirm `git -C "$IMPLEMENTATION_WORKTREE" rev-parse HEAD` equals the `CANDIDATE_SHA` you recorded, and that `START_SHA..CANDIDATE_SHA` contains only the intended paths. Never measure an uncommitted tree or use a moving `HEAD` as the candidate. Verify `HEAD` is attached before any push, never pipe `git push` into `tail`/`head`, and push only through Step 7's ship or when asked.

If the change touches the parser, find out whether it moves parser output: dispatch a fresh executor in measurement-only mode against the base and candidate projection worktrees, which builds the tooling on each side, generates card data from each against the same pinned data root, and diffs the two. Report what changed. `./scripts/gen-card-data.sh` and `cargo coverage` do not answer this question.

### Step 5 — Verify the committed candidate

Run the checks in a clean worktree at `CANDIDATE_SHA`, not in the implementation worktree — a check that passes against uncommitted edits has told you nothing about what you are shipping.

**Per-commit candidates** run formatting and the executor's focused set only. Report the result as narrowed; it is not a suite pass.

**The acceptance candidate** — the candidate Step 6 closes on, which in a chartered run is the phase's last commit — runs every gate the changed surface calls for, once: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, the full workspace test suite, `./scripts/check-interaction-bindings.sh --check`, `cargo coverage` with no card regressed and `cargo semantic-audit` with zero new findings for Rust paths, `pnpm run type-check`, `pnpm lint` and the frontend tests for frontend paths, the parser gate for parser paths. A failure goes to a fix executor, and its candidate repeats this full set. A lint failure — rustfmt, a compiler warning, or a clippy lint in the `style`, `complexity` or `perf` group — closes like a `text` finding, with no Step 6 re-review, only when its correction is semantics-neutral: judged by the orchestrator from the committed diff, it changes no drop timing, locking, error handling, control flow or evaluation order. A pure representation change, such as boxing an enum variant's payload, qualifies, as does removing an unused import; handling a `must_use` result does not. The orchestrator applies a fix the tool states verbatim on the candidate, any other such fix goes to a fix executor, and the commit either one makes repeats this full set. A lint in any other clippy group, and a correction that is not neutral or whose neutrality is uncertain, are `behavior`: each goes to a fix executor like any other failure, and its candidate returns to Step 6. Markdown-only policy changes need scope and diff checks; do not run Cargo or Tilt for them.

### Step 6 — Review the immutable candidate

Spawn a fresh agent to invoke `review-engine-impl` against `BASE_SHA..CANDIDATE_SHA`, with the original task, reviewed plan, in-scope paths, prior findings (including any small-change-lane constraints), attempt history, and a review pack when the [scout](scout.md)'s trigger applies. It reviews the diff and the checks that were run; additional checks must answer a concrete unresolved claim within the task scope. Missing evidence returns to the orchestrator, not an independent tooling project. After the result, apply the [run limits](#run-limits) before any fix or return to planning.

- **A design round** (any `behavior` finding): a fix executor starts from the reviewed `CANDIDATE_SHA` with the findings as constraints, then Steps 4–6 repeat.
- **A round without `behavior` findings closes the loop:** comment-only corrections (below) are applied by the orchestrator. Other `text` findings go to a fix executor, which applies the supplied text verbatim, followed by Steps 4–5, with no Step 6 re-review, because closing a `text` finding changes nothing the code does. `machinery` findings close as in Step 2.

**Comment-only corrections.** A finding is a comment-only correction when closing it adds or removes no non-comment line of a `.rs` file — a false, stale or missing sentence in a `//`, `///` or `//!` comment, a citation, a label — and the finding supplies the replacement text or the repair is deleting the false sentence. A correction in any other file kind goes to a fix executor. When every finding of a round is one, no implementation executor is dispatched and no review follows. The orchestrator applies each correction itself, inside the phase's `SCOPE_PATHS` (a correction whose file is outside the list goes to a fix executor), with Step 2's two-sided verification, commits once on the reviewed candidate, and records the class and the before/after text in the phase-fit record. The commit is proven comment-only by diffing the reviewed SHA (the head the review saw) against the correction commit's SHA, both named explicitly: `git diff --no-renames --name-only <reviewed SHA> <correction SHA>` lists only `.rs` paths in `SCOPE_PATHS`, `git diff --no-renames -U0 <reviewed SHA> <correction SHA>` has at least one hunk, and over its hunk lines (file headers excluded) every `^[+-]` line matches `^[+-][[:space:]]*//` and, read at its coordinate, lies outside a string literal, with a control showing the same predicate rejects a code line that carries a trailing `//` comment. The correction commit reruns Step 5 and, for parser paths, Step 4's parser measurement, in full, because AI-CONTRIBUTOR §6 reuses evidence from the same candidate and which comment lines an instrument reads is not predictable from the diff. Each correction gets one direct instrument check with its control leg for the claim the finding named — the census or grep that flagged the sentence no longer returns it, and did at the reviewed candidate. A correction whose proof, instrument check or reruns fail, a stale generated binding included (its regeneration is not comment text), is dropped: the implementation worktree returns to the reviewed SHA and the findings go to a fix executor. When in doubt, it is a fix-executor finding. A correction round mixed with other findings dispatches the fix executor and folds the corrections into it as constraints.

### Step 7 — Final acceptance

Accept when the plan is clean or closed by the small-change lane; the review returns no `behavior` finding and its `text` findings are applied as Step 6 says (comment-only corrections proven), or only residuals the run limits allow, listed; the completion checks pass at the candidate; and `rev-parse HEAD == CANDIDATE_SHA` — the correction commit when one exists. In a chartered run this is per-phase acceptance, with `PHASE_BASE_SHA` substituted; it emits no Final Report snapshot, ship, or PR handoff, which are run-level only.

After final acceptance of a run a maintainer invoked directly, ship it with `/ship-commits` as [pr-handoff.md](pr-handoff.md#ship-through-the-merge-queue) describes, unless the task says local-only. Otherwise, when the task includes opening a PR, follow [pr-handoff.md](pr-handoff.md).

## Final Report

Return after final acceptance:

1. Plan-review rounds with each round's finding tags, whether the small-change lane was used, and the final clean result.
2. What changed, grouped by subsystem and file.
3. Key architectural decisions.
4. `BASE_SHA`, accepted `CANDIDATE_SHA`, the materialized `SCOPE_PATHS` (extensions included), and run-artifact root.
5. The `START_SHA` each round began from, and what the parser measurement found when the change touched the parser.
6. Verification commands run and results, separated into preparatory and completion evidence.
7. Implementation-review rounds with each round's finding tags, reviewed SHA, any Step 6 correction commit with its phase-fit entry, and the final result (clean, corrections applied and proven, or listed residuals).
8. Checkpoint commit hash and staged file list.
9. Coverage impact for parser changes.
10. Deviations from the plan with reasons.
11. Self-flagged risks and judgment calls (yours + executor's).
12. Remaining items, if any, with reasons.
13. The budget, the phase-fit verdict and record path, every expansion case with its prediction and outcome, and abandoned candidates from approved restarts.
14. The planning / review / implementation / verification time totals, candidates accepted, any process-effort note, and which steps had a scout pack.
15. When shipped, the `/ship-commits` final report.
16. Chartered runs additionally: the items [chartered.md](chartered.md) lists.
