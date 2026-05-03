---
phase: 54-draft-data-pipeline
plan: 01
subsystem: draft-data-pipeline
tags: [draft, data-pipeline, mtgjson, build-tooling]
dependency_graph:
  requires: [53-01, 53-02, 53-03]
  provides: [set-pool-types, mtgjson-extraction, draft-pool-gen-binary, fetch-draft-sets-script]
  affects: [54-02]
tech_stack:
  added: []
  patterns: [mtgjson-per-set-extraction, uuid-to-name-resolution, weighted-sheet-collation]
key_files:
  created:
    - crates/draft-core/src/set_pool.rs
    - crates/draft-core/src/extraction.rs
    - crates/draft-core/src/bin/draft_pool_gen.rs
    - scripts/fetch-draft-sets.sh
  modified:
    - crates/draft-core/src/lib.rs
    - crates/draft-core/Cargo.toml
    - .gitignore
decisions:
  - "Used typed Rarity enum instead of stringly-typed rarity field"
  - "BTreeMap for sheets gives deterministic serialization order"
  - "Sort sheet cards and pack slots by name for deterministic output"
  - "Basic land detection via supertypes field with fallback to land-named sheets"
metrics:
  duration: "7m 23s"
  completed: "2026-05-03T00:20:14Z"
  tasks: 3
  files_created: 4
  files_modified: 3
---

# Phase 54 Plan 01: Draft Data Pipeline — Types, Extraction, and Tooling Summary

MTGJSON booster data extraction pipeline with typed LimitedSetPool, extraction logic resolving UUIDs to card names, CLI binary for batch processing, and download script for per-set JSON files.

## Completed Tasks

| Task | Name | Commit | Key Files |
|------|------|--------|-----------|
| 1 | Define draft pool types and MTGJSON extraction logic | 8aa85d039 | set_pool.rs, extraction.rs, lib.rs, Cargo.toml |
| 2 | Create download script and CLI binary | b41397490 | fetch-draft-sets.sh, draft_pool_gen.rs, Cargo.toml, .gitignore |
| 3 | Extraction tests with fixture data | 4a3d550b5 | extraction.rs (fmt only; tests written inline in Task 1) |

## Implementation Details

### Types (set_pool.rs)
- `LimitedSetPool`: top-level per-set pool with pack variants, sheets, prints, basic lands
- `PackVariant`: weighted pack variant with slot-to-sheet mappings (models MTGJSON's `boosters[]` array)
- `SheetDefinition`: named card pool with weighted card entries and foil/balance_colors flags
- `SheetCard`: card within a sheet with resolved name, rarity, weight
- `PackSlot`: slot in a pack variant pointing to sheet choices
- `LimitedCardPrint`: card printing metadata for the set
- `Rarity`: typed enum (Common, Uncommon, Rare, Mythic, Special, Bonus)

### Extraction (extraction.rs)
- `extract_set_pool(json)` parses MTGJSON per-set JSON, resolves UUIDs to card names via same-file `cards[]` array
- Returns `Ok(None)` for sets without `booster.play` section
- Missing UUIDs logged and skipped gracefully
- `extract_all_set_pools(dir)` batch processes all JSON files in a directory
- 6 inline tests covering: happy path, no-booster sets, invalid JSON, missing UUIDs, rarity mapping, basic land detection

### Tooling
- `scripts/fetch-draft-sets.sh`: downloads per-set MTGJSON files to `data/mtgjson/sets/` with --force and per-set filtering
- `draft-pool-gen` binary: reads `data/mtgjson/sets/*.json`, produces `client/public/draft-pools.json`

## Deviations from Plan

### Note on Task 3

Tests were written inline in extraction.rs as part of Task 1 (Rust convention for `#[cfg(test)]` modules). Task 3 commit contains only the `cargo fmt` formatting fix. No behavioral deviation — all 6 required tests exist and pass.

### Out-of-scope formatting

`cargo fmt -p draft-core` revealed formatting issues in pre-existing Phase 53 files (pick_pass.rs, session.rs, types.rs, validation.rs, view.rs). Per multi-agent safety rules, these were not committed by this agent.

## Verification Results

- `cargo check -p draft-core` — pass
- `cargo build --bin draft-pool-gen` — pass
- `cargo test -p draft-core -- extraction` — 6 tests pass
- `cargo clippy -p draft-core -- -D warnings` — clean
- `scripts/fetch-draft-sets.sh` — executable, contains correct MTGJSON URL
- `data/mtgjson/sets/` — covered by .gitignore
