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

**Milestone Goal:** Replace Forge's GPL card data with MTGJSON (MIT) + Oracle text parser for typed ability definitions, add comprehensive test coverage, relicense as MIT/Apache-2.0, and extend to N-player/Commander.

- [x] **Phase 21: Schema & MTGJSON Foundation** - Define typed ability enums and MTGJSON card metadata loader that everything else builds on
- [x] **Phase 22: Test Infrastructure** - Build the GameScenario test harness and rules correctness test suite before any cards are migrated (completed 2026-03-10)
- [x] **Phase 23: Unified Card Loader** - Wire MTGJSON metadata + typed ability definitions into CardDatabase and prove it end-to-end with sample cards (completed 2026-03-10)
- [x] **Phase 24: Card Migration** - Convert all engine-supported cards via automated migration tool with behavioral parity validation (completed 2026-03-10)
- [x] **Phase 25: Forge Removal & Relicensing** - Remove all GPL data, feature-gate Forge parser, and relicense as MIT/Apache-2.0 (completed 2026-03-11)
- [x] **Phase 26: Polish & Fix Multiplayer** - Lobby, P2P, Tauri sidecar, concede/emotes/timers, connection UX (completed 2026-03-11)
- [x] **Phase 27: Aura Casting & Triggered Targeting** - Aura enchant targeting, triggered ability target selection, exile return tracking (completed 2026-03-11)

## Phase Details

### Phase 21: Schema & MTGJSON Foundation
**Goal**: The engine has typed ability definitions and can load card metadata from MTGJSON
**Depends on**: Nothing (first phase of v1.2)
**Requirements**: DATA-01, DATA-02, DATA-04
**Success Criteria** (what must be TRUE):
  1. `cargo test` includes a passing test that loads card metadata (name, mana cost, types, P/T, colors, keywords, oracle text, layout) from MTGJSON
  2. Typed Rust enums (Effect, TriggerMode, StaticMode, ReplacementEvent) represent all ability definitions
  3. JSON Schema generated from Rust types documents the ability format
  4. Round-trip test: typed ability definitions serialized and deserialized produce identical structures
**Plans**: 5 plans

Plans:
- [x] 21-01-PLAN.md — Define typed enums (Effect, StaticMode, ReplacementEvent) and add schemars/insta deps — completed 2026-03-10
- [x] 21-02-PLAN.md — Refactor all ~13 consumer files to use typed ability structs — completed 2026-03-10
- [x] 21-03-PLAN.md — MTGJSON loader, typed ability definitions, schema generation, and snapshot tests — completed 2026-03-10
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
**Goal**: The engine can load cards from MTGJSON metadata + typed ability definitions as its primary path, proven end-to-end with sample cards
**Depends on**: Phase 21
**Requirements**: DATA-03, MIGR-04
**Success Criteria** (what must be TRUE):
  1. `CardDatabase::load_json()` loads a card by merging MTGJSON metadata with typed ability definitions and produces a valid CardFace that the engine can use to play a game
  2. Multi-face cards (Adventure, Transform, Modal DFC) load correctly with both faces populated
  3. Loaded cards include MTGJSON scryfallOracleId and the frontend can use it for image lookups via Scryfall API
  4. A smoke test game using 5-10 JSON-loaded cards completes without errors (cards can be cast, abilities resolve, combat works)
**Known Concerns** (from Phase 21 review):
  - Implicit abilities must be made explicit during loading: basic lands need synthesized mana abilities (e.g., Forest → `{T}: Add {G}`), equipment needs Equip activated ability from `K:Equip:N` keyword, planeswalkers need loyalty costs wired through `AbilityCost::Loyalty` instead of `remaining_params["PW_Cost"]`
  - Cross-validation needed: test that parsed ability definitions match MTGJSON card data for completeness (catch missing/incomplete definitions at test time, not runtime)
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
  1. The card data pipeline generates typed ability definitions for all 32,300+ cards whose mechanics have registered engine handlers
  2. All previously supported cards (every card that passed the old CI coverage gate) still pass when loaded via the JSON path
  3. Per-card behavioral parity tests confirm that sampled migrated cards produce identical game outcomes as the original Forge format (sampling across mechanic categories, not exhaustive per-card)
  4. CI coverage gate passes against JSON card data with 100% Standard-legal coverage preserved
**Known Concerns** (from Phase 21 review):
  - Card data pipeline must generate explicit definitions for implicit abilities: basic lands (mana production), equipment (Equip activated ability from keyword), planeswalkers (loyalty cost in `AbilityCost::Loyalty` not string params)
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
  6. All card ability data uses native typed JSON schema (generated from Oracle text parser)
  7. `cargo test --all` passes and `card-data.json` uses the new format
**Plans**: 6 plans

**Execution Order:**
Wave 1: Plan 01 (type definitions + serde roundtrip tests)
Wave 2: Plans 02 + 03 + 06 (parallel — layers/filter subsystem, triggers/effects pipeline, bulk handler updates)
Wave 3: Plan 04 (migration binary + JSON migration)
Wave 4: Plan 05 (frontend + CI verification + human verify)

Plans:
- [x] 28-01-PLAN.md — Type system overhaul: TargetFilter, Duration, PtValue, ContinuousModification, typed definition structs, Effect cleanup, serde roundtrip tests — completed 2026-03-11
- [ ] 28-02-PLAN.md — Layers/filter/deck_loading rewrite: typed ContinuousModification, TargetFilter matching, svars removal
- [x] 28-03-PLAN.md — Triggers/effects core/parser rewrite: typed TriggerDefinition, SubAbility chain, cleanup/mana handlers, forge-compat gating — completed 2026-03-11
- [ ] 28-04-PLAN.md — Migration binary + 32K JSON file migration + json_loader update + schema regeneration
- [ ] 28-05-PLAN.md — Frontend type updates, WASM rebuild, full CI verification, human gameplay verification
- [ ] 28-06-PLAN.md — Bulk effect handler updates + Effect::Other test rewrites across 22 files

### Phase 27: Aura Casting, Triggered Ability Targeting, and "Until Leaves" Exile Return
**Goal**: Implement full Aura spell support (targeting + attachment), triggered ability target selection, and "until source leaves the battlefield" exile return tracking
**Depends on**: Phase 28
**Requirements**: P27-AURA, P27-TRIG, P27-EXILE, P27-FILTER, P27-TEST, P27-TYPED
**Success Criteria** (what must be TRUE):
  1. Aura spells prompt the player for an enchant target during casting and attach to that target on resolution
  2. Triggered abilities with typed target filters prompt the player for target selection before going on the stack
  3. Cards exiled with Duration::UntilHostLeavesPlay return to the battlefield when the source leaves
  4. General filter matching in targeting.rs handles typed TargetFilter (including NonType properties)
  5. `cargo test --all` passes with new tests covering all three features
  6. Phase 27 context must be rewritten to use typed data model (no Forge-style params or SVars)
**Plans**: 4 plans

**Execution Order:**
Wave 1: Plan 01 (type contracts, typed targeting, card data fixes, frontend types)
Wave 2: Plans 02 + 03 (parallel — Aura casting/attachment, triggered targeting + exile return)
Wave 3: Plan 04 (gap closure — TriggerTargetSelection UI wiring + context doc rewrite)

Plans:
- [x] 27-01-PLAN.md — Type contracts, find_legal_targets_typed, PendingTrigger serde, GameState fields, frontend types, card data fixes
- [x] 27-02-PLAN.md — Aura enchant target selection during casting + attachment on resolution
- [x] 27-03-PLAN.md — Triggered ability target selection + exile return tracking
- [ ] 27-04-PLAN.md — Gap closure: TriggerTargetSelection UI wiring + CONTEXT.md typed model rewrite

### Phase 29: Support N Players in Engine and on Board in React for Various Formats
**Goal**: Extend the engine's hardcoded 2-player model to support N players (2-6), update the board UI to render multiple player areas, introduce format-awareness (Standard, Commander, Free-for-All, Two-Headed Giant), and update networking/lobby/deck builder/AI for multiplayer formats
**Depends on**: Phase 28
**Requirements**: NP-FORMAT, NP-SEAT, NP-ITER, NP-PRIORITY, NP-TURNS, NP-ELIM, NP-COMBAT, NP-ATTACK-TARGET, NP-COMMANDER, NP-CMDZONE, NP-CMDTAX, NP-OPPONENT-MIGRATION, NP-WASM, NP-SERVER, NP-FILTER, NP-AI-MIGRATE, NP-BOARD-UI, NP-PLAYERAREA, NP-COMPACT, NP-1V1-PARITY, NP-COMMANDER-UI, NP-ATTACK-UI, NP-BLOCK-UI, NP-AI-THREAT, NP-AI-SEARCH, NP-AI-SEAT, NP-LOBBY, NP-READY-UP, NP-SPECTATOR, NP-DISCONNECT, NP-DECKBUILDER, NP-LEGALITY, NP-COMMANDER-DECK, NP-SETUP-FLOW, NP-PRESETS, NP-PRECONS, NP-INTEGRATION, NP-WASM-BUILD
**Success Criteria** (what must be TRUE):
  1. FormatConfig with factory methods creates valid configurations for Standard, Commander, FFA, and 2HG
  2. Priority passes clockwise through all living players; stack resolves when all have passed consecutively
  3. Turn rotation follows seat_order, skipping eliminated players
  4. Elimination follows CR 800.4: permanents exiled, spells removed, game continues with remaining players
  5. Per-creature attack target selection works in multiplayer (splitting attacks across opponents)
  6. Commander rules: command zone, +2 tax per cast, 21 commander damage loss, zone redirection
  7. Zero remaining PlayerId(1 - x.0) patterns in the codebase
  8. Board UI renders 2-6 players with full/focused/compact modes, 1v1 visual parity maintained
  9. AI plays competently in multiplayer with threat-aware evaluation and scaled search
  10. Lobby supports format-aware game creation, ready-up, and spectators
  11. Deck builder supports Commander format with color identity enforcement
  12. Format-first game setup flow with presets and pre-built Commander decks
**Plans**: 16 plans

**Execution Order:**
Wave 1: Plan 01 (foundation types — FormatConfig, player iteration, GameState extensions)
Wave 2: Plans 02 + 03 + 04 (parallel — priority/turns/elimination + 2HG team turns, combat targeting, commander rules)
Wave 3: Plans 05 + 06 (parallel — core engine PlayerId(1-x) migration, WASM/server/AI crate migration)
Wave 4: Plans 09 + 14 + 15 + 16 (parallel — AI adaptation, effects migration A-M, remaining game module migration, effects migration N-Z)
Wave 5: Plan 07 (board UI refactor with PlayerArea/CompactStrip)
Wave 6: Plans 08 + 10 (parallel — combat UI, lobby/networking format-awareness + P2P enforcement)
Wave 7: Plan 11 (deck builder commander support)
Wave 8: Plan 12 (game setup flow + precons)
Wave 9: Plan 13 (integration verification + precon validation + gameplay checkpoint)

Plans:
- [ ] 29-01-PLAN.md — FormatConfig types, player iteration functions, GameState/Player extensions
- [ ] 29-02-PLAN.md — N-player priority, turn rotation, 2HG team turns, elimination system, SBA updates
- [ ] 29-03-PLAN.md — Per-creature attack target selection, AttackTarget enum, combat damage with commander tracking
- [ ] 29-04-PLAN.md — Commander rules: command zone, tax, zone redirection, color identity enforcement
- [ ] 29-05-PLAN.md — PlayerId(1-x) migration in core engine modules (~9 files)
- [ ] 29-06-PLAN.md — WASM bridge, server session, protocol, state filtering, AI crate migration for N players
- [ ] 29-07-PLAN.md — Board UI: PlayerArea (full/focused/compact), CompactStrip, GameBoard N-player layout, Commander display
- [ ] 29-08-PLAN.md — Combat UI: AttackTargetPicker, "Attack all" button, multi-defender blocking
- [ ] 29-09-PLAN.md — AI: threat-aware evaluation, paranoid search, scaled budgets, multi-opponent combat
- [ ] 29-10-PLAN.md — Lobby: format-aware host setup, ready room, spectator support, disconnect handling, P2P enforcement
- [ ] 29-11-PLAN.md — Deck builder: Commander support, format legality badges, color identity validation
- [ ] 29-12-PLAN.md — Game setup: format-first flow, FormatPicker, game presets, pre-built Commander decks
- [ ] 29-13-PLAN.md — Integration verification: WASM rebuild, precon validation, full test suite, gameplay checkpoint
- [ ] 29-14-PLAN.md — PlayerId(1-x) migration in effects modules A-M (~13 files)
- [ ] 29-15-PLAN.md — PlayerId(1-x) migration in remaining game modules + tests (~13 files)
- [ ] 29-16-PLAN.md — PlayerId(1-x) migration in effects modules N-Z (~15 files)

### Phase 30: Implement Building Blocks for Adventure and Event-Context Mechanics

**Goal:** Deliver four composable engine building blocks: event-context target resolution, parser possessive references, Adventure casting subsystem (CR 715), and damage prevention disabling via GameRestriction system
**Requirements**: BB-01, BB-02, BB-03, BB-04, BB-ALL
**Depends on:** Phase 29
**Plans:** 4/4 plans complete

**Execution Order:**
Wave 1: Plan 01 (type definitions + parser patterns for all building blocks)
Wave 2: Plan 02 (pipeline wiring — trigger event threading, prevention gating, AddRestriction handler)
Wave 3: Plan 03 (Adventure casting engine + AI support)
Wave 4: Plan 04 (Adventure frontend + Bonecrusher Giant integration test + human verify)

Plans:
- [x] 30-01-PLAN.md — Type definitions (TargetFilter event-context variants, GameRestriction, CastingPermission, AddRestriction effect) + parser event-context possessive patterns — completed 2026-03-16
- [x] 30-02-PLAN.md — Pipeline wiring: trigger event threading, event-context target resolution, prevention gating, restriction cleanup, AddRestriction handler — completed 2026-03-16
- [x] 30-03-PLAN.md — Adventure casting subsystem: cast choice, exile-on-resolve, cast-from-exile, AI support — completed 2026-03-16
- [x] 30-04-PLAN.md — Adventure frontend UI, TypeScript types, Bonecrusher Giant integration test, human verification — completed 2026-03-16

### Phase 31: Kaito, Bane of Nightmares Mechanics

**Goal:** Deliver five composable engine building blocks motivated by Kaito, Bane of Nightmares: Ninjutsu runtime (~30 cards), Emblem infrastructure (dozens of planeswalkers), compound static conditions, planeswalker-to-creature conditional animation, and a scalable "for each" dynamic quantity system. Full parser coverage for all new patterns. Kaito fully playable when complete.
**Requirements**: K31-NINJA, K31-EMBLEM, K31-COND, K31-ANIM, K31-QTY, K31-PARSE, K31-INT
**Depends on:** Phase 30
**Plans:** 1/5 plans executed

**Execution Order:**
Wave 1: Plans 01 + 02 + 04 (parallel — compound conditions + animation, dynamic quantities, Ninjutsu runtime)
Wave 2: Plan 03 (emblem infrastructure — depends on Plan 01 for compound conditions)
Wave 3: Plan 05 (integration test + emblem UI + human verification — depends on all prior plans)

Plans:
- [ ] 31-01-PLAN.md — Compound static conditions (And/Or/HasCounters) + layer evaluation + parser for planeswalker animation pattern
- [ ] 31-02-PLAN.md — Dynamic quantity system: QuantityExpr on Draw/LoseLife/Mill, PlayerFilter, resolve_quantity, "for each" parser
- [ ] 31-03-PLAN.md — Emblem infrastructure: Effect::CreateEmblem, is_emblem on GameObject, immunity, layer extension, parser
- [ ] 31-04-PLAN.md — Ninjutsu runtime: GameAction::ActivateNinjutsu, combat integration, handler, AI support, frontend types
- [ ] 31-05-PLAN.md — Kaito integration test, card data regeneration, emblem command zone UI, human gameplay verification
