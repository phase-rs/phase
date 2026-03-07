# Roadmap: Forge.ts

## Overview

Forge.ts delivers a playable MTG game engine by building bottom-up through the rules engine dependency graph: types and build system first, then card parsing, then the core game loop, then abilities/effects, then triggers and combat, then advanced rules (layers, replacements), then platform bridges and UI, and finally AI and card coverage expansion. Each phase produces a testable, verifiable layer that the next phase depends on. The engine is built in Rust (pure, no platform dependencies) before any UI or platform integration, ensuring correctness through property-based testing at every level.

## Phases

**Phase Numbering:**
- Integer phases (1, 2, 3): Planned milestone work
- Decimal phases (2.1, 2.2): Urgent insertions (marked with INSERTED)

Decimal phases appear between their surrounding integers in numeric order.

- [ ] **Phase 1: Project Scaffold & Core Types** - Cargo workspace, dual-target build (native + WASM), React skeleton, CI, and core enum/struct definitions
- [x] **Phase 2: Card Parser & Database** - Parse Forge's .txt card format into typed structures with indexing and lazy loading (completed 2026-03-07)
- [ ] **Phase 3: Game State Engine** - Turn structure, priority, stack, zones, mana system, and state-based actions
- [ ] **Phase 4: Ability System & Effects** - Ability parsing, effect handler registry, targeting, conditions, and top effect types
- [ ] **Phase 5: Triggers & Combat** - Event bus, trigger matching, full combat system with keyword abilities
- [ ] **Phase 6: Advanced Rules** - Replacement effects, seven-layer continuous effects, static abilities
- [ ] **Phase 7: Platform Bridges & UI** - Tauri/WASM adapters, full React game UI, deck builder, Scryfall images, QoL features
- [ ] **Phase 8: AI & Multiplayer** - AI opponent with game tree search, WebSocket multiplayer, Standard card coverage target

## Phase Details

### Phase 1: Project Scaffold & Core Types
**Goal**: A working build system that compiles Rust to both native and WASM, with a React frontend skeleton that can import WASM bindings, and all core type definitions that drive the entire engine
**Depends on**: Nothing (first phase)
**Requirements**: PLAT-03
**Success Criteria** (what must be TRUE):
  1. `cargo build` produces a native binary and `cargo build --target wasm32-unknown-unknown` produces a WASM module from the same source
  2. The React app renders a placeholder screen that successfully imports and calls a WASM function
  3. Core type definitions (GameState, GameAction, GameEvent, Zone, Phase, ManaColor) exist as Rust enums/structs with TypeScript types auto-generated via tsify
  4. CI pipeline runs tests and reports WASM binary size on every commit
**Plans**: 2 plans

Plans:
- [ ] 01-01-PLAN.md — Cargo workspace, WASM build pipeline, and core Rust type definitions
- [ ] 01-02-PLAN.md — React frontend with EngineAdapter (PLAT-03), WASM integration, and CI pipeline

### Phase 2: Card Parser & Database
**Goal**: Forge's 32,300+ card definition files can be parsed into typed Rust structures, indexed, and queried by name
**Depends on**: Phase 1
**Requirements**: PARSE-01, PARSE-02, PARSE-03, PARSE-04, ABIL-01
**Success Criteria** (what must be TRUE):
  1. A Forge .txt card file (e.g., Lightning Bolt) is parsed into a typed CardDefinition struct with all fields (Name, ManaCost, Types, PT, Oracle, abilities)
  2. Multi-face cards (Split, Transform, MDFC, Adventure) parse correctly into their respective face structures
  3. Card database loads and indexes cards by name, returning results in under 10ms for single-card lookup
  4. Ability strings (A:, T:, S:, R: lines) parse into typed AbilityDefinition structures with identified ApiType, cost, and parameters
**Plans**: 3 plans

Plans:
- [ ] 02-01-PLAN.md — Card types (CardFace, CardRules, CardLayout, ManaCost, CardType) and sub-parsers (ManaCost, CardType, Ability)
- [ ] 02-02-PLAN.md — Card file parser with single-card and multi-face support (ALTERNATE, all layout types)
- [ ] 02-03-PLAN.md — Card database with directory loading, name indexing, and face-level lookup

### Phase 3: Game State Engine
**Goal**: Two players can take turns through a full MTG turn cycle -- untap, draw, play lands, tap for mana, pass priority, and have state-based actions enforced
**Depends on**: Phase 2
**Requirements**: ENG-01, ENG-02, ENG-03, ENG-04, ENG-05, ENG-06
**Success Criteria** (what must be TRUE):
  1. A game progresses through all turn phases (untap, upkeep, draw, main1, combat phases, main2, end, cleanup) with correct priority passes
  2. A player can play a land, tap it for mana, and the mana pool contains the correct color/amount
  3. Spells placed on the stack resolve in LIFO order with both players receiving priority between each resolution
  4. State-based actions fire automatically (creature with 0 toughness dies, player at 0 life loses, legend rule enforced)
  5. London mulligan works correctly (draw 7, choose to mulligan, draw 7 again, put N cards on bottom)
**Plans**: 3 plans

Plans:
- [ ] 03-01-PLAN.md — Foundation types (GameObject, expanded GameState/Player, ManaPool restructure) and zone operations
- [ ] 03-02-PLAN.md — Turn progression, priority system, stack resolution, and apply() engine entry point
- [ ] 03-03-PLAN.md — Mana payment system, state-based actions, London mulligan, and full integration

### Phase 4: Ability System & Effects
**Goal**: Cards can be cast with costs paid, targets chosen, and effects resolved -- a player can cast Lightning Bolt targeting a creature, Counterspell targeting a spell, and Giant Growth on their own creature
**Depends on**: Phase 3
**Requirements**: ABIL-02, ABIL-03, ABIL-04, ABIL-05, ABIL-06, ABIL-07
**Success Criteria** (what must be TRUE):
  1. A spell with mana cost can be cast by paying its cost (including hybrid, phyrexian, and X costs), placed on the stack, and resolved
  2. Targeted spells validate legal targets on cast and recheck on resolution (fizzling if target becomes illegal)
  3. Sub-ability chains resolve correctly (e.g., a spell that deals damage AND draws a card executes both in sequence)
  4. SVar resolution works for conditional ability references (SubAbility$, Execute$, ReplaceWith$)
  5. The top 15 effect types (Draw, DealDamage, ChangeZone, Pump, Destroy, Counter, Token, GainLife, LoseLife, Tap, Untap, AddCounter, RemoveCounter, Sacrifice, DiscardCard) all resolve correctly
**Plans**: 3 plans

Plans:
- [ ] 04-01-PLAN.md — Runtime ability types (ResolvedAbility, TargetRef) and effect handler registry with 15 top effect handlers
- [ ] 04-02-PLAN.md — Casting flow, targeting system, cost payment, and engine integration (CastSpell/ActivateAbility)
- [ ] 04-03-PLAN.md — SVar resolution, condition system, sub-ability chaining, and integration tests (Lightning Bolt, Counterspell, Giant Growth)

### Phase 5: Triggers & Combat
**Goal**: Creatures can attack, block, deal damage, and die -- triggering abilities fire automatically in correct order, enabling creature-based gameplay
**Depends on**: Phase 4
**Requirements**: TRIG-01, TRIG-02, TRIG-03, TRIG-04, COMB-01, COMB-02, COMB-03, COMB-04, KWRD-01, KWRD-02
**Success Criteria** (what must be TRUE):
  1. Creatures can be declared as attackers and blockers with legality validation (tapped creatures can't attack, flying creatures can only be blocked by flying/reach)
  2. Combat damage resolves correctly with first strike, double strike, trample, deathtouch, and lifelink all interacting properly
  3. ETB triggers fire when a creature enters the battlefield and are placed on the stack with correct APNAP ordering
  4. Death triggers fire when creatures die in combat, and post-combat state-based actions clean up the battlefield
  5. 50+ keyword abilities are registered and functional (flying, haste, hexproof, ward, flashback, kicker, cycling, etc.)
**Plans**: TBD

Plans:
- [ ] 05-01: TBD
- [ ] 05-02: TBD
- [ ] 05-03: TBD

### Phase 6: Advanced Rules
**Goal**: The engine handles MTG's most complex rule interactions -- replacement effects intercept events before they happen, continuous effects are evaluated through the seven-layer system, and static abilities modify the game state correctly
**Depends on**: Phase 5
**Requirements**: REPL-01, REPL-02, REPL-03, REPL-04, STAT-01, STAT-02, STAT-03, STAT-04
**Success Criteria** (what must be TRUE):
  1. A replacement effect (e.g., "If a creature would die, exile it instead") correctly intercepts the event and applies the modification
  2. When multiple replacement effects could apply to the same event, the affected player chooses which applies, and each replacement modifies the event only once
  3. Continuous effects are evaluated through all seven layers in correct order (copy, control, text, type, color, ability, P/T)
  4. Within a layer, timestamp ordering is respected, and intra-layer dependencies are detected and resolved correctly
  5. Static abilities that grant keywords or modify characteristics (e.g., "All creatures you control get +1/+1") apply and unapply correctly as permanents enter and leave the battlefield
**Plans**: TBD

Plans:
- [ ] 06-01: TBD
- [ ] 06-02: TBD

### Phase 7: Platform Bridges & UI
**Goal**: A player can launch the app (desktop or browser), see the game board with card images, interact with all game elements through a responsive UI, build decks, and play a full game visually
**Depends on**: Phase 6
**Requirements**: UI-01, UI-02, UI-03, UI-04, UI-05, UI-06, UI-07, UI-08, UI-09, UI-10, UI-11, DECK-01, DECK-02, DECK-03, PLAT-01, PLAT-02, PLAT-04, QOL-01, QOL-02, QOL-03
**Success Criteria** (what must be TRUE):
  1. The battlefield displays all permanents with correct tap state, attachments, counters, and card images loaded from Scryfall
  2. A player can see their hand, click a card to cast it, select targets with valid-target highlighting, and pay mana costs with auto-tap (and manual override)
  3. The stack, phase tracker, life totals, and game log are all visible and update in real time as the game progresses
  4. The deck builder allows searching cards, adding/removing them, viewing mana curve, and importing .dck/.dec files
  5. The app runs as both a Tauri desktop app and a PWA in the browser, with touch-optimized layout working well on tablets
**Plans**: TBD

Plans:
- [ ] 07-01: TBD
- [ ] 07-02: TBD
- [ ] 07-03: TBD
- [ ] 07-04: TBD

### Phase 8: AI & Multiplayer
**Goal**: A player can sit down and play a complete game of Magic against a competent AI opponent, or connect to another player over the network -- with Standard-format card coverage sufficient for real gameplay
**Depends on**: Phase 7
**Requirements**: AI-01, AI-02, AI-03, AI-04, AI-05, MP-01, MP-02, MP-03, MP-04, PLAT-05
**Success Criteria** (what must be TRUE):
  1. The AI opponent plays lands, casts spells at reasonable times, makes combat decisions (attack/block), and provides a challenging game at multiple difficulty levels
  2. Two players can connect via WebSocket and play a full game with hidden information handled correctly (neither player sees the other's hand or library order)
  3. Network games support reconnection -- a disconnected player can rejoin and resume the game
  4. At least 60% of current Standard-legal cards are playable with correct behavior
  5. A card coverage dashboard shows which cards and effects are supported vs. missing
**Plans**: TBD

Plans:
- [ ] 08-01: TBD
- [ ] 08-02: TBD
- [ ] 08-03: TBD

## Progress

**Execution Order:**
Phases execute in numeric order: 1 -> 2 -> 3 -> 4 -> 5 -> 6 -> 7 -> 8

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Project Scaffold & Core Types | 2/2 | Complete | 2026-03-07 |
| 2. Card Parser & Database | 3/3 | Complete   | 2026-03-07 |
| 3. Game State Engine | 2/3 | In Progress|  |
| 4. Ability System & Effects | 0/3 | Not started | - |
| 5. Triggers & Combat | 0/3 | Not started | - |
| 6. Advanced Rules | 0/2 | Not started | - |
| 7. Platform Bridges & UI | 0/4 | Not started | - |
| 8. AI & Multiplayer | 0/3 | Not started | - |
