# plan-review kit for pantheriq-foam

Staged here for review before moving into `C:/git/oc/pantheriq-foam`. Not part of phase.rs — never git-add this folder as part of any phase.rs commit.

## What this is

Formalizes the adversarial plan-hardening pattern already proven once by hand in `KB/Planning/SPRINT-rj-035-adversarial-review.md` in pantheriq-foam, so it runs as a repeatable loop instead of a one-off manual review.

Sits between the existing `coordinator` (drafts the sprint plan) and `tech-lead` (executes it) personas. Tech Lead is untouched. Coordinator received one small, explicit addition (see "Modification to an existing file" below) so the handoff to plan-review happens automatically instead of relying on you to remember to type `/plan-review`.

A second, parallel piece — `impl-review` — does the analogous thing for implementation output: it generalizes the existing `code-reviewer` agent (previously dispatched only for "risky changes," single-pass) into a mandatory, looping gate that runs after any implementer/Tech Lead diff, re-reviewing with a fresh dispatch after every fix round until clean.

## What plan-review does

1. Dispatches a fresh, independent `plan-reviewer` agent against the sprint's current `plan_content`.
2. The agent verifies every checkable factual claim in the plan against ground truth (git state, knowledge DBs, Reference/, Confluence/Jira within scope) and returns a structured critique: verdict, what to preserve, numbered defects by severity, decisions the plan is dodging, and a prescribed task structure — same shape as the SPRINT-rj-035 precedent.
3. The `plan-review` skill applies the fix directives to the plan (it didn't author the original plan, so this isn't grading its own work).
4. A **fresh** `plan-reviewer` agent re-reviews the revised plan. Loop continues until a round comes back clean — no round cap.
5. Once clean, the plan is committed. Coordinator's automatic handoff (below) means this now starts without you invoking `/plan-review` yourself.

## What impl-review does

1. Dispatches a fresh `code-reviewer` agent (the existing one, unmodified) against a diff — an implementer's worktree branch, or Tech Lead's in-context work.
2. On FAIL/CONDITIONAL PASS: fixes get applied without editing inside an implementer's worktree directly (a fresh corrective `implementer` agent is dispatched instead, mirroring Tech Lead's existing integration rule) — or in-context if the diff was Tech Lead's own work.
3. A **fresh** `code-reviewer` agent re-reviews. Loop continues until PASS — no round cap, no more "risky changes only" discretion.
4. Hands the clean branch/diff back to Tech Lead to integrate — it never merges anything itself.

## Files, and where they go

| This kit | Destination in pantheriq-foam |
|---|---|
| `agents/plan-reviewer.md` | `.claude/agents/plan-reviewer.md` |
| `skills/plan-review/SKILL.md` | `.claude/skills/plan-review/SKILL.md` |
| `skills/impl-review/SKILL.md` | `.claude/skills/impl-review/SKILL.md` (no new agent — reuses the existing `code-reviewer`) |

## Already staged live in pantheriq-foam

All three files above already exist at their destination paths (created directly there). They're untracked/uncommitted — new additions only. This kit folder holds duplicate copies for your review workflow.

## Modification to an existing file

`.claude/skills/coordinator/SKILL.md` — **one addition**, not a rewrite. A new step 6 was appended to the end of the "Sprint Planning" section (after the existing commit-and-register step):

> 6. **Hand off to plan-review immediately — do not wait for the human to invoke it.** A freshly-written plan has not yet been adversarially checked; announce the handoff (e.g. "Plan for SPRINT-\<ns\>-NNN complete and committed. Handing off to plan-review for adversarial hardening before Tech Lead executes.") and then invoke the `plan-review` skill via the Skill tool, passing the sprint ID as its argument. Do this every time you finish writing or substantially revising a sprint's `plan_content` — not only on first creation. You do not review the plan yourself; that is deliberately a separate, independent skill's job (see `.claude/skills/plan-review/SKILL.md`).

This was made directly in the live pantheriq-foam file (not staged as a full copy here, since it's a small diff against a file this kit didn't otherwise touch) — this README is the record of exactly what changed. Nothing else in `coordinator/SKILL.md` was altered. If you'd rather this be a manual `/plan-review` invocation instead of automatic, revert just this addition.

## What's still fully manual

- `/tech-lead` is still a human-invoked persona switch after plan-review hands off — Coordinator does not chain into Tech Lead automatically, and impl-review does not chain into anything after it either. Only the Coordinator→plan-review handoff is automatic.
- True prompt-level auto-suggestion (skills recommending themselves based on your wording, independent of any persona's own logic) would require building the hook infrastructure `skill-rules.json` describes but this repo doesn't actually wire up (`skill-activation-prompt.ts` / a `PreToolUse` skill-verification-guard don't exist here — only `restrict_writes.sh` is hooked). Not built; say so if you want it.
