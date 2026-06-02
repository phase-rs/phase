---
name: project-reference
description: Reference lookup for phase.rs — build/test/cargo/clippy commands, cargo aliases, WASM build, card-data pipeline and jq lookups, frontend (pnpm) commands, coverage report, crate/workspace architecture, engine internals, WASM bridge, AI engine, multiplayer server, frontend layers, environment variables, releasing (cargo-release), and CI. Use when you need to look up how to build, test, run, generate card data, release, or where a crate, module, command, or env var lives.
---

# phase.rs Project Reference

Lookup material for build commands, the verification cadence, architecture, environment variables, releasing, and CI. The always-on rules (design principles, Tilt-not-cargo, building blocks, CR annotations, Rust idioms) live in the root `CLAUDE.md` / `AGENTS.md`; this skill holds the on-demand reference that doesn't need to be resident every turn.

## Tilt resources & operational rules

**Tilt is always running and continuously rebuilds on file changes.** Do NOT run `cargo build`, `cargo clippy`, `cargo test -p engine`, `pnpm run type-check`, or `pnpm lint` directly — they compete for cargo target locks. Check Tilt logs instead.

**Available Tilt resources** (defined in `Tiltfile`):
| Resource | What it does | Triggers on |
|----------|-------------|-------------|
| `clippy` | `cargo clippy --all-targets -- -D warnings` | `crates/` changes |
| `test-engine` | `cargo test -p engine` | `crates/engine/src/` changes |
| `test-ai` | `cargo test -p phase-ai` | `crates/engine/src/` or `crates/phase-ai/src/` changes |
| `wasm` | WASM build (depends on clippy) | engine/AI/WASM src changes |
| `card-data` | `./scripts/gen-card-data.sh` | `crates/engine/src/` changes |
| `check-frontend` | `pnpm run type-check && pnpm lint` | `client/src/` changes |
| `test-frontend` | `pnpm test -- --run` | `client/src/` changes |
| `server` | Build + serve phase-server | server src changes |
| `coverage` | `cargo coverage` | Manual trigger only |

**How to check results:**
```bash
tilt logs clippy --tail 30 --since 2m          # Recent clippy output
tilt logs test-engine --tail 50 --since 2m     # Recent test results
tilt logs card-data --tail 20 --since 1m       # Card data gen output
tilt logs check-frontend --tail 30 --since 2m  # TS type-check + lint
```

**How to wait for current results without dumping logs:**
```bash
./scripts/tilt-wait.sh clippy test-engine card-data        # wait until all settle (no timeout)
./scripts/tilt-wait.sh --interval 10 clippy                # poll faster for a single fast resource
./scripts/tilt-wait.sh --timeout 600 clippy test-engine    # bound the wait
```
Exit codes: `0` all ok, `1` a resource is in terminal error (`updateStatus=error` with no in-flight build), `124` timeout (only when `--timeout` is set). The script prints one `<resource> status=… current=… last=…` line per tick so you can see why a wait is taking time without paying for log payloads. After exit 1, fetch details with `tilt logs <resource> --tail 50 --since 2m`.

**Rules:**
- After saving files, wait ~10-30s for Tilt to detect changes and rebuild, then check logs.
- Do not wait for every Tilt resource to go green after every minor edit. Use the smallest relevant signal while iterating, and batch broader Tilt verification at natural checkpoints such as before marking an issue fixed, after a risky engine change, or before handing work off.
- Prefer the `tilt get uiresource` polling loop when waiting on multiple resources; use logs after a resource reports `error` or when you need detailed output.
- Do not treat `.status.buildHistory[0].error` as actionable while `.status.currentBuild.spanID` is present. Build history may still contain the previous failed run while Tilt is compiling a newer one.
- Only diagnose/fix a resource error after `updateStatus == "error"` and `currentBuild.spanID == "none"`. `pending` with no current span usually means the resource is queued behind another resource or cargo lock; wait instead of starting manual cargo commands.
- Use `--follow` only when you need to stream live output (e.g., waiting for a build in progress).
- Use `--since` to limit output — don't dump entire build history.
- If a resource shows errors, fix your code and Tilt will automatically rebuild.
- Only run cargo/pnpm commands directly if Tilt is confirmed not running. Detect with `tilt get uiresource clippy >/dev/null 2>&1` (exit 0 = Tilt up; exit non-zero = Tilt down or unreachable). `tilt status` is **not** a valid subcommand — do not use it.
- `cargo fmt --all` is the one exception — always run it directly since Tilt doesn't auto-format.

## Verification cadence (risk-scaled)

`cargo fmt --all` is always run directly — Tilt does not auto-format. For everything else, prefer Tilt logs (`tilt logs <resource>`) / `./scripts/tilt-wait.sh` when Tilt is up, and fall back to direct cargo/pnpm only when Tilt is confirmed down (`tilt get uiresource clippy >/dev/null 2>&1`; exit 0 = up).

```bash
# Always run fmt directly — Tilt does not auto-format.
cargo fmt --all

# Fast parser iteration: small parser/AST-only changes while moving through many
# bug reports. Broader Tilt resources may keep running in the background; do not
# wait on card-data for every tiny parser edit.
./scripts/check-parser-combinators.sh
if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 180 clippy
else
  cargo clippy --all-targets -- -D warnings
fi

# Full Rust verification: before marking an issue fixed-unreleased, after
# non-trivial engine/runtime changes, before long handoffs, and before PR or
# release boundaries. Parser changes invalidate card-data, but card-data is a
# checkpoint resource, not a per-micro-edit throttle.
if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 240 clippy test-engine card-data
else
  cargo clippy --all-targets -- -D warnings
  cargo test -p engine
  ./scripts/gen-card-data.sh
fi

# Frontend verification:
if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 180 check-frontend
else
  (cd client && pnpm run type-check && pnpm lint)
fi
```

After `tilt-wait.sh` returns non-zero, fetch details with `tilt logs <resource> --tail 50 --since 2m`. After direct cargo/pnpm failures, the output is already on stdout.

These blocks are designed for interactive use, where a non-zero exit from `tilt-wait.sh` or a cargo command is surfaced via the printed status line and the operator fixes it before re-verifying. **In a `set -e` shell or scripted/CI harness, the `if` construct will swallow the inner non-zero exit** — wrap each branch with `|| exit $?` (or restructure as `tilt get uiresource clippy >/dev/null 2>&1 && tilt-wait.sh ... || cargo ...`) when copy-pasting into automation.

The one-shot audit binaries (`cargo coverage`, `cargo semantic-audit`, `cargo parser-gaps`, `cargo rules-audit`) are not continuous Tilt resources — invoke them directly in both modes.

## Build & Development Commands

Run `./scripts/setup.sh` for full onboarding (Scryfall sidecars → card data → WASM → pnpm install). Auto-detects Tilt and defers WASM + card-data to `tilt up` when present. Flags: `--agent` skips Scryfall art for LLM contributors (see `docs/AI-CONTRIBUTOR.md`); `--no-tilt` forces the inline build path.

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
cargo test-all                      # Run all tests (nextest, excludes phase-tauri)
cargo clippy-strict                 # clippy -D warnings
cargo serve                         # Run phase-server (release)
cargo coverage                      # Card support coverage report (reads data/card-data.json)
cargo parser-gaps                   # Parser gap analysis report
cargo rules-audit                   # MTG Comprehensive Rules audit (requires --features audit)
cargo semantic-audit                # Semantic audit of parsed card data (outputs data/semantic-audit.json + .md)
cargo scrape-feeds                  # Scrape metagame feeds from MTGGoldfish
cargo tune-ai                       # Run AI weight tuning (requires --features tune)
```

### WASM Build
```bash
./scripts/build-wasm.sh             # Build WASM (release): compile → wasm-bindgen → wasm-opt
./scripts/build-wasm.sh debug       # Build WASM (debug)
```
Requires `wasm-bindgen-cli` (v0.2.121) and optionally `wasm-opt` (binaryen). Output goes to `client/src/wasm/` (gitignored, regenerated).

### Card Data Pipeline
```bash
./scripts/gen-card-data.sh                                         # export all cards → client/public/card-data.json
cargo run --bin oracle-gen -- data --filter "card name"             # single card (debug)
cargo run --bin oracle-gen -- data --filter "name1|name2|name3"     # multiple cards (pipe-separated, substring match)
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
feed-scraper    — Metagame deck scraper (MTGGoldfish)
```

**Crate dependency flow**: `engine` ← `phase-ai` ← `engine-wasm` / `server-core` ← `phase-server` (`feed-scraper` is standalone)

### Engine Internals (`crates/engine/src/`)

- **`types/`** — Core data types: `GameState`, `GameAction`, `GameEvent`, `GameObject`, `Phase`, `Zone`, `ManaPool`, abilities, triggers. All types use `serde` for serialization across the WASM boundary.
- **`game/engine.rs`** — Main `apply(state, action) -> ActionResult` function. Pure reducer pattern: takes game state + action, returns events + new waiting_for state.
- **`game/`** — Game logic modules (turns, priority, stack, combat, SBA, targeting, mana, layers, triggers, replacement, static abilities, zones, casting, etc.). `ls crates/engine/src/game/` for the full set.
- **`game/effects/`** — One module per effect handler (deal_damage, counter, draw, destroy, bounce, change_zone, etc.). New effects are added as modules here following the existing handler pattern. `ls crates/engine/src/game/effects/` for the full set.
- **`parser/`** — Oracle text parser: converts MTGJSON Oracle text into typed `AbilityDefinition` structs. Main dispatcher in `oracle.rs`, with specialized sub-parsers (`oracle_effect/`, `oracle_trigger.rs`, `oracle_static.rs`, `oracle_replacement.rs`, `oracle_cost.rs`, `oracle_keyword.rs`, `oracle_casting.rs`, `oracle_class.rs`, `oracle_saga.rs`, etc.). **`oracle_nom/`** is the shared nom 8.0 combinator foundation — all parser branches delegate atomic and structural parsing operations to these combinators. `dispatch_line_nom` in `oracle.rs` is the primary dispatch path for lines not caught by earlier priority checks. See `.claude/skills/oracle-parser/SKILL.md` for the authoritative parser reference.
- **`ai_support/`** — Engine-side AI support: `legal_actions()` generates validated candidate actions for all `WaitingFor` states. Lives in the engine crate so both WASM and server consumers share the same logic.
- **`database/`** — Card database. `CardDatabase::load_json(mtgjson_path)` loads MTGJSON; `CardDatabase::from_export(path)` loads the pre-built `card-data.json` used at runtime by WASM and server.

### Card Data Format (`data/`)

- **`mtgjson/`** — MTGJSON atomic card data
- **`client/public/card-data.json`** — Pre-built card data consumed at runtime by WASM and server

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

**The frontend is strictly a display layer.** It receives fully-resolved state from the engine and renders it. It must not compute derived game values, filter game objects by rules logic, or infer anything the engine should provide. If a component needs data the engine doesn't currently expose, the fix is to add it to the engine's output — not to compute it client-side.

- **`adapter/`** — Transport-agnostic `EngineAdapter` interface with five implementations:
  - `WasmAdapter` — Direct WASM calls (browser/PWA), serialized through async queue
  - `TauriAdapter` — Tauri IPC (desktop), dynamically imported to avoid bundling in web
  - `WebSocketAdapter` — WebSocket to phase-server (multiplayer), with reconnection (3 attempts)
  - `P2PHostAdapter` / `P2PGuestAdapter` — WebRTC peer-to-peer via PeerJS
  - `createAdapter()` auto-detects platform (Tauri vs browser)
- **`stores/`** — Zustand stores (`gameStore`, `uiStore`, `animationStore`, `multiplayerStore`, `preferencesStore`).
- **`components/`** — React components organized by domain (board, card, combat, hand, lobby, stack, targeting, zone, etc.). `ls client/src/components/` for the full set.
- **`services/`** — Scryfall image fetching, IndexedDB image cache, deck parsing/compatibility, metagame feeds, game persistence, Tauri sidecar.
- **`hooks/`** — Game dispatch, card image, keyboard shortcuts, long-press, phase info, player ID, hover, etc.
- **`pages/`** — React Router pages (Menu, Game, GameSetup, Multiplayer, DeckBuilder, MyDecks, Coverage).

### Key Patterns

- **Discriminated unions everywhere**: Rust `enum` with `#[serde(tag = "type", content = "data")]` maps to TS `{ type: string; data: ... }` unions. See `GameAction`, `GameEvent`, `WaitingFor` in `adapter/types.ts`.
- **Persistent-container hot fields**: Hot `GameState` zones use `im` (15.x, `Arc`-backed HAMT/RRB) so `GameState::clone()` is O(log n) structural share instead of deep-copy. `im` is re-exported as `engine::im`. Writes use `push_back`/`pop_back`/`iter_mut` (not std `Vec`'s `push`/`pop`/`values_mut`). Caveat: `im::Vector::truncate(n)` panics if `n > len` — guard the length or use `im_ext` helpers. Backing choice is localized to `types/game_state.rs` + `types/player.rs`.
- **Event-driven updates**: `submit_action()` returns `ActionResult { events, waiting_for }`. The frontend processes events for animations/logging, then updates state.
- **AI is player 1**: In WASM mode, `get_ai_action()` always computes for `PlayerId(1)`.

## Environment Variables

- `PORT` — phase-server listen port (default `9374`)
- `PHASE_DATA_DIR` — Card data root for phase-server (default `"data"`)
- `PHASE_CARDS_PATH` — Override card data directory for binaries (`coverage-report`, `card-data-export`)
- `PHASE_LOG_DIR` — Log directory for phase-server. When set, logs to files instead of stdout (main log: `<dir>/phase-server.log`, per-game logs: `<dir>/games/<code>.log`)
- `PHASE_CORS_ORIGIN` — Custom CORS origin for phase-server (default: allows common dev ports)
- `PHASE_LOG_JSON` — Enable JSON-formatted log output for phase-server

## Releasing

Use `cargo-release` via the workspace alias — **never tag manually with `git tag`**.

```bash
git pull --rebase origin main         # Rebase onto latest (avoids push rejection from automated PRs)
cargo release-local 0.1.3             # Bump all versions, commit, and tag locally
git push origin main --follow-tags    # Push the release commit + tag
```

`cargo release-local` (alias for `cargo release --workspace --execute --no-confirm --no-publish --no-push`) handles:
- Workspace `Cargo.toml` version (shared across all crates)
- `client/package.json`, `client/src-tauri/Cargo.toml`, `client/src-tauri/tauri.conf.json` via `pre-release-replacements`
- Creating a `release: v{version}` commit and `v{version}` tag

## CI

GitHub Actions runs two parallel jobs:
1. **Rust**: fmt → clippy → test → coverage-report → tarpaulin → WASM build → wasm-bindgen → wasm-opt → size report
2. **Frontend**: pnpm install → lint → type-check → test with coverage

## Planning

Project planning docs live in `.planning/` with phase-based organization (phases 01-09+). Each phase has CONTEXT, RESEARCH, PLAN, SUMMARY, and VERIFICATION docs. `PROJECT.md` contains the project manifest with requirements and key decisions.
