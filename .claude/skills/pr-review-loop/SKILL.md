---
name: pr-review-loop
description: Use to run a continuous, hands-off review sweep over open contributor PRs in phase.rs — select unreviewed/updated PRs, dispatch an isolated agent to review each against the architecture/idiom/value lenses, post one verdict comment per PR, then poll for new PRs and real content commits until told to stop. Use when the user says "review PRs starting from N", "keep reviewing new PRs", "run the PR review loop", or asks to watch open PRs and leave a review on each. Read-only — it comments, it never checks out or rewrites PRs (that is `pr-contribution-handler`).
---

# PR Review Loop

Continuously review open contributor PRs and leave one verdict comment per PR, re-reviewing only when a PR's actual code changes, polling for new PRs on an interval until the user stops.

This skill is the **orchestration loop**. It does not contain review lenses — each per-PR review is performed by a spawned agent running the **`review-impl`** skill. Keep that boundary: review criteria live in `review-impl`; this file owns only *which* PRs get reviewed, *when*, and *how the sweep paces itself*.

## Relationship to sibling skills

- **`review-impl`** — the per-PR findings checklist (correct seam · idiomatic code at that seam · does it provide value, plus surface-specific lenses). The spawned reviewer runs this. Do not duplicate its lenses here.
- **`pr-contribution-handler`** — checks out and *fixes* PRs end-to-end. This loop is strictly read-only: it posts a comment and moves on. It never checks out, edits, or enqueues a PR.

## Arguments

| Arg | Meaning | Default |
|-----|---------|---------|
| `floor` | Lowest PR number to consider | lowest open PR |
| `interval` | Poll wait when caught up | 15 minutes |
| `defer_to` | Reviewer logins to defer to — skip any PR already carrying their comment/review | empty |

Resolve once per invocation, then reuse:

```bash
ACTING_LOGIN=$(gh api user --jq '.login')          # runner identity — NEVER hardcode a name
REPO=$(gh repo view --json nameWithOwner --jq '.nameWithOwner')   # phase.rs repo, derived not literal
```

This skill is phase.rs-bound (it assumes Comprehensive Rules, the `review-impl` lenses, and that rtk corrupts `gh pr diff`). Do not point it at an arbitrary repo.

## Source of truth

**GitHub is the ledger.** Per-PR dedup is reconstructed each sweep from the acting login's own comment timestamps — there is no external state file. This is durable and crash-idempotent: if the orchestrator dies after a comment posts, the next sweep sees the comment and dedups correctly. Two different people can run this loop without colliding, because each keys on *their own* login's comments.

A running tally carried in the wakeup prompt is an *optional cache* to skip re-deriving timestamps. It is never authoritative — when cache and GitHub disagree, GitHub wins.

## One sweep

### 1. Select candidates

List open PRs `>= floor`, ascending, excluding **(a)** any authored by `ACTING_LOGIN` (don't review your own work) — fold the author filter into the `jq` so the emitted number list is already clean:

```bash
gh pr list --repo "$REPO" --state open --limit 100 \
  --json number,author \
  --jq ".[] | select(.number >= $floor and .author.login != \"$ACTING_LOGIN\") | .number" | sort -n
```

Then exclude **(b)** any PR already carrying a comment or review by a login in `defer_to` — defer to that reviewer rather than piling on. Per candidate `$n`:

```bash
skip=""
for who in $defer_to; do
  c=$(gh pr view "$n" --repo "$REPO" --json comments --jq "[.comments[] | select(.author.login==\"$who\")] | length")
  r=$(gh pr view "$n" --repo "$REPO" --json reviews  --jq "[.reviews[]  | select(.author.login==\"$who\")] | length")
  { [ "$c" != "0" ] || [ "$r" != "0" ]; } && { skip="$who"; break; }
done
[ -n "$skip" ] && continue   # a defer_to reviewer is already engaged
```

### 2. Per-PR dedup gate — the loop's efficiency core

For each surviving candidate, decide review / re-review / skip. Query each field with its **own** `gh pr view --json X --jq` call — combining fields into one blob and piping through a shell var triggers jq control-char parse errors.

The "ledger" is *all* of the acting login's prior activity on the PR — a plain comment (what this loop posts) **or** a formal review (a human runner may have left one). Take the max timestamp across both; one extra cheap call avoids redundantly re-reviewing a PR whose only prior verdict was a formal review:

```bash
lc=$(gh pr view "$n" --repo "$REPO" --json comments \
  --jq "[.comments[] | select(.author.login==\"$ACTING_LOGIN\") | .createdAt] | max // empty")
lr=$(gh pr view "$n" --repo "$REPO" --json reviews \
  --jq "[.reviews[]  | select(.author.login==\"$ACTING_LOGIN\") | .submittedAt] | max // empty")
last=$(printf '%s\n%s\n' "$lc" "$lr" | grep -v '^$' | sort | tail -n1)   # ISO-8601 sorts lexically
```

- **No prior activity (`last` empty)** → first review. Go to step 3.
- **Prior activity exists** → check for an *actual code commit* after it. An actual code commit is a **non-merge** commit (a merge/rebase-from-main commit has ≥2 parents and does not change the PR's own content):

```bash
gh api "repos/$REPO/pulls/$n/commits" --paginate \
  --jq ".[] | select(.commit.committer.date > \"$last\") | select((.parents|length)==1) | .sha"
```

  - **No actual code commit after `last`** → **skip. Do not post a comment.** Prior verdict stands. (Merge-from-main and other rebase noise advance the tip's date without changing content — they are not a reason to re-review.)
  - **One or more actual code commits after `last`** → re-review (step 3, re-review protocol).

**Trivial fix-up shortcut:** if a re-review is triggered on an already-approved PR by a small commit that exactly addresses a prior finding, verify the hunks yourself via the API diff (below) and post a short confirmation instead of spawning an agent.

> Commit messages describe intermediate states, not the net diff, and a PR title may not match its diff. When in doubt, diff the head tree against `origin/main` rather than trusting messages.

### 3. Dispatch a reviewer

For each PR genuinely needing review, spawn **one opus general-purpose agent in worktree isolation** that:

1. Fetches the ground-truth diff via the **GitHub API**, never `gh pr diff` (rtk corrupts it into fabricated content):
   ```bash
   gh api "repos/$REPO/pulls/$n.diff" -H "Accept: application/vnd.github.v3.diff"
   ```
2. Runs the **`review-impl`** skill against the diff (the three lenses + surface-specific lenses live there).
3. Applies the orchestration discipline below.
4. Posts **exactly one** comment via `gh pr comment "$n" --repo "$REPO" --body ...` containing an explicit verdict line, e.g. `VERDICT: approve` / `VERDICT: request-changes` / `VERDICT: approve with comments`. Use a plain comment, **not** a formal `gh pr review --approve/--request-changes` — a non-maintainer bot identity stacking formal review states is noisy and can interfere with required-review/merge-queue gates. (Override only if the user asks for formal reviews.)

Bound concurrency: on a large first-run backlog, dispatch sequentially or in a small parallel batch rather than spawning an agent per PR all at once.

### Orchestration discipline (every review, first or re-)

These are loop-level checks that sit *around* the `review-impl` lenses — stated as principles, applied to whatever PR is in hand:

- **Already fixed on `origin/main`?** A branch predating a just-landed fix will duplicate it. Recommend rebase + drop the dup — but keep any *superior test* the PR adds (resolve a duplicate into net coverage gain rather than a flat reject).
- **PR content vs dirty-tree drift.** Diff against `origin/main` to separate the PR's real changes from concurrent working-tree noise. Watch for corrupted generated files, accidental binaries, submodule gitlink artifacts (mode 160000), and CI-unsafe hunks (e.g. a hardcoded frozen-allowlist count that matches neither base nor concurrent tree).
- **Fix vs detector-suppression.** A "coverage exemption" commit is a genuine fix only if the supported-count goes *up*; if supported stays down while the swallow count drops, it's suppression, not a fix.
- **Reachability + discriminating test.** A fix must be reachable in production, and its test must actually exercise the failure path — not an empty, doc-only, or pin-only test, and not a fixture so degenerate it takes a different internal branch than real input.
- **Added behavior must not over-fire at a shared sink.** A fix that adds an effect at a sink shared by other paths can mis-fire for unrelated cards — verify the trigger condition is scoped correctly.

### Re-review protocol (when an actual code commit landed after my last comment)

1. Read the prior comment; mark each prior finding **ADDRESSED / PARTIAL / NOT**.
2. **Re-examine whether the prior finding was itself correct.** Trace the *actual* parser/AST or code path on base and HEAD — do not re-reason from a static read of the dispatch order. A prior "this drops cards" finding can be a false positive that only a real trace refutes.
3. When a test was rebased off the fix it originally shipped with (fix landed separately, PR is now test-only), **run the test against current `origin/main`** to confirm it is green on the landed seam alone — and keep it if it covers a subtlety the landed fix's own test missed.

### 4. Pace or stop

- **Caught up** (every candidate reviewed or skipped) → schedule the next sweep after `interval`, carrying this skill's loop prompt forward (optionally with the non-authoritative tally cache). Use the interval arg; never a literal duration.
- **User says stop / pause** → end the loop by **omitting** the next wakeup. Do not schedule a placeholder tick. (`resume` re-invokes the skill.)

## Tooling gotchas

- `gh pr diff` is rtk-corrupted — always fetch diffs via `gh api .../pulls/N.diff`.
- jq "Invalid string: control characters" → query each field with its own `gh pr view --json X --jq` call; never pipe a combined multi-field blob through a shell var.
- `gh pr view --jq` rejects extra `--arg`; put a value in a shell var and string-interpolate it into the filter.
- Reviewers run in worktree isolation and only ever *read* the PR + *comment* — they must never touch the dirty main working tree or other agents' worktrees.
