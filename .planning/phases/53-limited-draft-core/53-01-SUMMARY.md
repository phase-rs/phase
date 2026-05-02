---
phase: 53-limited-draft-core
plan: 01
subsystem: engine-formats
tags: [format, limited, draft, engine]
dependency_graph:
  requires: []
  provides: [GameFormat::Limited, FormatConfig::limited(), FormatGroup::Limited]
  affects: [draft-core, engine-wasm, server-core]
tech_stack:
  added: []
  patterns: [exhaustive-match-extension, format-registry]
key_files:
  created: []
  modified:
    - crates/engine/src/types/format.rs
    - crates/engine/src/game/deck_validation.rs
decisions:
  - "Limited sideboard policy is Unlimited (rest of pool acts as sideboard)"
  - "Limited legality_format returns None (pool validation deferred to draft-core LimitedDeckValidator)"
  - "Limited does not grant free first mulligan (consistent with constructed 2-player rules)"
metrics:
  duration: "9m 1s"
  completed: "2026-05-02"
  tasks_completed: 1
  tasks_total: 1
  test_results: "6030 passed, 8 ignored, 0 failed"
---

# Phase 53 Plan 01: Add GameFormat::Limited Summary

GameFormat::Limited variant with FormatGroup::Limited, FormatConfig::limited() (40-card min, 20 life, 2-player), and full format registry entry (LIM short label).

## Task Results

| Task | Name | Commit | Files |
| ---- | ---- | ------ | ----- |
| 1 (RED) | Add failing tests for GameFormat::Limited | 3bb45ac95 | crates/engine/src/types/format.rs |
| 1 (GREEN) | Implement GameFormat::Limited with all match arms | 0ec28eea7 | crates/engine/src/types/format.rs, crates/engine/src/game/deck_validation.rs |

## TDD Gate Compliance

- RED gate: `test(53-01)` commit 3bb45ac95 -- 8 new tests fail (Limited variant does not exist)
- GREEN gate: `feat(53-01)` commit 0ec28eea7 -- all 28 format tests pass, 6030 engine tests pass
- REFACTOR gate: not needed (implementation is minimal, no cleanup required)

## Implementation Details

### Changes to format.rs
- Added `Limited` variant to `FormatGroup` enum
- Added `Limited` variant to `GameFormat` enum (between Standard and Commander)
- Added `FormatConfig::limited()` constructor: 20 life, 2 players, 40-card deck, no singleton, no command zone
- Added `GameFormat::Limited` to all 5 exhaustive match arms: `legality_format` (None), `sideboard_policy` (Unlimited), `grants_free_first_mulligan` (not included, false), `label` ("Limited"), `for_format` (limited())
- Added registry entry with short_label "LIM", group FormatGroup::Limited

### Changes to deck_validation.rs
- Added `GameFormat::Limited` to the compatibility pass-through arm alongside FreeForAll/TwoHeadedGiant (Limited deck validation handled by draft-core, not constructed validator)

### Tests Added (8 new)
- `format_config_limited` -- all field values
- `limited_legality_format_is_none` -- no constructed legality
- `limited_sideboard_policy_is_unlimited` -- rest of pool is sideboard
- `limited_no_free_first_mulligan` -- no free mulligan
- `limited_label` -- "Limited" display name
- `limited_for_format_roundtrip` -- for_format(Limited) == limited()
- `limited_in_registry` -- registry contains LIM entry with FormatGroup::Limited
- Added `FormatConfig::limited()` to existing `format_config_serde_roundtrip` test

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Updated deck_validation.rs exhaustive match**
- **Found during:** Task 1 GREEN phase
- **Issue:** Adding Limited to GameFormat caused non-exhaustive match in `deck_validation.rs:635` (`format_compatibility_check`)
- **Fix:** Added `GameFormat::Limited` to the pass-through arm with FreeForAll/TwoHeadedGiant (returns `true` -- Limited deck validation is handled by draft-core's LimitedDeckValidator, not the constructed validator)
- **Files modified:** crates/engine/src/game/deck_validation.rs
- **Commit:** 0ec28eea7

## Known Stubs

None -- all code paths are fully wired.

## Verification

- `cargo test -p engine -- format`: 28 passed
- `cargo test -p engine`: 6030 passed, 8 ignored, 0 failed
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --all -- --check`: clean

## Self-Check: PASSED
