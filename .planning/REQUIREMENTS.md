# Requirements: Forge.ts

**Defined:** 2026-03-07
**Core Value:** A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent — with all cards behaving correctly according to MTG comprehensive rules.

## v1 Requirements

Requirements for initial release. Each maps to roadmap phases.

### Engine Core

- [x] **ENG-01**: Full turn structure (untap, upkeep, draw, main1, combat phases, main2, end, cleanup)
- [x] **ENG-02**: Priority system with LIFO stack resolution
- [x] **ENG-03**: State-based actions with fixpoint loop checking
- [x] **ENG-04**: Zone management (library, hand, battlefield, graveyard, stack, exile, command)
- [x] **ENG-05**: Mana system (5 colors, colorless, generic, hybrid, phyrexian, X costs, snow)
- [x] **ENG-06**: London mulligan

### Card Parser

- [x] **PARSE-01**: Parse Forge's .txt card definition format (Name, ManaCost, Types, PT, K, A, T, S, R, SVar, Oracle)
- [x] **PARSE-02**: Support all multi-face card types (Split, Flip, Transform, Meld, Adventure, MDFC)
- [x] **PARSE-03**: Card database indexing by name with lazy loading
- [x] **PARSE-04**: ManaCost and CardType sub-parsers

### Abilities & Effects

- [x] **ABIL-01**: Ability parser for A:, T:, S:, R: strings into typed structures
- [x] **ABIL-02**: SVar resolution (SubAbility$, Execute$, ReplaceWith$)
- [x] **ABIL-03**: Cost parser (mana costs, tap, sacrifice, discard, life payment)
- [x] **ABIL-04**: Target system with legality validation and rechecks on resolution
- [x] **ABIL-05**: Condition system (ConditionPresent$, ConditionCompare$)
- [x] **ABIL-06**: All 202 effect type handlers via registry
- [x] **ABIL-07**: Sub-ability chaining

### Triggers

- [x] **TRIG-01**: Event bus for game events
- [x] **TRIG-02**: Trigger matching by mode against registered triggers
- [x] **TRIG-03**: APNAP ordering for simultaneous triggers
- [x] **TRIG-04**: All 137 trigger mode handlers

### Replacement Effects

- [x] **REPL-01**: Replacement effect pipeline intercepting events before they resolve
- [x] **REPL-02**: Per-event application tracking (each replacement modifies an event only once)
- [x] **REPL-03**: Player choice when multiple replacements apply
- [x] **REPL-04**: All 45 replacement effect handlers

### Static Abilities & Layers

- [x] **STAT-01**: Seven-layer continuous effect evaluation per Rule 613
- [x] **STAT-02**: Timestamp ordering within layers
- [x] **STAT-03**: Intra-layer dependency detection
- [x] **STAT-04**: All 61 static ability type handlers

### Combat

- [x] **COMB-01**: Attack/block declaration with legality validation
- [x] **COMB-02**: Damage assignment (first strike, double strike, trample, deathtouch, lifelink)
- [x] **COMB-03**: Combat keyword interactions (flying/reach, menace, vigilance, haste, indestructible)
- [x] **COMB-04**: Death triggers and post-combat state-based actions

### Keywords

- [x] **KWRD-01**: Keyword registry mapping keywords to static abilities, triggers, replacements, and combat modifiers
- [x] **KWRD-02**: 50+ keyword ability implementations (flying, trample, haste, hexproof, ward, flashback, kicker, cycling, etc.)

### UI

- [x] **UI-01**: Battlefield layout with permanents, tap state, attachments, counters
- [x] **UI-02**: Hand display with legal-play highlighting
- [x] **UI-03**: Stack visualization
- [x] **UI-04**: Phase/turn tracker
- [x] **UI-05**: Life total display
- [x] **UI-06**: Targeting UI with valid target highlighting
- [x] **UI-07**: Mana payment UI with auto-tap and manual override
- [x] **UI-08**: Card preview/zoom with Scryfall images
- [x] **UI-09**: Choice prompts for modal effects
- [x] **UI-10**: Game log
- [x] **UI-11**: Touch-optimized responsive design (great on tablets)

### Deck Management

- [x] **DECK-01**: Deck builder with card search and filtering
- [x] **DECK-02**: Import .dck/.dec files from Forge
- [x] **DECK-03**: Mana curve and color distribution display

### AI

- [x] **AI-01**: Legal action enumeration from any game state
- [x] **AI-02**: Board evaluation heuristic
- [x] **AI-03**: Per-card ability decision logic (Forge-level)
- [x] **AI-04**: Game tree search (leveraging Rust native performance)
- [x] **AI-05**: Multiple difficulty levels

### Multiplayer

- [x] **MP-01**: WebSocket server for authoritative game state
- [x] **MP-02**: Hidden information handling (library order, opponent's hand)
- [x] **MP-03**: Action synchronization (send actions, not full state)
- [x] **MP-04**: Reconnection support

### Platform

- [x] **PLAT-01**: Tauri desktop app (Windows, macOS, Linux)
- [x] **PLAT-02**: PWA + WASM build for tablet/browser
- [x] **PLAT-03**: EngineAdapter abstraction (Tauri IPC and WASM bindings)
- [x] **PLAT-04**: Scryfall card image caching (on-demand with local cache)
- [x] **PLAT-05**: Standard format card coverage (60-70%+ of current Standard-legal cards)

### Quality of Life

- [x] **QOL-01**: Undo for unrevealed-information actions
- [x] **QOL-02**: Keyboard shortcuts (pass turn, full control, tap all lands)
- [x] **QOL-03**: Card coverage dashboard (which cards/effects are supported)

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
| ENG-01 | Phase 3 | Complete |
| ENG-02 | Phase 3 | Complete |
| ENG-03 | Phase 3 | Complete |
| ENG-04 | Phase 3 | Complete |
| ENG-05 | Phase 3 | Complete |
| ENG-06 | Phase 3 | Complete |
| PARSE-01 | Phase 2 | Complete |
| PARSE-02 | Phase 2 | Complete |
| PARSE-03 | Phase 2 | Complete |
| PARSE-04 | Phase 2 | Pending |
| ABIL-01 | Phase 2 | Pending |
| ABIL-02 | Phase 4 | Complete |
| ABIL-03 | Phase 4 | Complete |
| ABIL-04 | Phase 4 | Complete |
| ABIL-05 | Phase 4 | Complete |
| ABIL-06 | Phase 4 | Complete |
| ABIL-07 | Phase 4 | Complete |
| TRIG-01 | Phase 5 | Complete |
| TRIG-02 | Phase 5 | Complete |
| TRIG-03 | Phase 5 | Complete |
| TRIG-04 | Phase 5 | Complete |
| REPL-01 | Phase 6 | Complete |
| REPL-02 | Phase 6 | Complete |
| REPL-03 | Phase 6 | Complete |
| REPL-04 | Phase 6 | Complete |
| STAT-01 | Phase 6 | Complete |
| STAT-02 | Phase 6 | Complete |
| STAT-03 | Phase 6 | Complete |
| STAT-04 | Phase 6 | Complete |
| COMB-01 | Phase 5 | Complete |
| COMB-02 | Phase 5 | Complete |
| COMB-03 | Phase 5 | Complete |
| COMB-04 | Phase 5 | Complete |
| KWRD-01 | Phase 5 | Complete |
| KWRD-02 | Phase 5 | Complete |
| UI-01 | Phase 7 | Complete |
| UI-02 | Phase 7 | Complete |
| UI-03 | Phase 7 | Complete |
| UI-04 | Phase 7 | Complete |
| UI-05 | Phase 7 | Complete |
| UI-06 | Phase 7 | Complete |
| UI-07 | Phase 7 | Complete |
| UI-08 | Phase 7 | Complete |
| UI-09 | Phase 7 | Complete |
| UI-10 | Phase 7 | Complete |
| UI-11 | Phase 7 | Complete |
| DECK-01 | Phase 9 | Complete |
| DECK-02 | Phase 7 | Complete |
| DECK-03 | Phase 9 | Complete |
| AI-01 | Phase 8 | Complete |
| AI-02 | Phase 8 | Complete |
| AI-03 | Phase 8 | Complete |
| AI-04 | Phase 8 | Complete |
| AI-05 | Phase 8 | Complete |
| MP-01 | Phase 8 | Complete |
| MP-02 | Phase 8 | Complete |
| MP-03 | Phase 8 | Complete |
| MP-04 | Phase 8 | Complete |
| PLAT-01 | Phase 7 | Complete |
| PLAT-02 | Phase 7 | Complete |
| PLAT-03 | Phase 1 | Complete |
| PLAT-04 | Phase 7 | Complete |
| PLAT-05 | Phase 8 | Complete |
| QOL-01 | Phase 7 | Complete |
| QOL-02 | Phase 10 | Complete |
| QOL-03 | Phase 7 | Complete |

**Coverage:**
- v1 requirements: 57 total
- Mapped to phases: 57
- Unmapped: 0
- Pending (gap closure): 3 (DECK-01, DECK-03, QOL-02)

---
*Requirements defined: 2026-03-07*
*Last updated: 2026-03-07 after roadmap creation*
