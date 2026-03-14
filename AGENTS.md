# AGENTS.md

This file provides guidance to AI coding agents when working with code in this repository.

## Project Overview

phase.rs is a Magic: The Gathering game engine written in Rust (compiling to native + WASM) with a React/TypeScript frontend. It implements MTG game rules using functional architecture (discriminated unions, pure reducers, immutable state) with an Arena-quality browser UI. Card data is sourced from MTGJSON (MIT-licensed) with custom typed JSON ability definitions.

## Design Principles — READ THIS FIRST

**Above all else, this project prioritizes idiomatic Rust and clean, extensible architecture. This is non-negotiable and overrides convenience, speed-of-delivery, or "getting it working." Every code change must pass through this lens before anything else.**

- **Idiomatic Rust, always.** Use Rust's type system, ownership model, and idioms to their fullest. Prefer `enum` over stringly-typed data. Prefer exhaustive `match` over fallback defaults. Prefer trait-based polymorphism over dynamic dispatch when the type set is known. If the idiomatic path is harder, take it anyway — shortcuts compound into debt.
- **The engine is the source of truth.** All game logic, validation, derived state, and rules live in the `engine` crate. Transport layers (WASM bridge, Tauri IPC, WebSocket server) are thin serialization boundaries — zero game logic allowed.
- **Push logic down, not out.** If multiple consumers need the same behavior, it belongs in the engine. Never duplicate logic across adapters. When in doubt, put it in the engine.
- **Extend, don't hack.** New features should slot cleanly into existing patterns (effect handlers, game modules, ability definitions). If a feature requires working around the architecture, the architecture should be extended first.
- **Compose from building blocks.** Every new capability should be decomposed into reusable primitives that unlock future features. A one-off solution that handles one card is worse than a composable building block that handles fifty. Before writing specific logic, ask: "What is the general pattern here?" and build that instead. Examples: `contains_possessive`/`contains_object_pronoun` for Oracle text matching, `ChangeZone` + `Shuffle` composition for compound shuffles, the sub_ability chain for multi-step effects.
- **Production quality, always.** Write code as if a professional team will audit every line. No "good enough for now." No tech debt IOUs. Every function should be clear, every abstraction should earn its keep, and every pattern should be consistent across the codebase. If you're about to write something that duplicates existing logic, stop and factor out the shared building block first.

### When in Doubt

- Is this logic in the right crate? → It probably belongs in `engine`.
- Am I fighting the type system? → Redesign the types, don't work around them.
- Should I add a special case? → Extend the existing pattern instead.
- Am I solving one card or a pattern? → Build the building block, not the special case.
- Is this the Rust way? → Check how `std` and well-known crates solve similar problems.

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
cargo coverage                      # Card support coverage report (reads data/card-data.json)
```

### WASM Build
```bash
./scripts/build-wasm.sh             # Build WASM (release): compile → wasm-bindgen → wasm-opt
./scripts/build-wasm.sh debug       # Build WASM (debug)
```
Requires `wasm-bindgen-cli` (v0.2.114) and optionally `wasm-opt` (binaryen). Output goes to `client/src/wasm/` (gitignored, regenerated).

### Card Data Pipeline
```bash
./scripts/gen-card-data.sh          # export cards → client/public/card-data.json
```

### Card Data Lookup
```bash
jq '.["lightning bolt"]' client/public/card-data.json                    # Full card data
jq '.["card name"] | .abilities[]' client/public/card-data.json          # Just abilities
jq '.["card name"] | {abilities: [.abilities[]? | select(.effect.type == "Unimplemented")], triggers: [.triggers[]? | select(.mode == "Unknown")]}' client/public/card-data.json  # Unimplemented gaps
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
cargo coverage                                  # Card support coverage (JSON report, alias)
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

### Engine Internals (`crates/engine/src/`)

- **`types/`** — Core data types: `GameState`, `GameAction`, `GameEvent`, `GameObject`, `Phase`, `Zone`, `ManaPool`, abilities, triggers. All types use `serde` for serialization across the WASM boundary.
- **`game/engine.rs`** — Main `apply(state, action) -> ActionResult` function. Pure reducer pattern: takes game state + action, returns events + new waiting_for state.
- **`game/`** — Game logic modules: `turns`, `priority`, `stack`, `combat`, `combat_damage`, `sba` (state-based actions), `targeting`, `mana_payment`, `mana_abilities`, `mulligan`, `layers` (MTG Rule 613), `triggers`, `replacement`, `static_abilities`, `keywords`, `zones`, `casting`, `commander`, `day_night`, `deck_loading`, `derived`, `devotion`, `elimination`, `filter`, `game_object`, `morph`, `planeswalker`, `players`, `scenario`, `transform`, `coverage`.
- **`game/effects/`** — Effect handlers (~28 modules), including: `animate`, `attach`, `bounce`, `change_zone`, `choose_card`, `cleanup`, `copy_spell`, `counter`, `counters`, `deal_damage`, `destroy`, `dig`, `discard`, `draw`, `explore`, `fight`, `gain_control`, `life`, `mana`, `mill`, `proliferate`, `pump`, `sacrifice`, `scry`, `surveil`, `tap_untap`, `token`. New effects are added as modules here following the existing handler pattern.
- **`parser/`** — Oracle text parser: converts MTGJSON Oracle text into typed `AbilityDefinition` structs. See `docs/parser-instructions.md` for architecture and contribution guide.
- **`database/`** — Card database with three loading paths:
  - `CardDatabase::load_json(mtgjson_path)` — MTGJSON
  - `CardDatabase::from_export(path)` — Pre-built `card-data.json` (used at runtime by WASM and server)

### Card Data Format (`data/`)

- **`mtgjson/`** — MTGJSON atomic card data
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
- **`components/`** — React components organized by domain: `animation/`, `board/`, `card/`, `chrome/`, `combat/`, `controls/`, `deck-builder/`, `hand/`, `hud/`, `lobby/`, `log/`, `mana/`, `menu/`, `modal/`, `multiplayer/`, `settings/`, `splash/`, `stack/`, `targeting/`, `ui/`, `zone/`
- **`services/`** — `scryfall.ts` (card image API), `imageCache.ts` (IndexedDB caching via idb-keyval), `deckParser.ts`
- **`hooks/`** — `useGameDispatch`, `useCardImage`, `useKeyboardShortcuts`, `useLongPress`, `usePhaseInfo`, `usePlayerId`
- **`pages/`** — `MenuPage`, `GamePage`, `GameSetupPage`, `PlayPage`, `MultiplayerPage`, `DeckBuilderPage` (React Router)

### Key Patterns

- **Discriminated unions everywhere**: Rust `enum` with `#[serde(tag = "type", content = "data")]` maps to TS `{ type: string; data: ... }` unions. See `GameAction`, `GameEvent`, `WaitingFor` in `adapter/types.ts`.
- **Immutable game state**: Engine uses `rpds` (persistent data structures) for structural sharing. State is never mutated in place on the Rust side.
- **Event-driven updates**: `submit_action()` returns `ActionResult { events, waiting_for }`. The frontend processes events for animations/logging, then updates state.
- **AI is player 1**: In WASM mode, `get_ai_action()` always computes for `PlayerId(1)`.

## Environment Variables

- `PORT` — phase-server listen port (default `9374`)
- `PHASE_DATA_DIR` — Card data root for phase-server (default `"data"`)
- `PHASE_CARDS_PATH` — Override card data directory for binaries (`coverage-report`, `card-data-export`)

## Documentation (`docs/`)

- **`docs/parser-instructions.md`** — Oracle parser architecture and contribution guide: how to add new effect types, when to intercept before subject stripping, enum patterns, and common pitfalls.
- **`.claude/skills/add-engine-effect/SKILL.md`** — Complete checklist for adding a new effect to the engine: types → parser → resolver → targeting → multiplayer filter → frontend → AI → tests. Covers every registration point that must be updated in lockstep. **Use this as the authoritative guide for any new effect work.**

## Conventions

### Rust Idioms — Write It Right the First Time

These patterns must be used on first write, not fixed after clippy complains:

- **`strip_prefix`/`strip_suffix`** over `starts_with` + manual slicing: `if let Some(rest) = s.strip_prefix("foo")` not `if s.starts_with("foo") { &s[3..] }`
- **Iterator methods** over range-indexed loops: `for item in slice.iter().skip(1)` not `for i in 1..slice.len()`
- **`rsplit(' ').next()`** to get the last word, not `rsplit().collect::<Vec>().first()`
- **Exhaustive `match`** without wildcard fallbacks when the enum is known — let the compiler catch missing arms
- **Reuse existing building blocks** before writing one-off string logic. Search the codebase for helpers like `contains_possessive`, `contains_object_pronoun`, `parse_target`, `parse_type_phrase`, `parse_number` in `oracle_util.rs` and `oracle_target.rs`

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
