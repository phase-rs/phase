---
name: pr-polish
description: Alternate `review-impl` and `pr-contribution-handler` on a PR until it is truly enqueue-ready — no new review findings, zero unresolved inline threads, all CI checks green, and two consecutive quiet polls after CI settles. Use when the user wants a PR polished to merge-queue-ready without setting a fixed number of rounds. Phrases like "polish this PR", "keep reviewing until it's mergeable", "loop until done".
---

# PR Polish

**Goal.** Drive a PR to enqueue-ready by alternating self-review and address rounds until **all** of the following hold:

1. The most recent `review-impl` round produces **zero new findings** (no new inline comments, no new top-level reviews with a non-empty body).
2. Every inline review thread reachable via GraphQL reports `isResolved: true`.
3. Every non-bot, non-author top-level review has been acknowledged (replied-to) OR resolved via a thread it spawned.
4. Every non-bot, non-author issue comment has been acknowledged (replied-to).
5. Every CI check is `bucket: pass` or `skipping` — none `fail` or still pending.
6. **Two consecutive post-CI polls** (~60s apart) stay clean — no new threads, no new non-empty reviews, no new issue comments. Bots (CodeRabbit, Sentry, anything that posts late) frequently arrive after CI settles; a single green snapshot is not sufficient.

**Do not stop at a fixed number of rounds.** If round N introduces new comments, round N+1 is required. Cap at `MAX_ROUNDS = 10` as a safety valve, but expect 2–5 in practice.

## When to use

- "Polish PR #N" / "polish this PR" / "keep reviewing and addressing until it's mergeable" / "loop /review + /address until done" / "make sure the PR is actually enqueue-ready"

## Prerequisites

- GitHub CLI 2.46.0 or newer. This skill depends on `gh pr checks --json`;
  older `gh` versions do not support that flag.

## When NOT to use

- User wants just one review pass → `review-impl`.
- User wants to address already-posted comments without further self-review → `pr-contribution-handler`.
- A fixed round count is requested ("do 3 rounds") → honour the count instead of converging.
- PR is from an external contributor and hasn't been triaged yet → run `pr-contribution-handler` first (security pre-check + architecture review), then `pr-polish` if convergence is still needed.
- Engine/parser-only PR with no review comments — there's nothing to converge on; `pr-contribution-handler` alone is sufficient.

## Relation to other skills

| Skill | Role here |
|-------|-----------|
| `review-impl` | The inner review step. Produces findings (`[HIGH]/[MED]/[LOW]`) or posts them as `🔴/🟠/🟡/🔵` inline PR comments. |
| `pr-contribution-handler` | The inner address step. Resolves comments, runs Architecture Review, formats, commits, pushes. Has its own babysit polling loop for CI + late comments. |
| `pr-polish` (this skill) | The **outer loop** that alternates the two until convergence. |

`pr-polish` is the outermost layer. It calls the other two via the `Skill()` tool and verifies convergence against GitHub state directly — never trusting a child skill's summary.

## TodoWrite

Before starting, write two todos so the user can see the loop progression:

- `Round {current}: review-impl + pr-contribution-handler on PR #{N}` — current iteration.
- `Final polish polling: 2 consecutive clean polls, CI green, 0 unresolved` — runs after the last non-empty review round.

Update the `current` round counter at the start of each iteration; mark `completed` only when the round's address step finishes (all new threads addressed + resolved).

## Find the PR

```bash
ARG_PR="${ARG:-}"
# Normalize URL → numeric ID if the arg is a pull-request URL.
if [[ "$ARG_PR" =~ ^https?://github\.com/[^/]+/[^/]+/pull/([0-9]+) ]]; then
  ARG_PR="${BASH_REMATCH[1]}"
fi
PR="${ARG_PR:-$(gh pr list --head "$(git branch --show-current)" --repo phase-rs/phase --json number --jq '.[0].number')}"
if [ -z "$PR" ] || [ "$PR" = "null" ]; then
  echo "No PR found for current branch. Provide a PR number or URL as the skill arg."
  exit 1
fi
echo "Polishing PR #$PR"
```

## The outer loop

```text
round = 0
while round < MAX_ROUNDS:
    round += 1
    baseline = snapshot_state(PR)     # see "Snapshotting state" below

    Skill(skill="review-impl", args="--post-inline " + PR_URL)
    # The skill posts new findings as inline 🔴/🟠/🟡/🔵 PR comments.

    findings = diff_state(PR, baseline)
    if findings.total == 0:
        break  # no new findings → go to polish polling

    Skill(skill="pr-contribution-handler", args=PR_URL)
    # Handler resolves comments + runs babysit polling (CI green + thread quiet)
    # before returning. It does NOT enqueue — pr-polish owns enqueue authority.

# Post-loop: polish polling (see below).
polish_polling(PR)
```

### Snapshotting state

Before each `review-impl`, capture a baseline so the diff after the review reflects **only** what the review just added (not pre-existing threads):

```bash
# Inline threads — total count + latest databaseId per thread
gh api graphql -f query="
{
  repository(owner: \"phase-rs\", name: \"phase\") {
    pullRequest(number: ${PR}) {
      reviewThreads(first: 100) {
        totalCount
        nodes {
          id
          isResolved
          comments(last: 1) { nodes { databaseId } }
        }
      }
    }
  }
}" > "/tmp/polish_baseline_threads_${PR}.json"

# Paginate if hasNextPage is true (use endCursor in subsequent after:"<...>" calls).
# Per-PR filenames keep concurrent polish runs (different PRs on the same
# machine, or across users sharing /tmp) from clobbering each other's baselines.

# Top-level reviews — count + latest id per non-empty review
gh api "repos/phase-rs/phase/pulls/${PR}/reviews" --paginate \
  --jq '[.[] | select((.body // "") != "") | {id, user: .user.login, state, submitted_at}]' \
  > "/tmp/polish_baseline_reviews_${PR}.json"

# Issue comments — count + latest id per non-bot, non-author comment.
# Bots are filtered by .user.type == "Bot" (GitHub sets this for app/bot
# accounts like coderabbitai, github-actions, sentry-io). The author is
# filtered by comparing login to the PR author.
AUTHOR=$(gh api "repos/phase-rs/phase/pulls/${PR}" --jq '.user.login')
gh api "repos/phase-rs/phase/issues/${PR}/comments" --paginate \
  --jq --arg author "$AUTHOR" \
      '[.[] | select(.user.type != "Bot" and .user.login != $author)
            | {id, user: .user.login, created_at}]' \
  > "/tmp/polish_baseline_issue_comments_${PR}.json"
```

### Diffing after a review

After `review-impl` runs, any of these counting as "new findings" means another address round is needed:

- New inline thread `id` not in the baseline.
- An existing thread whose latest comment `databaseId` is higher than the baseline's (new reply on an old thread).
- A new top-level review `id` with a non-empty body.
- A new issue comment `id` from a non-bot, non-author user.

If any of the four buckets is non-empty → not done; invoke `pr-contribution-handler` and loop.

## Polish polling

Once `review-impl` produces zero new findings, do **not** exit yet. Bots (CodeRabbit, Sentry, anything attached to the repo) commonly post late reviews after CI settles — 30–90 seconds after the final push. Poll at 60-second intervals:

```text
NON_SUCCESS_BUCKETS = {"fail", "cancel"}    # gh pr checks buckets
clean_polls = 0
required_clean = 2
while clean_polls < required_clean:
    # 1. CI gate
    ci = fetch_check_buckets(PR)
    if any ci.bucket in NON_SUCCESS_BUCKETS:
        Skill(skill="pr-contribution-handler", args=PR_URL)
        baseline = snapshot_state(PR)   # reset — pushes invalidated old baseline
        clean_polls = 0
        continue
    if any ci.bucket == "pending":
        sleep 60; continue              # wait without counting as clean

    # 2. Comment / thread gate
    threads = fetch_unresolved_threads(PR)
    new_issue_comments = diff_against_baseline(issue_comments)
    new_reviews = diff_against_baseline(reviews)
    if threads or new_issue_comments or new_reviews:
        Skill(skill="pr-contribution-handler", args=PR_URL)
        baseline = snapshot_state(PR)   # reset — handler resolved these
        clean_polls = 0
        continue

    # 3. Mergeability gate
    mergeable = gh api repos/phase-rs/phase/pulls/${PR} --jq '.mergeable'
    if mergeable == "CONFLICTING":
        # pr-contribution-handler resolves merge conflicts as part of its flow
        Skill(skill="pr-contribution-handler", args=PR_URL)
        baseline = snapshot_state(PR)
        clean_polls = 0
        continue
    if mergeable == "UNKNOWN":
        sleep 60; continue              # GitHub still computing

    clean_polls += 1
    sleep 60
```

Only after `clean_polls == 2` do you report `POLISH:READY-TO-ENQUEUE`.

### Concrete CI fetch (don't parse `gh pr checks` text columns)

The `fetch_check_buckets(PR)` step above must use `gh pr checks --json`, which requires GitHub CLI 2.46.0 or newer. Do not use the default text output: job names can contain spaces and parentheses, so `gh pr checks $PR | awk '{print $2}'` extracts garbage instead of status.

```bash
ci_json=$(gh pr checks $PR --repo phase-rs/phase --json name,state,bucket)
pending=$(echo "$ci_json" | jq '[.[] | select(.bucket == "pending")] | length')
failed=$(echo "$ci_json"  | jq '[.[] | select(.bucket == "fail" or .bucket == "cancel")] | length')
clean=$(echo  "$ci_json"  | jq '[.[] | select(.bucket == "pass" or .bucket == "skipping")] | length')

# Buckets are: pass | fail | pending | cancel | skipping
# (`gh pr checks` does NOT expose `conclusion` as a JSON field — only `bucket`.
#  Don't confuse with the GitHub REST check_runs endpoint, which DOES.)
```

### Why 2 clean polls, not 1

A single green snapshot can be misleading — the final CI check often completes ~30s before a bot posts its delayed review. One quiet cycle does not prove the PR is stable; two consecutive cycles with no new threads/reviews/issue-comments arriving gives high confidence nothing else is incoming.

### Why checking every source each poll

`pr-contribution-handler`'s babysit mode already re-checks its own comments, but `pr-polish` sits a level above and must also catch:

- New top-level reviews (some bots post structured feedback only after several CI green cycles).
- Issue comments from human reviewers (not caught by inline-thread polling).
- Sentry bug predictions that land on new line numbers post-push.
- Merge conflicts introduced by a race between your push and another agent's merge to `main`.

## Auto-continue: do NOT end your response between child skills

`pr-polish` is a single orchestration task — one invocation drives the PR all the way to enqueue-ready. When a child `Skill()` call returns control to you:

- Do NOT summarize and stop.
- Do NOT wait for user confirmation to continue.
- Immediately, in the same response, perform the next loop step: state diff → decide next action → next `Skill()` call or polling sleep.

The child skill returning is a **loop iteration boundary**, not a conversation turn boundary. Keep going until one of the exit conditions is met (2 consecutive clean polls, `MAX_ROUNDS` hit, or an unrecoverable error).

If the user needs to approve a risky action mid-loop (e.g., a force-push or destructive git operation), pause there — but not at the routine "round N finished, round N+1 needed" boundary. Those are silent transitions.

## Run /pr-polish in the foreground — never in a background agent

Spawning `pr-polish` inside an `Agent(subagent_type=general-purpose)` background task does not work reliably. Background agents don't share the parent's skill registry the same way, so `Skill(skill="review-impl")` and `Skill(skill="pr-contribution-handler")` calls aren't always available — the agent ends up manually replicating logic, which is fragile and tends to stall on the first rate-limit hiccup.

Run `pr-polish` inline in the foreground conversation. If the user asks for "pr-polish + e2e-frontend-test in parallel", split them: foreground `pr-polish`, then E2E can go to a background agent (because it doesn't itself need to invoke other skills).

## You MUST invoke `review-impl` every round — even when bot reviews already exist

A common failure mode: Gemini / Sentry / another bot have already posted findings on the PR, and the orchestrator skips the `review-impl` step on the assumption that "review has been done." That's wrong — the outer loop's purpose is to layer **the agent's own review** on top of the bot reviews, catching issues bots miss (CR annotation correctness, building-block reuse, sibling coverage, hidden coupling, parser combinator discipline). If the orchestrator only addresses bot findings without ever running its own review, the loop converges to "bot-clean" but not "agent-reviewed-clean," and the user reasonably asks "did pr-polish even read the diff?"

**Self-check before reporting `POLISH:READY-TO-ENQUEUE`:** confirm at least one `Skill(skill="review-impl")` call appears in the current orchestration. If none, the loop is incomplete — go back and run one round.

## Bot review trigger

Phase.rs uses **Gemini** as its automated PR reviewer. Pushing new commits does NOT always re-trigger Gemini — sometimes it sits on its last review until explicitly asked. When the polish polling loop is about to exit on the second clean cycle, post one explicit `/gemini review` comment to force a fresh bot pass, then reset `clean_polls = 0` and continue polling. This catches bot findings that would have arrived if the bot had naturally re-reviewed.

```bash
# Right before declaring POLISH:READY-TO-ENQUEUE, request one final bot pass:
gh pr comment "$PR" --repo phase-rs/phase --body "/gemini review"

# Then reset clean_polls and continue the polish polling loop.
# Gemini typically responds within 1–5 minutes; the 60s poll cadence will
# pick up any new findings on the next cycle.
clean_polls=0
```

Only do this once per `pr-polish` invocation (track with a `bot_recheck_issued` flag). If the bot finds nothing in the round after `/gemini review`, the second clean-poll cycle will fire and the loop exits legitimately.

Do NOT use `/gemini review` as a substitute for `review-impl` — they cover different lenses. Gemini catches surface-level issues; `review-impl` enforces phase.rs-specific architecture (CR annotations, parser combinators, building-block reuse, engine/frontend boundary).

## Phase.rs verification per round

Each `pr-contribution-handler` invocation already runs `cargo fmt --all` and the Tilt-first verification cadence (clippy / test-engine / card-data, or check-frontend for client changes). `pr-polish` does NOT re-run these between rounds — it trusts the child skill's verification. If a child skill reports verification failure, treat it as a failure round: re-brief and try again.

The only thing `pr-polish` itself verifies is:

- GitHub thread / review / comment state via GraphQL + REST.
- CI bucket status via `gh pr checks`.
- Mergeability via `gh api .../pulls/{N}`.

## Thread resolution integrity (critical)

**Review threads MUST NOT be resolved via GraphQL unless a real code fix has been committed and pushed first.**

This is the most common failure mode: a child agent calls `resolveReviewThread` to make unresolved counts drop without actually fixing anything. That produces a false "done" signal that gets past the convergence check.

**The only valid resolution sequence:**

1. Read the thread and understand what it's asking.
2. Make the actual code change.
3. `git commit` and `git push`.
4. Reply to the thread with the commit SHA (e.g. "Fixed in `abc1234`").
5. THEN call `resolveReviewThread`.

After each round, paginate **all** review-thread pages and verify the unresolved count yourself — never trust the child skill's claim of "0 unresolved":

```bash
# Step 1: total count
TOTAL=$(gh api graphql -f query='{
  repository(owner: "phase-rs", name: "phase") {
    pullRequest(number: '${PR}') {
      reviewThreads { totalCount }
    }
  }
}' | jq '.data.repository.pullRequest.reviewThreads.totalCount')
echo "Total threads: $TOTAL"

# Step 2: paginate all pages and count unresolved + sniff fake resolutions
CURSOR=""; UNRESOLVED=0; FAKE_RESOLUTIONS="[]"
while true; do
  AFTER=${CURSOR:+", after: \"$CURSOR\""}
  PAGE=$(gh api graphql -f query="
  {
    repository(owner: \"phase-rs\", name: \"phase\") {
      pullRequest(number: ${PR}) {
        reviewThreads(first: 100${AFTER}) {
          pageInfo { hasNextPage endCursor }
          nodes {
            isResolved
            comments(last: 1) {
              nodes { body author { login } }
            }
          }
        }
      }
    }
  }")
  UNRESOLVED=$(( UNRESOLVED + $(echo "$PAGE" | jq '[.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved==false)] | length') ))
  # `.author` is null for deleted GitHub users; `?` + `// "ghost"` keeps
  # jq from crashing with "cannot index null" when sniffing fake resolutions.
  # `.body` can also be null for body-less review comments; default to "" so
  # the test() at the end never sees null.
  PAGE_FAKES=$(echo "$PAGE" | jq '[.data.repository.pullRequest.reviewThreads.nodes[]
      | select(.isResolved == true)
      | {body: (.comments.nodes[0].body // ""), author: (.comments.nodes[0].author.login? // "ghost")}
      | select(.body[0:120] | test("Fixed in|Removed in|Addressed in") | not)]')
  FAKE_RESOLUTIONS=$(echo "$FAKE_RESOLUTIONS $PAGE_FAKES" | jq -s 'add')
  HAS_NEXT=$(echo "$PAGE" | jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.hasNextPage')
  CURSOR=$(echo "$PAGE" | jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.endCursor')
  [ "$HAS_NEXT" = "false" ] && break
done
echo "Unresolved: $UNRESOLVED"
echo "Suspect-resolved (no Fixed-in/Removed-in/Addressed-in SHA): $FAKE_RESOLUTIONS"
```

If `UNRESOLVED > 0` or `FAKE_RESOLUTIONS` is non-empty, the loop is not done — re-invoke `pr-contribution-handler` with the actual count and a note about which threads look fake.

## GitHub rate limits

This skill issues many GraphQL calls (one review-thread query per outer iteration plus per-poll queries inside polish polling). Expect the GraphQL budget to be tight on large PRs. When `gh api rate_limit --jq .resources.graphql.remaining` drops below ~200, back off:

- Fall back to REST for reads (flat `/pulls/{N}/comments`, `/pulls/{N}/reviews`, `/issues/{N}/comments`).
- Queue thread resolutions (GraphQL-only) until the budget resets; keep making progress on fixes + REST replies meanwhile.
- `sleep 5` between any batch of ≥20 writes to avoid secondary rate limits (HTTP 403 `abuse`).

For HTTP 403 `abuse` errors (secondary rate limit on writes): wait **2–3 minutes**, not 60s — secondary limits take longer to clear.

## Safety valves

- `MAX_ROUNDS = 10` — if review+address rounds exceed this, stop and escalate to the user with a summary of what's still unresolved. A PR that cannot converge in 10 rounds has systemic issues that need human judgment.
- Every `review-impl` round must dedupe before posting (the child skill handles this; verify it actually fetched existing comments first).
- If `pr-contribution-handler`'s security pre-check fires a hard-stop on a contributor PR (prompt injection, CI hijack, skill tampering), `pr-polish` exits immediately and surfaces the report. Never try to "polish through" a security issue.

## Reporting

When the skill finishes (either via two clean polls or hitting `MAX_ROUNDS`), produce a compact summary:

```
PR #{N} polished ({rounds_completed} rounds):
- {X} inline threads opened and resolved
- {Y} CI failures fixed
- {Z} new commits pushed
- Final state: CI green, {total} threads all resolved, mergeable.
- POLISH:READY-TO-ENQUEUE

Recommended next step:
  gh pr merge {N} --squash --auto    # see ship-commits skill
```

`pr-polish` does **not** enqueue. Authorization for enqueueing follows the rules in `pr-contribution-handler` § Enqueue (default mode = report the command; authorized mode = run it). The user (or `pr-contribution-handler` in authorized mode) owns the final `gh pr merge` call.

If exiting via `MAX_ROUNDS`, flag explicitly:

```
PR #{N} polish stopped at {MAX_ROUNDS} rounds — NOT enqueue-ready:
- {N} threads still unresolved: {paths/titles}
- CI status: {summary}
- Suspect-resolved threads with no commit SHA: {list, if any}
Needs human review.
```
