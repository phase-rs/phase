---
phase: 53-limited-draft-core
plan: 02
subsystem: draft-core
tags: [draft, types, validation, pack-source, crate-creation]
dependency_graph:
  requires: []
  provides: [draft-core-crate, DraftKind, DraftSession, PackSource-trait, LimitedDeckValidator]
  affects: [53-03-PLAN]
tech_stack:
  added: [draft-core crate]
  patterns: [workspace crate, trait-based pack abstraction, multiset validation]
key_files:
  created:
    - crates/draft-core/Cargo.toml
    - crates/draft-core/src/lib.rs
    - crates/draft-core/src/types.rs
    - crates/draft-core/src/pack_source.rs
    - crates/draft-core/src/validation.rs
  modified: []
decisions:
  - "Used String instead of &'static str for DraftError::InvalidTransition.action to enable serde roundtrip"
  - "FixturePackSource ignores RNG parameter — determinism from seat/pack_number, real PackSource in Phase 54 uses RNG"
  - "PackSource::generate_pack takes &mut dyn RngCore for trait-object safety"
metrics:
  duration: 486s
  completed: "2026-05-02T00:00:00Z"
  tasks_completed: 2
  tasks_total: 2
  tests_added: 21
---

# Phase 53 Plan 02: draft-core Crate Foundation Summary

New draft-core workspace crate with all draft types, PackSource trait with FixturePackSource, and LimitedDeckValidator with multiset pool validation and unlimited basic lands.

## Tasks Completed

| Task | Name | Commit | Key Files |
|------|------|--------|-----------|
| 1 | Create draft-core crate with types and DraftKind | 159c178b4 | Cargo.toml, lib.rs, types.rs, pack_source.rs, validation.rs |
| 2 | PackSource trait + FixturePackSource + LimitedDeckValidator tests | 509746546 | pack_source.rs, validation.rs |

## Implementation Details

### Types (types.rs)
- **DraftKind**: Quick (1 human, Bo1), Premier (8 humans, Bo1), Traditional (8 humans, Bo3) — all pod_size=8
- **PassDirection**: Left/Right with `for_pack()` (even=Left, odd=Right) and `next_seat()` wrapping
- **DraftStatus**: 9 lifecycle states (Lobby through Abandoned)
- **DraftSession**: Full session state with seats, packs, pools, submissions, pairings, match records
- **DraftAction/DraftDelta**: Action/event discriminated unions for reducer pattern
- **DraftError**: Typed errors with thiserror derives

### PackSource (pack_source.rs)
- **PackSource trait**: `generate_pack(&self, rng, seat, pack_number) -> DraftPack` — public, implementable by external crates
- **FixturePackSource**: Deterministic test fixture generating cards named "{set_code} Card {seat}-{pack}-{i}"

### Validation (validation.rs)
- **validate_limited_deck**: Multiset validation — checks min deck size, pool membership, copy counts
- **STANDARD_BASIC_LANDS**: Plains, Island, Swamp, Mountain, Forest, Wastes — unlimited in pool
- **LimitedDeckError**: TooFewCards, NotInPool, ExceedsPoolCount — errors accumulate (no early return)

## Test Coverage

21 tests across 3 modules:
- **types** (9): DraftKind pod_size/human_seats/match_config, PassDirection for_pack/next_seat, serde roundtrips
- **pack_source** (4): correct card count, determinism, different-seat differentiation, naming convention
- **validation** (8): valid 40-card, too few, not in pool, exceeds pool count, unlimited basics, Wastes, error accumulation, pool duplicates

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] DraftError::InvalidTransition uses String instead of &'static str**
- **Found during:** Task 1
- **Issue:** Plan specified `action: &'static str` but this prevents serde Deserialize (can't deserialize into a static reference)
- **Fix:** Changed to `action: String` for full serde roundtrip support
- **Files modified:** types.rs
- **Commit:** 159c178b4

## Self-Check: PASSED
