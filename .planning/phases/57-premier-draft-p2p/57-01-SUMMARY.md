---
phase: 57-premier-draft-p2p
plan: 01
subsystem: draft-core
tags: [tournament, pairing, swiss, single-elimination, standings, view]
dependency_graph:
  requires: []
  provides: [TournamentFormat, PodPolicy, PickStatus, Swiss pairing, SE bracket, match results, standings view]
  affects: [draft-wasm DraftConfig construction]
tech_stack:
  added: []
  patterns: [Swiss bracket pairing with rematch avoidance, seeded SE bracket, reducer-computed standings]
key_files:
  created: []
  modified:
    - crates/draft-core/src/types.rs
    - crates/draft-core/src/session.rs
    - crates/draft-core/src/view.rs
    - crates/draft-core/src/pick_pass.rs
    - crates/draft-wasm/src/lib.rs
decisions:
  - Swiss pairing uses ChaCha20Rng seeded from config.rng_seed XOR round for deterministic shuffle
  - TournamentFormat and PodPolicy have Default derives (Swiss, Competitive) with serde(default) on DraftConfig fields for backward compatibility
  - PickStatus derived from session.current_pack presence (not per-seat pick tracking which is P2P host responsibility)
  - PairingView winner_seat determined by comparing cumulative match_wins between the two pairing players
metrics:
  duration_seconds: 634
  completed: 2026-05-03T10:54:08Z
  tasks_completed: 3
  tasks_total: 3
  files_modified: 5
---

# Phase 57 Plan 01: Tournament Core Types and Reducers Summary

Swiss/SE tournament pairing generation, match result recording, round advancement, and standings view computation in draft-core reducer.

## Tasks Completed

| Task | Name | Commit | Key Changes |
|------|------|--------|-------------|
| 1 | Add TournamentFormat, PodPolicy, PickStatus enums and extend types | c77f89b1c | 3 new enums, DraftConfig extended, DraftAction/DraftDelta new variants, DraftSession.current_round |
| 2 | Implement pairing, result, round, seat replacement reducers | 94c7f980e | Swiss pairing with rematch avoidance, SE seeded bracket, match result recording, round advancement, seat replacement |
| 3 | Extend DraftPlayerView with pick_status, standings, timer, pairings | c3bcbc158 | StandingEntry, PairingView structs, per-seat pick_status, standings computation, pairing views |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Added Default derives and serde(default) for DraftConfig backward compatibility**
- **Found during:** Task 1
- **Issue:** draft-wasm crate constructs DraftConfig from JS-provided JSON. Adding required fields would break existing callers.
- **Fix:** Added `#[derive(Default)]` with `#[default]` on Swiss/Competitive variants, and `#[serde(default)]` on the new DraftConfig fields.
- **Files modified:** crates/draft-core/src/types.rs, crates/draft-wasm/src/lib.rs

**2. [Rule 1 - Bug] Fixed apply_submit_deck transition for Premier/Traditional drafts**
- **Found during:** Task 2
- **Issue:** `apply_submit_deck` transitioned to `DraftStatus::Complete` when all decks were submitted, but Premier/Traditional drafts need tournament play after deckbuilding.
- **Fix:** Changed transition to `DraftStatus::Pairing` for Premier/Traditional, keeping `DraftStatus::Complete` for Quick Draft only.
- **Files modified:** crates/draft-core/src/session.rs
- **Commit:** 94c7f980e

**3. [Rule 1 - Bug] Adjusted pick_status test expectations**
- **Found during:** Task 3
- **Issue:** Plan suggested pick_status would show Picked after individual seat picks, but the reducer uses simultaneous pick/pass (all 8 seats pick before packs pass), so individual pick tracking isn't available at the session level.
- **Fix:** pick_status shows Pending during drafting when current_pack is Some, Picked only when current_pack is None (between pack rounds). Per-seat pick tracking is the P2P host's responsibility.
- **Files modified:** crates/draft-core/src/view.rs

## Self-Check: PASSED
