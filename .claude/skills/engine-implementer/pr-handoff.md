# PR preparation and handoff

Read this after [SKILL.md](SKILL.md) Step 7 when the task includes opening a PR. Local-only implementation does not synchronize a remote.

## Prepare the completed work for a PR

Perform this handoff after the pipeline completes and before the caller's final review and Gate A. The rule is simple: finish the work, synchronize once, then test and review the code being submitted. A commit SHA identifies that code; it is not another artifact to create.

1. Confirm the actual PR target repository and base branch. Use `upstream` for the contributor fork workflow, or `origin` only when it points to the target repository. Fetch that branch once and retain the fetched commit as the comparison base. If the target is unclear or fetching fails, report the blocker rather than claiming the branch is synchronized.
2. With no active workers or checks, and a clean feature checkout containing the committed work, merge that fetched commit. Do not merge into a projection/completion worktree or mutate a candidate under active verification. If conflicts need edits, delegate them to a scoped general-purpose worker; this is PR preparation, not the checkpoint executor mode that requires a clean `START_SHA`. The orchestrator commits the resolution. Preserve unrelated work; never stash it. Keep the original task scope and [run limits](SKILL.md#run-limits); a conflict that invalidates the approved design returns to plan review.
3. Once the merge is complete and the checkout is clean, run the Step 5 checks required by the changed surface on the resulting committed branch. For parser changes, compare parser output against the fetched upstream commit using Step 4's measurement procedure. Use the full diff from that fetched commit to the branch head, so upstream changes are not attributed to this PR. Reuse prior evidence only when its committed head and comparison base both match; a pre-merge pass does not verify a later merge.
4. Hand the resulting commit and complete PR diff to the caller's ordinary final `review-engine-impl` and Gate A (§5–6 of [AI-CONTRIBUTOR.md](../../../docs/AI-CONTRIBUTOR.md#5-validate-the-review-actually-happened-and-was-addressed)). Do not substitute checkpoint-mode or phase-only review for this full-head review. The reviewer tags its findings as `review-engine-impl` directs for engine-implementer reviews, and this loop is bounded like any other review loop by the [run limits](SKILL.md#run-limits). A round without `behavior` findings closes as SKILL.md Step 6 closes one: after a second consecutive such round, apply its comment-only corrections and list its remaining `text` findings as residuals. Delegate any fixes to a scoped worker and commit them, then repeat the required checks and final review for the new commit against the same fetched base. A comment-only correction the final review returns is applied by the orchestrator under [Step 6's class and proof](SKILL.md#step-6--review-the-immutable-candidate) instead of by a worker; the required checks, the final review and Gate A then repeat at the correction commit as for any new commit. Do not refetch in that loop: later upstream movement does not restart this preparation.

Keep earlier accepted checkpoints and phase-chain evidence intact. If synchronization changes the branch head, use the historical/current-head handoff below; do not relabel old results as verification of the new code or introduce more SHA fields. This can require repeating checks when synchronization changes code. A later deliberate merge or rebase invalidates the current-head evidence again.

## Post-acceptance PR handoff (non-gating)

Final acceptance emits an immutable Final Report snapshot in one of two shapes. Uncorrected: `Pipeline-reviewed head == Current branch head == accepted CANDIDATE_SHA`, `Correction commit: none`, `Pipeline status: current`, and `Current-head review: none`. Corrected, when the accepted candidate is a Step 6 correction commit: `Pipeline-reviewed head: <reviewed SHA>`, `Correction commit: <correction SHA>` with its phase-fit entry, `Current branch head == correction SHA == accepted CANDIDATE_SHA`, `Pipeline status: current`, and `Current-head review: none` — the Step 6 review saw only the reviewed SHA. Do not alter or replace that snapshot, the accepted candidate SHA after acceptance.

Copy the following mutable `PR handoff` block into the PR body beside the retained pipeline report:

```text
Pipeline-reviewed head: <the candidate the last Step 6 review saw>
Correction commit: <the snapshot's Correction commit>
Current branch head: <current branch SHA>
Pipeline status: current | historical — <reason>
Current-head review: none | clean at <SHA> | findings at <SHA>
```

Whenever the branch head changes after acceptance, including through a rebase, update `Current branch head`, set `Pipeline status` to `historical — <reason>`, and reset `Current-head review` to `none`. When current-head evidence is desired, run ordinary `review-engine-impl` against the complete current PR/head — not checkpoint mode and not an incremental-only diff — then record `clean at <SHA>` or `findings at <SHA>` for that exact SHA. Each future head change repeats the reset. `Pipeline status` remains historical unless the current head again equals the original accepted `CANDIDATE_SHA`, in which case set it to `current`.

Residual findings shipped under the [decline path](SKILL.md#run-limits) are listed under the PR's Validation Failures heading with their evidence and follow-up.

If later work invalidates the approved plan or architecture, return to plan review. Otherwise, this is a concise reporting and navigation flow only: it is not a gate, GitHub automation, an executor change, or a PR-handler change.
