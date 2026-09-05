# START HERE — custom-format-engine Phase A (migrated machine)

**You cannot resume the originating Claude Code session.** Session transcripts are local to the
machine that created them (`~/.claude-profiles/<profile>/projects/<slug>/*.jsonl`) and are not in
this repo. This file plus `SESSION_HANDOFF.md` plus `.handoff/engine-implementer-runs/` are the
complete replacement for that transcript. Read all three before touching code.

## 0. What this branch is

- Branch: `wip/custom-format-phase-1d` on `myfork` (`git@github.com:rykerwilliams/phase.git`)
- Charter: `docs/proposals/custom-format-engine/IMPLEMENTATION_PLAN.md`
- Pipeline: the `engine-implementer` skill, Phase A (engine-side admission gate)
- Tip commit is a **WIP checkpoint, not a landable commit**. Its 993-line implementation draft was
  **never compiled, linted, or tested** — the executor agent was killed by an account-level rate
  limit while queued behind cargo build-lock contention, after reporting all edits done and
  `cargo fmt` clean. Treat it as an unverified draft.
- `HEAD~1` (`e0b06e12a`) is the last clean, reviewed commit. It is also pushed on its own as
  `myfork engine/custom-format-phase-1d` so the reviewed line stays separable from the draft.

## 1. Setup on the new machine

```bash
git clone git@github.com:rykerwilliams/phase.git phase && cd phase
git remote add origin git@github.com:phase-rs/phase.git   # upstream, READ-ONLY
git remote rename origin upstream 2>/dev/null || true      # optional; see the push rule below
git fetch --all
git checkout -b custom-format-phase-1d myfork/wip/custom-format-phase-1d
./scripts/fetch-comp-rules.sh    # docs/MagicCompRules.txt is gitignored; CR verification needs it
```

**Push rule, non-negotiable: push only to `myfork`. Never push to the upstream `phase-rs/phase`
repo.** All PRs are opened from fork branches.

## 2. Immediate next action

Do **not** re-plan or re-implement — the design is reviewed and accepted. Only verification and
small in-scope fixes remain. From the worktree:

```bash
cargo fmt --all
cargo check -p phase-engine --all-targets
cargo clippy -p phase-engine -p lobby-broker -p server-core -p seat-reducer -p engine-wasm -p phase-server --all-targets -- -D warnings
cargo test -p phase-engine --lib                # FULL, unscoped
cargo test -p phase-engine --test integration   # FULL, unscoped
cargo test -p lobby-broker -p server-core -p seat-reducer -p phase-server -p engine-wasm
cargo engine-inventory
cargo check --workspace --all-targets
node scripts/check-protocol-version.mjs         # must stay green, untouched — no bump in this phase
```

Then: if clean, amend/replace the WIP commit with a real `fix(engine): ...` commit, and dispatch a
**review-impl round 4 (Opus)** against diff span `e0b06e12a..<new head>`. Loop fix ↔ review until
clean. That closes the HIGH finding from Phase A round-3 review-impl and Phase A can go to final
acceptance (step 7 of the `engine-implementer` pipeline).

`SESSION_HANDOFF.md` §"The accepted plan (v3)" lists exactly what the diff should contain and the
six required revisions (R1–R6) to verify landed. Read it — it is the spec for this diff.

## 3. Environment facts that were true on the old machine (re-check, don't assume)

- **Tilt was down for this worktree** every round, so verification fell back to direct `cargo`.
  On the new machine, check first: `tilt get uiresource clippy >/dev/null 2>&1` (exit 0 = up). If
  Tilt is up, prefer `tilt logs <resource>` / `./scripts/tilt-wait.sh` per CLAUDE.md and do **not**
  run cargo directly — it causes target-lock contention.
- The old machine ran 20–30 concurrent worktrees on one drive; builds took 10 min to 2+ hours from
  lock contention. A fresh machine should be far faster. If it isn't, that's a real problem, not
  the known-benign contention.
- One pre-existing, unrelated integration failure on the old machine:
  `cr733_authority_matrix_covers_the_fresh_write_census` (`python3` not on PATH). If Python is
  installed on the new machine this should now pass. It is unrelated to this phase either way.

## 4. Standing rules for this pipeline

- **Model assignment (user standing instruction): Opus for all planning and all review steps;
  Sonnet for all implementation steps.** Do not deviate without the user re-confirming.
- **Ship each phase as its own PR.** No direct merge to `main`, no bundling phases into one PR.
- Lead/orchestrator owns commits and checkpoints; subagents never commit.
- **CR-annotation discipline is mandatory.** Every CR number must be grepped against
  `docs/MagicCompRules.txt` by *you*, not trusted because a prior round checked it.
- **Regenerable criteria**: assert with a re-runnable grep or registry loop, never a frozen count.
  A planner twice miscounted `GameFormat`'s variants (15 vs. the real 23) before this rule stuck.
- Multi-agent safety (CLAUDE.md): never `git stash`, never `checkout`/`restore`/`reset` files you
  did not write, never revert another agent's work.

## 5. Remaining roadmap after Phase A

Phase B (real per-deck Custom-format evaluation) → Phase C1 (frontend: carry resolved config) →
Phase C2 (frontend: remove the hosting gate) → Phase D (`swedish_old_school()` registry preset).
Each runs its own full plan → review-plan → implement → review-impl cycle.

## 6. Process record

`.handoff/engine-implementer-runs/20260902-custom-format-phase-1d/` — `phase-fit` (append-only,
chronological) and `phase-charter`. Copied out of `.git/`, which does not survive a clone. This is
the authoritative round-by-round history: charter rounds 1–6, Phase A plan rounds 1–7 and
acceptance, three review-impl rounds, and three rounds of this fix-plan's own review loop.

**Delete `HANDOFF.md`, `SESSION_HANDOFF.md`, and `.handoff/` before this work becomes a real PR.**
They are migration scaffolding, not project documentation.
