---
name: ship-commits
description: Use when shipping local commits to main via the merge queue. Creates an isolated worktree based on origin/main, cherry-picks the named commits into a fresh branch, pushes with --no-verify, opens a PR with `gh pr create --fill`, and enqueues with `gh pr merge --squash --auto`. Outbound counterpart to `pr-contribution-handler`. Use when the user says "ship this", "push to main", "send through the queue", "PR this work", or has finished a chunk of work and wants it on main.
---

# Ship Commits

Take local commits and land them on `main` through the merge queue, using an isolated worktree to avoid carrying other agents' concurrent work into the PR.

## Why a worktree

The main working dir is **assumed to contain other unrelated changes** from concurrent agents — unstaged work, other branches, in-progress commits on `main` that aren't yet shipped. Branching in place risks:

- Carrying unrelated commits into the PR (if local `main` is ahead of `origin/main` with multiple agents' work).
- Disturbing other agents' branch state with `git checkout` operations.
- Leaving the user on a topic branch they didn't expect to be on.

A worktree based on `origin/main` gives a clean branch we cherry-pick into, isolated from everything else.

## When to use

- User has finished a discrete chunk of work and wants it shipped.
- One or more commits exist locally (named explicitly, or "the last N commits on main", or "this commit").
- Goal: branch → push → PR → enqueue, with the queue handling rebase + CI + merge in the background.

## When NOT to use

- Work is uncommitted. Run `/commit` first (commit by pathspec — your memory's `feedback_shared_index_commit_pathspec`). **Uncommitted work being shipped must not be left dirty on local `main`.**
- A PR for these commits already exists. Use `gh pr merge <N> --auto` directly to enqueue.
- The commits are already on `origin/main`. Nothing to do.
- Repo isn't `phase-rs/phase`. This skill encodes phase.rs-specific conventions.

## Phase.rs-specific conventions

| Convention | Why |
|------------|-----|
| **Squash-only merges.** `--squash` is mandatory; `--merge` and `--rebase` are server-disabled. | Repo keeps `main` as one-commit-per-feature. |
| **`--auto` is mandatory on `gh pr merge`.** | Without it, the merge bypasses the queue and tries to merge immediately — fails on protected `main`. |
| **`--no-verify` on push.** | Pre-push hooks duplicate validation that Tilt/CI has already done locally. CI/queue will re-validate. |
| **Branch protection on `main` blocks direct push for non-admins.** | Queue handles serialization for parallel PRs. |
| **Worktree-based shipping.** | Main working dir has concurrent changes from other agents — don't disturb them. |

## Sequence

### 0. Reconcile already-shipped commits (run this FIRST)

**Squash-merges leave the originals stranded on local `main`.** When a prior ship's PR squash-merges, its commits collapse into one *new* commit on `origin/main` with a different SHA and patch-id. The original commits still sit on local `main`, invisible to any SHA- or `git cherry` patch-id comparison. Left alone they pile up across sessions and — worse — get **re-shipped**, because Step 1's `git rev-list origin/main..main` re-lists them. Clear them before doing anything else.

The safe, squash-aware test is content-based: a 3-way merge of local `main` into `origin/main` that yields `origin/main`'s *exact tree* means local `main` contributes no new content, so every ahead-commit is already shipped and resetting loses nothing.

```bash
git fetch origin main
ahead=$(git rev-list --count origin/main..main)
if [ "$ahead" -eq 0 ]; then
  echo "local main not ahead of origin/main — nothing to reconcile"
else
  merged_tree=$(git merge-tree --write-tree origin/main main 2>/dev/null); mt_exit=$?
  origin_tree=$(git rev-parse 'origin/main^{tree}')
  if [ "$mt_exit" -eq 0 ] && [ "$merged_tree" = "$origin_tree" ]; then
    # Every ahead-commit's content is already in origin/main (incl. via squash).
    # Multi-agent guard: never discard another agent's uncommitted tracked work.
    # (reset --hard preserves untracked files; it only drops tracked modifications.)
    if git diff --quiet && git diff --cached --quiet; then
      git reset --hard origin/main
      echo "reset local main to origin/main — dropped $ahead already-shipped commit(s)"
    else
      echo "WARNING: $ahead ahead-commit(s) are already shipped, but the working tree has"
      echo "uncommitted tracked changes — NOT resetting. Resolve those first, then re-run."
    fi
  else
    # merge-tree conflicted, OR local main adds content origin/main lacks.
    echo "local main has $ahead ahead-commit(s) NOT fully contained in origin/main:"
    git --no-pager log --oneline origin/main..main
    echo "(genuine unshipped work, or a PR that diverged from local main during review.)"
  fi
fi
```

Outcomes:
- **Reset happened** → re-evaluate what (if anything) is actually left to ship before continuing.
- **Left alone, clean (`mt_exit` 0 but tree differs)** → there is genuine unshipped work; proceed to Step 1 to ship it.
- **Left alone, divergent (`mt_exit` non-zero)** → the ahead-commits look shipped but the merged PR diverged from local `main` (e.g., a fix was added during review, as happens when a cherry-pick onto a newer `origin/main` needed a follow-up). The clean fix is still `git reset --hard origin/main` once nothing local is worth keeping — **surface this to the user and let them decide; do not silently discard.**

**Then prune ship worktrees whose PR has merged.** Each ship leaves a `../forge.rs-ship-*` worktree on a `ship/<topic>` branch (Step 8). After the PR squash-merges, that worktree is dead weight and its build artifacts (`target/`, `node_modules/`) pile up on disk. Remove the ones whose branch is now fully contained in `origin/main`:

```bash
git worktree list --porcelain | awk '/^worktree /{wt=$2} /^branch /{print wt"\t"$2}' \
| while IFS=$'\t' read -r wt ref; do
    case "$ref" in refs/heads/ship/*) ;; *) continue ;; esac        # only OUR ship/* worktrees — never another agent's
    br=${ref#refs/heads/}
    merged=$(git merge-tree --write-tree origin/main "$br" 2>/dev/null)
    [ "$merged" = "$(git rev-parse 'origin/main^{tree}')" ] || continue   # not yet merged (PR still in queue) — keep
    if git -C "$wt" diff --quiet && git -C "$wt" diff --cached --quiet; then
      # merged + no tracked changes → only gitignored build output remains, so --force is safe
      git worktree remove --force "$wt" && git branch -D "$br" \
        && echo "pruned merged ship worktree $wt ($br)"
    else
      echo "ship worktree $wt ($br) is merged but has uncommitted TRACKED changes — leaving it for review"
    fi
  done
git worktree prune    # drop stale admin refs for worktrees whose dir was deleted manually
```

This only ever touches `ship/*` worktrees this skill created; `forge.rs-pr` and `.claude/worktrees/agent-*` are filtered out. `--force` is deliberate — a merged ship worktree's sole uncommitted content is gitignored build output (`target/`/`node_modules/`), and the tracked-diff guard refuses if anything real is dirty.

### 1. Identify the commits to ship

If the user named commits explicitly (SHAs, "the last commit", "HEAD~3..HEAD"), use them. Otherwise, ask once: which commits?

**Hard gate: do not ship uncommitted source changes by recreating them in the ship worktree.** If the work to ship currently exists as tracked modifications in local `main`, first commit that exact work in the source worktree by pathspec, then verify the shipped pathspecs are clean there before continuing. The source worktree may contain unrelated dirty files from other agents, but the files you are shipping must not remain dirty.

Use this checklist before creating the ship worktree:

```bash
# 1) Identify the paths that belong to the work being shipped.
SHIPPED_PATHS="path/one path/two ..."

# 2) If any shipped path is dirty, commit it in the source worktree first.
git status --short -- $SHIPPED_PATHS
# If output is non-empty: run /commit or create a normal local commit scoped to $SHIPPED_PATHS.

# 3) Re-check. This must print nothing before shipping.
git status --short -- $SHIPPED_PATHS
```

If the re-check still shows shipped paths dirty, stop and fix that before shipping. Do not proceed with a PR while the same work remains as uncommitted local `main` changes. This prevents the user from seeing the work both "shipped" and still dirty locally.

Generated planning/review artifacts are not part of the shipped code unless the user explicitly requested them. If you created untracked artifacts while preparing the shipment (`.claude/wf/*`, `.agents/pr-review/*`, compiler crash dumps, logs), either remove your own artifacts or explicitly report them and get approval before leaving them behind.

Resolve to a concrete list of SHAs in chronological order (oldest first):

```bash
# Examples — pick the one that matches the user's intent:
git rev-list --reverse origin/main..HEAD            # all commits on current branch ahead of origin/main
git rev-list --reverse origin/main..main            # all commits on local main ahead of origin/main
git rev-list --reverse <BASE>..<TIP>                # explicit range
echo <SHA1> <SHA2>                                  # specific SHAs (already in order)
```

Capture as a space-separated list: `SHAS="abc123 def456 ..."`. Verify they exist:

```bash
for sha in $SHAS; do git cat-file -e "$sha" || { echo "missing: $sha"; exit 1; }; done
```

### 2. Derive branch name + worktree path

Use the first commit's subject (or user-provided topic) for both, kebab-case, no timestamps:

```bash
TOPIC=$(git log -1 --format=%s "$(echo $SHAS | awk '{print $1}')" | head -c 50 | tr -cd 'a-zA-Z0-9 -' | tr ' ' '-' | tr -s '-' | sed 's/-$//')
BRANCH="ship/$TOPIC"
WORKTREE="../forge.rs-ship-$TOPIC"
```

If the branch already exists locally or remotely, append `-2`, `-3`, etc. Don't reuse an existing branch — that complicates the cherry-pick path.

### 3. Create the worktree

```bash
git fetch origin main
git worktree add "$WORKTREE" -b "$BRANCH" origin/main
cd "$WORKTREE"
```

The worktree starts at `origin/main` HEAD with a fresh branch checked out. No working-tree contamination from the main dir.

### 4. Cherry-pick the commits

```bash
git cherry-pick $SHAS
```

If a cherry-pick fails with a conflict, do NOT auto-resolve — abort and surface to the user:

```bash
# On conflict:
git cherry-pick --abort
cd -
git worktree remove "$WORKTREE"
# Report: "Cherry-pick of <SHA> failed with conflicts against origin/main.
#         The commit assumes state that isn't in origin/main yet — likely
#         depends on another unshipped commit. Either ship the dependency
#         first or rebase manually."
```

Conflicts almost always mean a missing dependency commit, which the user needs to resolve manually.

### 5. Push and open PR

```bash
git push --no-verify -u origin "$BRANCH"
gh pr create --fill
```

`--no-verify` skips pre-push hooks (Tilt already validated; CI re-validates). `--fill` populates title/body from commit messages.

Capture the PR number from `gh pr create` output for the next step.

### 6. Enqueue

```bash
gh pr merge "$PR_NUMBER" --auto
```

Do NOT wait — the queue is async. Move on.

If `gh pr merge` errors:

- `not in mergeable state` → CI hasn't reported yet. Wait 10s, retry once. If still failing, surface to user.
- `auto-merge is not allowed` / `merge queue not enabled` → repo setting drift; surface to user.
- Auth errors → surface to user.

Never retry blindly.

### 7. Reconcile local main (post-ship)

Re-run the **Step 0 reconciliation** (`cd -` back to the main working dir first). Immediately after enqueue the PR hasn't merged, so `origin/main` hasn't advanced and the merge-tree test shows `main` still ahead — it correctly **no-ops**, leaving the just-shipped commits in place until the queue lands them. The durable cleanup then happens automatically on the *next* ship-commits run (Step 0), once the PR has squash-merged.

Do **not** reintroduce a SHA-equality reset here. A squash-merge changes the SHA, so `git rev-list origin/main..main == $SHAS` never matches after merge and the commits accumulate forever — that is the exact bug Step 0's content-based test fixes.

If the commits were shipped from a feature branch (not `main`), leave that branch alone.

Then verify source-worktree hygiene for the shipped paths recorded in Step 1:

```bash
git status --short -- $SHIPPED_PATHS
```

This must print nothing for work that was originally uncommitted on local `main`. If it prints shipped paths, the shipment is incomplete operationally: either the work was not committed before shipping, or the source worktree still contains duplicate local modifications. Do not silently leave that state. Clean only files you own and only after confirming they are represented by the shipped commits; otherwise stop and report the exact dirty paths.

### 8. Worktree disposition

Default: leave the worktree at `$WORKTREE` so the user can inspect it if the queue rejects the PR. It's gitignored at the repo level (worktrees live above the repo root). It will be **auto-pruned by Step 0 on the next ship-commits run** once its PR squash-merges (along with its build artifacts) — so you don't have to remember to clean it up. To remove it sooner: `git worktree remove "$WORKTREE"` once the PR lands.

If the user explicitly asks for clean-up-as-you-go, remove immediately after enqueue:

```bash
git worktree remove "$WORKTREE"
```

## Multi-commit batch ship

When the user has several independent chunks to ship in parallel:

1. For each chunk: do steps 2–6 in its own worktree (each branched from `origin/main`, not from a previous chunk's branch).
2. Fire all enqueues without waiting between them — the queue batches up to its configured group size and runs CI once on the synthesized group.
3. Step 7 (reconcile local main) runs once at the end; it is content-based (see Step 0) and no-ops until the PRs squash-merge, so it never needs to know which commits belonged to which chunk.
4. Report each PR + enqueue status in one final summary.

Branching each chunk off `origin/main` (not stacked) avoids dependency chains where one PR's failure blocks the others.

## Final report

For each shipped PR, report:

- PR number + URL
- Branch name + worktree path
- Commits included (SHA + subject, in cherry-pick order)
- Enqueue status: `enqueued: yes` with timestamp, or `enqueued: no` with the exact `gh pr merge` error
- Whether local `main` was reset (and why, or why not)
- Source-worktree hygiene: whether the shipped pathspecs are clean on local `main`; list any remaining dirty shipped paths explicitly
- Worktree disposition (left in place vs. removed)

Do not claim "merged" — the queue is async. The correct status at end-of-skill is `enqueued`.
