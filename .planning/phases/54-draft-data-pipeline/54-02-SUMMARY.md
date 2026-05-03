---
phase: 54-draft-data-pipeline
plan: 02
subsystem: draft-data-pipeline
tags: [draft, pack-generator, weighted-selection, pack-source]
dependency_graph:
  requires: [54-01]
  provides: [pack-generator, mtgjson-pack-source]
  affects: [55-draft-server, 56-draft-frontend]
tech_stack:
  added: []
  patterns: [weighted-random-selection, without-replacement-sampling]
key_files:
  created:
    - crates/draft-core/src/pack_generator.rs
  modified:
    - crates/draft-core/src/lib.rs
decisions:
  - "Weighted selection uses cumulative-sum walk with precomputed total_weight"
  - "Without-replacement sampling uses swap_remove on mutable pool for O(n) per pick"
  - "Rarity serialized via Debug trait to_lowercase for consistency with DraftCardInstance string field"
metrics:
  duration: "4m 55s"
  completed: "2026-05-03T00:29:00Z"
  tasks: 2
  files_created: 1
  files_modified: 1
---

# Phase 54 Plan 02: PackGenerator — Weighted Pack Generation from LimitedSetPool Summary

PackGenerator implementing PackSource trait with weighted variant selection and weighted card-from-sheet selection without replacement, backed by LimitedSetPool data from Plan 01.

## Completed Tasks

| Task | Name | Commit | Key Files |
|------|------|--------|-----------|
| 1 | Implement PackGenerator with weighted selection and PackSource trait | 6679d34e7 | pack_generator.rs, lib.rs |
| 2 | PackGenerator tests -- determinism, slot counts, weighted distribution | 7810898df | pack_generator.rs |

## Implementation Details

### PackGenerator (pack_generator.rs)
- `PackGenerator::new(set_pool)` wraps a `LimitedSetPool`
- `impl PackSource for PackGenerator` generates deterministic packs from `(seed, seat, pack_number)`
- Pack generation flow: select variant by weight -> for each slot, resolve sheet by weight -> pick N cards from sheet without replacement by card weight
- `weighted_select()`: cumulative-sum walk over `(index, weight)` pairs with precomputed total
- `weighted_select_n()`: without-replacement sampling using swap_remove on a mutable pool copy
- Instance IDs formatted as `"{set_code}-{seat}-{pack}-{card_index}"` matching FixturePackSource convention
- No set-specific special-case code -- bonus sheets, Mystical Archive, etc. are just different sheet configs in the pool data

### Test Coverage (8 tests)
1. `test_deterministic_generation`: Same seed + seat + pack = identical output
2. `test_correct_pack_size`: Pack has exactly sum(slot.count) cards (14 = 10+3+1)
3. `test_no_duplicate_cards_in_pack`: All instance_ids and card names unique within a pack
4. `test_variant_weight_distribution`: Over 2000 iterations, weight-1/10 variant selected ~10% (100-350 range)
5. `test_different_seats_different_packs`: Different seat indices produce different instance_ids
6. `test_set_code_matches`: All cards have correct set_code from pool
7. `test_rarity_from_sheet`: Cards from rareMythic sheet have "rare" or "mythic" rarity string
8. `test_bonus_sheet_variant`: Both pack variants reachable over 100 iterations

## Deviations from Plan

None - plan executed exactly as written.

## Verification Results

- `cargo check -p draft-core` -- pass
- `cargo test -p draft-core -- pack_generator` -- 8 tests pass
- `cargo clippy -p draft-core -- -D warnings` -- clean
- `rustfmt --check pack_generator.rs` -- clean
- PackGenerator implements PackSource trait (verified by compilation)
- No set-specific special-case code in pack_generator.rs

## Self-Check: PASSED

- pack_generator.rs: FOUND
- Commit 6679d34e7: FOUND
- Commit 7810898df: FOUND
