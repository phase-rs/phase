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
| 3. Implement | **Spawned `engine-implementation-executor` agent** | Baseline measurement, surgical edits, and preparatory checks; never commits |
| 4. Checkpoint + measure | This thread, then a fresh measurement executor | Orchestrator creates the candidate commit; isolated executor measures that immutable candidate |
| 5. Complete verification | This thread | Verify the committed candidate, never an in-flight working tree |
| 6. Review implementation | **Spawned `general-purpose` agent** invoking `/review-impl` | Independent review of the immutable base-to-candidate diff |
| 7. Final acceptance | This thread | Accept only the exact reviewed checkpoint candidate |

**Runtimes without subagent spawning (contributor environments — Codex CLI, plain LLM sessions).** The pipeline's value comes from context isolation between author and reviewer, not from the spawning mechanism. If your runtime cannot spawn agents, do NOT silently degrade to reviewing your own work in the same context — that is the failure mode this skill exists to prevent. Instead: run each step against a fresh context (new session/conversation per step when your runtime supports it), and for every review step hand the reviewer ONLY the artifact under review (the full plan, or the unified diff), the original task description, `CLAUDE.md`, and the relevant skill (`/review-engine-plan` or `/review-impl`) — never the conversation that produced it. If even that is impossible, say so explicitly in the final report and in the PR body under a "Validation Failures" heading; do not claim the review loop ran clean.

The orchestrator never authors content itself. Its only jobs are: spawn agents, route their output to the next step, loop review steps until clean, own the commit, and gracefully cull each spawned agent once its output is consumed (send a `shutdown_request` and wait for the `shutdown_response` ack — spawned agents now carry `SendMessage`, so they cull gracefully instead of being pane-killed). The structured report each agent returns stays the authoritative step handoff; SendMessage is an additive progress/acknowledgment channel, not a replacement.

## Run ownership, checkpoint identity, and the canonical receipt

Before dispatching an executor, the orchestrator records `BASE_SHA`, a frozen in-scope path representation, its SHA256, and an externally owned run directory. The representation is a file made exactly by `printf '%s\0' "${SCOPE_PATHS[@]}" | LC_ALL=C sort -z`, with no duplicate path records; `scope_paths_sha256` is the SHA256 of those exact NUL-delimited bytes. `BASE_SHA` never changes. The orchestrator alone stages or commits scope paths; executors never stage, commit, amend, or move `HEAD`.

Every implementation/fix dispatch has a named `START_SHA` and `IMPLEMENTATION_WORKTREE`. The first round has `START_SHA == BASE_SHA`; a fix round starts from the prior reviewed `CANDIDATE_SHA` in a fresh implementation worktree. Before edits and immediately before checkpoint, record that `HEAD == START_SHA`, the index has no executor staging, and the authorized unstaged delta is exactly the frozen scope. A changed `HEAD`, staged entry, unexpected path, or changed diff digest stops the run. The checkpoint commit is the sole candidate identity: explicitly stage only the frozen paths, commit only those paths, record `CANDIDATE_SHA`, and immediately prove `rev-parse HEAD == CANDIDATE_SHA`. Never measure an uncommitted tree or a moving `HEAD`.

Keep artifacts outside all worktrees. Each checkpoint owns one un-hashed canonical receipt, for example `<git-common-dir>/engine-implementer-runs/<run-id>/candidates/<CANDIDATE_SHA>/receipt`. It is the only provenance contract: do not create manifests, seals, provenance envelopes, replica/quorum records, or a parallel ledger.

The receipt is UTF-8 with LF line endings and exactly one final LF. It has one `key=value` line per field, no CR or NUL, and percent-encodes every UTF-8 byte except `[A-Za-z0-9._~-]` using uppercase hex. Keys appear exactly once in this fixed order: `format=engine-implementer-receipt-v1`, `base_sha`, `start_sha`, `candidate_sha`, `head_sha`, `scope_paths_path`, `scope_paths_sha256`, `scoped_diff_command`, `scoped_diff_path`, `scoped_diff_size`, `scoped_diff_sha256`, `base_source_hash`, `candidate_source_hash`, `projection_authority_diff_command`, `projection_authority_diff_path`, `projection_authority_diff_size`, `projection_authority_diff_sha256`, `projection_forced_reason`, and `parser_evidence`. `scope_paths_path` names the frozen NUL-delimited, `LC_ALL=C sort -z` representation above. `scoped_diff_command` is exactly:

```bash
git -C "$IMPLEMENTATION_WORKTREE" -c color.ui=false -c diff.noprefix=false -c core.quotepath=true -c diff.orderFile=/dev/null -c diff.interHunkContext=0 -c diff.suppressBlankEmpty=false diff --no-color --no-ext-diff --no-textconv --no-renames --diff-algorithm=myers --no-indent-heuristic --full-index --binary --src-prefix=a/ --dst-prefix=b/ --unified=3 "$BASE_SHA" "$CANDIDATE_SHA" -- "${SCOPE_PATHS[@]}"
```

Next come the mandatory `source_hash_record.base.*` and `source_hash_record.candidate.*` groups, each in this order: `command`, `expected_sha`, `head_before`, `detached_before`, `clean_before`, `head_after`, `detached_after`, `clean_after`, `exit`, `stdout_path`, `stdout_sha256`, `stderr_path`, `stderr_sha256`, `artifact_path`, `artifact_size`, `artifact_sha256`. Each group records the exact `scripts/engine-source-hash.sh` invocation and its SHA-bound output artifact. `detached_before` and `detached_after` prove `git symbolic-ref -q HEAD` exited exactly `1` with empty stdout and stderr; any other result is `CANNOT_ANSWER` during measurement, not detached-state evidence. These source-hash groups and the `projection_authority_diff_*` record are mandatory even when hashes are equal. `projection_authority_diff_command` is exactly `git -C "$IMPLEMENTATION_WORKTREE" diff --name-only -z "$BASE_SHA" "$CANDIDATE_SHA" -- Cargo.toml .cargo/config.toml rust-toolchain.toml scripts/engine-source-hash.sh`; capture its NUL-delimited stdout as the recorded path/size/SHA256 artifact. `projection_forced_reason` is exactly one of `NONE`, `SOURCE_HASH_DIFFERENCE`, `EXCLUDED_PROJECTION_AUTHORITY_INPUT_CHANGED`, or `SOURCE_HASH_DIFFERENCE_AND_EXCLUDED_PROJECTION_AUTHORITY_INPUT_CHANGED`. Next comes no projection section only when `projection_forced_reason=NONE`; otherwise write `projection_step_count` followed by every `projection_step.<N>.*` group in strictly ascending zero-based `N`; each group contains, in this order, `side`, `command`, `env`, `worktree`, `target`, `expected_sha`, `head_before`, `detached_before`, `head_after`, `detached_after`, `clean_before`, `clean_after`, `exit`, `stdout_path`, `stdout_sha256`, `stderr_path`, `stderr_sha256`, `produced_artifacts`. The detached fields use the same exact exit-`1` / empty-stdout-and-stderr proof. Then write `completion_check_count` and each strictly ascending `completion_check.<N>.*` group with the same record fields, `artifact_count` and each strictly ascending `artifact.<N>.path`, `.size`, `.sha256`, then `declared_absent_input_count` and each strictly ascending `declared_absent_input.<N>.path`. No indexed member may be omitted; a conditional section is omitted only as stated. Artifact rows cover every source-hash output, authority-diff output, projection output, command stdout/stderr capture, scoped diff, and completion artifact. The receipt itself is never listed or hashed; receipt validation is an external reviewer/final-acceptance gate and never a `completion_check` row or receipt artifact. A malformed, unordered, duplicate, missing, or digest-mismatched receipt is `CANNOT_ANSWER`.

The parser-impact decision happens only after the checkpoint. Measurement-only has exactly two outcomes: `MEASURED` or `CANNOT_ANSWER`. Any identity, detached/clean-state, command, source-hash, authority-diff, projection, receipt, artifact, or digest failure is `CANNOT_ANSWER`; retain completed records but do not claim parser evidence. On `MEASURED`, record `scripts/engine-source-hash.sh "$BASE_SHA"` and `scripts/engine-source-hash.sh "$CANDIDATE_SHA"` in the mandatory `source_hash_record.base` / `.candidate` receipt groups, including their SHA-bound output artifacts. Also run and record the exact `projection_authority_diff_command`. Set `projection_forced_reason=NONE` only when the source hashes are equal and that NUL-delimited path artifact is empty. A source-hash difference and/or any path in that artifact forces `parser_evidence=PROJECTED_PARSE_DIFF`, exactly one direct base projection, and exactly one direct candidate projection from the same read-only, pinned `AtomicCards.json`; choose the canonical reason that names both causes when both apply. Only `NONE` permits `parser_evidence=NO_PARSE_AFFECTING_CHANGE` and no projection inputs, projection records, or comparator outputs. Record the pinned `AtomicCards.json` path and SHA256 as artifacts whenever projection is forced. Do not download, regenerate, copy, sample, or compare replicas.

For each projection use its detached clean worktree and an isolated target directory. Build exactly with `CARGO_TARGET_DIR=<receipt-target> cargo build --profile tool --features cli --bin oracle-gen --bin coverage-report --bin coverage-parse-diff`. Run that side's `oracle-gen` directly against the pinned data root, write its `card-data.json` and `card-names.json` under the projection directory, then run that side's `coverage-report` directly against the projection directory to write `coverage-data.json`. Capture the exact command, environment, worktree identity/clean checks, binary target, exit status, stdout/stderr, and every produced artifact in a projection-step receipt group. Each group's ordered `detached_before` and `detached_after` fields must prove `git symbolic-ref -q HEAD` exited exactly `1` with empty stdout and stderr; any other result is `CANNOT_ANSWER`. Invoke the **base-built** comparator directly once against the two projected coverage files:

```bash
"$BASE_TARGET/tool/coverage-parse-diff" "$BASE_PROJECTION/coverage-data.json" "$CANDIDATE_PROJECTION/coverage-data.json" \
  --base-sha "$BASE_SHA" --head-sha "$CANDIDATE_SHA" \
  --markdown "$RUN_ROOT/parse-diff.md" --json "$RUN_ROOT/parse-diff.json" --max-clusters <N>
```

Record that comparator invocation and both outputs as projection steps/artifacts. `parser_evidence=PROJECTED_PARSE_DIFF` only after all required base, candidate, and comparator records verify. Do not put these operational artifacts in the implementation diff unless the reviewed plan explicitly scopes them.

## Inputs

Either:

1. A task description (cards, CR rules, Oracle text patterns, affected subsystems, expected behavior), or
2. A pre-existing plan — treat as a draft unless it has already passed `/review-engine-plan` to clean.

Before Step 3, prepare and verify a clean `IMPLEMENTATION_WORKTREE` at `START_SHA`. After its checkpoint, prepare clean detached base and candidate projection worktrees at `BASE_SHA` and `CANDIDATE_SHA`, and a distinct clean detached `COMPLETION_WORKTREE` at `CANDIDATE_SHA`; no projection or completion worktree is used for implementation. Per `feedback_session_default_no_worktree`, do not re-ask about worktrees during an active pipeline session — use the session default.

## Pipeline

### Step 1 — Produce the plan

Spawn a `general-purpose` agent and instruct it to invoke `/engine-planner`. The agent returns a plan with every mandatory architectural section.

**Spawn inputs:** task description; in-scope file/subsystem hints; any prior reviewer findings (none on first round).

Do not author or edit the plan in this thread — surgical-fix mode (below) is the one exception, and only under its three measured conditions. If the returned plan is missing sections or is superficial, send the same inputs plus an explicit "missing sections" note to a **fresh** planning agent — do not patch it yourself.

### Step 2 — Review the plan until clean (unbounded loop)

Spawn a `general-purpose` agent and instruct it to invoke `/review-engine-plan` against the full plan.

**Reviewer spawn inputs:** the full plan; the original task description.

If the reviewer returns gaps, spawn a **fresh** planning agent (Step 1 inputs plus the reviewer's findings as additional constraints) to produce a revised plan, then spawn a **fresh** reviewer agent against the revised plan.

**Repeat until a full review round returns zero gaps.** There is no iteration cap — "two rounds and ship" is not acceptable. Stop only for:

- a true human design decision the planner cannot resolve,
- missing external access (CR text unavailable, file inaccessible), or
- an environment blocker that makes review impossible.

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
- **Record the mode switch, its three measurements, the spot-vs-design classification of each round's findings, each attempted edit's two-sided verification result (pass or fail) and sweep boundary, and why the mode ends** in `<git-common-dir>/engine-implementer-runs/<run-id>/surgical-mode-switch`, never in the plan text the fresh re-reviewer and the executor read — recording it there hands the one remaining independent check a prior verdict. It is a process record of this loop, not provenance: it carries no candidate identity and duplicates no receipt field, so the canonical-receipt rule above does not reach it. Append one numbered entry per round, never overwrite: ending surgical mode and re-entering it later continues the same numbered sequence, and clobbering an earlier entry loses the exit that entry recorded. Every exit is then auditable rather than asserted. Each entry carries that round's classification, edit results, and sweep boundaries; only a round that enters the mode records the switch and its three measurements, and only a round that ends the mode records why.

**This does not contradict `$bug-triage`'s fixpoint gate.** That gate requires whole-plan re-review because *"revisions routinely INTRODUCE new gaps in untouched-looking areas"* — planner **rewrites** do. A check-and-replace at a named coordinate does not rewrite, which is why it is the safe tool once the design has stopped moving. `$review-engine-plan` ends its loop with *"or the caller stops the process"* and states no criteria; this section is those criteria, and it lives here because the orchestrator is that caller.

This is not a licence for "two rounds and ship". The unbounded loop remains the default and the burden of proof is on leaving it: no measurement, no switch. Surgical mode is scoped to this Step 2 plan-review loop only — Step 6's implementation-review loop never uses it, because there the artifact is a committed candidate that only an executor may edit under the frozen-scope and receipt contract.

### Step 3 — Dispatch implementation

Spawn the `engine-implementation-executor` agent.

**Spawn inputs:** mode `implementation/fix`; the reviewed clean plan in full; `BASE_SHA`; named `START_SHA`; frozen in-bounds / out-of-bounds path list and its SHA256; named `IMPLEMENTATION_WORKTREE`; the canonical receipt path; and any prior reviewer findings (none on first round). First round: `START_SHA == BASE_SHA`. Fix round: `START_SHA` is the previously reviewed `CANDIDATE_SHA`, never a moving branch head.

The implementation executor edits only its frozen scope and runs **preparatory** checks. Preparatory success is not completion evidence. Its existing discriminating-test, selected-authority, coverage-honesty, maintainer-simulation, and CR-annotation gates remain the authoritative gates; do not restate or replace them here.

If the executor returns "stop and return" items (plan contradicts current code, ad hoc parser dispatch unavoidable, CR uncertain), do NOT improvise around them. Loop back to Step 1, feed the executor's findings into `/engine-planner` as new constraints, and re-run Steps 1–3.

**Large JSON fixture constraint.** Any repository-bound JSON fixture ≳100KB (test fixtures, game-state dumps, generated maps — not runtime/config JSON whose consumers read plain `.json`) gets `gzip -9 -n` (`-n` keeps the archive byte-reproducible) and loads via the established inflate pattern: `include_bytes!("….json.gz")` + a test-local `gunzip` helper using `flate2::read::GzDecoder` (examples: `tests/integration/combo_infinite_pile.rs`, `cr733_resolved_commands_p0.rs`). Never commit the uncompressed twin alongside the `.json.gz`. If a fixture is regenerated by a script, note in the reading test that regeneration requires re-gzipping.

### Step 4 — Checkpoint, then measure the committed candidate

Before Step 3, the orchestrator records `IMPLEMENTATION_WORKTREE`'s staged/unstaged path snapshot and clean `HEAD == START_SHA` attestation. Before staging, it repeats the stable-HEAD check and records its exact implementation delta: the approved path list and the SHA256 of each approved path's `START_SHA..working-tree` diff. It must first prove, from that snapshot, that no pre-existing staged or unstaged change overlaps an approved path; if attribution is ambiguous, stop and return rather than unstage, sweep in, or overwrite another agent's work. The checkpoint is the candidate commit: stage each approved path by explicit pathspec — never `git add -A` — and never commit without explicit pathspec because the shared index can sweep in other agents' staged files (`feedback_git_add_file_bundles_concurrent_work`, `feedback_shared_index_commit_pathspec`). Stage and commit only the explicit approved paths, never alter unrelated index entries, then record the full `CANDIDATE_SHA`. Immediately run `git -C "$IMPLEMENTATION_WORKTREE" rev-parse HEAD` and compare its output to `CANDIDATE_SHA`; a mismatch stops the run. Verify that `START_SHA..CANDIDATE_SHA` contains only the recorded authorized delta (paths and diff digests), and retain the original `BASE_SHA..CANDIDATE_SHA` diff for final review; otherwise stop and return. This is an orchestrator-only commit; the executor never performs it. Do not measure an uncommitted tree or use a moving `HEAD` as the candidate identity. Verify HEAD is on a branch before any explicitly requested push (`feedback_verify_head_attached_before_push`), never pipe `git push` into `tail`/`head` (`feedback_git_push_no_pipe`), and never push unless explicitly requested.

Spawn a **fresh** `engine-implementation-executor` in mode `measurement-only` with `BASE_SHA`, `CANDIDATE_SHA`, named `IMPLEMENTATION_WORKTREE`, frozen scope paths, the canonical receipt path, a detached clean base worktree, a detached clean candidate worktree, and the read-only pinned data root containing `AtomicCards.json`. This executor makes no source edits and no commits.

The measurement-only executor runs `scripts/engine-source-hash.sh` in the respective detached base/candidate projection worktrees at `BASE_SHA` and `CANDIDATE_SHA`, stores both SHA-bound outputs in the mandatory `source_hash_record.base` / `.candidate` receipt groups, and runs the exact four-path NUL-safe `projection_authority_diff_command` from the receipt contract. It returns `CANNOT_ANSWER` for any identity, detached/clean-state, command, source-hash, authority-diff, projection, receipt, artifact, or digest failure; otherwise it returns `MEASURED`. Equal hashes set `parser_evidence=NO_PARSE_AFFECTING_CHANGE` and permit no parser projection only when that authority-diff artifact is empty (`projection_forced_reason=NONE`). A source-hash difference or any authority-diff path forces the canonical non-`NONE` reason and requires the exact one base projection, one candidate projection, and base-built comparator defined in the receipt contract. It records every command, environment, base/candidate worktree identity before and after the operation, clean state before and after, output capture, produced artifact, and artifact digest. `./scripts/gen-card-data.sh` and `cargo coverage` are never projection evidence.

### Step 5 — Committed-candidate completion verification

Completion verification occurs only after the checkpoint, in the distinct clean detached `COMPLETION_WORKTREE` at `CANDIDATE_SHA`, and records detached identity, `HEAD`, and clean checks at start and end. Every completion or parser command must either use `git -C "$COMPLETION_WORKTREE"` or execute with `COMPLETION_WORKTREE` as its working directory. The candidate parser gate must enumerate its range NUL-safely with `git -C "$COMPLETION_WORKTREE" diff --name-only -z "$BASE_SHA" "$CANDIDATE_SHA" -- crates/engine/src/parser/`; any loop reading it uses `IFS= read -r -d ''`. The required set is both scope-/plan-derived checks and every surface-derived gate applicable to the changed paths in the executor's existing implementation/fix verification blocks: formatting for implementation changes, the Rust/engine/parser Tilt-first-or-isolated-direct block for Rust/engine/parser paths, the frontend Tilt-first-or-isolated-direct block for frontend paths, and the parser preparatory gate for parser paths. Instantiate those blocks only after substituting `COMPLETION_WORKTREE` for `IMPLEMENTATION_WORKTREE` and `CANDIDATE_SHA` for `START_SHA`; every recorded completion command must show those substitutions and must never name `IMPLEMENTATION_WORKTREE` or `START_SHA`. Use candidate-SHA-bound CI or Tilt evidence only when the evidence itself proves that binding; otherwise use the isolated direct fallback already specified by the applicable block. Never promote the implementation executor's preparatory result to completion evidence. Its receipt completion-check groups must enumerate every required check and, for each, record the exact command, exit result, detached `CANDIDATE_SHA` identity and clean status at start and end, including ordered `detached_before` and `detached_after` proofs that `git symbolic-ref -q HEAD` exited exactly `1` with empty stdout and stderr; any other result is an operational failure. A missing check, nonzero/unknown exit result, identity mismatch, detached-state failure, or dirty status fails completion.

For Markdown-only policy updates, the mandatory completion-check set is limited to scope-path, SHA-identity, and Markdown/diff checks; do not run Cargo or Tilt. Receipt validation is an external reviewer/final-acceptance gate, never a self-referential completion-check record or receipt artifact.

### Step 6 — Review the immutable candidate

Spawn a fresh `general-purpose` agent and instruct it to invoke `/review-impl` against `BASE_SHA..CANDIDATE_SHA`, the original task, reviewed plan, frozen scope paths, `START_SHA`, implementation-worktree start/end stable-HEAD attestations, the detached `COMPLETION_WORKTREE` start/end identity attestations, and the canonical receipt. The reviewer validates the receipt before applying the universal lenses: exact fixed-field order; percent encoding; no CR/NUL; every conditional/indexed section; candidate/head and worktree identity; the frozen NUL-sorted scope representation and hash; the exact scoped-diff and authority-diff commands and bytes; both mandatory source-hash records even on equal hashes; every artifact size/SHA256; and every projection/completion group's ordered `detached_before`/`detached_after` proof that `git symbolic-ref -q HEAD` exited exactly `1` with empty stdout and stderr (any other result is an operational failure). It must accept an omitted projection only if the source hashes are equal, the authority-diff artifact is empty, and `projection_forced_reason=NONE`; otherwise, it must reject it. It must reject any forced-projection receipt only when it lacks the pinned `AtomicCards.json`, isolated exact tool builds, exactly one direct base and candidate projection, or one base-built comparator invocation with both `--base-sha` and `--head-sha`. After validating it, the reviewer emits the receipt SHA256 externally in its review result; that digest never appears in the receipt. It must validate that first-round `START_SHA == BASE_SHA`, each fix `START_SHA` is the prior reviewed candidate, and the implementation worktree was clean at start and stable at checkpoint. The reviewer MUST fail missing or unsuccessful mandatory completion checks, verify the originally reported bug or requirement is actually fixed via the existing discriminating runtime-test gate, and audit the existing maintainer-simulation and coverage-honesty artifacts.

If review returns findings, spawn a **fresh** implementation/fix executor with the findings as constraints. The round is always:

```text
edit + preparatory checks → orchestrator checkpoint → fresh measurement-only executor
→ committed-candidate completion verification → fresh BASE_SHA..CANDIDATE_SHA review
```

Every round keeps the original `BASE_SHA` and frozen scope paths. Every checkpoint receives its own receipt. Never review a diff that includes a later unmeasured fix.

### Step 7 — Final acceptance

Accept only when the plan-review loop is clean, the reviewer emitted a matching external receipt SHA256, final acceptance independently revalidates the receipt and every recorded artifact, parser evidence and completion checks pass, every surgical-fix mode round has the complete numbered entry Step 2 requires, and the fresh implementation review returns zero findings. The receipt validation/hash and those entries are acceptance evidence, never completion-check rows or receipt artifacts. Immediately run `git -C "$IMPLEMENTATION_WORKTREE" rev-parse HEAD` and compare its output to `CANDIDATE_SHA`; if it differs, the review is stale and the current head must repeat the checkpoint-to-review sequence. Do not treat review of an ancestor as review of current work.

### Post-acceptance PR handoff (non-gating)

Final acceptance emits an immutable Final Report snapshot: `Pipeline-reviewed head == Current branch head == accepted CANDIDATE_SHA`, `Pipeline status: current`, and `Current-head review: none`. Do not alter or replace that snapshot, the accepted candidate SHA, or any canonical receipt evidence after acceptance.

Copy the following mutable `PR handoff` block into the PR body beside the retained pipeline report:

```text
Pipeline-reviewed head: <accepted CANDIDATE_SHA>
Current branch head: <current branch SHA>
Pipeline status: current | historical — <reason>
Current-head review: none | clean at <SHA> | findings at <SHA>
```

Whenever the branch head changes after acceptance, including through a rebase, update `Current branch head`, set `Pipeline status` to `historical — <reason>`, and reset `Current-head review` to `none`. When current-head evidence is desired, run ordinary `/review-impl` against the complete current PR/head — not checkpoint mode and not an incremental-only diff — then record `clean at <SHA>` or `findings at <SHA>` for that exact SHA. Each future head change repeats the reset. `Pipeline status` remains historical unless the current head again equals the original accepted `CANDIDATE_SHA`, in which case set it to `current`.

If later work invalidates the approved plan or architecture, return to plan review. Otherwise, this is a concise reporting and navigation flow only: it is not a gate, a new receipt requirement, GitHub automation, an executor change, or a PR-handler change.

## Final Report

Return after final acceptance:

1. Plan-review rounds (count), whether surgical-fix mode was used, and final clean result.
2. What changed, grouped by subsystem and file.
3. Key architectural decisions.
4. `BASE_SHA`, accepted `CANDIDATE_SHA`, frozen scope paths, and run-artifact root.
5. `START_SHA`/`IMPLEMENTATION_WORKTREE` records for every round; the canonical receipt path, parser-evidence branch, direct-projection records when hashes differ, and completion-check identity.
6. Verification commands run and results, separated into preparatory and completion evidence.
7. Implementation-review rounds (count), reviewed SHA, and final clean result.
8. Checkpoint commit hash and staged file list.
9. Coverage impact for parser changes.
10. Deviations from the plan with reasons.
11. Self-flagged risks and judgment calls (yours + executor's).
12. Remaining items, if any, with reasons.
