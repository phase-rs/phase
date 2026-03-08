# Phase 10: Fix Undo/WASM State Sync - Research

**Researched:** 2026-03-08
**Domain:** WASM state management, Zustand store synchronization
**Confidence:** HIGH

## Summary

The undo system in `gameStore.ts` restores client-side `GameState` from a history stack but does not synchronize the WASM engine's thread-local `GAME_STATE`. After undo, the next `submitAction` operates on the WASM engine's un-reverted state, causing a desync between what the UI shows and what the engine computes.

The fix requires three coordinated changes: (1) a new WASM binding `restore_game_state` that deserializes a `GameState` from JS and replaces the thread-local `GAME_STATE`, (2) a new `restoreState` method on the `EngineAdapter` interface and its implementations, and (3) wiring `gameStore.undo()` to call `adapter.restoreState()` after reverting client state.

**Primary recommendation:** Add a `restore_game_state(state: JsValue)` WASM export, propagate it through the adapter layer, and call it in `gameStore.undo()`.

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| QOL-02 | Keyboard shortcuts (pass turn, full control, tap all lands) | Keyboard shortcuts already implemented in Phase 7 (useKeyboardShortcuts.ts). This phase fixes the undo desync that breaks the Z-key undo shortcut's post-undo gameplay. The remaining QOL-02 gap is the WASM state sync, not the shortcuts themselves. |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| wasm-bindgen | 0.2.114 | Rust<->JS bridge for WASM exports | Already used; all existing WASM bindings use it |
| serde-wasm-bindgen | (current) | JsValue <-> Rust struct serialization | Already used for all state transfer across WASM boundary |
| zustand | 5.x | Client state management | Already used; gameStore manages undo history |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| rand_chacha | (current) | Deterministic RNG reconstruction | RNG is `serde(skip)` on GameState; must reconstruct from seed after deserialization |
| vitest | (current) | Frontend testing | Existing test infrastructure for gameStore tests |

No new dependencies required. All changes use existing libraries.

## Architecture Patterns

### Current Architecture (the bug)

```
User presses Z (undo)
  → gameStore.undo() pops stateHistory
  → Client gameState restored to previous snapshot
  → WASM GAME_STATE still holds LATEST state (not reverted)
  → User takes action
  → adapter.submitAction() → WASM submit_action()
  → Engine applies action to WRONG (latest) state
  → DESYNC: client shows one thing, engine computed another
```

### Target Architecture (the fix)

```
User presses Z (undo)
  → gameStore.undo() pops stateHistory
  → Client gameState restored to previous snapshot
  → adapter.restoreState(previousState) called
  → WASM restore_game_state(state_js) deserializes & replaces thread-local
  → RNG reconstructed from rng_seed
  → User takes action
  → adapter.submitAction() → WASM submit_action()
  → Engine applies action to RESTORED state
  → IN SYNC
```

### Pattern: WASM State Injection

The key pattern already exists in the codebase. `initialize_game()` sets the thread-local via:
```rust
GAME_STATE.with(|gs| {
    *gs.borrow_mut() = Some(state);
});
```

The new `restore_game_state` follows the identical pattern but deserializes from JS input instead of constructing fresh.

### RNG Reconstruction

**Critical detail:** `GameState.rng` is `#[serde(skip)]` with a `default_rng()` fallback (seed 0). After deserialization, the RNG must be reconstructed from `rng_seed`:

```rust
state.rng = ChaCha20Rng::seed_from_u64(state.rng_seed);
```

This pattern is already proven in the existing `game_state_serializes_and_roundtrips` test (line 278 of `game_state.rs`). However, note that reconstructing from the original seed means the RNG stream position is reset to the beginning, not to where it was mid-game. For undo of unrevealed-information actions this is acceptable since those actions don't consume RNG, but this is worth documenting.

### Anti-Patterns to Avoid

- **Sending full state on every action:** Don't change `submitAction` to also send state. The current pattern (engine owns mutable state, client reads snapshots) is correct. Only undo needs injection.
- **Storing RNG stream position:** Don't try to track how many RNG bytes were consumed. The undo-eligible actions (PassPriority, DeclareAttackers, DeclareBlockers, ActivateAbility) don't use RNG, so seed-based reconstruction is sufficient.
- **Adding undo to WebSocketAdapter:** Multiplayer undo would require server-side state management changes. Keep `restoreState` as a no-op or throw in WsAdapter.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| JS-to-Rust state deserialization | Manual field-by-field reconstruction | `serde_wasm_bindgen::from_value::<GameState>()` | Already used in `initialize_game` for DeckPayload; handles all nested types automatically |
| RNG reconstruction | Custom RNG state tracking | `ChaCha20Rng::seed_from_u64(state.rng_seed)` | Deterministic; same approach used in `new_two_player()` and roundtrip test |

## Common Pitfalls

### Pitfall 1: Forgetting RNG Reconstruction
**What goes wrong:** After `serde_wasm_bindgen::from_value`, the `rng` field gets `default_rng()` (seed 0) instead of the game's actual seed.
**Why it happens:** `#[serde(skip)]` means serde uses the default, not the serialized value.
**How to avoid:** Immediately after deserialization, set `state.rng = ChaCha20Rng::seed_from_u64(state.rng_seed)`.
**Warning signs:** Random outcomes (shuffling, coin flips) produce different results after undo.

### Pitfall 2: undo() Not Awaiting restoreState
**What goes wrong:** `undo()` is currently synchronous. Adding an async `restoreState` call means undo must become async, or the WASM call must be synchronous.
**Why it happens:** The `EngineAdapter` interface uses `Promise`-based methods, but WASM calls in `WasmAdapter` are actually synchronous (wrapped in the async queue).
**How to avoid:** Make `restoreState` synchronous in WasmAdapter (direct WASM call, no queue needed since undo is user-initiated and won't race with other operations). Alternatively, make `undo()` async. The simpler approach: since the WASM call is synchronous under the hood, expose a synchronous `restoreState` and call it directly.

### Pitfall 3: Adapter Interface Compatibility
**What goes wrong:** Adding `restoreState` to `EngineAdapter` breaks `TauriAdapter` and `WebSocketAdapter`.
**Why it happens:** The interface is shared across all transport implementations.
**How to avoid:** Add `restoreState` as optional (`restoreState?(state: GameState): void`) or add it to all adapters. For WsAdapter, throw an error (undo not supported in multiplayer). For TauriAdapter, add a matching IPC command or throw.

### Pitfall 4: State History Contains Stale References
**What goes wrong:** The `stateHistory` stores GameState objects from `adapter.getState()`. These are plain JS objects (serde serialization output), so they're safe to re-inject. No reference staleness issue exists here.
**Why it happens:** N/A - this is a non-issue because serde serialization produces fresh plain objects.
**How to avoid:** No action needed. Confirmed safe.

## Code Examples

### WASM Binding: restore_game_state

```rust
// Source: follows pattern from initialize_game() in crates/engine-wasm/src/lib.rs
#[wasm_bindgen]
pub fn restore_game_state(state_js: JsValue) -> Result<(), JsValue> {
    let mut state: GameState = serde_wasm_bindgen::from_value(state_js)
        .map_err(|e| JsValue::from_str(&format!("Failed to deserialize GameState: {}", e)))?;

    // Reconstruct RNG from seed (serde skips the rng field)
    state.rng = ChaCha20Rng::seed_from_u64(state.rng_seed);

    GAME_STATE.with(|gs| {
        *gs.borrow_mut() = Some(state);
    });

    Ok(())
}
```

### EngineAdapter Interface Addition

```typescript
// Source: client/src/adapter/types.ts
export interface EngineAdapter {
  initialize(): Promise<void>;
  initializeGame(deckData?: unknown): Promise<GameEvent[]> | GameEvent[];
  submitAction(action: GameAction): Promise<GameEvent[]>;
  getState(): Promise<GameState>;
  restoreState(state: GameState): void;  // New: for undo sync
  dispose(): void;
}
```

### WasmAdapter.restoreState

```typescript
// Source: follows pattern from wasm-adapter.ts
restoreState(state: GameState): void {
  this.assertInitialized();
  restore_game_state(state);
}
```

### gameStore.undo() Updated

```typescript
// Source: client/src/stores/gameStore.ts
undo: () => {
  const { stateHistory, adapter } = get();
  if (stateHistory.length === 0 || !adapter) return;

  const previous = stateHistory[stateHistory.length - 1];

  // Sync WASM engine state with the restored client state
  adapter.restoreState(previous);

  set({
    gameState: previous,
    waitingFor: previous.waiting_for,
    events: [],
    stateHistory: stateHistory.slice(0, -1),
  });
},
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Client-only undo (Phase 7) | Client + WASM sync undo (Phase 10) | This phase | Eliminates desync bug |

**No external ecosystem changes needed.** This is purely an internal integration fix.

## Open Questions

1. **RNG stream position after undo**
   - What we know: `rng_seed` reconstruction resets RNG to stream start, not mid-game position. Undo-eligible actions don't consume RNG.
   - What's unclear: If a future undoable action category consumes RNG, this would cause divergent random outcomes post-undo.
   - Recommendation: Accept seed-based reconstruction for now. Document the constraint that UNDOABLE_ACTIONS must not include RNG-consuming actions.

2. **TauriAdapter restoreState**
   - What we know: Tauri uses IPC commands. A matching `restore_game_state` Rust command would be needed.
   - What's unclear: Whether server-core has similar thread-local state management.
   - Recommendation: Add a no-op or stub with TODO for TauriAdapter. The Tauri backend would need a matching command, but desktop app isn't the current focus.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | vitest (latest, via pnpm) |
| Config file | `client/vitest.config.ts` (or vite config) |
| Quick run command | `cd client && pnpm test -- --run src/stores/__tests__/gameStore.test.ts` |
| Full suite command | `cd client && pnpm test -- --run` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| QOL-02 | undo() calls adapter.restoreState with previous state | unit | `cd client && pnpm test -- --run src/stores/__tests__/gameStore.test.ts` | Partial (undo tests exist, restoreState assertion needed) |
| QOL-02 | restore_game_state WASM binding deserializes and replaces state | unit | `cargo test -p engine-wasm restore` | No -- Wave 0 |
| QOL-02 | After undo + submitAction, engine uses restored state | integration | Manual (WASM integration test) | No |

### Sampling Rate
- **Per task commit:** `cd client && pnpm test -- --run src/stores/__tests__/gameStore.test.ts`
- **Per wave merge:** `cd client && pnpm test -- --run && cargo test -p engine-wasm`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] Update `client/src/stores/__tests__/gameStore.test.ts` -- add test asserting `adapter.restoreState` called during undo
- [ ] `cargo test -p engine-wasm` -- add test for `restore_game_state` round-trip (Rust-side unit test)

## Sources

### Primary (HIGH confidence)
- `crates/engine-wasm/src/lib.rs` -- Current WASM bindings, thread-local pattern
- `crates/engine/src/types/game_state.rs` -- GameState struct, serde config, RNG skip, roundtrip test
- `client/src/stores/gameStore.ts` -- Current undo implementation
- `client/src/adapter/wasm-adapter.ts` -- Current WasmAdapter (no restoreState)
- `client/src/adapter/types.ts` -- EngineAdapter interface
- `.planning/v1.0-MILESTONE-AUDIT.md` -- Gap identification and fix description

### Secondary (MEDIUM confidence)
- `client/src/hooks/useKeyboardShortcuts.ts` -- Z-key undo trigger
- `client/src/adapter/tauri-adapter.ts` -- TauriAdapter pattern for interface compatibility

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - no new dependencies, all patterns exist in codebase
- Architecture: HIGH - direct extension of existing WASM binding pattern
- Pitfalls: HIGH - RNG reconstruction is documented and tested in existing codebase

**Research date:** 2026-03-08
**Valid until:** 2026-04-08 (stable domain, no external dependencies changing)
