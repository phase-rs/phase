---
name: pr-review-loop
description: "Use to run a continuous review sweep over open contributor PRs in phase.rs. The skill is a thin orchestration layer over scripts/pr_review.py: discover candidates, detect stale reviews/follow-ups, dispatch review-impl for PRs that need judgment, and delegate authorized merge handling to pr-contribution-handler."
---

# PR Review Loop

Continuously review open contributor PRs, reprocessing only when GitHub state indicates new information: changed head, author follow-up, stale approval, stale request-changes, CI transition, queue drop, or a policy/hard-stop condition.

This skill is intentionally small. Mutable policy and contributor-specific state do **not** live here.

## Sources Of Truth

- **GitHub is authoritative** for PR head, author, reviews, comments, labels, CI, and merge-queue state.
- **Repo policy** lives in `.agents/pr-review-policy.toml` and must contain only repo-level, non-personal rules: path classifiers, domain capabilities, labels, hard-stop path patterns, generated-file patterns, and default gates.
- **Local review memory** lives outside the repo by default under `~/.local/state/pr-review/<owner>__<repo>/` unless `PR_REVIEW_STATE_DIR` or `--state-dir` is set. This directory contains:
  - `review-events.jsonl` — the sole canonical store: an append-only local event log with locked, deduplicated, `fsync`'d appends.
  - `review-summary.json` — generated token-minimal summary derived from the log.
  - A stray `review-state.sqlite` from an older build is an orphaned leftover; it is no longer read or written, and is safe to ignore or delete manually.
- **Never Read `review-events.jsonl` directly.** It is unbounded and not token-shaped; all queries must go through the `pr_review.py` CLI (`scan`/`inspect`/`recommend`/`analytics`/`compact`). `review-summary.json` is the only state file intended for direct reading.
- **No hardcoded names.** Contributor standings, frontend exceptions, reviewer identities, private overrides, and one-off maintainer policy belong in local/private state, never in this skill.
- **Contributor standing lives in `private-overrides.json`** under `contributor_standing` (`skip`/`probation`/`watch`/`trusted`, lowercase-matched logins). It sits in the gitignored state dir on the review host; other hosts see only derived standing. The narrative quality log is a historical appendix — the event log, via recorded `signals`, is the data authority for per-contributor patterns.
- **Gittensor PR-history enrichment is advisory.** `pr_review.py` fetches the public Gittensor PR feed by default and adds a `gittensor` block to packets when the author appears there. A high closed-PR count across other repos adds the generic `gittensor-closed-heavy` proof risk flag. Use it to increase caution and require concrete proof; do not cite it as a public accusation or reject a PR on that signal alone.

## Commands

Use the CLI from the repo root:

```bash
python3 scripts/pr_review.py scan --repo phase-rs/phase --config .agents/pr-review-policy.toml
python3 scripts/pr_review.py inspect <PR> --repo phase-rs/phase --mode full
python3 scripts/pr_review.py recommend <PR> --repo phase-rs/phase
python3 scripts/pr_review.py recommend <PR> --repo phase-rs/phase --emit-event
python3 scripts/pr_review.py record --event-json -
python3 scripts/pr_review.py compact
```

`record` validates each event's `event_type` and (when present) `outcome` against a closed vocabulary and lowercases the outcome on write; an out-of-vocabulary event is rejected with exit 1 and the allowed values, and `--force` bypasses validation (flagging the event `"forced": true`). The preferred recording path is to add `--emit-event` to `inspect`/`recommend`, fill the returned `event_skeleton` (its prefilled timestamp gives idempotent retries), and pipe it back to `record --event-json -`.

Import legacy state once:

```bash
python3 scripts/pr_review.py import \
  --tracker /Users/matt/dev/forge.rs-pr-tracker.tsv \
  --quality /Users/matt/dev/forge.rs-contributor-quality.md
python3 scripts/pr_review.py compact
```

## Sweep Protocol

At sweep start, read `.agents/pr-review/campaign-hotspots.toml` if present (lead-maintained, untracked, updated at campaign routing/ship events). It lists: migrated areas with the rule now in force and the REDIRECT pointer for authors; coverage-delta semantics (an honest coverage DECREASE — silent swallows converted to explicit `Effect::Unimplemented` — is acceptable and must never be "restored"; in CI only the `engine_regress` bucket, a card LOSING a previously-supported handler, is fatal); diff content outside the contributor's control (generated-file dirt, export nondeterminism, staleness caused by maintainer-side churn — strip/normalize/rebase these yourself, never request changes for them); and files owned by in-flight campaign units (a contributor PR racing those files → hold + surface to the maintainer). Hotspots inform routing and comment content; they are NOT an additional review gate — if a hotspot prohibition reaches review without CI redding it, that is a gate gap: file it, don't make the contributor absorb the miss.

1. Resolve the acting identity from GitHub. Do not review PRs authored by the acting login.
2. Run `scan`. Use `action_counts` / `candidates_by_action` for routing; do not infer legacy bucket names. Treat its result as a triage packet, not a final approval gate.
3. Every packet (and `recommend` output) carries an advisory `contributor` block — standing, scrutiny, `scrutiny_reasons`, `recurrence`, `first_contribution` — derived from the local event log plus `contributor_standing` overrides; it is `null` only when the PR has no author login. Scale review depth by it: `first_contribution` → full evidence bar, and point the author at the `docs/AI-CONTRIBUTOR.md` gates in the first review comment; `elevated` → dig specifically into the recurring signals named in `scrutiny_reasons`; `maintainer_attention` → include the contributor in the sweep report for the maintainer. `light_touch_eligible` permits a lighter pass only while scrutiny is `normal`.
4. Every packet carries separate `artifacts`, `architecture_scope`, and `proof` blocks. The gates have independent modes. Artifact verification remains audit-only until its immutable post-publication cutoff is activated; while it is in audit mode, report `audit_would_decline` and continue the existing review flow unchanged. Architecture scope is enforced immediately when `architecture_scope.mode = "enforce"`, with no creation-time cutoff: an unauthorized architecture trigger declines before implementation-diff review. Neither audit result authorizes a comment, close, dequeue, or other mutation. An enforced decline is a blocker in its own right, and proof, Tier, standing, quality, praise, or frontend permission cannot waive it. Claimed parse impact remains optional manual quality evidence. `proof.proof_gap` remains the independent risk-evidence gate.
5. For each candidate:
   - `hard_stop` / `request_changes` — surface the precise blocker; do not enqueue.
   - `decline` — do not fetch or review the implementation diff. Live-recheck `state`, `headRefOid`, body, and auto-merge, rerun `recommend`, and proceed only if the same current head still returns `decline`. Put the packet's complete `decline_comment` in a temp file, post it with `gh pr comment <PR> --body-file <file>`, then run `gh pr close <PR>`, record a `decline` event with the structured evidence, and stop. If auto-merge is enabled, disable it before commenting/closing. Audit-only `audit_would_decline` is reporting data and authorizes no comment, close, dequeue, or mutation.
   - `skip` — disambiguate by `reason`: `closed` / `self_authored` need no action; `contributor_standing_skip` is an explicit maintainer standing override — record the skip and move on without reviewing. A skip-listed contributor touching hard-stop paths still surfaces as `request_changes` (safety outranks the skip).
   - `blocked` — current head already has blocking maintainer feedback. Read the blocking feedback before deciding to wait. If any part of the blocker is "the branch is stale / needs a rebase", first classify the cause per **Maintainer-Caused Staleness** below; when our own churn broke it, the rebase or port is ours to do regardless of size, and the maintainer-fixup cap in this route does not apply. A formal `CHANGES_REQUESTED` state is not by itself a reason to keep waiting: if later maintainer feedback on the same head says the blocker is resolved, no unresolved finding remains, or the PR is otherwise clean-but-stuck because the formal review state was not cleared, delegate the PR to `pr-contribution-handler` in authorized mode to live-check, approve, label, and enqueue. If the only remaining blockers are maintainer-fixup sized, delegate the PR to `pr-contribution-handler` in authorized mode instead of making the contributor do another round-trip. Maintainer-fixup sized means small, local, low-risk corrections that do not change the accepted design or require new product/rules judgment: replacing/removing an incorrect CR citation while preserving the already-reviewed logic, resolving a small merge conflict where the target logic already exists on one side, stripping accidental generated/noise hunks, fixing a single failing regression caused by main drift when the accepted design is unchanged, or threading an obviously missing renamed helper/import through the existing implementation. Do not use this path when there is any unresolved substantive behavior, architecture, proof-gap, test-discrimination, parse-diff, security, or hard-stop concern; keep the PR blocked until a new head or author follow-up. If the contributor remains inactive and the blockers are not maintainer-fixup sized, follow the requested-changes expiry actions below instead of leaving the PR blocked indefinitely.
   - `defer` — record the deferral event; do not approve, enqueue, or merge. If the recommendation carries `label_to_apply`, add that label to the PR for maintainer filtering before moving on. Label names must come from repo policy, not from this skill.
   - `hold_ci` — record a non-terminal hold only when the packet is incomplete or an external condition prevents review. CI being pending, unknown, or red is not itself a review/enqueue blocker; merge-when-ready will wait for required checks.
   - `hold` with reason `insufficient_admission_data` — enforcement cannot prove whether the PR is legacy because `createdAt` is missing or malformed. This is a maintainer/data blocker: do not inspect the implementation diff, comment, close, dequeue, or otherwise mutate the PR. Report the evidence and wait for corrected source data.
   - `queued` — auto-merge is already enabled or the PR is already in the queue. Treat this as no action only while required checks are pending or green. If any required check is terminal red, the PR is not across the finish line: delegate to `pr-contribution-handler` in authorized mode to inspect the failing check, apply a maintainer-fixup-sized repair when appropriate, re-approve/re-enable auto-merge if a push disabled it, or report a real blocker. Do not leave approved-but-red PRs to sit merely because they were previously enqueued.
   - `dequeue_stale_for_handler` / `update_branch_for_handler` / `approve_ready_for_handler` / `warn_stale_changes_for_handler` / `close_stale_changes_for_handler` — advisory only; delegate execution to `pr-contribution-handler` in authorized mode. `update_branch_for_handler` fires on both `BEHIND` (auto-resolvable via `gh pr update-branch`) and `CONFLICTING` (a real conflict; `update-branch` 422s). Check `mergeStateStatus` first, and note that neither status detects semantic staleness — see **Maintainer-Caused Staleness**.
   - `review` — fetch an `inspect --mode full` packet, then run `review-impl` against the current head and GitHub API/local diff evidence. For engine/parser-surface PRs, the parse-diff sticky comment (`<!-- coverage-parse-diff -->`) is REQUIRED review evidence: fetch its full body and confront the card-level diff against the PR's claimed scope. The packet's `parse_diff` field carries presence/state/`updated_at`. If state is `baseline_pending` on a stale branch (the `review_parse_baseline_pending` reason), route to update-branch first only when the PR history does not already contain a maintainer merge-main commit for the same baseline-pending churn. Repeated maintainer merge commits are PR spam: if the current head is otherwise clean and authorized mode would enqueue it after green CI, delegate to `pr-contribution-handler` to approve/enqueue now; if it is not clean, hold or review without mutating the branch. If the comment is absent but engine source changed, treat it as missing evidence: check whether CI ran for the current head before reviewing. If the review finds only a couple small, local, low-risk fixes between the PR and mergeability, do those maintainer fixups through `pr-contribution-handler` instead of requesting another contributor round-trip; use the same maintainer-fixup boundary as the `blocked` route above. If the manual quality gate passes, tell the handler to apply the existing policy-configured `quality` label with the type label; do not invent a `quality_recommended` event.
6. Record every material outcome with `record`. Attach `signals` (closed vocabulary, validated at record time) to the outcome event for observations from THIS review only, never re-recorded history. The vocabulary has two halves: defect signals (feed score penalties, windowed recurrence, and scrutiny) and praise signals (`right-seam`, `scope-discipline`, `discriminating-runtime-test`, `parameterized-not-proliferated`, `evidence-backed-pushback` — feed a capped score credit only, never recurrence or scrutiny). Never invent tokens: an out-of-vocabulary signal is rejected at record time, and if a needed concept is missing the fix is a vocabulary addition in `pr_review.py`, not a `--force`. Regenerate summaries with `compact` when useful.

After any delegated approval/label/enqueue operation, independently run `gh pr view <PR> --json labels` and verify the expected type label and, when requested, `quality` label are actually present. Delegation success without the live labels is incomplete handling.

Use `wrong-or-stale-cr-annotation` for an incorrect, unrelated, or stale CR citation and `duplicated-domain-vocabulary` when a PR creates a second name/type/helper for an existing domain concept. If a recorded signal was factually wrong, append `review_correction` with `corrects_event_id` and only the mistaken signal subset; never compensate by adding praise.

## Review Freshness

Approval freshness is attached to a head, not to a PR number. A post-approval force-push, same-head newer blocking maintainer activity, author follow-up after review, or queue drop must re-surface the PR. A terminal local event never overrides newer GitHub activity.

The CLI models freshness using:

- current `headRefOid`;
- latest maintainer comment/review and the commit SHA attached to formal reviews;
- author follow-ups;
- substantive vs merge-only commits;
- review decision;
- CI status as evidence only, not as a pre-review or merge-when-ready gate;
- labels and merge-queue membership.

## Maintainer-Caused Staleness

Before blocking a PR on "needs rebase", classify **why** it went stale. Causation, not size, decides who does the work.

- **Maintainer-caused** — main's own refactor churn invalidated the branch, or a PR merged during the review window rewrote the same seam. Signature: the branch was fine when it was opened or last reviewed, and it broke without the author touching it. **The maintainer does the rebase/port.** The maintainer-fixup size cap in the `blocked` route does NOT apply here: it caps discretionary fixups, and this is not discretionary. If we deleted the API out from under a contributor, we are the ones who know the replacement, and the contributor should not pay for our campaign. Porting a branch across an in-flight internal refactor is legitimate maintainer work at any size.
- **Contributor-caused** — fork hygiene (unrelated history, an orphan or force-pushed branch, no merge-base), or the author's own change colliding with long-settled main code. Bounce it, unless it is mechanically cheap to fix, in which case just fix it.

A PR can be stale for both reasons at once, and can also carry substantive blockers of its own. Rebasing it does not clear those — resolve the staleness, then re-review the delta on the new head.

**Textual vs semantic staleness.** `mergeStateStatus` is a textual check and is not evidence the branch is healthy:

- *Textual* (`DIRTY` / `CONFLICTING`) — announces itself. Cheapest case.
- *Semantic* (`BEHIND`, or even `MERGEABLE`) — git merges it without a single conflict and it still does not compile, or worse, compiles and silently drops behavior. This is strictly nastier than `DIRTY` precisely because nothing flags it. A `MERGEABLE` branch sitting under a refactor that deleted a type it depends on will report clean right up until the build fails.

**Never resolve a semantic conflict by picking a side.** Where both sides changed the same seam, name the behavior each side contributes, then require **a discriminating test from each side green on the rebased head** before approving. Taking `--ours` or `--theirs` on a seam both PRs deliberately modified silently drops one of them, and by construction neither PR's own tests will catch it.

**Mechanics.** Push to the contributor's branch only when `maintainerCanModify` is true. Rebase their commits preserving `%an`/`%ae`, and put the port or conflict resolution in a separate follow-up commit trailing `Co-authored-by:` the contributor — their work stays theirs, and the diff shows exactly what we changed and why. Re-verify locally before pushing; do not push a port that only compiles.

## Review Bar

The bar is still owned by `review-impl` and `pr-contribution-handler`:

- correct architectural seam;
- idiomatic implementation at that seam;
- maintainability and building-block reuse;
- value proportional to blast radius;
- discriminating tests that would fail on revert;
- rules/CR evidence when the repo policy enables the MTG Comprehensive Rules domain;
- no unresolved blocking feedback.

The CLI may recommend that a PR is ready for handler execution only when its structured gates say so, but the recommendation is advisory. Queue readiness is never satisfied from cache; the executor must live-check GitHub.

### Test-only and regression-test PRs

A PR that adds only tests — no engine, parser, or frontend behavior change — must clear a **high value bar before it is worth merging.** The suite already carries extensive coverage, so a new test earns its place only when it meaningfully reduces risk that existing tests do not already cover. A test-only PR is not "safe because it only adds tests"; a redundant or non-discriminating test is net-negative — it slows the suite, dilutes intent, and adds maintenance cost for no signal.

**Start from skepticism about standalone test PRs.** In this repo, bug fixes almost always ship their own regression test in the *same* PR that fixes the bug. So a separate, later PR adding *another* test for an already-fixed issue is usually redundant by construction — the fixing PR already covered it. Before evaluating the test itself, find the PR that fixed the linked issue and check whether it already added a regression test (`gh pr list --search "<issue#> in:body"`, or read the closing commit); if coverage already exists, the new test is duplicative and does **not** clear the bar — record `value-bar` citing the existing test. A standalone test-only PR only makes sense when the coverage genuinely does not already exist: a bug fixed long ago with no test, or a still-uncovered edge of a mechanic. Conversely, if the linked issue is still **open** (the bug is unfixed), a test asserting the fixed behavior is premature — it will fail against current `main`; that is a block until the fix lands, not a merge. Weigh:

- **Does it guard a real, non-obvious regression?** A test pinning a defect that was actually fixed (a linked issue with a genuine bug) clears the bar. A test restating behavior that existing tests already exercise does not.
- **Is it discriminating?** It must fail on the pre-fix code or against a plausible wrong implementation. A test that passes regardless of the behavior it claims to cover is worthless. Verify the `mod` registration in `tests/integration/main.rs` for integration tests — an unregistered test compiles to nothing and shows green (inert false-green).
- **Is the coverage unique?** Grep for existing tests over the same card, mechanic, or issue before accepting. Duplicated coverage of an already-tested path is a reason to decline, not merge.
- **Is the blast radius justified?** Large test files that re-link the engine (see the `no_top_level_test_binaries` guard) must earn their compile cost.

**Evaluate every test-only PR independently.** A series of narrowly scoped test PRs earns no collective presumption of value: each PR must cite the specific existing coverage it does *not* duplicate, name the distinct regression it guards, and demonstrate a failure against the plausible pre-fix or wrong implementation. “This is another test for the same area” is not merge value.

Default to the **`value-bar`** signal — with a concrete "what already covers this" citation — when a test-only PR does not clear this bar, and do not enqueue it on green CI alone. Approve only when the added coverage is genuinely load-bearing. Apply the same discriminating-test standard whether tests ship alone or alongside a fix.

## Review Comment Format

Every review comment the loop posts — a single-mechanic review or a decomposed multi-mechanic pass — uses one structured, evidence-first format. Lead with the verdict, group findings by severity, anchor every finding to `file:line`, and cite evidence inline rather than paraphrasing it.

Structure:

1. **One-line verdict in bold** — the decision and its shape (e.g. "7 of 8 mechanics clean; 1 blocked on a rules defect — recommend splitting the blocked mechanic so the rest land now.").
2. **Findings grouped by severity, most severe first**, under clear headers:
   - `## 🔴 Blocker` — anything that must change before merge. State the defect, anchor it to `file:line`, and give the evidence: the **grep-verified CR number** and/or the **verbatim Oracle text or official ruling** (quoted, never paraphrased — a paraphrase is exactly how a fabricated clause survives review). Then point to the fix: name the existing building block to reuse or the general pattern to adopt, not just "this is wrong."
   - `## 🟡 Non-blocking` — concerns and design smells that can ride along or land as follow-ups; say why each does not block.
   - `## ✅ Clean` (or a praise section) — credit what is right, specifically (verified Oracle text, grep-verified CR annotations, discriminating registered tests). Contributors calibrate on what passed as much as on what failed.
3. **A concrete recommendation in bold** — the actionable next step: `request-changes` with the exact fix, `split` the blocked mechanic (and what to keep on the landing side), `approve on settled green`, and so on.

Reference example: [PR #5534 review](https://github.com/phase-rs/phase/pull/5534#pullrequestreview-4675737531).

Keep the tone specific and grounded: credit correct work, cite evidence for every claim, and avoid contrastive "not X, but Y" filler. The goal is an auditable decision — a maintainer skimming the comment should see the blocker, its evidence, and the fix without opening the diff.

## Authorized Mode

When the user explicitly authorizes maintainer actions, the loop may pass clean PRs to `pr-contribution-handler`. That skill owns assignee locks, checkout/worktree handling, fixups, formal approval, labels, update-branch, enqueue, dequeue, and live GraphQL verification.

When delegating labeling, require the handler to classify by the actual diff: ordinary additive engine, parser, or tooling capabilities are **enhancement** by default, even when they touch several files. Reserve **feature** for a genuinely broad mechanic or product change spanning distinct subsystems (for example, a combo workflow that jointly changes engine rules, priority handling, UI, and AI). Never infer `feature` from a `feat:` title, file count, or author identity.

Do not perform GitHub mutations from this skill except ordinary review/comment actions explicitly required by the current sweep and policy-configured deferral labels. Approval, queue, update-branch, dequeue, and merge execution still belongs to `pr-contribution-handler`.

## Drift Rule

`.agents/skills` is a symlink to `.claude/skills`, so `.claude/skills/pr-review-loop/SKILL.md` is the single physical copy for both Claude Code and Codex. Do not create a separate file under `.agents/`; if the symlink is ever replaced with a real directory, restore it rather than maintaining two copies.
