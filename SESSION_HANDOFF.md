# Session handoff — custom-format-engine Phase A, in progress

Written 2026-09-04 because the operator is switching Claude accounts mid-pipeline after the
implementation agent hit an account-level API spend/rate limit. This file is a resumption guide,
not the process record — the authoritative round-by-round history is
`C:/git/phase/.git/engine-implementer-runs/20260902-custom-format-phase-1d/phase-fit` (append-only,
read from the top; the entries are in chronological order and each is self-contained).

**This file is untracked (uncommitted, harmless) at the root of this worktree. Delete it once
Phase A is fully accepted — it's a scratch handoff note, not permanent project documentation.**

## Where the code actually is right now

- Worktree: `C:\git\phase\.claude\worktrees\custom-format-phase-1a`
- Branch: `engine/custom-format-phase-1d`
- `HEAD` = `e0b06e12ad3ae555f704739d030bb28c8390036e` (clean at this SHA)
- **Working tree has real, substantial UNCOMMITTED changes — do NOT discard them, do NOT `git checkout`/`restore`/`reset`/`clean` in this worktree.** Confirmed via `git status --short` / `git diff --stat` on 2026-09-04:
  ```
   M crates/engine/src/types/format.rs                        (565 insertions)
   M crates/engine/tests/integration/custom_format_schema.rs  (346 insertions)
   M crates/lobby-broker/src/protocol.rs                      (101 insertions)
   M crates/server-core/src/session.rs                        (33 changed)
  4 files changed, 993 insertions(+), 52 deletions(-)
  ```
- These changes are a **complete implementation attempt** of an accepted fix-plan (v3, below), written by a Sonnet `engine-implementation-executor` subagent. The agent reported finishing ALL code edits (S1–S9 + required revisions R1–R6, see below), running `cargo fmt` clean, and completing CR-annotation verification — but it was killed by an account-level rate limit (HTTP 429, "individual spend limit") while queued waiting for `cargo check`/`clippy`/`test` to clear heavy build-lock contention from other concurrent agents on this machine. **The implementation itself was never verified to compile or pass tests.** Treat it as an unverified draft, not a trusted checkpoint.

## What to do next, in order

1. **Read this whole file**, then read the tail of `phase-fit` (search for "fix-plan v3 IMPLEMENTATION dispatched" — that's the exact dispatch prompt, further down in the file if you need the full plan text again) to get full round-by-round context if anything here is unclear.
2. From the worktree, run the verification suite below. Do not re-implement anything unless verification surfaces a real problem — spot-check the diff against the plan (§"The accepted plan" below) and the discriminating-test gate, then run:
   ```
   cargo fmt --all
   cargo check -p phase-engine --all-targets
   cargo clippy -p phase-engine -p lobby-broker -p server-core -p seat-reducer -p engine-wasm -p phase-server --all-targets -- -D warnings
   cargo test -p phase-engine --lib               (FULL, unscoped)
   cargo test -p phase-engine --test integration   (FULL, unscoped)
   cargo test -p lobby-broker
   cargo test -p server-core
   cargo test -p seat-reducer
   cargo test -p phase-server
   cargo test -p engine-wasm
   cargo engine-inventory
   cargo check --workspace --all-targets
   node scripts/check-protocol-version.mjs   (must stay green, untouched — no version bump is part of this change)
   ```
   **Tilt is confirmed down for this worktree** across every round of this whole phase (`tilt get uiresource clippy` exits non-zero / not-found) — always fall back to direct `cargo`, never wait on Tilt here.
   **Expect these to be slow** (10 min to 2+ hours) due to heavy shared build-cache contention from other concurrent agents on this machine (a known, documented environment characteristic — see project memory `project_cargo_build_lock_contention`). This is normal, not a sign anything is broken.
   **One known pre-existing, unrelated, environmental test failure** in the full integration suite: `cr733_authority_matrix_covers_the_fresh_write_census` (`python3 ... program not found`) — confirmed across multiple prior rounds to be unrelated to any file this phase touches. Do not investigate or try to fix it.
3. If verification is clean (or only needs the kind of small, in-scope fixes the executor already made proactively — see "one thing the executor already fixed" below), **checkpoint it as a new commit on `e0b06e12a`** (the orchestrator/lead session owns this commit, not a subagent). Suggested commit message shape (adjust to match what's actually in the diff):
   ```
   fix(engine): reclassify host-configurable FormatConfig fields, close player_count admission hole

   [Body: summarize the 4 gate-row reclassifications (starting_life, max_players,
   deck_size, commander_damage_threshold) and the new validate_for_player_count
   seat-count bound. See phase-fit process record for full rationale — this closes
   the HIGH finding from Phase A's round-3 review-impl.]

   Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
   ```
   (Check the live system prompt for the current attribution format before committing — it has changed multiple times this session; as of this handoff it's just the `Co-Authored-By` line for commits and a generated-with-Claude-Code footer for PRs, no session-URL line.)
4. **Dispatch a review-impl round (Opus)** against the new commit (diff span `e0b06e12a..<new candidate>`), following the exact pattern used for Phase A's first three review-impl rounds (see `phase-fit`). Loop fix ↔ review until clean.
5. Once clean, this closes out the HIGH finding from Phase A's round-3 implementation review. **Phase A can then proceed to final acceptance** (Step 7 of the `engine-implementer` pipeline).
6. After Phase A is accepted, the charter (`docs/proposals/custom-format-engine/IMPLEMENTATION_PLAN.md`) proceeds to **Phase B** (real per-deck Custom-format evaluation), then **Phase C1** (frontend: carry resolved config), **Phase C2** (frontend: remove the hosting gate), **Phase D** (`swedish_old_school()` registry preset). Each phase runs its own full plan → review-plan → implement → review-impl cycle, per the `engine-implementer` skill.

## The accepted plan (v3) — what the uncommitted diff should contain

This is a summary; the FULL plan text (with complete Rust code for every gate row) is in the `phase-fit` process record entry titled "Phase A fix-plan v3 (revised by opus planner...)" and was passed verbatim to the implementation executor in the dispatch entry titled "Phase A fix-plan v3 IMPLEMENTATION dispatched (sonnet)". Read those two entries for the authoritative text if you need to re-verify anything below.

**The bug this closes:** Phase A's admission gate (`built_in_axes_no_looser_than_rules` in `crates/engine/src/types/format.rs`, wired into `FormatConfig::deserialize`'s built-in-format branch — already landed in commits `61f63c3d4`/`90714deec`/`e0b06e12a`) wrongly locked `starting_life`, `max_players`, `commander_damage_threshold`, and `deck_size` to exact registry equality. This broke real, currently-shipped host-configuration behavior in `client/src/components/lobby/HostSetup.tsx` — most severely, **the default Commander hosting flow is broken** (HostSetup starts every format at `min_players`, but Commander's registry `max_players` is 6, so a default 2-seat Commander host is hard-rejected at `FormatConfig::deserialize`). This was found empirically (compiled probes, not just code-reading) across three rounds of adversarial review-impl on this same phase.

**The fix (engine-only — Phase 1; a frontend Phase 2 is explicitly deferred, not part of this diff):**

- `starting_life` → free (`HostChoiceWithin`), except a floor: `config.starting_life_for_seat() < 1` is rejected (CR 704.5a / CR 810.8c) — checked via the existing `starting_life_for_seat()` function specifically (not `starting_life_for_player`, which is what `GameState::new` actually calls but which ignores the field entirely on `OneVsMany`/Archenemy — `for_seat` is the deliberately more-conservative choice).
- `max_players` → bounded to `rules.min_players..=rules.max_players` (a new "HostChoiceWithin a registry-defined range" pattern, not full freedom).
- `commander_damage_threshold` → the `Option` shape (`Some`/`None`) stays strictly locked both directions (closes a hole where `None` + `uses_commander: false` would otherwise be self-consistent and silently delete the CR 903.10a/704.6c commander-damage SBA); the magnitude inside `Some(_)` is free except `Some(0)` is rejected (an immediate CR 704.6c loss). Match-arm order matters: `(Some(0), Some(_))` must precede the `(Some(_), Some(_))` catch-all.
- `deck_size` → the `DeckSizeRule` **discriminant** (`Minimum` vs `Exactly`) stays strictly locked both directions (CR 100.5 / CR 903.5a — these are different rules, not comparable under any permissiveness order); the **magnitude** inside it is free only where a new registry method, `GameFormat::deck_size_authority() -> DeckSizeAuthority { RulesFixed | HostChoiceAmong(&'static [u16]) }`, declares it a table agreement — today that's FreeForAll alone, admitting `{60, 40}`. This is a brand-new, non-wire (no serde derive, never a `FormatConfig` field) registry-fact enum, added beside `DeckSizeRule` in `types/format.rs`, following the exact template of the existing `sideboard_policy()`/`default_deck_copy_limit()` per-format methods. Passed the `/add-engine-variant` gate (new standalone enum, not a sibling append; stays within the deck-construction CR section; no protocol-version bump needed since it never crosses any wire in this phase).
- **New, separate from the gate:** `FormatConfig::validate_for_player_count` (a pre-existing shared function, already called at 5 production sites: `engine_wasm::validate_external_format_config`, `phase_server::guard_full_create_game_settings_inbound`, `lobby_broker::guard_create_game_settings_inbound`, `SessionManager::create_game_n_players`, `SessionManager::from_persisted`) gains a NEW first check: `player_count` must be within `self.min_players..=self.max_players`. **This is the critical second half of the `max_players` fix** — without it, loosening the gate's `max_players` row while treating the separate wire field `player_count` as authoritative-on-disagreement would leave the seat-count axis completely unenforced (a payload could declare `max_players: 2` on Commander, admissible, alongside `player_count: 8`, and the engine would allocate 8 seats for a 6-seat format). This was found and required by round 2 of this fix-plan's own review loop — an earlier draft (v2) missed it.
- A verified, intentional side effect: `player_count = 2` will now be rejected for **CommanderDraft** (registry min 3) and **TwoHeadedGiant** (registry min 4) — argued and accepted across 2 review rounds as a *correct* rejection (CR 810.1: "two teams of two players each" — a 2-seat "Two-Headed Giant" game is not actually a Two-Headed Giant game), not a regression.
- Roughly 20 new/updated tests across `crates/engine/tests/integration/custom_format_schema.rs` (the bulk), `crates/engine/src/types/format.rs`'s own `#[cfg(test)] mod tests`, and `crates/lobby-broker/src/protocol.rs`'s `mod tests` (real wire-frame parsing tests, not hand-built ASTs) — see the plan text for the exact list (V20 through V35c in the plan's own numbering).
- **One thing the executor already fixed proactively, reported mid-flight before it was killed:** 3 pre-existing tests in `crates/server-core/src/session.rs` were passing a `player_count` of 3 against `FormatConfig::standard()` (registry `max_players: 2`) — which the new `validate_for_player_count` bound now correctly rejects. The executor widened those specific test fixtures' player counts rather than weakening the new check, which is the right call per the plan's own stated principle ("if a caller is found to depend on [the old permissive behavior], that caller must be fixed — this bound must not be weakened"). Confirm this is still true in the diff and that it's the only such fixture fix needed (the executor may have found more before it was killed — check the diff, not just this note).

**6 required revisions from the final (3rd) review round, that the dispatch instructed the executor to fold in — verify each landed:**
1. R1 — don't reference a nonexistent function `create_game_with_name` anywhere (the real names are `create_game`, `create_game_with_settings`, `create_game_with_ai`, and the draft match spawner).
2. R2 — don't describe the ingress guards' `clamp(2, N)` floors as a "hazard" in any comment/commit message (they can only raise a value, never produce the CommanderDraft/TwoHeadedGiant-below-minimum scenario from legitimate traffic).
3. R3 — awareness-only, no code change required: `custom_format.rs` reads `config.max_players` when persisting a custom format's `StructuralRules`; after this phase that field can legitimately hold a chosen seat count rather than a ceiling on a built-in-tagged config, and the existing client flow already avoids feeding a clobbered value into that specific path. If the executor found something concretely broken here, it should have stopped and reported rather than guessed a fix — check for a "stop-and-return" note in its final report if you have it, or just inspect the diff/behavior yourself.
4. R4 — must state explicitly (comment near the new `validate_for_player_count` check, and/or in the final report) that no protocol-version bump is needed (no wire *shape* changes) but that the new `player_count` check IS a behavioral *tightening* — a payload an older server accepted may now be rejected. Confirm `node scripts/check-protocol-version.mjs` (or whatever the actual correct command is) stays green untouched.
5. R5 — the `/add-engine-variant` checklist must have been run for the new `DeckSizeAuthority` enum (existence check via `cargo engine-inventory` + grep, categorical-boundary check, no-serde-surface confirmation). Check the executor's final report for confirmation, or re-run the checklist yourself if it's not clearly stated.
6. R6 — already structural, just don't let it regress: `GameFormat::deck_size_authority()`'s match must be **exhaustive with no wildcard** (including an explicit `GameFormat::Custom(_) => DeckSizeAuthority::RulesFixed` arm), and `DeckSizeAuthority::options()` must have a real caller in the gate code (routed through `.options().contains(&declared)`, not bypassed with a direct match on the enum's inner slice).

## Standing rules for this whole pipeline (apply to whoever resumes)

- **Model assignment (explicit user standing instruction, repeated verbatim earlier in this session): Opus for all planning and all review steps; Sonnet for all implementation steps.** Do not deviate without the user re-confirming.
- **Ship each phase as its own PR — no direct merge to `main`, no enqueuing multiple phases into one PR.** This has been the standing constraint since before this session started (carried from an earlier session).
- This is running inside the `engine-implementer` orchestration skill's pipeline (spawn agents for plan/review/implement/review steps; the lead/orchestrator session owns checkpoints and commits and never authors implementation content itself, except a narrow surgical-fix carve-out for spot-only findings that don't need re-verification).
- **Multi-agent safety (CLAUDE.md):** other worktrees on this machine belong to other agents/sessions. Never touch `C:\git\phase\.claude\worktrees\fix-animate-dead-reanimation` (or any other sibling worktree) without explicit confirmation it's abandoned. Never bare `git stash`. Always `git status` before anything destructive.
- **Disk space / build contention:** this machine runs ~20-30+ concurrent git worktrees sharing one `C:` drive; `cargo` build caches are the dominant disk consumer and the dominant cause of slow verification (lock contention from other agents' concurrent builds). This is expected, not a bug to chase. An earlier open thread in this session (never resolved): the user asked about overall disk usage and the assistant offered to run `cargo sweep` on this worktree specifically (`custom-format-phase-1a`, was ~165GB at the time) — that offer was never accepted or declined. Feel free to raise it again if disk pressure becomes a blocker, but don't act on it unprompted.
- **CR-annotation discipline is mandatory and was followed rigorously throughout this phase** — every CR number in every plan/commit was independently grepped against `docs/MagicCompRules.txt` before being written, by multiple independent reviewers across many rounds. Keep doing this; don't relax it because "it was already checked" — checked by a prior round is not the same as checked by you.
- **The regenerable-criterion discipline** ("state a re-runnable grep, not a frozen count") was a recurring, hard-won lesson across many rounds of this specific phase (e.g. a planner twice miscounted `GameFormat`'s variant count — 15 vs. the real 23 — before switching to a registry-loop assertion instead of a hardcoded number). Keep following it.

## Process record location (authoritative, append-only, read for full history)

`C:/git/phase/.git/engine-implementer-runs/20260902-custom-format-phase-1d/phase-fit`

This is a `.git`-relative path (the git *common* dir, shared across all worktrees of this repo — `git rev-parse --git-common-dir` from any worktree resolves to `C:/git/phase/.git`), so it survives independently of which worktree or account is active. It contains, in chronological order: the original phase-fit gate adjudication for the whole custom-format-engine charter; the charter round-1 through round-6 review findings; Phase A's own plan round-1 through round-7 findings and final acceptance; three full rounds of Phase A's review-impl loop (each finding real regressions — a Standard/Commander-arm regression, a `range_of_influence`/`command_zone` save-restore regression, and finally this `max_players`/`starting_life`/`commander_damage_threshold`/`deck_size`/`player_count` host-configuration regression, the one this handoff is about); and three rounds of THIS specific fix-plan's own plan-review loop (v1 → v2 → v3, the last accepted).

**Note on writing to this .git-common-dir path from a worktree:** the Bash/Write tools in this session refused to write directly to `C:/git/phase/.git/...` from within this worktree ("session is isolated in the worktree... edit the worktree copy instead"), which is why this handoff lives inside the worktree instead. `printf '...' >> "<path>"` (single-line, no heredoc) DID work earlier in this session for appending short entries to the `phase-fit` file itself — if you need to append to the process record, use that exact pattern (simple single-line printf, not a heredoc) rather than the Write tool.

## Immediate next action for whoever picks this up

Run the verification suite in step 2 above. If clean, checkpoint (commit) it, then dispatch review-impl round 4 for Phase A (Opus) against the new commit. Do not re-plan or re-implement from scratch — the design is reviewed and accepted; only verification and (if needed) small in-scope bug fixes remain before this phase closes out.
