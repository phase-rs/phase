# Roadmap: Forge.rs

## Milestones

- ✅ **v1.0 MVP** — Phases 1-12 (shipped 2026-03-08) — [archive](milestones/v1.0-ROADMAP.md)
- ✅ **v1.1 Arena UI** — Phases 13-20 (shipped 2026-03-10) — [archive](milestones/v1.1-ROADMAP.md)
- 🚧 **v1.2 Migrate Data Source & Add Tests** — Phases 21-25 (in progress)

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
- [ ] **Phase 22: Test Infrastructure** - Build the GameScenario test harness and rules correctness test suite before any cards are migrated
- [ ] **Phase 23: Unified Card Loader** - Wire MTGJSON metadata + ability JSON into CardDatabase and prove it end-to-end with sample cards
- [ ] **Phase 24: Card Migration** - Convert all engine-supported cards via automated migration tool with behavioral parity validation
- [ ] **Phase 25: Forge Removal & Relicensing** - Remove all GPL data, feature-gate Forge parser, and relicense as MIT/Apache-2.0

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
**Plans**: TBD

Plans:
- [ ] 23-01: TBD
- [ ] 23-02: TBD

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
**Plans**: TBD

Plans:
- [ ] 24-01: TBD
- [ ] 24-02: TBD
- [ ] 24-03: TBD

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
**Plans**: TBD

Plans:
- [ ] 25-01: TBD
- [ ] 25-02: TBD

## Progress

**Execution Order:**
Phases 21 and 22 can execute in parallel. Phase 23 requires 21. Phase 24 requires 22 + 23. Phase 25 requires 24.

| Phase | Milestone | Plans Complete | Status | Completed |
|-------|-----------|----------------|--------|-----------|
| 1-12 | v1.0 | 40/40 | Complete | 2026-03-08 |
| 13-20 | v1.1 | 43/43 | Complete | 2026-03-10 |
| 21. Schema & MTGJSON Foundation | v1.2 | Complete    | 2026-03-10 | 2026-03-10 |
| 22. Test Infrastructure | 2/3 | In Progress|  | - |
| 23. Unified Card Loader | v1.2 | 0/? | Not started | - |
| 24. Card Migration | v1.2 | 0/? | Not started | - |
| 25. Forge Removal & Relicensing | v1.2 | 0/? | Not started | - |
