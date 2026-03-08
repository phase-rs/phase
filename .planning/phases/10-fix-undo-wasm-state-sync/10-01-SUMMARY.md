---
phase: 10-fix-undo-wasm-state-sync
plan: 01
subsystem: engine
tags: [wasm, undo, state-sync, wasm-bindgen, zustand]

# Dependency graph
requires:
  - phase: 07-ui-game-board
    provides: WASM adapter layer and gameStore with undo stateHistory
provides:
  - restore_game_state WASM export for state replacement with RNG reconstruction
  - EngineAdapter.restoreState interface method across all adapters
  - gameStore.undo() wired to sync WASM engine via adapter.restoreState
affects: []

# Tech tracking
tech-stack:
  added: [rand_chacha (engine-wasm)]
  patterns: [WASM state restoration with RNG seed reconstruction]

key-files:
  created: []
  modified:
    - crates/engine-wasm/Cargo.toml
    - crates/engine-wasm/src/lib.rs
    - client/src/adapter/types.ts
    - client/src/adapter/wasm-adapter.ts
    - client/src/adapter/tauri-adapter.ts
    - client/src/adapter/ws-adapter.ts
    - client/src/stores/gameStore.ts
    - client/src/stores/__tests__/gameStore.test.ts

key-decisions:
  - "RNG reconstructed from rng_seed after deserialization to avoid default seed 0"
  - "restoreState is synchronous (no queue needed) since undo is user-initiated"

patterns-established:
  - "State restoration pattern: deserialize + reconstruct non-serializable fields (RNG)"

requirements-completed: [QOL-02]

# Metrics
duration: 3min
completed: 2026-03-08
---

# Phase 10 Plan 01: Fix Undo/WASM State Sync Summary

**restore_game_state WASM binding with RNG reconstruction, wired through EngineAdapter to gameStore.undo()**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-08T17:10:03Z
- **Completed:** 2026-03-08T17:13:09Z
- **Tasks:** 2
- **Files modified:** 8

## Accomplishments
- Added restore_game_state WASM export that deserializes GameState and replaces thread-local with RNG reconstruction from seed
- Extended EngineAdapter interface with restoreState across all three adapter implementations
- Wired gameStore.undo() to call adapter.restoreState(previous) before setting client state, fixing the desync bug

## Task Commits

Each task was committed atomically:

1. **Task 1: Add restore_game_state WASM binding and restoreState to adapter layer** - `9642828` (feat)
2. **Task 2: Wire gameStore.undo() to call adapter.restoreState and add tests** - `18d289a` (feat)

## Files Created/Modified
- `crates/engine-wasm/Cargo.toml` - Added rand_chacha dependency
- `crates/engine-wasm/src/lib.rs` - Added restore_game_state WASM binding
- `client/src/adapter/types.ts` - Added restoreState to EngineAdapter interface
- `client/src/adapter/wasm-adapter.ts` - WasmAdapter.restoreState calls WASM binding
- `client/src/adapter/tauri-adapter.ts` - TauriAdapter.restoreState throws unsupported
- `client/src/adapter/ws-adapter.ts` - WebSocketAdapter.restoreState throws unsupported
- `client/src/stores/gameStore.ts` - undo() calls adapter.restoreState before state revert
- `client/src/stores/__tests__/gameStore.test.ts` - Tests for restoreState call, null adapter, empty history

## Decisions Made
- RNG reconstructed from rng_seed after deserialization (serde skips RNG, default would be seed 0)
- restoreState is synchronous on WasmAdapter (no queue needed, undo is user-initiated single call)
- TauriAdapter and WebSocketAdapter throw AdapterError (undo not supported for those transports)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- Pre-existing clippy warnings in engine crate (34 errors) prevent `cargo clippy -p engine-wasm` from passing; these are not caused by this plan's changes. `cargo check -p engine-wasm` confirms clean compilation.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Undo/WASM state sync bug is fixed
- No further phases depend on this fix

---
*Phase: 10-fix-undo-wasm-state-sync*
*Completed: 2026-03-08*
