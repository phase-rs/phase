# Requirements: Forge.ts

**Defined:** 2026-03-07
**Core Value:** A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent — with all cards behaving correctly according to MTG comprehensive rules.

## v1 Requirements

Requirements for initial release. Each maps to roadmap phases.

### Engine Core

- [ ] **ENG-01**: Full turn structure (untap, upkeep, draw, main1, combat phases, main2, end, cleanup)
- [ ] **ENG-02**: Priority system with LIFO stack resolution
- [ ] **ENG-03**: State-based actions with fixpoint loop checking
- [ ] **ENG-04**: Zone management (library, hand, battlefield, graveyard, stack, exile, command)
- [ ] **ENG-05**: Mana system (5 colors, colorless, generic, hybrid, phyrexian, X costs, snow)
- [ ] **ENG-06**: London mulligan

### Card Parser

- [x] **PARSE-01**: Parse Forge's .txt card definition format (Name, ManaCost, Types, PT, K, A, T, S, R, SVar, Oracle)
- [x] **PARSE-02**: Support all multi-face card types (Split, Flip, Transform, Meld, Adventure, MDFC)
- [ ] **PARSE-03**: Card database indexing by name with lazy loading
- [x] **PARSE-04**: ManaCost and CardType sub-parsers

### Abilities & Effects

- [x] **ABIL-01**: Ability parser for A:, T:, S:, R: strings into typed structures
- [ ] **ABIL-02**: SVar resolution (SubAbility$, Execute$, ReplaceWith$)
- [ ] **ABIL-03**: Cost parser (mana costs, tap, sacrifice, discard, life payment)
- [ ] **ABIL-04**: Target system with legality validation and rechecks on resolution
- [ ] **ABIL-05**: Condition system (ConditionPresent$, ConditionCompare$)
- [ ] **ABIL-06**: All 202 effect type handlers via registry
- [ ] **ABIL-07**: Sub-ability chaining

### Triggers

- [ ] **TRIG-01**: Event bus for game events
- [ ] **TRIG-02**: Trigger matching by mode against registered triggers
- [ ] **TRIG-03**: APNAP ordering for simultaneous triggers
- [ ] **TRIG-04**: All 137 trigger mode handlers

### Replacement Effects

- [ ] **REPL-01**: Replacement effect pipeline intercepting events before they resolve
- [ ] **REPL-02**: Per-event application tracking (each replacement modifies an event only once)
- [ ] **REPL-03**: Player choice when multiple replacements apply
- [ ] **REPL-04**: All 45 replacement effect handlers

### Static Abilities & Layers

- [ ] **STAT-01**: Seven-layer continuous effect evaluation per Rule 613
- [ ] **STAT-02**: Timestamp ordering within layers
- [ ] **STAT-03**: Intra-layer dependency detection
- [ ] **STAT-04**: All 61 static ability type handlers

### Combat

- [ ] **COMB-01**: Attack/block declaration with legality validation
- [ ] **COMB-02**: Damage assignment (first strike, double strike, trample, deathtouch, lifelink)
- [ ] **COMB-03**: Combat keyword interactions (flying/reach, menace, vigilance, haste, indestructible)
- [ ] **COMB-04**: Death triggers and post-combat state-based actions

### Keywords

- [ ] **KWRD-01**: Keyword registry mapping keywords to static abilities, triggers, replacements, and combat modifiers
- [ ] **KWRD-02**: 50+ keyword ability implementations (flying, trample, haste, hexproof, ward, flashback, kicker, cycling, etc.)

### UI

- [ ] **UI-01**: Battlefield layout with permanents, tap state, attachments, counters
- [ ] **UI-02**: Hand display with legal-play highlighting
- [ ] **UI-03**: Stack visualization
- [ ] **UI-04**: Phase/turn tracker
- [ ] **UI-05**: Life total display
- [ ] **UI-06**: Targeting UI with valid target highlighting
- [ ] **UI-07**: Mana payment UI with auto-tap and manual override
- [ ] **UI-08**: Card preview/zoom with Scryfall images
- [ ] **UI-09**: Choice prompts for modal effects
- [ ] **UI-10**: Game log
- [ ] **UI-11**: Touch-optimized responsive design (great on tablets)

### Deck Management

- [ ] **DECK-01**: Deck builder with card search and filtering
- [ ] **DECK-02**: Import .dck/.dec files from Forge
- [ ] **DECK-03**: Mana curve and color distribution display

### AI

- [ ] **AI-01**: Legal action enumeration from any game state
- [ ] **AI-02**: Board evaluation heuristic
- [ ] **AI-03**: Per-card ability decision logic (Forge-level)
- [ ] **AI-04**: Game tree search (leveraging Rust native performance)
- [ ] **AI-05**: Multiple difficulty levels

### Multiplayer

- [ ] **MP-01**: WebSocket server for authoritative game state
- [ ] **MP-02**: Hidden information handling (library order, opponent's hand)
- [ ] **MP-03**: Action synchronization (send actions, not full state)
- [ ] **MP-04**: Reconnection support

### Platform

- [ ] **PLAT-01**: Tauri desktop app (Windows, macOS, Linux)
- [ ] **PLAT-02**: PWA + WASM build for tablet/browser
- [x] **PLAT-03**: EngineAdapter abstraction (Tauri IPC and WASM bindings)
- [ ] **PLAT-04**: Scryfall card image caching (on-demand with local cache)
- [ ] **PLAT-05**: Standard format card coverage (60-70%+ of current Standard-legal cards)

### Quality of Life

- [ ] **QOL-01**: Undo for unrevealed-information actions
- [ ] **QOL-02**: Keyboard shortcuts (pass turn, full control, tap all lands)
- [ ] **QOL-03**: Card coverage dashboard (which cards/effects are supported)

## v2 Requirements

Deferred to future release. Tracked but not in current roadmap.

### Game Modes

- **MODE-01**: Draft/Sealed limited format support
- **MODE-02**: Quest/Adventure campaign mode
- **MODE-03**: Commander format (multiplayer, command zone, commander tax)
- **MODE-04**: Best-of-3 with sideboarding

### Polish

- **POL-01**: Card movement animations and spell effects
- **POL-02**: Sound effects and music
- **POL-03**: Custom UI layout (draggable/resizable panels)
- **POL-04**: Auto-yield system for repetitive triggers
- **POL-05**: Macro system (record/replay action sequences)

### Platform Expansion

- **PLATX-01**: Native iOS/Android via Tauri v2 mobile
- **PLATX-02**: App Store / Play Store distribution

## Out of Scope

Explicitly excluded. Documented to prevent scope creep.

| Feature | Reason |
|---------|--------|
| Collection/economy system | Open-source with all cards available; paywalls are antithetical |
| Cosmetics store | Not a revenue product; zero gameplay value |
| Chat system | Moderation burden; game log is sufficient |
| Social features (guilds, friends, leaderboards) | Backend infrastructure beyond scope |
| Alchemy/digital-only mechanics | Support paper MTG rules only |
| Real-time card rulings lookup | Correct rules engine makes rulings unnecessary |
| Direct Java class port | Functional architecture chosen over mirroring Forge's OOP |

## Traceability

Which phases cover which requirements. Updated during roadmap creation.

| Requirement | Phase | Status |
|-------------|-------|--------|
| ENG-01 | Phase 3 | Pending |
| ENG-02 | Phase 3 | Pending |
| ENG-03 | Phase 3 | Pending |
| ENG-04 | Phase 3 | Pending |
| ENG-05 | Phase 3 | Pending |
| ENG-06 | Phase 3 | Pending |
| PARSE-01 | Phase 2 | Complete |
| PARSE-02 | Phase 2 | Complete |
| PARSE-03 | Phase 2 | Pending |
| PARSE-04 | Phase 2 | Pending |
| ABIL-01 | Phase 2 | Pending |
| ABIL-02 | Phase 4 | Pending |
| ABIL-03 | Phase 4 | Pending |
| ABIL-04 | Phase 4 | Pending |
| ABIL-05 | Phase 4 | Pending |
| ABIL-06 | Phase 4 | Pending |
| ABIL-07 | Phase 4 | Pending |
| TRIG-01 | Phase 5 | Pending |
| TRIG-02 | Phase 5 | Pending |
| TRIG-03 | Phase 5 | Pending |
| TRIG-04 | Phase 5 | Pending |
| REPL-01 | Phase 6 | Pending |
| REPL-02 | Phase 6 | Pending |
| REPL-03 | Phase 6 | Pending |
| REPL-04 | Phase 6 | Pending |
| STAT-01 | Phase 6 | Pending |
| STAT-02 | Phase 6 | Pending |
| STAT-03 | Phase 6 | Pending |
| STAT-04 | Phase 6 | Pending |
| COMB-01 | Phase 5 | Pending |
| COMB-02 | Phase 5 | Pending |
| COMB-03 | Phase 5 | Pending |
| COMB-04 | Phase 5 | Pending |
| KWRD-01 | Phase 5 | Pending |
| KWRD-02 | Phase 5 | Pending |
| UI-01 | Phase 7 | Pending |
| UI-02 | Phase 7 | Pending |
| UI-03 | Phase 7 | Pending |
| UI-04 | Phase 7 | Pending |
| UI-05 | Phase 7 | Pending |
| UI-06 | Phase 7 | Pending |
| UI-07 | Phase 7 | Pending |
| UI-08 | Phase 7 | Pending |
| UI-09 | Phase 7 | Pending |
| UI-10 | Phase 7 | Pending |
| UI-11 | Phase 7 | Pending |
| DECK-01 | Phase 7 | Pending |
| DECK-02 | Phase 7 | Pending |
| DECK-03 | Phase 7 | Pending |
| AI-01 | Phase 8 | Pending |
| AI-02 | Phase 8 | Pending |
| AI-03 | Phase 8 | Pending |
| AI-04 | Phase 8 | Pending |
| AI-05 | Phase 8 | Pending |
| MP-01 | Phase 8 | Pending |
| MP-02 | Phase 8 | Pending |
| MP-03 | Phase 8 | Pending |
| MP-04 | Phase 8 | Pending |
| PLAT-01 | Phase 7 | Pending |
| PLAT-02 | Phase 7 | Pending |
| PLAT-03 | Phase 1 | Complete |
| PLAT-04 | Phase 7 | Pending |
| PLAT-05 | Phase 8 | Pending |
| QOL-01 | Phase 7 | Pending |
| QOL-02 | Phase 7 | Pending |
| QOL-03 | Phase 7 | Pending |

**Coverage:**
- v1 requirements: 57 total
- Mapped to phases: 57
- Unmapped: 0

---
*Requirements defined: 2026-03-07*
*Last updated: 2026-03-07 after roadmap creation*
