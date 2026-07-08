---
name: pr-review-loop
description: Use to run a continuous review sweep over open contributor PRs in phase.rs. The skill is a thin orchestration layer over scripts/pr_review.py: discover candidates, detect stale reviews/follow-ups, dispatch review-impl for PRs that need judgment, and delegate authorized merge handling to pr-contribution-handler.
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

1. Resolve the acting identity from GitHub. Do not review PRs authored by the acting login.
2. Run `scan`. Use `action_counts` / `candidates_by_action` for routing; do not infer legacy bucket names. Treat its result as a triage packet, not a final approval gate.
3. Every packet (and `recommend` output) carries an advisory `contributor` block — standing, scrutiny, `scrutiny_reasons`, `recurrence`, `first_contribution` — derived from the local event log plus `contributor_standing` overrides; it is `null` only when the PR has no author login. Scale review depth by it: `first_contribution` → full evidence bar, and point the author at the `docs/AI-CONTRIBUTOR.md` gates in the first review comment; `elevated` → dig specifically into the recurring signals named in `scrutiny_reasons`; `maintainer_attention` → include the contributor in the sweep report for the maintainer. `light_touch_eligible` permits a lighter pass only while scrutiny is `normal`.
4. Every packet also carries a `proof` block. Treat `proof.proof_gap == true` as a queue-safety blocker. Missing AI-contributor template sections and unchecked/manual verification are context, not blockers by themselves. Skipped/delegated verification, all commits coauthored by an agent account, elevated contributor scrutiny, or `gittensor-closed-heavy` raise the proof bar. Use discretion: explicit checked test evidence can be sufficient for behavior that cannot be meaningfully proven by manual network interaction, but a high-risk PR with no concrete proof must not be passed to the handler for enqueue.
5. For each candidate:
   - `hard_stop` / `request_changes` — surface the precise blocker; do not enqueue.
   - `skip` — disambiguate by `reason`: `closed` / `self_authored` need no action; `contributor_standing_skip` is an explicit maintainer standing override — record the skip and move on without reviewing. A skip-listed contributor touching hard-stop paths still surfaces as `request_changes` (safety outranks the skip).
   - `blocked` — current head already has blocking maintainer feedback. Read the blocking feedback before deciding to wait. A formal `CHANGES_REQUESTED` state is not by itself a reason to keep waiting: if later maintainer feedback on the same head says the blocker is resolved, no unresolved finding remains, or the PR is otherwise clean-but-stuck because the formal review state was not cleared, delegate the PR to `pr-contribution-handler` in authorized mode to live-check, approve, label, and enqueue. If the only remaining blockers are maintainer-fixup sized, delegate the PR to `pr-contribution-handler` in authorized mode instead of making the contributor do another round-trip. Maintainer-fixup sized means small, local, low-risk corrections that do not change the accepted design or require new product/rules judgment: replacing/removing an incorrect CR citation while preserving the already-reviewed logic, resolving a small merge conflict where the target logic already exists on one side, stripping accidental generated/noise hunks, fixing a single failing regression caused by main drift when the accepted design is unchanged, or threading an obviously missing renamed helper/import through the existing implementation. Do not use this path when there is any unresolved substantive behavior, architecture, proof-gap, test-discrimination, parse-diff, security, or hard-stop concern; keep the PR blocked until a new head or author follow-up. If the contributor remains inactive and the blockers are not maintainer-fixup sized, follow the requested-changes expiry actions below instead of leaving the PR blocked indefinitely.
   - `defer` — record the deferral event; do not approve, enqueue, or merge. If the recommendation carries `label_to_apply`, add that label to the PR for maintainer filtering before moving on. Label names must come from repo policy, not from this skill.
   - `hold_ci` — record a non-terminal hold only when the packet is incomplete or an external condition prevents review. CI being pending, unknown, or red is not itself a review/enqueue blocker; merge-when-ready will wait for required checks.
   - `queued` — auto-merge is already enabled or the PR is already in the queue. Treat this as no action only while required checks are pending or green. If any required check is terminal red, the PR is not across the finish line: delegate to `pr-contribution-handler` in authorized mode to inspect the failing check, apply a maintainer-fixup-sized repair when appropriate, re-approve/re-enable auto-merge if a push disabled it, or report a real blocker. Do not leave approved-but-red PRs to sit merely because they were previously enqueued.
   - `dequeue_stale_for_handler` / `update_branch_for_handler` / `approve_ready_for_handler` / `warn_stale_changes_for_handler` / `close_stale_changes_for_handler` — advisory only; delegate execution to `pr-contribution-handler` in authorized mode.
   - `review` — fetch an `inspect --mode full` packet, then run `review-impl` against the current head and GitHub API/local diff evidence. For engine/parser-surface PRs, the parse-diff sticky comment (`<!-- coverage-parse-diff -->`) is REQUIRED review evidence: fetch its full body and confront the card-level diff against the PR's claimed scope. The packet's `parse_diff` field carries presence/state/`updated_at`. If state is `baseline_pending` on a stale branch (the `review_parse_baseline_pending` reason), route to update-branch first — that is the one staleness case where updating is the remedy, since the CI diff is merge-base-pinned and immune to branch staleness (see `ci.yml` "Parse-detail diff vs base baseline" step). If the comment is absent but engine source changed, treat it as missing evidence: check whether CI ran for the current head before reviewing. If the review finds only a couple small, local, low-risk fixes between the PR and mergeability, do those maintainer fixups through `pr-contribution-handler` instead of requesting another contributor round-trip; use the same maintainer-fixup boundary as the `blocked` route above. If an additive PR is elegant, low-churn, and demonstrably well executed, tell the handler to apply the policy-configured `quality` label in addition to the normal type label.
6. Record every material outcome with `record`. Attach `signals` (closed vocabulary, validated at record time) to the outcome event for observations from THIS review only, never re-recorded history. The vocabulary has two halves: defect signals (feed score penalties, windowed recurrence, and scrutiny) and praise signals (`right-seam`, `scope-discipline`, `discriminating-runtime-test`, `parameterized-not-proliferated`, `evidence-backed-pushback` — feed a capped score credit only, never recurrence or scrutiny). Never invent tokens: an out-of-vocabulary signal is rejected at record time, and if a needed concept is missing the fix is a vocabulary addition in `pr_review.py`, not a `--force`. Regenerate summaries with `compact` when useful.

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

## Authorized Mode

When the user explicitly authorizes maintainer actions, the loop may pass clean PRs to `pr-contribution-handler`. That skill owns assignee locks, checkout/worktree handling, fixups, formal approval, labels, update-branch, enqueue, dequeue, and live GraphQL verification.

Do not perform GitHub mutations from this skill except ordinary review/comment actions explicitly required by the current sweep and policy-configured deferral labels. Approval, queue, update-branch, dequeue, and merge execution still belongs to `pr-contribution-handler`.

## Drift Rule

`.agents/skills` is a symlink to `.claude/skills`, so `.claude/skills/pr-review-loop/SKILL.md` is the single physical copy for both Claude Code and Codex. Do not create a separate file under `.agents/`; if the symlink is ever replaced with a real directory, restore it rather than maintaining two copies.
