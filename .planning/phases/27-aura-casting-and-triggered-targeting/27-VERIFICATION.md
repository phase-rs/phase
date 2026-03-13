---
phase: 27-aura-casting-and-triggered-targeting
verified: 2026-03-11T12:00:00Z
status: human_needed
score: 6/6 success criteria verified
re_verification:
  previous_status: gaps_found
  previous_score: 5/6
  gaps_closed:
    - "TriggerTargetSelection UI wired in TargetingOverlay.tsx and GamePage.tsx"
    - "27-CONTEXT.md rewritten to reflect typed data model (no Forge-style params)"
  gaps_remaining: []
  regressions: []
human_verification:
  - test: "Cast an Aura spell (e.g. Sheltered by Ghosts) in the browser against AI opponent"
    expected: "Targeting overlay activates showing valid enchant targets; player selects target; Aura enters battlefield attached to the selected creature; Aura's static ability applies"
    why_human: "Visual rendering and Aura attachment display cannot be verified programmatically"
  - test: "Play Banishing Light or Sheltered by Ghosts — ETB trigger fires requiring target selection"
    expected: "Targeting overlay activates with 'Choose a target for triggered ability' (no Cancel button); player selects a nonland permanent an opponent controls; it is exiled; when the source leaves battlefield, the exiled permanent returns"
    why_human: "End-to-end trigger-target-exile-return flow requires live gameplay verification"
---

# Phase 27: Aura Casting, Triggered Ability Targeting, and "Until Leaves" Exile Return — Verification Report

**Phase Goal:** Implement full Aura spell support (targeting + attachment), triggered ability target selection, and "until source leaves the battlefield" exile return tracking
**Verified:** 2026-03-11T12:00:00Z
**Status:** human_needed
**Re-verification:** Yes — after gap closure (previous status: gaps_found, 5/6)

---

## Goal Achievement

### Observable Truths (from ROADMAP.md Success Criteria)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Aura spells prompt the player for an enchant target during casting and attach to that target on resolution | VERIFIED | `casting.rs` lines 112-147: `is_aura` check + `Keyword::Enchant(filter)` extraction + `find_legal_targets_typed`. `stack.rs` lines 70-86: `attach_to` called after move to Battlefield. 11 aura tests pass. |
| 2 | Triggered abilities with typed target filters prompt the player for target selection before going on the stack | VERIFIED | Engine: `process_triggers` sets `pending_trigger`, returns `WaitingFor::TriggerTargetSelection`. UI: `TargetingOverlay.tsx` line 23 handles both `"TargetSelection"` and `"TriggerTargetSelection"`. `GamePage.tsx` line 507 renders overlay for both variants. |
| 3 | Cards exiled with Duration::UntilHostLeavesPlay return to the battlefield when the source leaves | VERIFIED | `change_zone.rs` records `ExileLink` on exile. `engine.rs` line 465: `check_exile_returns` called in apply loop after SBAs. 4 exile return tests pass. |
| 4 | General filter matching in targeting.rs handles typed TargetFilter (including NonType properties) | VERIFIED | `find_legal_targets_typed` delegates to `filter::matches_target_filter_controlled`. 6 tests pass including NonType/Permanent/Opponent filter variants. |
| 5 | `cargo test --all` passes with new tests covering all three features | VERIFIED | 616 engine + 55 phase-ai + 51 server-core + 42 phase-tauri + 10 phase-server = all passing, 0 failures. |
| 6 | Phase 27 context rewritten to use typed data model (no Forge-style params or SVars) | VERIFIED | `27-CONTEXT.md` updated 2026-03-11: all 5 implementation decision sections describe typed model (`Keyword::Enchant(TargetFilter)`, `find_legal_targets_typed`, `TargetCondition::NonType(CoreType)`, `Duration::UntilHostLeavesPlay`). No `ValidTgts`, `compat_params`, or Forge-style filter strings remain. |

**Score:** 6/6 success criteria verified

---

## Required Artifacts

### Plan 01 Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/engine/src/game/targeting.rs` | `find_legal_targets_typed` function | VERIFIED | Exists with 6 passing tests covering all TargetFilter variants. Called from `casting.rs` and `triggers.rs`. |
| `crates/engine/src/types/game_state.rs` | ExileLink, TriggerTargetSelection, pending_trigger, exile_links | VERIFIED | `ExileLink` struct, `WaitingFor::TriggerTargetSelection` variant, `pending_trigger` and `exile_links` fields all present. |
| `crates/engine/src/types/ability.rs` | `ResolvedAbility.duration` field | VERIFIED | `pub duration: Option<Duration>` with `#[serde(default)]`. |
| `client/src/adapter/types.ts` | TriggerTargetSelection in WaitingFor union | VERIFIED | `{ type: "TriggerTargetSelection"; data: { player: PlayerId; legal_targets: TargetRef[] } }` |
| `data/abilities/sheltered_by_ghosts.json` | Trigger execute with ChangeZone, typed filter, UntilHostLeavesPlay | VERIFIED | Execute field present with correct typed filter and duration. |
| `data/abilities/banishing_light.json` | Execute field with ChangeZone to Exile | VERIFIED | Execute field present with correct typed filter. |
| `data/abilities/oblivion_ring.json` | First trigger execute field | VERIFIED | First trigger has execute; second trigger correctly has no execute (ExileLink handles return). |

### Plan 02 Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/engine/src/game/casting.rs` | Aura targeting using find_legal_targets_typed | VERIFIED | Lines 112-147: full Aura detection and targeting flow. |
| `crates/engine/src/game/stack.rs` | Aura attachment on resolution | VERIFIED | Lines 70-86: post-battlefield-entry Aura attachment via `effects::attach::attach_to`. |

### Plan 03 Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/engine/src/game/triggers.rs` | Target detection in process_triggers | VERIFIED | `extract_target_filter_from_effect` at line 253. Auto-target/multi-target/skip logic at line 155. |
| `crates/engine/src/game/engine.rs` | TriggerTargetSelection handler + check_exile_returns | VERIFIED | `TriggerTargetSelection` match arm at line 415. `check_exile_returns` at line 820, wired at line 465. |
| `crates/engine/src/game/effects/change_zone.rs` | ExileLink recording on UntilHostLeavesPlay | VERIFIED | Lines 45-55: `exile_links.push(ExileLink { ... })` when duration matches. |
| `client/src/components/targeting/TargetingOverlay.tsx` | Handles both TargetSelection and TriggerTargetSelection | VERIFIED | Line 23: `isTargetSelection = type === "TargetSelection" \|\| type === "TriggerTargetSelection"`. Line 97: distinct instruction text for trigger targeting. Line 111: no Cancel button for trigger targeting (mandatory). |
| `client/src/pages/GamePage.tsx` | Renders TargetingOverlay for TriggerTargetSelection | VERIFIED | Line 507: `(waitingFor?.type === "TargetSelection" \|\| waitingFor?.type === "TriggerTargetSelection") && waitingFor.data.player === playerId && <TargetingOverlay />` |

---

## Key Link Verification

| From | To | Via | Status | Details |
|------|----|-----|--------|---------|
| `casting.rs` | `targeting.rs` | `targeting::find_legal_targets_typed` | WIRED | Line 126: call with `(&filter, player, object_id)` |
| `stack.rs` | `effects/attach.rs` | `attach::attach_to` | WIRED | Line 83: `effects::attach::attach_to(state, entry.id, *target_id)` |
| `triggers.rs` | `targeting.rs` | `targeting::find_legal_targets_typed` | WIRED | Line 157: call with `(target_filter, trigger.controller, trigger.source_id)` |
| `engine.rs` | `triggers.rs` | `state.pending_trigger` | WIRED | Lines 430-433: `state.pending_trigger.take()` in TriggerTargetSelection handler |
| `change_zone.rs` | `game_state.rs` | `exile_links.push` | WIRED | Lines 50-54: `state.exile_links.push(ExileLink { exiled_id, source_id })` |
| `engine.rs` | `game_state.rs` | `check_exile_returns` | WIRED | Line 465 in apply loop |
| `TargetingOverlay.tsx` | engine `TriggerTargetSelection` | `isTargetSelection` flag | WIRED | Line 23: both types activate targeting mode; `pendingCast` is null for trigger case (correct) |
| `GamePage.tsx` | `TargetingOverlay.tsx` | conditional render | WIRED | Line 507: both WaitingFor variants trigger overlay render |

---

## Requirements Coverage

P27- requirement IDs are defined in ROADMAP.md (not in REQUIREMENTS.md).

| Requirement | Source Plan(s) | Description | Status | Evidence |
|-------------|---------------|-------------|--------|----------|
| P27-AURA | 27-02 | Aura spell casting with enchant target selection and attachment | SATISFIED | `casting.rs` + `stack.rs` implement full Aura flow; 11 tests pass |
| P27-TRIG | 27-03, 27-04 | Triggered ability target selection before stack placement | SATISFIED | Engine complete; `TargetingOverlay.tsx` and `GamePage.tsx` both handle `TriggerTargetSelection` |
| P27-EXILE | 27-03 | "Until leaves battlefield" exile return tracking | SATISFIED | `ExileLink` recorded in `change_zone.rs`; `check_exile_returns` fires correctly; 4 tests pass |
| P27-FILTER | 27-01 | Typed TargetFilter matching including NonType properties | SATISFIED | `find_legal_targets_typed` + `filter::matches_target_filter_controlled`; 6 tests pass |
| P27-TEST | 27-01, 27-02, 27-03 | `cargo test --all` passes with new tests | SATISFIED | 774+ tests pass across workspace, 0 failures |
| P27-TYPED | 27-01, 27-04 | Phase 27 context rewritten to typed data model | SATISFIED | `27-CONTEXT.md` updated 2026-03-11; all Forge-style references replaced with typed model |

**Orphaned requirement check:** No P27- IDs found in `.planning/REQUIREMENTS.md` that are not claimed by a plan.

---

## Anti-Patterns Found

No TODO/FIXME/placeholder comments, empty return stubs, or wiring red flags found in modified source files. All implementations are substantive.

---

## Human Verification Required

### 1. Aura Visual Attachment

**Test:** In the browser, cast an Aura spell (e.g. Sheltered by Ghosts) against the AI. Select a legal enchant target from the targeting overlay.
**Expected:** Targeting overlay activates showing valid enchant targets. After selection, the Aura enters the battlefield rendered visually attached to the target creature. The Aura's static ability applies to the enchanted creature.
**Why human:** Visual rendering of Aura attachment and static ability application cannot be verified programmatically.

### 2. Triggered Ability Target Selection End-to-End

**Test:** Play Banishing Light (or Sheltered by Ghosts). Allow the ETB trigger to fire.
**Expected:** Targeting overlay activates with the message "Choose a target for triggered ability" and no Cancel button visible. Player selects a nonland permanent an opponent controls. Permanent moves to exile. When Banishing Light leaves the battlefield, the exiled permanent returns to the battlefield.
**Why human:** Full trigger-target-exile-return chain requires live gameplay to verify correct ordering, UI responsiveness, and return timing.

---

## Gaps Summary

No gaps remain. Both previously-identified gaps have been closed:

**Gap 1 — TriggerTargetSelection UI (was Blocker) — CLOSED.** `TargetingOverlay.tsx` now handles both `"TargetSelection"` and `"TriggerTargetSelection"` on line 23. `GamePage.tsx` renders the overlay for both on line 507. The overlay shows distinct instruction text for trigger targeting ("Choose a target for triggered ability") and correctly omits the Cancel button since triggered abilities are mandatory in MTG.

**Gap 2 — 27-CONTEXT.md not updated (was Minor) — CLOSED.** `27-CONTEXT.md` was rewritten on 2026-03-11. All five implementation decision sections now describe the typed model. No Forge-style references remain.

All 6 success criteria verified. Phase goal is achieved. Human verification is needed only to confirm visual and interactive behavior in the live browser UI.

---

_Verified: 2026-03-11T12:00:00Z_
_Verifier: Claude (gsd-verifier)_
