# Roadmap: Forge.rs

## Milestones

- ✅ **v1.0 MVP** — Phases 1-12 (shipped 2026-03-08) — [archive](milestones/v1.0-ROADMAP.md)
- ✅ **v1.1 Arena UI** — Phases 13-20 (shipped 2026-03-10) — [archive](milestones/v1.1-ROADMAP.md)
- 🚧 **v1.2 Migrate Data Source & Add Tests** — Phases 21-28 (in progress)

## Phases

<details>
<summary>✅ v1.0 MVP (Phases 1-12) — SHIPPED 2026-03-08</summary>

- [x] Phase 1: Project Scaffold & Core Types (2/2 plans) — completed 2026-03-07
- [x] Phase 2: Card Parser & Database (3/3 plans) — completed 2026-03-07
- [x] Phase 3: Game State Engine (3/3 plans) — completed 2026-03-08
- [x] Phase 4: Ability System & Effects (3/3 plans) — completed 2026-03-08
- [x] Phase 5: Triggers & Combat (3/3 plans) — completed 2026-03-08
- [x] Phase 6: Advanced Rules (3/3 plans) — completed 2026-03-08
- [x] Phase 7: Platform Bridges & UI (9/9 plans) — completed 2026-03-08
- [x] Phase 8: AI & Multiplayer (5/5 plans) — completed 2026-03-08
- [x] Phase 9: Wire DeckBuilder to Game Engine (3/3 plans) — completed 2026-03-08
- [x] Phase 10: Fix Undo/WASM State Sync (1/1 plan) — completed 2026-03-08
- [x] Phase 11: Tech Debt Cleanup (3/3 plans) — completed 2026-03-08
- [x] Phase 12: Multiplayer Wiring & Final Cleanup (2/2 plans) — completed 2026-03-08

</details>

<details>
<summary>✅ v1.1 Arena UI (Phases 13-20) — SHIPPED 2026-03-10</summary>

- [x] Phase 13: Foundation & Board Layout (5/5 plans) — completed 2026-03-09
- [x] Phase 14: Animation Pipeline (4/4 plans) — completed 2026-03-09
- [x] Phase 15: Game Loop & Controllers (3/3 plans) — completed 2026-03-09
- [x] Phase 16: Audio System (3/3 plans) — completed 2026-03-09
- [x] Phase 17: MTG-Specific UI (5/5 plans) — completed 2026-03-09
- [x] Phase 18: Mechanic Implementation (5/5 plans) — completed 2026-03-09
- [x] Phase 19: MTGA Visual Fidelity (8/8 plans) — completed 2026-03-09
- [x] Phase 20: Engine Completeness (10/10 plans) — completed 2026-03-10

</details>

### 🚧 v1.2 Migrate Data Source & Add Tests (In Progress)

**Milestone Goal:** Replace Forge's GPL card data with MTGJSON (MIT) + custom ability JSON format, add comprehensive test coverage, and relicense the project as MIT/Apache-2.0.

- [x] **Phase 21: Schema & MTGJSON Foundation** - Define the typed ability JSON schema and MTGJSON card metadata loader that everything else builds on
- [x] **Phase 22: Test Infrastructure** - Build the GameScenario test harness and rules correctness test suite before any cards are migrated (completed 2026-03-10)
- [x] **Phase 23: Unified Card Loader** - Wire MTGJSON metadata + ability JSON into CardDatabase and prove it end-to-end with sample cards (completed 2026-03-10)
- [x] **Phase 24: Card Migration** - Convert all engine-supported cards via automated migration tool with behavioral parity validation (completed 2026-03-10)
- [x] **Phase 25: Forge Removal & Relicensing** - Remove all GPL data, feature-gate Forge parser, and relicense as MIT/Apache-2.0 (completed 2026-03-11)
- [x] **Phase 26: Polish & Fix Multiplayer** - Lobby, P2P, Tauri sidecar, concede/emotes/timers, connection UX (completed 2026-03-11)

## Phase Details

### Phase 21: Schema & MTGJSON Foundation
**Goal**: The engine has a validated, schema-documented ability format and can load card metadata from MTGJSON
**Depends on**: Nothing (first phase of v1.2)
**Requirements**: DATA-01, DATA-02, DATA-04
**Success Criteria** (what must be TRUE):
  1. `cargo test` includes a passing test that loads card metadata (name, mana cost, types, P/T, colors, keywords, oracle text, layout) from MTGJSON StandardAtomic.json for a known card
  2. A hand-authored ability JSON file for a test card deserializes into the engine's AbilityDefinition/TriggerDefinition/StaticDefinition/ReplacementDefinition types without error
  3. Running `cargo test` produces (or validates against) a JSON Schema file that documents every field in the ability format, usable for editor autocompletion
  4. Round-trip test: an ability JSON file serialized from Rust types and deserialized back produces identical typed structures
**Plans**: 5 plans

Plans:
- [x] 21-01-PLAN.md — Define typed enums (Effect, StaticMode, ReplacementEvent) and add schemars/insta deps — completed 2026-03-10
- [x] 21-02-PLAN.md — Refactor all ~13 consumer files to use typed ability structs — completed 2026-03-10
- [x] 21-03-PLAN.md — MTGJSON loader, ability JSON file, schema generation, and snapshot tests — completed 2026-03-10
- [x] 21-04-PLAN.md — Gap closure: thread typed Effect through ResolvedAbility and replace string dispatch — completed 2026-03-10

### Phase 22: Test Infrastructure
**Goal**: Developers can write self-contained rules correctness tests that run in CI with no filesystem dependencies
**Depends on**: Nothing (parallel with Phase 21 -- no dependency on new card format)
**Requirements**: TEST-01, TEST-02, TEST-03
**Success Criteria** (what must be TRUE):
  1. A GameScenario test can set up a board state (add specific cards, set phase/turn, set life totals) and execute actions without touching the filesystem or loading card data files
  2. Scenario-based tests for core mechanics pass: ETB triggers fire, combat damage resolves correctly, stack resolves LIFO, state-based actions check triggers, layer system applies in order, keyword interactions (e.g., deathtouch + trample) produce correct outcomes
  3. `insta` snapshot tests capture GameState after action sequences and `cargo test` fails if snapshots change unexpectedly
  4. `cargo test --all` completes with zero silent skips (no tests that pass by doing nothing when files are missing)
**Plans**: 3 plans

Plans:
- [x] 22-01-PLAN.md — GameScenario builder, CardBuilder, GameRunner, GameSnapshot, and integration test scaffolding — completed 2026-03-10
- [x] 22-02-PLAN.md — Rules tests: ETB triggers, stack resolution, state-based actions, targeting — completed 2026-03-10
- [ ] 22-03-PLAN.md — Rules tests: combat damage, keyword interactions, layer system, and snapshot golden masters

### Phase 23: Unified Card Loader
**Goal**: The engine can load cards from MTGJSON metadata + ability JSON as its primary path, proven end-to-end with sample cards
**Depends on**: Phase 21
**Requirements**: DATA-03, MIGR-04
**Success Criteria** (what must be TRUE):
  1. `CardDatabase::load_json()` loads a card by merging MTGJSON metadata with an ability JSON file and produces a valid CardFace that the engine can use to play a game
  2. Multi-face cards (Adventure, Transform, Modal DFC) load correctly with both faces populated
  3. Loaded cards include MTGJSON scryfallOracleId and the frontend can use it for image lookups via Scryfall API
  4. A smoke test game using 5-10 JSON-loaded cards completes without errors (cards can be cast, abilities resolve, combat works)
**Known Concerns** (from Phase 21 review):
  - Implicit abilities must be made explicit during loading: basic lands need synthesized mana abilities (e.g., Forest → `{T}: Add {G}`), equipment needs Equip activated ability from `K:Equip:N` keyword, planeswalkers need loyalty costs wired through `AbilityCost::Loyalty` instead of `remaining_params["PW_Cost"]`
  - Cross-validation needed: test that ability JSON files match MTGJSON card data for completeness (catch missing/incomplete card definitions at test time, not runtime)
  - Vanilla creatures with empty ability vectors are already handled (`casting.rs` synthesizes a Spell marker) — no action needed
**Plans**: 2 plans

Plans:
- [x] 23-01-PLAN.md — JSON loader module with merge logic, implicit ability synthesis, type contracts, and load_json()
- [x] 23-02-PLAN.md — Ability JSON files for 8 smoke test cards, integration smoke test, and cross-validation

### Phase 24: Card Migration
**Goal**: All engine-supported cards are converted to the new format with automated tooling, and behavioral parity is validated
**Depends on**: Phase 22, Phase 23
**Requirements**: MIGR-01, MIGR-03, MIGR-05, TEST-04
**Success Criteria** (what must be TRUE):
  1. An automated migration tool converts Forge .txt card definitions to ability JSON files, and it has processed all 32,300+ Forge cards (producing ability files for every card whose mechanics have registered engine handlers)
  2. All previously supported cards (every card that passed the old CI coverage gate) still pass when loaded via the JSON path
  3. Per-card behavioral parity tests confirm that sampled migrated cards produce identical game outcomes as the original Forge format (sampling across mechanic categories, not exhaustive per-card)
  4. CI coverage gate passes against JSON card data with 100% Standard-legal coverage preserved
**Known Concerns** (from Phase 21 review):
  - Migration tool must generate explicit ability JSON for implicit abilities: basic lands (mana production), equipment (Equip activated ability from keyword), planeswalkers (loyalty cost in `AbilityCost::Loyalty` not string params)
  - Migration tool should validate generated ability JSON against MTGJSON oracle text to catch omissions
**Plans**: 3 plans

Plans:
- [x] 24-01-PLAN.md — Cost parser enhancement + migration tool binary + execute migration — completed 2026-03-10
- [x] 24-02-PLAN.md — Standard card manifest, MTGJSON fixture expansion, and Forge vs JSON parity tests — completed 2026-03-10
- [x] 24-03-PLAN.md — JSON coverage gate in coverage report binary + CI dual-gate update — completed 2026-03-10

### Phase 25: Forge Removal & Relicensing
**Goal**: The project contains no GPL-licensed data and is relicensed as MIT/Apache-2.0
**Depends on**: Phase 24
**Requirements**: MIGR-02, LICN-01, LICN-02, LICN-03
**Success Criteria** (what must be TRUE):
  1. `data/cardsfolder/` and `data/standard-cards/` directories are deleted from the repository and the Forge parser is feature-gated behind `forge-compat` (not compiled by default)
  2. LICENSE file(s) specify MIT/Apache-2.0 dual license and all Cargo.toml files reflect the new license
  3. PROJECT.md constraints and key decisions are updated to reflect MTGJSON + own ability format (no mention of Forge as a runtime dependency)
  4. `coverage.rs` reads JSON format and the CI gate (100% Standard coverage) passes on the main branch with no Forge data present
  5. `cargo build --target wasm32-unknown-unknown` succeeds and `cargo test --all` passes with the `forge-compat` feature disabled
**Plans**: 3 plans

Plans:
- [x] 25-01-PLAN.md — Refactor all dispatch from string-based to typed pattern matching on enums — completed 2026-03-11
- [x] 25-02-PLAN.md — Feature-gate Forge code, delete Forge data, simplify coverage binary and CI — completed 2026-03-11
- [x] 25-03-PLAN.md — License files, Cargo.toml updates, and documentation scrub — completed 2026-03-11

### Phase 26: Polish and Fix Multiplayer with Lobby and Embedded Server
**Goal**: Two players can discover and join games via a real-time lobby, host games from desktop (Tauri sidecar) or browser (P2P WebRTC), and enjoy a polished multiplayer experience with concede, emotes, timers, and proper connection UX
**Depends on**: Phase 25
**Requirements**: MP-BUG-A, MP-BUG-B, MP-BUG-C, MP-BUG-D, MP-BUG-E, MP-IDENT, MP-LOBBY-SRV, MP-CONCEDE-SRV, MP-EMOTE-SRV, MP-TIMER-SRV, MP-LOBBY-UI, MP-MENU-FLOW, MP-HOST-SETUP, MP-WAITING, MP-SETTINGS, MP-P2P, MP-P2P-HOST, MP-P2P-GUEST, MP-SIDECAR, MP-CONNECT-UX, MP-SERVER-DETECT, MP-CONCEDE, MP-EMOTE, MP-TIMER-UI, MP-GAMEOVER, MP-OPPONENT-NAME
**Success Criteria** (what must be TRUE):
  1. All 5 multiplayer bugs (A-E) are fixed: stale session cleared, deck validated, opponent actions visible, getAiAction safe, dynamic player ID
  2. A real-time game lobby shows waiting public games, player count, and supports join by click or code entry with optional password
  3. Host setup screen configures display name, visibility, password, and per-turn timer
  4. Browser/PWA users can host P2P games via WebRTC (PeerJS) with host-authoritative engine
  5. Tauri desktop users can host games via embedded sidecar server
  6. Players can concede, send MTGA-style emotes, see opponent name, and return to lobby after game over
**Plans**: 6 plans

**Execution Order:**
Wave 1: Plans 01 + 02 (parallel — bug fixes + player identity, server lobby protocol)
Wave 2: Plans 03 + 04 (parallel — frontend lobby UI, P2P adapter)
Wave 3: Plans 05 + 06 (parallel — Tauri sidecar + connection UX, in-game multiplayer UX)

Plans:
- [x] 26-01-PLAN.md — Fix bugs A-E, create multiplayerStore, replace hardcoded PLAYER_ID — completed 2026-03-11
- [x] 26-02-PLAN.md — Extend server protocol with lobby, concede, emote, timer; create LobbyManager; wire into phase-server — completed 2026-03-11
- [x] 26-03-PLAN.md — Frontend lobby UI (LobbyView, HostSetup, WaitingScreen), menu flow, multiplayer settings — completed 2026-03-11
- [x] 26-04-PLAN.md — Port Alchemy P2P network layer, implement P2PHostAdapter/P2PGuestAdapter — completed 2026-03-11
- [x] 26-05-PLAN.md — Tauri sidecar configuration, smart server detection, CODE@IP parsing, connection dot — completed 2026-03-11
- [x] 26-06-PLAN.md — Concede dialog, emotes, opponent name, timer UI, enhanced game over with lobby return — completed 2026-03-11

## Progress

**Execution Order:**
Phases 21 and 22 can execute in parallel. Phase 23 requires 21. Phase 24 requires 22 + 23. Phase 25 requires 24. Phase 26 requires 25. Phase 28 requires 26. Phase 27 requires 28.

| Phase | Milestone | Plans Complete | Status | Completed |
|-------|-----------|----------------|--------|-----------|
| 1-12 | v1.0 | 40/40 | Complete | 2026-03-08 |
| 13-20 | v1.1 | 43/43 | Complete | 2026-03-10 |
| 21. Schema & MTGJSON Foundation | v1.2 | Complete    | 2026-03-10 | 2026-03-10 |
| 22. Test Infrastructure | 3/3 | Complete    | 2026-03-10 | - |
| 23. Unified Card Loader | 2/2 | Complete    | 2026-03-10 | - |
| 24. Card Migration | 3/3 | Complete    | 2026-03-10 | - |
| 25. Forge Removal & Relicensing | 3/3 | Complete   | 2026-03-11 | - |
| 26. Multiplayer Polish | 6/6 | Complete   | 2026-03-11 | 2026-03-11 |

### Phase 28: Native Ability Data Model
**Goal**: Eliminate all Forge scripting DSL from the card data format and engine runtime — every ability, trigger, static, replacement, SVar chain, and filter expression expressed through fully typed Rust structures with zero HashMap<String, String>, zero raw SVar strings, zero Forge filter syntax, and zero runtime string parsing
**Depends on**: Phase 26
**Requirements**: NAT-01, NAT-02, NAT-03, NAT-04, NAT-05, NAT-06
**Success Criteria** (what must be TRUE):
  1. `TriggerDefinition`, `StaticDefinition`, and `ReplacementDefinition` use typed struct fields instead of `params: HashMap<String, String>`
  2. `svars: HashMap<String, String>` eliminated from `CardFace` and `GameObject` — SubAbility chains resolved at data-load time as typed structures
  3. `remaining_params` field removed from `AbilityDefinition` — all parameters mapped to typed fields
  4. `TargetSpec` replaced with typed `TargetFilter` enum — no Forge filter strings at runtime
  5. `parser::ability::parse_ability()` gated behind `forge-compat` — zero Forge string parsing at runtime
  6. All 32K `data/abilities/*.json` files migrated to native typed JSON schema
  7. `cargo test --all` passes and `card-data.json` uses the new format
**Plans**: 6 plans

**Execution Order:**
Wave 1: Plan 01 (type definitions + serde roundtrip tests)
Wave 2: Plans 02 + 03 + 06 (parallel — layers/filter subsystem, triggers/effects pipeline, bulk handler updates)
Wave 3: Plan 04 (migration binary + JSON migration)
Wave 4: Plan 05 (frontend + CI verification + human verify)

Plans:
- [ ] 28-01-PLAN.md — Type system overhaul: TargetFilter, Duration, PtValue, ContinuousModification, typed definition structs, Effect cleanup, serde roundtrip tests
- [ ] 28-02-PLAN.md — Layers/filter/deck_loading rewrite: typed ContinuousModification, TargetFilter matching, svars removal
- [ ] 28-03-PLAN.md — Triggers/effects core/parser rewrite: typed TriggerDefinition, SubAbility chain, cleanup/mana handlers, forge-compat gating
- [ ] 28-04-PLAN.md — Migration binary + 32K JSON file migration + json_loader update + schema regeneration
- [ ] 28-05-PLAN.md — Frontend type updates, WASM rebuild, full CI verification, human gameplay verification
- [ ] 28-06-PLAN.md — Bulk effect handler updates + Effect::Other test rewrites across 22 files

### Phase 27: Aura Casting, Triggered Ability Targeting, and "Until Leaves" Exile Return
**Goal**: Implement full Aura spell support (targeting + attachment), triggered ability target selection, and "until source leaves the battlefield" exile return tracking
**Depends on**: Phase 28
**Success Criteria** (what must be TRUE):
  1. Aura spells prompt the player for an enchant target during casting and attach to that target on resolution
  2. Triggered abilities with typed target filters prompt the player for target selection before going on the stack
  3. Cards exiled with Duration::UntilHostLeavesPlay return to the battlefield when the source leaves
  4. General filter matching in targeting.rs handles typed TargetFilter (including NonType properties)
  5. `cargo test --all` passes with new tests covering all three features
  6. Phase 27 context must be rewritten to use typed data model (no Forge-style params or SVars)

Plans: TBD
