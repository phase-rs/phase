---
name: pr-review-loop
description: Use to run a continuous, hands-off review sweep over open contributor PRs in phase.rs — select unreviewed/updated PRs, dispatch an isolated agent to review each against the architecture/idiom/value lenses, post one verdict comment per PR, then poll for new PRs and real content commits until told to stop. Use when the user says "review PRs starting from N", "keep reviewing new PRs", "run the PR review loop", or asks to watch open PRs and leave a review on each. Read-only — it comments and never checks out or rewrites PRs (that is `pr-contribution-handler`). An opt-in authorized maintainer mode additionally delegates approve/label/enqueue to `pr-contribution-handler` when the runner genuinely has merge authority on the repo.
---

# PR Review Loop

Continuously review open contributor PRs and leave one verdict comment per PR, re-reviewing only when a PR's actual code changes, polling for new PRs on an interval until the user stops.

This skill is the **orchestration loop**. It does not contain review lenses — each per-PR review is performed by a spawned agent running the **`review-impl`** skill. Keep that boundary: review criteria live in `review-impl`; this file owns only *which* PRs get reviewed, *when*, and *how the sweep paces itself*.

## Relationship to sibling skills

- **`review-impl`** — the per-PR findings checklist (correct seam · idiomatic code at that seam · does it provide value, plus surface-specific lenses). The spawned reviewer runs this. Do not duplicate its lenses here.
- **`pr-contribution-handler`** — checks out and *fixes/enqueues* PRs end-to-end. The loop delegates to it in authorized mode; it is never reimplemented here.

**Two modes.** The sweep mechanics (candidate selection, dedup gate, pacing) are identical in both; they differ only in what happens after a verdict is reached:
- **Read-only review (default):** post exactly one verdict comment per PR and move on — never check out, edit, or enqueue. This is the rest of this file.
- **Authorized maintainer (opt-in — only when the runner genuinely has approve/enqueue authority on the repo):** after the verdict, additionally drive worthy PRs toward merge by delegating to `pr-contribution-handler` (approve / label / enqueue), and maintain the two persistent artifacts in *Persistent state (authorized mode)*. Everything the read-only mode does still applies; this is a superset.

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

**GitHub is the ledger.** Per-PR dedup is reconstructed each sweep from the acting login's own comment timestamps — in read-only mode there is no external state file. (Authorized mode adds two local cache/log files, see *Persistent state* below; they are an optional cache only, and GitHub still wins on any conflict.) This is durable and crash-idempotent: if the orchestrator dies after a comment posts, the next sweep sees the comment and dedups correctly. Two different people can run this loop without colliding, because each keys on *their own* login's comments.

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

*(Authorized mode only — zero-cost skip:* before the GitHub-derived gate below, consult the tracker (*Persistent state*). If its newest row for `$n` has `head_sha == current head` **and** a *terminal* verdict, skip without any further `gh` calls. On a non-terminal verdict, a mismatched/absent head, or any doubt, fall through to the GitHub-derived gate — GitHub wins.)

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

**Cheap pre-gate first — value & direction, before any deep-review spend (large/speculative feature PRs only).** A wrong-*direction* PR should cost a paragraph, not an agent. Before classifying a tier or dispatching a reviewer on a PR that introduces a **new subsystem / high file count / a feature with no prior maintainer buy-in**, answer two questions from the PR description + file list + ~30 seconds of judgment — no diff trace, no agent:
- **Direction fit:** is this a thing the project wants, or does it build a *parallel* path to infrastructure that already exists? Re-implementing an engine/subsystem the repo already owns (e.g. a second state-mutation layer beside `engine::apply`, a bespoke game-runner beside `phase-ai/duel_suite`) is wrong-by-construction regardless of code quality — a correctness review just confirms what the direction already disqualifies. If the direction is unwanted, **decline/close with a respectful redirect** to the wanted shape; do **not** spend a deep-review agent proving a bypass on a PR you'd decline anyway.
- **Visual evidence (heavily-frontend PRs):** a large frontend PR with **no screenshot or screen recording** does not proceed to deep FE review — request visual evidence first. Its absence is itself a weak signal the author didn't validate their own running build (the recurring failure mode: broken image loading / unpolished UX that only surfaces on hands-on checkout, which a screenshot would have pre-empted). Frontend value is visual; it must be shown, not just diffed.

Only PRs that clear this pre-gate (or aren't large speculative features) proceed to tier classification below. Record the pre-gate decline in the tracker/quality log like any other verdict.

**The bar is invariant; only the investigation depth scales.** Every PR, at every tier, must be validated against the same non-negotiable three lenses (owned by `review-impl` / `pr-contribution-handler`):
1. the change is at the **right architectural seam**;
2. the code is **idiomatic and follows repo patterns/standards AT that seam**;
3. the PR provides real **value** (and, for a fix, carries a discriminating test that fails on revert).

The tier does **not** change this bar — it changes **how much investigation you spend to become confident in it**. A small, isolated diff is *cheaper to be confident about*, not held to a lower standard. **If you cannot confidently clear all three lenses at a lower tier, that is not a pass — it is an escalation trigger.** Spend the least investigation that yields genuine confidence in the three lenses; never trade the bar for speed. Classify from the **diffstat + touched paths + contributor standing** (all cheap, no deep read):

- **Tier 0 — inline, no agent.** Trivial diffs (≲30 lines), test-only / doc-only, or a commit that exactly addresses a prior finding. You still confirm all three lenses — it's just fast on a tiny surface. Verify the hunks via the API diff, post a short verdict. (The trivial-fix-up shortcut from step 2.)
- **Tier 1 — light agent (e.g. sonnet, no worktree).** Small, single-surface, low-blast-radius changes (parser-only, one isolated card, a contained bugfix) from a contributor whose quality-log standing is `trusted`. The agent confirms the **same three lenses** against the read-only diff; "light" means **less machinery** (no worktree, no surface-specific deep-dives that don't apply to this diff) — **not a lower standard**. If the three lenses can't be confidently cleared from the diff alone, it escalates.
- **Tier 2 — full `review-impl`, opus agent in worktree isolation.** Required when the diff touches a **shared/hot seam** (targeting, casting/priority, layer system, combat, mana, stack), introduces **new engine machinery** (a new enum variant or public surface), is **large / multi-file**, is **AI/policy** (escalated rigor — decision-space/bail/cache-key/determinism), or comes from a contributor whose standing is `watch`/`probation` or who is **first-time** (no log entry). Runs the full `review-impl` lenses + an adversarial second pass.

**Route conservatively:** the default is Tier 2; drop to Tier 1/0 only when the diff is *demonstrably* small, isolated, and from a trusted contributor. Any whiff of a shared seam, new machinery, or value-uncertainty escalates. The classifier exists to avoid wasting *deep investigation* on trivia — never to skip *scrutiny* on risky code, and never to let a PR through without actually confirming seam, idiom, and value.

The dispatched reviewer (Tier 1 cheap agent, or Tier 2 opus in worktree isolation):

1. Fetches the ground-truth diff via the **GitHub API**, never `gh pr diff` (rtk corrupts it into fabricated content):
   ```bash
   gh api "repos/$REPO/pulls/$n.diff" -H "Accept: application/vnd.github.v3.diff"
   ```
2. Tier 2: runs the **`review-impl`** skill (three lenses + surface-specific lenses). Tier 1: applies just the three core lenses inline.
3. Applies the orchestration discipline below.
4. Posts **exactly one** comment via `gh pr comment "$n" --repo "$REPO" --body ...` containing an explicit verdict line, e.g. `VERDICT: approve` / `VERDICT: request-changes` / `VERDICT: approve with comments`. Use a plain comment, **not** a formal `gh pr review --approve/--request-changes` — a non-maintainer bot identity stacking formal review states is noisy and can interfere with required-review/merge-queue gates. (Override only if the user asks for formal reviews, e.g. authorized maintainer mode.)

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

## Authorized maintainer mode (opt-in)

Only when the runner has real approve/enqueue authority. The sweep, dedup, and tiered review above are unchanged; this adds what happens *after* the verdict:

- **`VERDICT: approve` and it clears the bar** → hand the PR to **`pr-contribution-handler`** to bring-current / fix-narrow / verify, then **repush-guard → approve → label → enqueue**:
  - **Repush guard (mandatory):** capture `EXPECT=headRefOid`, add `@me` as assignee, re-read the head, **ABORT if it moved**. A sticky `reviewDecision=APPROVED` survives a contributor force-push, so the only freshness signal is current head vs the head you reviewed.
  - **Label** one of the repo's valid labels; **enqueue** (squash-only: `gh pr merge <N> --auto --squash` — the response `! The merge strategy for main is set by the merge queue` is the EXPECTED success message, not an error); confirm `isInMergeQueue=true`. `mergeStateStatus=BLOCKED` is the *normal* resting state of a queued PR and `autoMergeRequest` reads `null` even when queued — queue membership is the affirmative signal.
  - **Serial-merge treadmill:** every PR that merges advances `main`, flipping every armed-but-not-`CLEAN` PR behind it to `BEHIND`, and a `BEHIND` armed PR does **not** auto-re-enter the queue. Bring it current with `gh api repos/$REPO/pulls/<N>/update-branch -X PUT`; CI re-greens and the armed auto-merge re-enters it. Expect to repeat this on the trailing PRs until the batch drains.
- **`VERDICT: request-changes`** → post the precise blocker (and the exact fix when it's a one-liner); record it. Narrow mechanical fixes (shared-`main.rs` test-registration conflicts, a one-line lint) are fine to land yourself; **wrong-seam / overbroad / unpolished is a BLOCK** — hand back.

CI green is necessary, not sufficient: `cancelled` ≠ `failed` (read the latest `completed/success`); a `BEHIND` PR's red can be a stale-base artifact main already fixed (verify the named test against a completed `origin/main` run); a test asserting *wrong* behavior is a false green.

## Persistent state (authorized mode only): tracker + quality log

Read-only mode keeps no state — GitHub is the ledger. Authorized mode adds two local files (outside the repo, never committed):

**PR tracker — in-flight state, pruned on merge.** Append-only TSV, newest row per PR authoritative; an *optional cache* over GitHub (GitHub wins on conflict) for enqueue bookkeeping + a zero-cost skip. Columns: `timestamp  pr  author  tier  verdict  label  enqueued  head_sha  notes`. `head_sha` = the head you assessed (high-water mark). **Verdict vocabulary** — *terminal* (skip at same head): `ENQUEUED · MERGED · CLOSED · CHANGES_REQUESTED · STILL-BLOCKED-same · SUPERSEDE-pending-close · DEFER-FE` (the last = a frontend PR left to the maintainer's own domain; skip at same head, no action); *non-terminal* (re-surface next pass even at same head): `…-pending-CI · …-needs-review · PARTIALLY-RESOLVED-judgment · CONFLICT-RESOLVED-awaiting-CI`. Append (never edit); record current head each time; `STILL-BLOCKED-same` for a new-but-non-fixing head; **prune all rows for a PR when it merges** (awk on the first `^[0-9]{4}$` field).

**Contributor-quality log — lifetime signal, never pruned, keyed BY CONTRIBUTOR.** A separate file precisely because the tracker is pruned on merge and this is *not reconstructible from GitHub* once PRs merge. The shape is chosen so a single lookup sets a PR's review tier — **one section per login**, not a flat chronological log (which would force scanning the whole file to assess one contributor):

```markdown
### <login> — standing: trusted | watch | probation   (updated <YYYY-MM-DD>)
<one-line rolling assessment: current trend + why>
signals: <running tally, e.g. clippy-not-run ×3 · ast-shape-only ×2>
- <YYYY-MM-DD> #<pr> — <observation + concrete evidence>
- <YYYY-MM-DD> #<pr> — <…>   (positives recorded too — the point is the trend)
```

- **`standing` is the actionable field and the ONE mutable line** — update it in place each time you reassess; everything below it is **append-only** (never edit or delete a past observation). Standing is the file's contract with the loop:
  - `trusted` → eligible for **Tier 0/1** review.
  - `watch` / `probation` → **Tier 2 minimum**, regardless of diff size.
  - no section yet (**first-time contributor**) → **Tier 2** by default; create the section after the first review.
- **Signal vocabulary** (one is a yellow flag; a recurring pattern moves standing toward `watch`/`probation`): `fmt-not-run` / `clippy-not-run` (lint red on submit); `ast-shape-only` (tests assert AST shape, no runtime `apply()` discrimination); **`tests-protect-wrong-behavior`** (after a rules flag, author adds tests asserting the *violating* behavior — a false green, the most insidious); `parsed-but-not-consumed` (new AST field no handler reads); `allow-noncombinator-escape-hatch` (substring dispatch on new parser surface); `revert-churn` (revert-of-revert instead of a clean rebase; net diff deletes others' merged work); `rebase-not-fix` (new head is only `Merge branch main`). Record **positives** (clean enum parameterization, genuine fail-on-revert tests, single-round responsiveness) — standing moves both directions.
- **Escalate the stance, not just the standing:** when defects recur, the hand-back should become directive ("this needs the full `engine-implementer` cycle, not another patch") rather than another round of the same nit.

This closes the loop: the quality log feeds the tier classifier (`standing` → tier floor), and each review's outcome feeds the quality log (signals + standing update). The bar (three lenses) is unchanged for everyone — standing only decides *how much investigation* a contributor's next PR warrants by default.

## Tooling gotchas

- `gh pr diff` is rtk-corrupted — always fetch diffs via `gh api .../pulls/N.diff`.
- jq "Invalid string: control characters" → query each field with its own `gh pr view --json X --jq` call; never pipe a combined multi-field blob through a shell var.
- `gh pr view --jq` rejects extra `--arg`; put a value in a shell var and string-interpolate it into the filter.
- Reviewers run in worktree isolation and only ever *read* the PR + *comment* — they must never touch the dirty main working tree or other agents' worktrees.
