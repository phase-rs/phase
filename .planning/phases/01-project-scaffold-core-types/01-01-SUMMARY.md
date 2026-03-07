---
phase: 01-project-scaffold-core-types
plan: 01
subsystem: engine
tags: [rust, wasm, serde, cargo-workspace, wasm-bindgen, tsify, mtg-types]

requires:
  - phase: none
    provides: greenfield project

provides:
  - Cargo workspace with engine (lib) and engine-wasm (cdylib) crates
  - All core MTG type definitions with serde serialization
  - WASM build pipeline (cargo build, wasm-bindgen, wasm-opt)
  - Tagged enum discriminated union pattern for GameAction/GameEvent

affects: [02-card-parser, 03-engine-core, 04-abilities, 05-triggers-combat, 06-layers, 07-ui, 08-ai]

tech-stack:
  added: [serde 1.x, rpds 1.2, wasm-bindgen 0.2.114, tsify 0.5, serde-wasm-bindgen 0.6]
  patterns: [tagged-enum-discriminated-unions, newtype-identifiers, workspace-separation]

key-files:
  created:
    - Cargo.toml
    - crates/engine/src/types/mod.rs
    - crates/engine/src/types/actions.rs
    - crates/engine/src/types/events.rs
    - crates/engine/src/types/game_state.rs
    - crates/engine-wasm/src/lib.rs
    - scripts/build-wasm.sh
  modified: []

key-decisions:
  - "Used tsify (not tsify-next) per RUSTSEC-2025-0048 advisory"
  - "Newtype wrappers in engine-wasm for tsify rather than feature flags on engine types"
  - "Simple enums (Zone, Phase, ManaColor) serialize as strings; complex enums (GameAction, GameEvent) use tag+content"
  - "Standard Rust collections for Phase 1; rpds deferred to Phase 3 state management"

patterns-established:
  - "Tagged enum pattern: #[serde(tag = 'type', content = 'data')] for action/event discriminated unions"
  - "Newtype identifiers: CardId(u64), ObjectId(u64), PlayerId(u8) for type safety"
  - "Workspace separation: pure engine crate (no WASM deps) + thin engine-wasm binding crate"

requirements-completed: [PLAT-03]

duration: 4min
completed: 2026-03-07
---

# Phase 1 Plan 01: Project Scaffold & Core Types Summary

**Cargo workspace with dual-target Rust engine (native + WASM), all MTG core types with tagged enum serialization, and wasm-bindgen exports**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-07T20:08:07Z
- **Completed:** 2026-03-07T20:12:25Z
- **Tasks:** 2
- **Files modified:** 19

## Accomplishments

- Cargo workspace compiles for both native and wasm32-unknown-unknown targets
- All core MTG types defined: GameState, GameAction, GameEvent, Zone, Phase, ManaColor, ManaPool, Player, CardDefinition, CardId, ObjectId, PlayerId
- 33 serialization tests covering tagged union format, roundtrips, defaults, and arithmetic
- engine-wasm exports ping() and create_initial_state() via wasm_bindgen with tsify type re-exports
- WASM build script with three-step pipeline (cargo build, wasm-bindgen, wasm-opt)

## Task Commits

Each task was committed atomically:

1. **Task 1: Create Cargo workspace with engine and engine-wasm crates** - `8263517` (feat)
2. **Task 2: Define all core MTG type definitions with serialization tests** - `8be28dc` (test)

## Files Created/Modified

- `Cargo.toml` - Workspace root with resolver 2, workspace deps, WASM-optimized release profile
- `.cargo/config.toml` - WASM build aliases
- `.gitignore` - Rust, Node, WASM output, OS entries
- `crates/engine/Cargo.toml` - Engine library crate config
- `crates/engine/src/lib.rs` - Module declaration
- `crates/engine/src/types/mod.rs` - Re-exports all type modules
- `crates/engine/src/types/identifiers.rs` - CardId, ObjectId newtypes
- `crates/engine/src/types/mana.rs` - ManaColor enum, ManaPool struct with arithmetic
- `crates/engine/src/types/phase.rs` - Phase enum (12 MTG turn phases)
- `crates/engine/src/types/zones.rs` - Zone enum (7 MTG zones)
- `crates/engine/src/types/card.rs` - CardDefinition stub
- `crates/engine/src/types/player.rs` - PlayerId, Player with life and mana pool
- `crates/engine/src/types/actions.rs` - GameAction enum with tagged serde
- `crates/engine/src/types/events.rs` - GameEvent enum with tagged serde
- `crates/engine/src/types/game_state.rs` - GameState struct with 2-player default
- `crates/engine-wasm/Cargo.toml` - WASM binding crate config (cdylib)
- `crates/engine-wasm/src/lib.rs` - wasm_bindgen exports + tsify re-exports
- `scripts/build-wasm.sh` - Three-step WASM build pipeline

## Decisions Made

- Used tsify (not tsify-next) per RUSTSEC-2025-0048 unmaintained advisory
- Newtype wrappers in engine-wasm for tsify rather than feature flags on engine types -- avoids coupling engine crate to WASM concerns
- Simple enums (Zone, Phase, ManaColor) serialize as plain strings for ergonomic TypeScript usage; complex enums (GameAction, GameEvent) use tag+content for discriminated unions
- Standard Rust collections for Phase 1 types; rpds deferred to Phase 3 when state management is built

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed clippy derivable_impls warnings**
- **Found during:** Task 2
- **Issue:** Manual Default impls for Phase and PlayerId were flagged as derivable
- **Fix:** Changed to `#[derive(Default)]` with `#[default]` attribute
- **Files modified:** phase.rs, player.rs
- **Committed in:** 8be28dc (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (1 bug)
**Impact on plan:** Minor clippy compliance fix. No scope creep.

## Issues Encountered

None

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- Engine crate ready for Phase 2 card parser (CardDefinition stub in place)
- All core types available for Phase 3 engine core (GameState, GameAction, GameEvent)
- WASM binding layer ready for Phase 7 UI integration
- Concern from research: verify rpds API coverage during Phase 3 (deferred per recommendation)

## Self-Check: PASSED

All key files verified present. Both task commits (8263517, 8be28dc) verified in git log.

---
*Phase: 01-project-scaffold-core-types*
*Completed: 2026-03-07*
