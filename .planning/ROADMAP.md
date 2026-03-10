# Roadmap: Forge.rs

## Milestones

- ✅ **v1.0 MVP** — Phases 1-12 (shipped 2026-03-08) — [archive](milestones/v1.0-ROADMAP.md)
- 🚧 **v1.1 Arena UI** — Phases 13-17 (in progress)

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

### v1.1 Arena UI

**Milestone Goal:** Replace the current frontend with a polished Arena-style UI ported from the Alchemy project, wired to the Rust/WASM engine via the existing EngineAdapter interface.

- [x] **Phase 13: Foundation & Board Layout** - Responsive board, hand, HUD, zones, game log, view model, and preferences infrastructure (completed 2026-03-09)
- [x] **Phase 14: Animation Pipeline** - Event normalizer, step-based animation queue, VFX, and visual feedback systems (completed 2026-03-09)
- [x] **Phase 15: Game Loop & Controllers** - Opponent controller abstraction, auto-advance game loop, and dispatch context (completed 2026-03-09)
- [x] **Phase 16: Audio System** - SFX, background music, volume controls via Web Audio API (completed 2026-03-09)
- [x] **Phase 17: MTG-Specific UI** - Stack visualization, mana payment, combat UI, priority controls, and MTGA visual polish (completed 2026-03-09)

## Phase Details

### Phase 13: Foundation & Board Layout
**Goal**: Players see a responsive, Arena-style game board with hand interaction, player HUD, zone viewers, game log, and preferences -- all wired to the Rust/WASM engine
**Depends on**: v1.0 (Phase 12)
**Requirements**: BOARD-01, BOARD-02, BOARD-03, BOARD-04, BOARD-05, BOARD-06, BOARD-07, BOARD-08, BOARD-09, HAND-01, HAND-02, HAND-03, HAND-04, HUD-01, HUD-02, HUD-03, ZONE-01, ZONE-02, ZONE-03, LOG-01, LOG-02, LOG-03, INTEG-01, INTEG-02, INTEG-03
**Success Criteria** (what must be TRUE):
  1. Game board renders permanents in multi-row layout grouped by type (creatures, non-creatures, lands), with responsive card sizing that adapts from mobile to desktop via CSS custom properties
  2. Player can fan out their hand from the bottom edge, see playable cards highlighted by legal actions from the engine, and drag cards onto the battlefield to play them
  3. Both player and opponent HUDs display life totals and mana pool summaries, with life total changes flashing red (damage) or green (gain)
  4. Player can open graveyard and exile zone viewer modals, zone indicators show card counts, and a scrollable game log displays color-coded events with verbosity filtering
  5. All UI components communicate through the EngineAdapter interface, with a GameObject view model mapping deep engine objects to flat component props, and preferences persisting to localStorage
**Plans**: 5 plans

Plans:
- [ ] 13-01-PLAN.md — View model layer and preferences store
- [ ] 13-02-PLAN.md — CSS responsive card sizing, tap rotation fix, event history
- [ ] 13-03-PLAN.md — Battlefield grouping, P/T display, attachment rendering
- [ ] 13-04-PLAN.md — Hand fan layout with drag-to-play, HUD components
- [ ] 13-05-PLAN.md — Full-screen layout, game log panel, zone viewers, WUBRG backgrounds

### Phase 14: Animation Pipeline
**Goal**: Game state changes produce fluid visual feedback -- particles, floating numbers, screen shake, card reveals, targeting arcs, and death animations -- driven by an event-normalized animation queue
**Depends on**: Phase 13
**Requirements**: ANIM-01, ANIM-02, ANIM-03, ANIM-04, ANIM-05, ANIM-06, VFX-01, VFX-02, VFX-03, VFX-04, VFX-05, VFX-06, VFX-07, VFX-08
**Success Criteria** (what must be TRUE):
  1. Engine events translate through the event normalizer into animation steps that play sequentially with configurable speed, and dying creatures remain visible during their death animation before removal
  2. Canvas particle effects fire on game events (damage, spells, combat) with WUBRG color mapping, and floating damage/heal numbers animate on affected permanents and players
  3. Screen shakes on combat damage, card reveal bursts play on creature/spell entry, damage vignette flashes on player damage, and turn/phase banners animate on transitions
  4. SVG block assignment lines connect attacker/blocker pairs during combat, and targeting arcs connect spells to their targets during resolution
  5. Player can configure VFX quality level (full/reduced/minimal) and animation speed in preferences
**Plans**: 4 plans

Plans:
- [ ] 14-01-PLAN.md — Animation types, WUBRG colors, event normalizer, preferences extensions
- [ ] 14-02-PLAN.md — Step-based animation store refactor, snapshot-before-dispatch wrapper
- [ ] 14-03-PLAN.md — VFX components: particles, screen shake, vignette, turn banner, card reveal
- [ ] 14-04-PLAN.md — AnimationOverlay wiring, combat VFX, preferences controls

### Phase 15: Game Loop & Controllers
**Goal**: The game plays smoothly end-to-end -- AI opponent acts automatically, trivial priority windows auto-pass, and dispatch flows through a React context without prop drilling
**Depends on**: Phase 14
**Requirements**: LOOP-01, LOOP-02, LOOP-03, LOOP-04
**Success Criteria** (what must be TRUE):
  1. AI opponent makes decisions automatically via WASM, and the same controller abstraction supports network opponents via WebSocket
  2. Game auto-advances through phases, waits for animations to complete before proceeding, and skips trivial priority windows (e.g. upkeep with no triggers)
  3. All components access dispatch and controller through React context (GameDispatchProvider) without prop drilling
**Plans**: 3 plans

Plans:
- [ ] 15-01-PLAN.md — Standalone dispatch, OpponentController types, auto-pass heuristics, phase stop preferences
- [ ] 15-02-PLAN.md — Game loop controller factory, GameProvider context, useGameDispatch simplification
- [ ] 15-03-PLAN.md — PhaseStopBar UI, GamePage simplification with GameProvider integration

### Phase 16: Audio System
**Goal**: Game events produce sound effects and background music plays during matches, all configurable by the player
**Depends on**: Phase 13 (preferences store only -- parallelizable with Phase 15)
**Requirements**: AUDIO-01, AUDIO-02, AUDIO-03, AUDIO-04, AUDIO-05
**Success Criteria** (what must be TRUE):
  1. Sound effects play on game events (damage, spells, combat, life changes) using Forge's audio assets via Web Audio API
  2. Background music plays during matches with track rotation through available tracks
  3. Player can independently control SFX and music volume with mute toggles, and audio initializes correctly on iOS/iPadOS after first user interaction
**Plans**: 3 plans

Plans:
- [ ] 16-01-PLAN.md — AudioManager singleton, SFX map, preferences store audio extension
- [ ] 16-02-PLAN.md — Dispatch pipeline SFX integration, music auto-start, AudioContext warm-up
- [ ] 16-03-PLAN.md — HUD master mute toggle, PreferencesModal audio controls

### Phase 17: MTG-Specific UI
**Goal**: Players interact with MTG-specific mechanics through polished UI -- stack visualization, mana payment, combat assignment, priority controls, and MTGA-quality visual polish (board sizing, hand fan, HUD)
**Depends on**: Phase 14, Phase 15
**Requirements**: STACK-01, STACK-02, STACK-03, STACK-04, MANA-01, MANA-02, MANA-03, COMBAT-01, COMBAT-02, COMBAT-03, COMBAT-04
**Success Criteria** (what must be TRUE):
  1. Spells and abilities on the stack display with card art and description in an Arena-style visualization, resolving visually in LIFO order
  2. Player sees priority pass/respond buttons when they have priority, can toggle auto-pass when no instant-speed actions are available, and can enable full-control mode for manual priority at every window
  3. Mana payment UI shows required cost with WUBRG symbols, handles hybrid/phyrexian/X costs with appropriate affordances, and the mana pool display updates in real-time
  4. Player can declare attackers and blockers by clicking highlighted legal options, and can review damage distribution across multiple blockers via a modal
  5. Hand fan uses dramatic rotation with perspective, board has zone visual hierarchy, HUD displays are prominent and Arena-quality
**Plans**: 5 plans

Plans:
- [ ] 17-01-PLAN.md — Foundation utilities (buttonStyles, usePhaseInfo, boardSizing) and visual polish (hand fan, board, HUD)
- [ ] 17-02-PLAN.md — Stack visualization upgrade (full card images, staggered pile, container-aware sizing)
- [ ] 17-03-PLAN.md — ActionButton unified orchestrator (combat controls, priority, resolve all)
- [ ] 17-04-PLAN.md — Mana payment upgrade (smart auto-pay, hybrid/phyrexian/X cost UI, ManaSymbol)
- [ ] 17-05-PLAN.md — BlockAssignmentLines (animated SVG), DamageAssignmentModal, GamePage wiring

## Progress

**Execution Order:**
Phases 15 and 16 are parallelizable (independent subsystems). All others are sequential: 13 -> 14 -> (15 || 16) -> 17.

| Phase | Milestone | Plans Complete | Status | Completed |
|-------|-----------|----------------|--------|-----------|
| 1-12 | v1.0 | 40/40 | Complete | 2026-03-08 |
| 13. Foundation & Board Layout | 5/5 | Complete    | 2026-03-09 | - |
| 14. Animation Pipeline | 4/4 | Complete    | 2026-03-09 | - |
| 15. Game Loop & Controllers | 3/3 | Complete    | 2026-03-09 | - |
| 16. Audio System | 3/3 | Complete    | 2026-03-09 | - |
| 17. MTG-Specific UI | 5/5 | Complete   | 2026-03-09 | - |

### Phase 18: Select candidates to support and implement stubbed mechanics

**Goal:** Implement stubbed game mechanics in tiered batches -- combat keywords, effect handlers, static abilities, and damage mechanics -- with test infrastructure, coverage reporting, and UI warnings for remaining gaps
**Requirements**: MECH-01, MECH-02, MECH-03, MECH-04, MECH-05, MECH-06, MECH-07, MECH-08, MECH-09, MECH-10
**Depends on:** Phase 15
**Plans:** 3/3 plans complete

Plans:
- [ ] 18-01-PLAN.md — Test helper infrastructure and combat evasion keywords (Fear, Intimidate, Skulk, Horsemanship)
- [ ] 18-02-PLAN.md — Core effect handlers (Mill, Scry, PumpAll, DamageAll, DestroyAll, ChangeZoneAll)
- [ ] 18-03-PLAN.md — Static abilities (Ward, Protection, CantBeBlocked) and Prowess trigger
- [ ] 18-04-PLAN.md — Dig, GainControl effects and Wither/Infect damage with poison counters
- [ ] 18-05-PLAN.md — Mechanic coverage report and UI warning badge

### Phase 19: Recreate the MTGA UI as faithfully as possible

**Goal:** Close the visual and interaction gap between the current Arena-inspired UI and the actual MTGA experience -- art-crop card presentation on battlefield, MTGA-faithful board layout with centered avatars and flanking phase indicators, cinematic animations (turn banner, death shatter, cast arcs), mode-first menu with deck gallery, and deck builder visual polish
**Requirements**: ARENA-01, ARENA-02, ARENA-03, ARENA-04, ARENA-05, ARENA-06, ARENA-07, ARENA-08, ARENA-09, ARENA-10, ARENA-11, ARENA-12
**Depends on:** Phase 17
**Success Criteria** (what must be TRUE):
  1. Battlefield permanents display Scryfall art_crop images with WUBRG color-coded borders, card name labels, P/T overlays for creatures, and loyalty shields for planeswalkers
  2. Board layout matches MTGA: creatures near center, lands far, centered player/opponent avatars, phase indicators flanking player avatar, no visible borders or bars
  3. Tapped permanents rotate 15-20 degrees (not 90) with slight opacity dim
  4. Golden curved targeting arcs connect spells to targets, orange glow on selected attackers/blockers, attackers slide forward when declared
  5. Cinematic layered turn banner (amber/slate), canvas death shatter fragments, card flight arcs for cast/resolve
  6. Full-screen MTGA-style mulligan with large card images, dramatic VICTORY/DEFEAT game over screens
  7. Mode-first menu flow with splash screen, deck gallery with art tiles, animated particle background
  8. Deck builder displays art-crop grid with instant card preview and visual color/type filtering
**Plans:** 8/8 plans complete

Plans:
- [ ] 19-01-PLAN.md — Image infrastructure (art_crop type extension, CSS vars) and ArtCropCard component
- [ ] 19-02-PLAN.md — Wire ArtCropCard into battlefield, 17deg tap rotation, instant CardPreview
- [ ] 19-03-PLAN.md — MTGA board layout (zone reordering, centered avatars, split phase indicators)
- [ ] 19-04-PLAN.md — Golden targeting arcs, orange combat glows, attacker slide-forward
- [ ] 19-05-PLAN.md — Cinematic TurnBanner, DeathShatter, CastArcAnimation, AnimationOverlay wiring
- [ ] 19-06-PLAN.md — Full-screen mulligan and dramatic game over screens
- [ ] 19-07-PLAN.md — Splash screen, mode-first MenuPage, DeckGallery, menu particles
- [ ] 19-08-PLAN.md — Deck builder art-crop grid, instant preview, color/type filtering

### Phase 20: Implement all remaining effects, keywords, statuses, and stubbed mechanics

**Goal:** Complete the engine's mechanic coverage to 100% of Standard-legal cards -- implement mana abilities (Rule 605), equipment/aura attachment, interactive WaitingFor choices (Scry/Dig/Surveil), planeswalker loyalty, transform/DFCs, day/night, morph/manifest, and batch-promote all remaining static/trigger/replacement stubs, with CI-gated coverage validation
**Requirements**: ENG-01, ENG-02, ENG-03, ENG-04, ENG-05, ENG-06, ENG-07, ENG-08, ENG-09, ENG-10, ENG-11, ENG-12, ENG-13, ENG-14, ENG-15, ENG-16, ENG-17, ENG-18, ENG-19
**Depends on:** Phase 19
**Success Criteria** (what must be TRUE):
  1. Mana abilities resolve instantly without the stack, activatable during mana payment
  2. Equipment and auras attach properly with SBA cleanup on host death
  3. Scry/Dig/Surveil emit interactive WaitingFor choices with MTGA-style UI and AI evaluation
  4. Planeswalker loyalty abilities activate with counter cost, once-per-turn, and 0-loyalty SBA
  5. DFCs transform between faces with characteristic swapping and zone-change reset
  6. Day/night tracks globally with Daybound/Nightbound auto-transformation
  7. Morph/manifest create face-down 2/2 creatures that can be turned face up
  8. Coverage report confirms 100% Standard-legal card support with CI gate preventing regressions
**Plans:** 5/10 plans executed

Plans:
- [ ] 20-01-PLAN.md — Mana abilities (Rule 605 instant resolution, ManaPayment activation)
- [ ] 20-02-PLAN.md — Equipment/Aura attachment (equip action, SBA, WaitingFor::EquipTarget)
- [ ] 20-03-PLAN.md — WaitingFor interactive choices (ScryChoice, DigChoice, SurveilChoice, CardChoiceModal, AI)
- [ ] 20-04-PLAN.md — Planeswalker loyalty (activation, once-per-turn, 0-loyalty SBA, damage redirect)
- [ ] 20-05-PLAN.md — Transform/DFC (face switching, zone reset, hover-to-peek UI)
- [ ] 20-06-PLAN.md — Static ability + trigger matcher batch promotion (Indestructible, CantBeCountered, FlashBack, AttackerBlocked, etc.)
- [ ] 20-07-PLAN.md — Effect handlers (Fight, Bounce, Explore, Proliferate, CopySpell) + replacement promotions
- [ ] 20-08-PLAN.md — Day/Night global state with Daybound/Nightbound transformation
- [ ] 20-09-PLAN.md — Morph/Manifest/Disguise face-down mechanics
- [ ] 20-10-PLAN.md — Standard card data curation + coverage CI gate
