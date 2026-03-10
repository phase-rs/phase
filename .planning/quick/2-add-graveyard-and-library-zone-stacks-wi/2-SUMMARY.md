# Quick Task 2: Add graveyard and library zone stacks with GY reveal, click modal, and library peek support

## Summary

Fixed LibraryPile bug and added data-driven library peek support. The graveyard pile and library pile components already existed with correct positioning (bottom-left player, top-right opponent). The graveyard already showed top card face-up with click-to-view modal.

## Changes

### Bug Fix
- **LibraryPile.tsx**: Fixed `library_size` (non-existent field, silently returned 0) → `library.length`

### Engine: MayLookAtTopOfLibrary static ability
- **static_abilities.rs**: Registered `MayLookAtTopOfLibrary` as a rule modification in `build_static_registry()`
- **player.rs**: Added `can_look_at_top_of_library: bool` derived field with `#[serde(skip_deserializing, default)]`
- **lib.rs (WASM bridge)**: Computes `can_look_at_top_of_library` per player by checking static abilities on battlefield permanents

### Frontend: Conditional peek rendering
- **types.ts**: Added `can_look_at_top_of_library?: boolean` to Player interface
- **LibraryPile.tsx**: When `can_look_at_top_of_library` is true (player 0 only), renders top card art with cyan border indicator instead of card-back pattern

## Architecture

All peek logic is data-driven from the backend:
1. Engine checks battlefield for permanents with `MayLookAtTopOfLibrary` static ability
2. WASM bridge computes the boolean flag per player before serialization
3. Frontend reads the flag and conditionally renders — no frontend-side ability logic

## Verification

- `cargo check -p engine -p engine-wasm` — passes
- `pnpm run type-check` — passes
- Library convention: `library[0]` = top of deck (per `zones.rs:91`)
