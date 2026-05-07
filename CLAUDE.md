# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

phase.rs is a Magic: The Gathering game engine written in Rust (compiling to native + WASM) with a React/TypeScript frontend. It implements MTG game rules using functional architecture (discriminated unions, pure reducers, immutable state) with an Arena-quality browser UI. Card data is sourced from MTGJSON (MIT-licensed) with custom typed JSON ability definitions.

## Design Principles — READ THIS FIRST

**Above all else, this project prioritizes three co-equal pillars: idiomatic Rust, composable building-block architecture, and strict fidelity to the MTG Comprehensive Rules. These are non-negotiable and override convenience, speed-of-delivery, or "getting it working." Every code change must pass through all three lenses before anything else.**

- **Idiomatic Rust, always.** Use Rust's type system, ownership model, and idioms to their fullest. Prefer `enum` over stringly-typed data. Prefer exhaustive `match` over fallback defaults. Prefer trait-based polymorphism over dynamic dispatch when the type set is known. If the idiomatic path is harder, take it anyway — shortcuts compound into debt.
- **Rules-correct over convenient — the #1 hard rule.** This is an MTG rules engine — correctness to the Comprehensive Rules is a hard requirement, not a nice-to-have. Every implementation pattern MUST be verified against the relevant CR section before it is considered complete. When a rules-correct implementation is more complex than a shortcut, take the complex path. A simpler implementation that gets the rules wrong is not simpler — it is wrong. If you are unsure whether a behavior is rules-correct, look up the CR section, annotate the code, and implement what the rules say, not what seems reasonable. "It works for most cases" is not acceptable when the CR specifies exact behavior. No game logic ships without CR validation.
- **Build for the class, not the card.** Every new enum variant, parser pattern, effect handler, or filter must handle a *category* of cards, not a single card. Before writing any logic, ask: "How many cards does this cover?" If the answer is one, you're building a special case — find the general pattern and build that instead. A one-off that works for one card but breaks for the next card with the same pattern is not a building block; it is technical debt.
- **The engine owns all logic.** All game rules, validation, derived state, and computed values live in the `engine` crate. Transport layers (WASM bridge, Tauri IPC, WebSocket server) are thin serialization boundaries — zero game logic allowed. If multiple consumers need the same behavior, it belongs in the engine. Never duplicate logic across adapters. When in doubt, put it in the engine.
- **The frontend is a display layer, not a logic layer.** The React client renders engine-provided state and dispatches user actions — nothing more. It must never compute, derive, transform, or re-interpret game data. If the frontend needs a value, the engine must provide it. Formatting for display (e.g., string interpolation of engine-provided fields) is acceptable; calculating, filtering, or inferring game state is not. Any "smart" frontend code is a bug — move it to the engine.
- **Compose from building blocks.** Every new capability should be decomposed into reusable primitives that unlock future features. Before writing specific logic, ask: "What is the general pattern here?" and build that instead. This applies equally to data modeling: when a new field or parameter needs to distinguish cases, use an existing typed enum (e.g., `ControllerRef`, `Comparator`, `Option<T>`) — never a raw `bool`. A boolean isn't composable; an existing type is self-documenting, extensible, and expresses the full design space. Examples: `contains_possessive`/`contains_object_pronoun` for Oracle text matching, `ChangeZone` + `Shuffle` composition for compound shuffles, `Option<ControllerRef>` for "whose turn is required" instead of `requires_your_turn: bool`.
- **Parameterize, don't proliferate.** Before adding a sibling variant to an enum, ask: *is the new variant a leaf-level parameterization of an existing variant's structural axis (scope, target, aggregate function, condition shape)?* If yes, refactor the existing variants into a parameterized form (e.g., `LifeTotal { player: PlayerScope }` instead of `LifeTotal` + `OpponentLifeTotal` + `TargetLifeTotal`; `UnlessQuantity { comparator, filter, count }` instead of `UnlessControlsCountMatching` + `UnlessControlsMatching` + `UnlessControlsOtherLeq`). Adding a sibling to an enum that should be parameterized compounds debt exponentially: one sibling is cheap, ten siblings make the eventual refactor multi-week as call sites multiply across parser, converter, resolver, and tests. **Sibling-cluster smell:** when an enum has three or more variants that share a name root (X / OpponentX / TargetX / AllX), differ only in a context label, or only differ in a comparator/aggregator/scope axis, that's a parameterization that didn't happen — refactor before extending. The strict-failure tag is the right place to leave coverage waiting while the architecture wins. **Categorical boundary rule:** the parameterization axis must lie within a single CR rule section. Life is CR 119 (player-only). Power/toughness are CR 208/209 (creature/planeswalker). Don't unify these under one `Life { target: {Self,Opponent,Creature}, type: {Total,Remaining} }` — that conflates rule sections the engine treats as separately resolvable. Cross-section unification belongs at `TargetFilter` or at the effect handler (`Effect::DealDamage` per CR 120 unifies all damage subjects), never at the leaf-reference layer. **Discoverability:** before any engine variant proposal, grep `data/engine-inventory.json` (auto-generated by `cargo engine-inventory`) for existence verification and sibling-cluster smells. The inventory is the canonical source of engine surface — replaces hand-maintained CLAUDE.md lists that drift. Run the workspace `add-engine-variant` skill checklist as the runnable gate; CLAUDE.md is the principle, the skill is the gate.
- **Nom combinators on the first pass — no exceptions.** All new parser code MUST use nom combinators (`tag()`, `alt()`, `value()`, `terminated()`, `pair()`, etc.) from the very first line written. Never write `find()`, `split_once()`, `contains()`, or `starts_with()` for parsing dispatch and then "plan to convert to combinators later." There is no later — write it correctly the first time. Use `nom_on_lower` bridge for mixed-case text, `tag().parse()` for already-lowercase text. Use existing building blocks (`parse_single_cost`, `parse_target`, `parse_for_each_clause`, etc.) for composed operations. If you catch yourself writing string matching for parsing, stop and rewrite with combinators before proceeding. This has been a recurring issue and is non-negotiable.
- **Extend, don't hack.** New features should slot cleanly into existing patterns (effect handlers, game modules, ability definitions). If a feature requires working around the architecture, the architecture should be extended first.
- **Trace before you build.** Before implementing a new pattern, trace how an existing analogous feature works end-to-end (e.g., trace `enter_tapped` before building `enter_with_counters`; trace `Changeling` before building a new CDA). This prevents reinventing existing infrastructure and ensures consistency.
- **Production quality, always.** Write code as if a professional team will audit every line. No "good enough for now." No tech debt IOUs. Every function should be clear, every abstraction should earn its keep, and every pattern should be consistent across the codebase.
- **Single authority for ability costs.** When an ability has costs (tap, sacrifice, pay life, discard, etc.), all cost resolution must go through one authoritative resolver function. Callers dispatch activation — they never inspect or handle individual cost components. This prevents scattered responsibility where every call site must remember to sacrifice Treasures, pay life, or handle future cost types. If you find yourself checking an ability's cost structure at a call site, you're in the wrong layer — push it into the resolver.

### When in Doubt

- Is this logic in the right crate? → It probably belongs in `engine`.
- Am I fighting the type system? → Redesign the types, don't work around them.
- Should I add a special case? → Extend the existing pattern instead.
- Am I solving one card or a pattern? → Build the building block, not the special case.
- Is this the Rust way? → Check how `std` and well-known crates solve similar problems.
- Does this match the Comprehensive Rules? → Look up the CR section, annotate the code, implement what it says.
- Am I computing something in the frontend? → Move it to the engine and expose it in the state.

### CRITICAL: Multi-Agent Safety — Do Not Revert Other Agents' Work

**NEVER revert, overwrite, remove, or undo changes that you did not make.** Multiple AI agents may be working on this codebase concurrently. If you encounter unfamiliar code, new types, new files, or changes you don't recognize:

1. **Do not delete or rewrite them.** They are another agent's in-progress work.
2. **Work around them.** Your edits must be surgical — add only what you need without disturbing surrounding code.
3. **Never use `Write` to replace an entire file** when `Edit` with a targeted `old_string`→`new_string` would suffice. Whole-file rewrites destroy other agents' concurrent changes.
4. **If a file has been modified since you last read it**, re-read it before editing. The new content is intentional.
5. **Never `git checkout`, `git restore`, or `git stash`** files you didn't modify. These operations destroy other agents' uncommitted work.
6. **Never use `git stash` for any reason.** Do not stash to test something, compare branches, or check pre-existing state. Stashing risks merge conflicts on pop and can destroy in-progress work across the working tree. If you need to verify pre-existing behavior, use `git show` or `git diff` against a commit ref instead.

Violating this rule causes cascading failures across the team. Treat every line you didn't write as load-bearing.

**Defer to other active agents to fix their own errors.** If you run into compile, clippy, formatting, or test errors that are unrelated to your own work, wait a few minutes and check again before intervening. Repeat this patience loop while the error appears likely to belong to another active agent's in-progress changes. If the same unrelated error is still present after multiple waiting iterations, such as roughly 10 minutes, then you may proceed to fix the issue while preserving all unrelated work.

### Agent Team Orchestration Standards

When creating or participating in an agent team (whether triggered by `/batch-mechanics` or auto-initiated):

1. **Use existing skills.** Every implementation must follow the relevant skill checklist (`/add-engine-effect`, `/add-keyword`, `/add-trigger`, etc.). No ad-hoc approaches.
2. **Teammates cannot spawn subagents.** All review subagents must be spawned by the lead. The lead receives the plan/implementation from the teammate, spawns a review subagent (model: opus), and sends feedback back to the teammate. This review loop repeats until clean (max 3 rounds).
3. **Sequential execution by default.** Multiple teammates must not implement concurrently unless their file sets are completely disjoint. Shared files like `types/ability.rs`, `effects/mod.rs`, and `parser/oracle.rs` are frequent collision points.
4. **Verify before committing.** Run `cargo fmt --all`, then check Tilt logs (`tilt logs clippy --tail 30 --since 2m`, `tilt logs test-engine --tail 50 --since 2m`) to confirm clippy and tests pass. For frontend changes, check `tilt logs check-frontend --tail 30 --since 2m`. Do NOT run cargo build/clippy/test directly — Tilt handles these continuously and running them manually causes target lock contention. TypeScript errors must not be committed.

### CRITICAL: Use Tilt Logs Instead of Running Builds

**Tilt is always running and continuously rebuilds on file changes.** Do NOT run `cargo build`, `cargo clippy`, `cargo test -p engine`, `pnpm run type-check`, or `pnpm lint` directly — these compete for cargo target locks and queue up redundant builds. Instead, check the Tilt logs for the relevant resource to see if your changes compiled/passed.

**Available Tilt resources** (defined in `Tiltfile`):
| Resource | What it does | Triggers on |
|----------|-------------|-------------|
| `clippy` | `cargo clippy --all-targets -- -D warnings` | `crates/` changes |
| `test-engine` | `cargo test -p engine` | `crates/engine/src/` changes |
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
resources=(clippy test-engine card-data)
deadline=$((SECONDS + 900))
until [[ $SECONDS -ge $deadline ]]; do
  all_done=1
  for r in $resources; do
    json=$(tilt get uiresource "$r" -o json)
    st=$(jq -r '.status.updateStatus // "unknown"' <<< "$json")
    current=$(jq -r '.status.currentBuild.spanID // "none"' <<< "$json")
    last=$(jq -r '.status.buildHistory[0].finishTime // "none"' <<< "$json")
    err=$(jq -r '.status.buildHistory[0].error // ""' <<< "$json")
    printf '%s status=%s current=%s last=%s\n' "$r" "$st" "$current" "$last"
    if [[ "$st" == "error" ]]; then
      printf '%s error=%s\n' "$r" "$err"
      exit 1
    fi
    [[ "$st" == "ok" ]] || all_done=0
  done
  [[ $all_done -eq 1 ]] && exit 0
  sleep 20
done
echo "Timed out waiting for Tilt resources" >&2
exit 124
```

**Rules:**
- After saving files, wait ~10-30s for Tilt to detect changes and rebuild, then check logs.
- Prefer the `tilt get uiresource` polling loop when waiting on multiple resources; use logs after a resource reports `error` or when you need detailed output.
- Use `--follow` only when you need to stream live output (e.g., waiting for a build in progress).
- Use `--since` to limit output — don't dump entire build history.
- If a resource shows errors, fix your code and Tilt will automatically rebuild.
- Only run cargo/pnpm commands directly if Tilt is confirmed not running (check `tilt status` or `curl -s localhost:10350/api/status`).
- `cargo fmt --all` is the one exception — always run it directly since Tilt doesn't auto-format.

### CRITICAL: Building Blocks and Architecture Purity

**Before writing any logic, search for existing building blocks.** Duplicating what these already do is a defect. Check these modules before writing new utility functions:

| Module | What lives here |
|--------|----------------|
| `parser/oracle_nom/` | **Nom 8.0 combinator foundation** — shared typed combinators used by all parser branches. `primitives.rs`: `parse_number`, `parse_number_or_x`, `parse_mana_symbol`, `parse_mana_cost`, `parse_color`, `parse_counter_type`, `parse_pt_modifier`, `parse_roman_numeral`. `target.rs`: target phrase combinators. `quantity.rs`: quantity expression combinators (including `parse_quantity_ref`, `parse_target_power_ref`). `duration.rs`: duration combinators. `condition.rs`: condition combinators. `filter.rs`: filter combinators. `error.rs`: `OracleResult` type, `parse_or_unimplemented` error boundary (converts nom `VerboseError` → `Effect::Unimplemented` with diagnostic trace), `format_verbose_error`. `context.rs`: `ParseContext` for stateful parsing. `bridge.rs`: `nom_on_lower` (run nom on lowercase, map remainder to original case), `nom_on_lower_required` (same with Result), `nom_parse_lower` (discard remainder). All parsers delegate atomic and structural operations to these combinators. `dispatch_line_nom` in `oracle.rs` uses these as the primary dispatch path for unclassified lines. |
| `parser/oracle_util.rs` | `TextPair` (paired original/lowercase slices with `strip_prefix`/`strip_suffix` for case-insensitive matching preserving original case), `parse_number` (delegates to `nom_primitives::parse_number` with word-boundary guard and X→0 fallback), mana symbol parsing, reminder text stripping, possessive/pronoun phrase matching, phrase variant helpers, subtype canonicalization, filter merging, `SELF_REF_TYPE_PHRASES` (normalization-safe self-reference constant), `SELF_REF_PARSE_ONLY_PHRASES` (parse-only self-references excluded from `~` normalization) |
| `parser/oracle_quantity.rs` | Semantic quantity interpretation: `parse_cda_quantity`, `parse_quantity_ref`, `parse_event_context_quantity`, `parse_for_each_clause` — maps Oracle text phrases to typed `QuantityExpr`/`QuantityRef` values |
| `parser/oracle_target.rs` | Target extraction from Oracle text (`"target creature"` → `TargetFilter`), type phrase parsing, event context refs |
| `parser/oracle_static.rs` | Static ability line parsing, continuous modification extraction (`"gets +N/+M and has flying"` → typed modifications), `strip_casting_prohibition_subject` (shared subject→`ProhibitionScope` extractor for all casting prohibition patterns) |
| `game/filter.rs` | Runtime `TargetFilter` evaluation against game objects and players |
| `game/zones.rs` | Zone manipulation primitives — creating, moving, adding, removing objects |
| `game/targeting.rs` | Target legality, zone queries (`zone_object_ids` for all objects in a zone), and target validation |
| `game/quantity.rs` | Dynamic quantity resolution (`QuantityExpr` → concrete `i32` from game state). `ObjectCount` uses `TargetFilter::extract_in_zone()` to count objects in the correct zone (not just battlefield). `CountersOnTarget` mirrors `TargetPower` pattern — resolves against the first object target. |
| `game/ability_utils.rs` | Ability construction, target slot wiring, chained ability building, target selection/validation |
| `game/keywords.rs` | Keyword presence queries, protection checks, keyword string parsing |

**Self-review every change as you go.** After writing code, ask:
1. Did I duplicate logic that an existing helper already handles?
2. Is this inline extraction something that should use a shared building block?
3. Would this logic work for 50 cards, or just the one I'm looking at?
4. Did I extend the general pattern, or write a special case?

If the answer to any of these is wrong, **stop and refactor before moving on.** Do not leave architectural debt for later — fix it now, in the same change.

**Test the building block, not the special case.** Tests should verify that composable primitives work correctly across their full input range — not just that one card's Oracle text parses. A parser test for "exile target creature" is more valuable than a test for a single card name. Effect handler tests should exercise the handler's parameters, not replay a single card's resolution. When a building block is extended, add tests for the new capability at the building-block level.

## Build & Development Commands

Run `./scripts/setup.sh` for full onboarding (gen card data → build WASM → pnpm install).

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
cargo semantic-audit                 # Semantic audit of parsed card data (outputs data/semantic-audit.json + .md)
cargo scrape-feeds                  # Scrape metagame feeds from MTGGoldfish
cargo tune-ai                       # Run AI weight tuning (requires --features tune)
```

### WASM Build
```bash
./scripts/build-wasm.sh             # Build WASM (release): compile → wasm-bindgen → wasm-opt
./scripts/build-wasm.sh debug       # Build WASM (debug)
```
Requires `wasm-bindgen-cli` (v0.2.114) and optionally `wasm-opt` (binaryen). Output goes to `client/src/wasm/` (gitignored, regenerated).

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

## Documentation (`docs/`)

- **`.claude/skills/oracle-parser/SKILL.md`** — Oracle parser single source of truth: architecture, nom combinator mandate, parsing priority system, AST type system, all helper modules, CR annotation protocol, and contribution checklists.
- **`docs/MagicCompRules.txt`** — Full MTG Comprehensive Rules text from Wizards of the Coast. **Gitignored — not redistributed in this repo.** Run `./scripts/fetch-comp-rules.sh` once to download a local copy. Use this as the authoritative source when verifying CR numbers, looking up rule text, or annotating new game logic. `grep -n "^702.180" docs/MagicCompRules.txt` to look up any rule.
- **`.claude/skills/add-engine-effect/SKILL.md`** — Complete checklist for adding a new effect to the engine: types → parser → resolver → targeting → multiplayer filter → frontend → AI → tests. Covers every registration point that must be updated in lockstep. **Use this as the authoritative guide for any new effect work.**

## Conventions

### Rust Idioms — Write It Right the First Time

These patterns must be used on first write, not fixed after clippy complains:

- **`strip_prefix`/`strip_suffix`** over `starts_with` + manual slicing: `if let Some(rest) = s.strip_prefix("foo")` not `if s.starts_with("foo") { &s[3..] }`. **Compose from `std` primitives** — chain `strip_prefix` calls for multi-part patterns: `s.strip_prefix(word)?.strip_prefix(' ')?` not `format!("{word} ")` + `strip_prefix`. The standard library's string methods are building blocks; use them compositionally rather than constructing new strings to match against. Note: `strip_prefix` is still correct for `TextPair` dual-string operations and structural uses (punctuation stripping, dynamic string prefixes), but NOT for parsing dispatch (use nom `tag()` instead).
- **`TextPair` for dual-string parsing** — when matching on lowercase text but preserving original casing, use `TextPair::new(original, &lower)` and its `strip_prefix`/`strip_suffix` methods instead of manually computing `&text[text.len() - rest.len()..]` or `&text[prefix.len()..]`. In functions where `TextPair` cannot be constructed (e.g., `parse_target` where `lower` is a local `String` with a shorter lifetime than the returned `&str`), the `text.len() - rest.len()` offset idiom remains correct. See `oracle_util.rs`.
- **`oracle_nom` combinators** (see design principle above): use `nom_on_lower` bridge for mixed-case text, `tag().parse()` for already-lowercase text, and existing building blocks (`parse_single_cost`, `parse_target`, `parse_for_each_clause`, etc.) for composed operations. Use `parse_number_or_x` when X resolves to 0 (costs, P/T, counters); use `parse_number` when X should remain as `Variable("X")` (effect quantities). `parse_article_number` guards against word-boundary bugs (e.g., "another" matching as "a").
- **Iterator methods** over range-indexed loops: `for item in slice.iter().skip(1)` not `for i in 1..slice.len()`
- **`rsplit(' ').next()`** to get the last word, not `rsplit().collect::<Vec>().first()`
- **Exhaustive `match`** without wildcard fallbacks when the enum is known — let the compiler catch missing arms
- **Reuse existing building blocks** before writing one-off string logic. See the helper reference table in the "Building Blocks and Architecture Purity" section above
- **NEVER match on verbatim Oracle text strings** (e.g. `if lower == "the number of cards in your hand is greater than your life total"`). This is the single most prohibited pattern in the codebase. Every Oracle phrase must be decomposed into typed building blocks (grammar prefix/suffix stripping, composable helpers, typed enum variants). A verbatim string match handles exactly one card and poisons the parser architecture permanently. Instead: identify the grammatical structure, add typed `QuantityRef`/`Comparator`/`FilterProp` variants as needed, and parse with `strip_prefix`/`split_once` + helpers so the pattern covers every card in the class.
- **Compose nom combinators, don't enumerate permutations.** When a pattern has N independent dimensions (prefix × quantity × target × suffix), compose them with chained `alt()` + `tag()` calls — never expand into N! individual `tag("full string")` arms. Each axis of variation should be a single `alt()` call, chained sequentially. Example: `alt((tag("you put "), tag("you've put "))).parse(i)?; alt((tag("a counter"), tag("one or more counters"))).parse(i)?; tag(" on a ").parse(i)?; alt((tag("permanent"), tag("creature"))).parse(i)?;` — not 8 separate `tag("you put a counter on a permanent ...")` alternatives. The same principle applies to condition extraction: `parse_inner_condition` in `oracle_nom/condition.rs` is the **single authority** for all game-state conditions. Trigger and static parsers must delegate to it — never re-implement condition recognition as bespoke string matching. Only source-referential patterns ("if you cast it", "if it's attacking") that fundamentally require the trigger source as context may live outside the combinator.
- **Nest nom combinators by prefix dispatch.** When multiple `alt()` branches share a common prefix (e.g., `"during your upkeep"`, `"during your end step"`, `"during your turn"`), nest them: `preceded(tag("during "), parse_during_phrase)` where `parse_during_phrase` is a sub-combinator that dispatches on the remainder. This eliminates redundant prefix matching and mirrors BNF grammar production rules. Factor shared sub-patterns (e.g., opponent possessive `"an opponent's "` / `"an opponents "`) into reusable combinators. **When NOT to nest**: don't nest when alternatives are leaf-level variants of the same concept (e.g., apostrophe normalization) with no shared structural prefix.
- **Word-boundary scanning for multi-position phrase matching.** When timing/keyword phrases can appear at any position in a string (not just the start), use a scanning loop that tries a nom combinator at each word boundary: `while !remaining.is_empty() { if let Ok((rest, val)) = combinator(remaining) { results.push(val); remaining = rest.trim_start(); } else { remaining = remaining.find(' ').map_or("", |i| remaining[i+1..].trim_start()); } }`. This replaces scattered `contains()` chains with a single combinator tried at word boundaries — more precise (matches complete phrases, not arbitrary substrings) and defines all patterns in one composable combinator. See `scan_timing_restrictions` in `oracle_casting.rs` and `scan_for_phase` in `oracle_trigger.rs`.
- **Separate abstraction layers in enum design.** An enum variant must belong to exactly one semantic layer — do not conflate different concepts in the same type. Example: `QuantityRef` (a *reference* to a dynamic game value: `HandSize`, `LifeTotal`) must not contain `Fixed { value: i32 }` (a *constant* that requires no game-state lookup). Instead, introduce a wrapping expression type (`QuantityExpr`) that is either a `Ref(QuantityRef)` or a `Fixed(i32)`. Ask: "Does this variant belong to the same abstraction as all the others, or does it belong one level up?" Wrong layer placement creates API confusion, breaks exhaustive match semantics, and forces callers to handle heterogeneous cases that should be uniform.

### MTG Comprehensive Rules Annotations

**Any code that implements, enforces, or directly references an MTG game rule MUST be annotated with the corresponding Comprehensive Rules (CR) number.** This is not optional — it is a required part of every rules-related change, same as `cargo fmt`.

**Lookup:** The full Comprehensive Rules text is available at `docs/MagicCompRules.txt`.

**CRITICAL — Verification is mandatory, not optional:**
Every CR number you write MUST be verified by grepping `docs/MagicCompRules.txt` BEFORE adding it to code. This is non-negotiable. Do NOT rely on memory or training data for CR numbers — the 701.x keyword action numbers and 702.x keyword ability numbers are especially prone to hallucination because they are arbitrary sequential assignments with no mnemonic pattern. A wrong CR number is worse than no CR number: it creates false confidence that code was verified against the wrong rule.

```bash
# REQUIRED before writing any CR annotation:
grep -n "^701.21" docs/MagicCompRules.txt   # Verify: is 701.21 really Sacrifice?
grep -n "^702.122" docs/MagicCompRules.txt  # Verify: is 702.122 really Crew?
```

If you cannot find the rule number in `docs/MagicCompRules.txt`, do NOT write the annotation. Flag it as "needs manual verification" instead.

**Format:**
```rust
// CR 704.5a: A player with 0 or less life loses the game.
// CR 702.2c + CR 702.19b: Deathtouch with trample assigns lethal (1) to each blocker.   // interacting rules use `+`
// CR 704.3 / CR 800.4: SBAs may have ended the game during phase auto-advance.          // alternatives use `/`
/// Checks state-based actions (CR 704).                                                  // doc comment on rule-implementing function
```

**Rules:**
- **Prefix:** Always `CR`. Never `Rule`, `MTG Rule`, or bare numbers.
- **Number format:** `CR XXX`, `CR XXX.Y`, or `CR XXX.Ya`. Regex: `CR \d{3}(\.\d+[a-z]?)?`
- **Description is mandatory.** A bare `CR 704.5a` with no explanation is not acceptable — grep output must be self-documenting.
- **Placement:** Directly above or inline with the code that implements the rule.

**When writing or modifying engine code (`crates/engine/`):**
1. If you are adding new game logic, identify which CR rule(s) it implements and annotate.
2. If you are modifying existing game logic, verify existing CR annotations are present and still accurate. Add missing annotations.
3. If existing code near your change uses an old format (`Rule 514.1`, `MTG Rule 727`, `MTG 702.36`), migrate it to the `CR` format as part of your change.
4. Do not annotate boilerplate, serialization, or plumbing — only code that implements a game rule.

**Lookup:** `grep -r "CR 704" crates/engine/` finds all state-based action implementations. `grep -rn "CR \d" crates/engine/` lists all rule annotations. The `mtg-rules-auditor` agent can produce a full coverage report on demand.

### General Conventions

- Rust: `cargo fmt` + `clippy -D warnings` enforced in CI
- TypeScript: ESLint with `@typescript-eslint/recommended`, unused vars prefixed with `_`
- Frontend uses Tailwind CSS v4, Framer Motion for animations
- Tests colocated in `__tests__/` directories (frontend) or inline `#[cfg(test)]` modules (Rust)
- The `release` profile is optimized for WASM size: `opt-level = 'z'`, LTO, single codegen unit, stripped

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
