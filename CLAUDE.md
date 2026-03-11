# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

phase.rs is a Magic: The Gathering game engine written in Rust (compiling to native + WASM) with a React/TypeScript frontend. It implements MTG game rules using functional architecture (discriminated unions, pure reducers, immutable state) with an Arena-quality browser UI. Card data is sourced from MTGJSON (MIT-licensed) with custom typed JSON ability definitions.

## Design Principles

- **Idiomatic Rust first.** Every decision should be through the lens of "what is the idiomatic Rust path, and what is the cleanest architecture." No shortcuts.
- **Clean, extensible, maintainable engine.** The engine crate is the source of truth — no logic in transport layers (WASM bridge, Tauri IPC, WebSocket server). Transport layers are thin serialization boundaries only.
- **Push logic down.** Derived state, validation, and game rules belong in the engine. If multiple consumers need the same behavior, it must live in the engine, not be duplicated per-adapter.

## Setup

```bash
./scripts/setup.sh    # Full onboarding: gen card data → build WASM → pnpm install
```

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

### Cargo Aliases (`.cargo/config.toml`)
```bash
cargo wasm                          # Build WASM (debug)
cargo wasm-release                  # Build WASM (release)
cargo test-all                      # cargo test --all
cargo clippy-strict                 # clippy -D warnings
cargo export-cards -- data/         # Run card-data-export binary
cargo serve                         # Run phase-server (release)
```

### WASM Build
```bash
./scripts/build-wasm.sh             # Build WASM (release): compile → wasm-bindgen → wasm-opt
./scripts/build-wasm.sh debug       # Build WASM (debug)
```
Requires `wasm-bindgen-cli` (v0.2.114) and optionally `wasm-opt` (binaryen). Output goes to `client/src/wasm/` (gitignored, regenerated).

### Card Data Pipeline
```bash
./scripts/gen-card-data.sh          # Sparse-clone Forge repo → export cards → client/public/card-data.json
```

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
pnpm tauri:dev                      # Tauri desktop dev
pnpm tauri:build                    # Tauri desktop build
```

### Coverage Report
```bash
cargo run --bin coverage-report -- data/        # Card support coverage (JSON report)
cargo run --bin coverage-report -- data/ --ci   # CI mode: exits 1 if gaps found
```

## Architecture

### Rust Workspace (`crates/`)

```
engine          — Core rules engine: types, game logic, parser, database
engine-wasm     — WASM bindings (wasm-bindgen + tsify) exposing engine to JS
phase-ai        — AI opponent: evaluation, legal actions, card hints, search
server-core     — Server-side game session management (tokio)
phase-server    — Axum WebSocket server for multiplayer
```

**Crate dependency flow**: `engine` ← `phase-ai` ← `engine-wasm` / `server-core` ← `phase-server`

### Feature Flags

- **`forge-compat`** (engine crate) — Gates Forge `.txt` card file parsing. Required by binaries: `card-data-export`, `migrate`, `coverage-report`.

### Engine Internals (`crates/engine/src/`)

- **`types/`** — Core data types: `GameState`, `GameAction`, `GameEvent`, `GameObject`, `Phase`, `Zone`, `ManaPool`, abilities, triggers. All types use `serde` for serialization across the WASM boundary.
- **`game/engine.rs`** — Main `apply(state, action) -> ActionResult` function. Pure reducer pattern: takes game state + action, returns events + new waiting_for state.
- **`game/`** — Game logic modules: `turns`, `priority`, `stack`, `combat`, `combat_damage`, `sba` (state-based actions), `targeting`, `mana_payment`, `mulligan`, `layers` (MTG Rule 613), `triggers`, `replacement`, `static_abilities`, `keywords`, `zones`, `casting`.
- **`game/effects/`** — Effect handlers: `draw`, `deal_damage`, `destroy`, `pump`, `token`, `counter`, `counters`, `sacrifice`, `discard`, `change_zone`, `life`, `tap_untap`.
- **`parser/`** — Ability text parser for SVar/SubAbility chain resolution (card file parser behind `forge-compat` feature gate).
- **`database/`** — Card database with three loading paths:
  - `CardDatabase::load_json(mtgjson_path, abilities_dir)` — MTGJSON + typed ability JSON
  - `CardDatabase::load(root)` — Forge `.txt` files (requires `forge-compat`)
  - `CardDatabase::from_export(path)` — Pre-built `card-data.json` (used at runtime by WASM and server)

### Card Data Format (`data/`)

- **`mtgjson/`** — MTGJSON atomic card data
- **`abilities/`** — 30K+ individual card JSON files with typed ability definitions (schema: `abilities`, `triggers`, `statics`, `replacements`, `faces`)
- **`cardsfolder/`** — Forge `.txt` card files (sparse-checked out via `gen-card-data.sh`)
- **`card-data.json`** → symlinked to `client/public/card-data.json` for runtime use

### WASM Bridge (`crates/engine-wasm/`)

Thin layer using `wasm-bindgen` + `serde-wasm-bindgen`. Thread-local `RefCell<Option<GameState>>` holds game state. Key exports: `initialize_game()`, `submit_action()`, `get_game_state()`, `get_ai_action()`. Uses `tsify` for TypeScript type generation.

### AI Engine (`crates/phase-ai/`)

Difficulty levels: `VeryEasy` (random) → `Easy` (basic heuristics) → `Medium` (combat-aware, 2-depth search) → `Hard` → `VeryHard` (deterministic best-move). Platform-aware budgeting reduces search limits on WASM vs native.

Key modules: `legal_actions`, `combat_ai` (attackers/blockers), `eval` (state/creature evaluation), `search` (minimax-like), `card_hints` (play-now hints for UI).

### Multiplayer Server (`crates/phase-server/`, `crates/server-core/`)

Axum WebSocket server with lobby management. Protocol uses discriminated unions:
- **`ClientMessage`** — `CreateGameWithSettings`, `JoinGameWithPassword`, `Action`, `Reconnect`, `Concede`, `Emote`, `SubscribeLobby`
- **`ServerMessage`** — `GameCreated`, `GameStarted`, `StateUpdate`, `OpponentDisconnected`, `GameOver`, `LobbyUpdate`, `PlayerCount`

State is filtered per-player (`filter_state_for_player`) to hide opponent's hand/library. Disconnected players get a 10-second reconnect grace period.

### React Frontend (`client/src/`)

- **`adapter/`** — Transport-agnostic `EngineAdapter` interface with four implementations:
  - `WasmAdapter` — Direct WASM calls (browser/PWA), serialized through async queue
  - `TauriAdapter` — Tauri IPC (desktop), dynamically imported to avoid bundling in web
  - `WebSocketAdapter` — WebSocket to phase-server (multiplayer), with reconnection (3 attempts)
  - `P2PHostAdapter` / `P2PGuestAdapter` — WebRTC via PeerJS (host runs local WASM, filters state for guest)
  - `createAdapter()` auto-detects platform (Tauri vs browser)
- **`stores/`** — Zustand stores: `gameStore` (game state + dispatch), `uiStore` (UI state), `animationStore`, `multiplayerStore` (game code, opponent, timer)
- **`components/`** — React components organized by domain: `board/`, `hand/`, `card/`, `controls/`, `stack/`, `mana/`, `targeting/`, `animation/`, `modal/`, `log/`, `deck-builder/`
- **`services/`** — `scryfall.ts` (card image API), `imageCache.ts` (IndexedDB caching via idb-keyval), `deckParser.ts`
- **`hooks/`** — `useGameDispatch`, `useCardImage`, `useKeyboardShortcuts`, `useLongPress`
- **`pages/`** — `MenuPage`, `GamePage`, `DeckBuilderPage` (React Router)

### Key Patterns

- **Discriminated unions everywhere**: Rust `enum` with `#[serde(tag = "type", content = "data")]` maps to TS `{ type: string; data: ... }` unions. See `GameAction`, `GameEvent`, `WaitingFor` in `adapter/types.ts`.
- **Immutable game state**: Engine uses `rpds` (persistent data structures) for structural sharing. State is never mutated in place on the Rust side.
- **Event-driven updates**: `submit_action()` returns `ActionResult { events, waiting_for }`. The frontend processes events for animations/logging, then updates state.
- **AI is player 1**: In WASM mode, `get_ai_action()` always computes for `PlayerId(1)`.

## Environment Variables

- `PORT` — phase-server listen port (default `9374`)
- `PHASE_DATA_DIR` — Card data root for phase-server (default `"data"`)
- `PHASE_CARDS_PATH` — Override card data directory for binaries (`coverage-report`, `card-data-export`)

## Conventions

- Rust: `cargo fmt` + `clippy -D warnings` enforced in CI
- TypeScript: ESLint with `@typescript-eslint/recommended`, unused vars prefixed with `_`
- Frontend uses Tailwind CSS v4, Framer Motion for animations
- Tests colocated in `__tests__/` directories (frontend) or inline `#[cfg(test)]` modules (Rust)
- The `release` profile is optimized for WASM size: `opt-level = 'z'`, LTO, single codegen unit, stripped

## CI

GitHub Actions runs two parallel jobs:
1. **Rust**: fmt → clippy → test → coverage-report → tarpaulin → WASM build → wasm-bindgen → wasm-opt → size report
2. **Frontend**: pnpm install → lint → type-check → test with coverage

## Planning

Project planning docs live in `.planning/` with phase-based organization (phases 01-09+). Each phase has CONTEXT, RESEARCH, PLAN, SUMMARY, and VERIFICATION docs. `PROJECT.md` contains the project manifest with requirements and key decisions.
