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

- Work is uncommitted. Run `/commit` first (commit by pathspec — your memory's `feedback_shared_index_commit_pathspec`).
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

### 1. Identify the commits to ship

If the user named commits explicitly (SHAs, "the last commit", "HEAD~3..HEAD"), use them. Otherwise, ask once: which commits?

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

### 7. Clean up the source of the shipped commits

If the commits were on local `main` ahead of `origin/main`, reset local main so the same commits don't get shipped twice on the next invocation:

```bash
cd -                                          # back to main working dir
git fetch origin main
# Only reset if local main contains exactly these shipped commits ahead of origin/main:
if [ "$(git rev-list origin/main..main)" = "$(echo $SHAS | tr ' ' '\n' | tac | tr '\n' ' ' | xargs)" ]; then
  git branch -f main origin/main
fi
```

If local `main` has commits OTHER than the shipped ones ahead of `origin/main` (another agent committed in parallel), do NOT reset — leave it and report. The user (or another ship invocation) will handle those.

If the commits were on a feature branch (not `main`), leave the branch alone.

### 8. Worktree disposition

Default: leave the worktree at `$WORKTREE` so the user can inspect it if the queue rejects the PR. It's gitignored at the repo level (worktrees live above the repo root). Remove later with `git worktree remove "$WORKTREE"` once the PR lands.

If the user explicitly asks for clean-up-as-you-go, remove immediately after enqueue:

```bash
git worktree remove "$WORKTREE"
```

## Multi-commit batch ship

When the user has several independent chunks to ship in parallel:

1. For each chunk: do steps 2–6 in its own worktree (each branched from `origin/main`, not from a previous chunk's branch).
2. Fire all enqueues without waiting between them — the queue batches up to its configured group size and runs CI once on the synthesized group.
3. Step 7 (reset local main) runs once at the end, only if local main contains exactly the union of shipped commits.
4. Report each PR + enqueue status in one final summary.

Branching each chunk off `origin/main` (not stacked) avoids dependency chains where one PR's failure blocks the others.

## Final report

For each shipped PR, report:

- PR number + URL
- Branch name + worktree path
- Commits included (SHA + subject, in cherry-pick order)
- Enqueue status: `enqueued: yes` with timestamp, or `enqueued: no` with the exact `gh pr merge` error
- Whether local `main` was reset (and why, or why not)
- Worktree disposition (left in place vs. removed)

Do not claim "merged" — the queue is async. The correct status at end-of-skill is `enqueued`.
