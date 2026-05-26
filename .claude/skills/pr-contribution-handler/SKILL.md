---
name: pr-contribution-handler
description: Use when asked to handle, harden, or shepherd one or more external contributor PRs. Checks out PRs in a worktree or main workspace, updates them against origin/main, resolves review comments, performs architecture-focused implementation review, decides whether fixes are inline or require engine-implementer review cycles, and usually closes explicitly deferred follow-up work while already in the PR.
---

# PR Contribution Handler

Use this skill when the user provides a GitHub PR number, URL, branch, or list of PRs and asks to handle contributor work end-to-end.

The goal is not just "make CI green." The goal is to leave the PR in the most idiomatic, maintainable, rules-correct shape reasonable for its scope.

## Required Source Workflows

Before changing code, read these files from the repo root and apply their logic:

- `$review-impl` for the implementation-gap review lenses.
- `.claude/agents/pr-review-comment-resolver.md` for phase.rs-specific review-comment fetching, categorization, prioritization, resolution, verification, and reporting.
- `.agents/skills/engine-implementer/SKILL.md` when the PR needs the full engine implementation plan/review cycle.

Do not paraphrase these from memory. Re-read them each time because they are the source of truth.

## Intake

1. Parse the PR number(s), URL(s), or branch name(s).
2. If the user did not specify where to work, ask one concise question: "Use a separate git worktree, or the current main workspace?" Recommend a worktree.
3. If multiple PRs are provided, process them sequentially unless the user explicitly asks for parallel work and the PRs have independent worktrees.
4. Capture the initial state:
   - `git status --short`
   - `gh pr view <PR> --json number,title,state,author,headRefName,headRepository,baseRefName,isCrossRepository,mergeStateStatus,reviewDecision,url`
   - `gh pr checks <PR>` if available

## Checkout

Prefer a worktree for contributor PRs. If using the main workspace, first verify the current changes are intentional and do not overwrite or stash them.

Worktree pattern:

```bash
git fetch origin main
git fetch origin pull/<PR>/head:pr/<PR>   # only when pr/<PR> does not already exist
git worktree add ../forge.rs-pr-<PR> pr/<PR>
cd ../forge.rs-pr-<PR>
```

If the local `pr/<PR>` branch already exists, inspect it before updating or checking it out. Do not force-reset it if it contains local work. Use a fresh branch name such as `pr/<PR>-review-<date>` when needed.

Main workspace pattern:

```bash
git fetch origin main
gh pr checkout <PR>
```

## Bring Current With `origin/main`

Fetch first, then ensure `origin/main` is an ancestor of the PR HEAD.

```bash
git fetch origin main
git merge-base --is-ancestor origin/main HEAD
```

If the check fails, merge `origin/main` into the PR branch unless the user explicitly requested a rebase:

```bash
git merge --no-edit origin/main
```

Resolve conflicts in the same architectural style as the surrounding code. Do not discard contributor changes. If conflicts reveal that the PR's approach is obsolete, finish the merge only after deciding whether the right resolution is an inline fix or a full implementation cycle.

## Review Comment Resolution

Apply `.claude/agents/pr-review-comment-resolver.md` directly:

1. Fetch PR reviews, issue comments, and inline review comments with `gh`.
2. Skip resolved or non-actionable comments.
3. Categorize actionable comments into tests, linting, functionality, style, and security.
4. Prioritize critical and high-impact comments first.
5. Fix by category with focused commits when possible.
6. Verify that each original comment is actually addressed, not merely made stale by line movement.

When a comment asks for a questionable design, satisfy the underlying concern while preserving this repo's architecture. If reviewer feedback conflicts with rules-correct engine behavior, document the conflict in the final report and implement the rules-correct path.

## Architecture Review

After comment resolution, run `$review-impl` against the PR diff.

Use this diff basis:

```bash
git diff --stat origin/main...HEAD
git diff origin/main...HEAD
```

Ask, explicitly: "Is this PR implemented in the most architecturally idiomatic manner possible for this repository?"

Apply the relevant lenses from `review-impl.md`, especially:

- class of cases vs one-off special case
- sibling coverage
- building-block reuse
- test adequacy
- parser combinator correctness
- engine/frontend boundary purity
- CR annotation correctness
- hidden-information filtering and adapter round trips
- AI classifier completeness, when relevant

## Inline Fix vs Full Engine Cycle

Make inline changes when the fix is local, well-understood, and does not require a new architectural plan. Typical inline fixes:

- small parser phrase coverage within an existing parser family
- missing tests for an already-correct implementation
- straightforward use of an existing helper
- local bug fix in one resolver, component, or adapter
- cleanup of a reviewer-requested nit that does not alter design

Use `$engine-implementer` and the full plan -> implement -> review cycle when the PR needs architectural redesign or new engine primitives. Typical triggers:

- new or changed `Effect`, `Keyword`, `TriggerCondition`, `ReplacementCondition`, `TargetFilter`, `QuantityRef`, or similar engine enum surface
- parser work that introduces a new grammar family or risks one-off Oracle matching
- CR behavior is uncertain or affects a core rule pipeline
- replacement, targeting, zone-change, SBA, layer, or cost-resolution behavior changes
- changes span engine + parser + AI + frontend/transport wiring
- the current PR shape solves one card/screen/case but should become a reusable building block
- fixing the PR safely requires a reviewed implementation plan rather than direct patching

If the full cycle is required but unavailable in the current environment, stop after writing the review findings and tell the user exactly why inline fixing would be risky.

## Explicit Deferrals

Search the PR body, comments, commits, and diff for deferrals:

- "TODO"
- "follow-up"
- "defer"
- "later"
- "not in this PR"
- "future work"
- "out of scope"

Default stance: do the deferred work while already in the PR.

Only leave a deferral when it is a significant hurdle, meaning at least one of these is true:

- it is materially larger than the PR itself
- it requires product/design input not present in the PR
- it needs a new architecture or full engine-implementer cycle separate from the PR's main change
- it crosses unrelated subsystems with high regression risk
- it cannot be verified in the current environment
- it depends on external access, data, or a different contributor's unresolved work

If leaving a deferral, make it explicit in the final report with evidence and a concrete follow-up recommendation. Do not accept vague "later" notes for work that can be finished now.

## Verification

Run formatting directly:

```bash
cargo fmt --all
```

For Rust/engine/parser changes, prefer Tilt and fall back only when Tilt is not running:

```bash
if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data
else
  cargo clippy --all-targets -- -D warnings
  cargo test -p engine
  ./scripts/gen-card-data.sh
fi
```

For frontend changes:

```bash
if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 180 check-frontend
else
  (cd client && pnpm run type-check && pnpm lint)
fi
```

For parser/card-data behavior, add focused parser tests and inspect generated card data for representative affected cards. Use one-shot audit commands such as `cargo coverage`, `cargo parser-gaps`, or `cargo semantic-audit` only when the PR's risk justifies them.

If Tilt reports an unrelated error, wait and re-check before touching it. Preserve other agents' work.

## Commit And Push

Create atomic commits for changes you make. Stage only files relevant to the PR handling work.

Suggested commit shapes:

- `fix(PR-<PR>): address review comments`
- `fix(PR-<PR>): harden implementation architecture`
- `test(PR-<PR>): cover deferred follow-up`

Do not push unless the user requested pushing or the invocation explicitly says to update the PR branch. If push access is unavailable, report the local commits and branch.

## Final Report

For each PR, report:

- checkout location and whether it was a worktree or main workspace
- update status against `origin/main`
- review comments resolved and any left manual
- architecture-review findings from `review-impl.md`
- inline fixes made vs full-cycle work invoked or recommended
- deferred items completed vs left open
- verification commands and results
- commits created and push status

Include evidence for claims, mark assumptions separately, and state confidence. Also include a short self-challenge: what evidence would contradict the conclusion that the PR is ready?
