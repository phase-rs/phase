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
  - `review-events.jsonl` — canonical append-only local event log.
  - `review-state.sqlite` — derived query index/cache.
  - `review-summary.json` — generated token-minimal summary.
- **No hardcoded names.** Contributor standings, frontend exceptions, reviewer identities, private overrides, and one-off maintainer policy belong in local/private state, never in this skill.

## Commands

Use the CLI from the repo root:

```bash
python3 scripts/pr_review.py scan --repo phase-rs/phase --config .agents/pr-review-policy.toml
python3 scripts/pr_review.py inspect <PR> --repo phase-rs/phase --mode full
python3 scripts/pr_review.py recommend <PR> --repo phase-rs/phase
python3 scripts/pr_review.py record --event-json -
python3 scripts/pr_review.py compact
```

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
3. For each candidate:
   - `hard_stop` / `request_changes` — surface the precise blocker; do not enqueue.
   - `blocked` — current head already has blocking maintainer feedback; wait for a new head or author follow-up.
   - `defer` — record the deferral event; do not approve, label, enqueue, or merge.
   - `hold_ci` — record a non-terminal hold only when the packet is incomplete or an external condition prevents review. CI being pending, unknown, or red is not itself a review/enqueue blocker; merge-when-ready will wait for required checks.
   - `dequeue_stale_for_handler` / `update_branch_for_handler` / `approve_ready_for_handler` — advisory only; delegate execution to `pr-contribution-handler` in authorized mode.
   - `review` — fetch an `inspect --mode full` packet, then run `review-impl` against the current head and GitHub API/local diff evidence.
4. Record every material outcome with `record`; regenerate summaries with `compact` when useful.

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

Do not perform GitHub mutations from this skill except ordinary review/comment actions explicitly required by the current sweep.

## Drift Rule

`.agents/skills/pr-review-loop/SKILL.md` is the canonical Codex-facing copy. Keep `.claude/skills/pr-review-loop/SKILL.md` byte-for-byte synchronized.
