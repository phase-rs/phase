# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Forge.rs is a Magic: The Gathering game engine written in Rust (compiling to native + WASM) with a React/TypeScript frontend. It ports the open-source MTG Forge project's card definitions and game rules using functional architecture (discriminated unions, pure reducers, immutable state) instead of Forge's Java class hierarchy.

## Build & Development Commands

### Rust Engine
```bash
cargo test --all                    # Run all Rust tests
cargo test -p engine                # Test engine crate only
cargo test -p engine -- test_name   # Run single test
cargo clippy --all-targets -- -D warnings  # Lint
cargo fmt --all -- --check          # Format check
cargo fmt --all                     # Auto-format
```

### WASM Build
```bash
./scripts/build-wasm.sh             # Build WASM (release)
./scripts/build-wasm.sh debug       # Build WASM (debug)
cargo wasm                          # Alias: cargo build --package engine-wasm --target wasm32-unknown-unknown
cargo wasm-release                  # Alias: release build
```
Requires `wasm-bindgen-cli` (v0.2.114) and optionally `wasm-opt` (binaryen). Output goes to `client/src/wasm/` (gitignored, regenerated).

### Frontend (client/)
```bash
cd client
pnpm install                        # Install dependencies
pnpm dev                            # Vite dev server
pnpm build                          # TypeScript check + Vite build
pnpm lint                           # ESLint
pnpm run type-check                 # TypeScript only (no emit)
pnpm test                           # Vitest (watch mode)
pnpm test -- --run                  # Vitest (single run, used in CI)
```

## Architecture

### Rust Workspace (`crates/`)

```
engine          — Core rules engine: types, game logic, parser, database
engine-wasm     — WASM bindings (wasm-bindgen + tsify) exposing engine to JS
forge-ai        — AI opponent: evaluation, legal actions, card hints, search
server-core     — Server-side game session management (tokio)
forge-server    — Axum WebSocket server for multiplayer
```

**Crate dependency flow**: `engine` ← `forge-ai` ← `engine-wasm` / `server-core` ← `forge-server`

### Engine Internals (`crates/engine/src/`)

- **`types/`** — Core data types: `GameState`, `GameAction`, `GameEvent`, `GameObject`, `Phase`, `Zone`, `ManaPool`, abilities, triggers. All types use `serde` for serialization across the WASM boundary.
- **`game/engine.rs`** — Main `apply(state, action) -> ActionResult` function. Pure reducer pattern: takes game state + action, returns events + new waiting_for state.
- **`game/`** — Game logic modules: `turns`, `priority`, `stack`, `combat`, `combat_damage`, `sba` (state-based actions), `targeting`, `mana_payment`, `mulligan`, `layers` (MTG Rule 613), `triggers`, `replacement`, `static_abilities`, `keywords`, `zones`, `casting`.
- **`game/effects/`** — Effect handlers: `draw`, `deal_damage`, `destroy`, `pump`, `token`, `counter`, `counters`, `sacrifice`, `discard`, `change_zone`, `life`, `tap_untap`.
- **`parser/`** — Parses Forge's `.txt` card definition format into typed Rust structures.
- **`database/`** — Card database (loads parsed cards).

### WASM Bridge (`crates/engine-wasm/`)

Thin layer using `wasm-bindgen` + `serde-wasm-bindgen`. Thread-local `RefCell<Option<GameState>>` holds game state. Key exports: `initialize_game()`, `submit_action()`, `get_game_state()`, `get_ai_action()`. Uses `tsify` for TypeScript type generation.

### React Frontend (`client/src/`)

- **`adapter/`** — Transport-agnostic `EngineAdapter` interface with three implementations:
  - `WasmAdapter` — Direct WASM calls (browser/PWA)
  - `TauriAdapter` — Tauri IPC (desktop)
  - `WsAdapter` — WebSocket (multiplayer)
  - `createAdapter()` auto-detects platform
- **`stores/`** — Zustand stores: `gameStore` (game state + dispatch), `uiStore` (UI state), `animationStore`
- **`components/`** — React components organized by domain: `board/`, `hand/`, `card/`, `controls/`, `stack/`, `mana/`, `targeting/`, `animation/`, `modal/`, `log/`, `deck-builder/`
- **`services/`** — `scryfall.ts` (card image API), `imageCache.ts` (IndexedDB caching via idb-keyval), `deckParser.ts`
- **`hooks/`** — `useGameDispatch`, `useCardImage`, `useKeyboardShortcuts`, `useLongPress`
- **`pages/`** — `MenuPage`, `GamePage`, `DeckBuilderPage` (React Router)

### Key Patterns

- **Discriminated unions everywhere**: Rust `enum` with `#[serde(tag = "type", content = "data")]` maps to TS `{ type: string; data: ... }` unions. See `GameAction`, `GameEvent`, `WaitingFor` in `adapter/types.ts`.
- **Immutable game state**: Engine uses `rpds` (persistent data structures) for structural sharing. State is never mutated in place on the Rust side.
- **Event-driven updates**: `submit_action()` returns `ActionResult { events, waiting_for }`. The frontend processes events for animations/logging, then updates state.
- **AI is player 1**: In WASM mode, `get_ai_action()` always computes for `PlayerId(1)`.

## Conventions

- Rust: `cargo fmt` + `clippy -D warnings` enforced in CI
- TypeScript: ESLint with `@typescript-eslint/recommended`, unused vars prefixed with `_`
- Frontend uses Tailwind CSS v4, Framer Motion for animations
- Tests colocated in `__tests__/` directories (frontend) or inline `#[cfg(test)]` modules (Rust)
- The `release` profile is optimized for WASM size: `opt-level = 'z'`, LTO, single codegen unit, stripped

## CI

GitHub Actions runs two parallel jobs:
1. **Rust**: fmt → clippy → test → WASM build → wasm-bindgen → wasm-opt → size report
2. **Frontend**: pnpm install → lint → type-check → test

## Planning

Project planning docs live in `.planning/` with phase-based organization (phases 01-09+). Each phase has CONTEXT, RESEARCH, PLAN, SUMMARY, and VERIFICATION docs. `PROJECT.md` contains the project manifest with requirements and key decisions.
