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

- [ ] **Phase 13: Foundation & Board Layout** - Responsive board, hand, HUD, zones, game log, view model, and preferences infrastructure
- [ ] **Phase 14: Animation Pipeline** - Event normalizer, step-based animation queue, VFX, and visual feedback systems
- [ ] **Phase 15: Game Loop & Controllers** - Opponent controller abstraction, auto-advance game loop, and dispatch context
- [ ] **Phase 16: Audio System** - SFX, background music, volume controls via Web Audio API
- [ ] **Phase 17: MTG-Specific UI** - Stack visualization, mana payment, combat UI, and priority controls

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
**Plans**: TBD

Plans:
- [ ] 14-01: TBD
- [ ] 14-02: TBD
- [ ] 14-03: TBD

### Phase 15: Game Loop & Controllers
**Goal**: The game plays smoothly end-to-end -- AI opponent acts automatically, trivial priority windows auto-pass, and dispatch flows through a React context without prop drilling
**Depends on**: Phase 14
**Requirements**: LOOP-01, LOOP-02, LOOP-03, LOOP-04
**Success Criteria** (what must be TRUE):
  1. AI opponent makes decisions automatically via WASM, and the same controller abstraction supports network opponents via WebSocket
  2. Game auto-advances through phases, waits for animations to complete before proceeding, and skips trivial priority windows (e.g. upkeep with no triggers)
  3. All components access dispatch and controller through React context (GameDispatchProvider) without prop drilling
**Plans**: TBD

Plans:
- [ ] 15-01: TBD

### Phase 16: Audio System
**Goal**: Game events produce sound effects and background music plays during matches, all configurable by the player
**Depends on**: Phase 13 (preferences store only -- parallelizable with Phase 15)
**Requirements**: AUDIO-01, AUDIO-02, AUDIO-03, AUDIO-04, AUDIO-05
**Success Criteria** (what must be TRUE):
  1. Sound effects play on game events (damage, spells, combat, life changes) using Forge's audio assets via Web Audio API
  2. Background music plays during matches with WUBRG-themed track selection based on player's deck colors
  3. Player can independently control SFX and music volume with mute toggles, and audio initializes correctly on iOS/iPadOS after first user interaction
**Plans**: TBD

Plans:
- [ ] 16-01: TBD

### Phase 17: MTG-Specific UI
**Goal**: Players interact with MTG-specific mechanics through polished UI -- stack visualization, mana payment, combat assignment, and priority controls
**Depends on**: Phase 14, Phase 15
**Requirements**: STACK-01, STACK-02, STACK-03, STACK-04, MANA-01, MANA-02, MANA-03, COMBAT-01, COMBAT-02, COMBAT-03, COMBAT-04
**Success Criteria** (what must be TRUE):
  1. Spells and abilities on the stack display with card art and description in an Arena-style visualization, resolving visually in LIFO order
  2. Player sees priority pass/respond buttons when they have priority, can toggle auto-pass when no instant-speed actions are available, and can enable full-control mode for manual priority at every window
  3. Mana payment UI shows required cost with WUBRG symbols, handles hybrid/phyrexian/X costs with appropriate affordances, and the mana pool display updates in real-time
  4. Player can declare attackers and blockers by clicking highlighted legal options, sees combat math bubbles previewing P/T trade outcomes, and can distribute damage across multiple blockers via a modal
**Plans**: TBD

Plans:
- [ ] 17-01: TBD
- [ ] 17-02: TBD

## Progress

**Execution Order:**
Phases 15 and 16 are parallelizable (independent subsystems). All others are sequential: 13 -> 14 -> (15 || 16) -> 17.

| Phase | Milestone | Plans Complete | Status | Completed |
|-------|-----------|----------------|--------|-----------|
| 1-12 | v1.0 | 40/40 | Complete | 2026-03-08 |
| 13. Foundation & Board Layout | 1/5 | In Progress|  | - |
| 14. Animation Pipeline | v1.1 | 0/3 | Not started | - |
| 15. Game Loop & Controllers | v1.1 | 0/1 | Not started | - |
| 16. Audio System | v1.1 | 0/1 | Not started | - |
| 17. MTG-Specific UI | v1.1 | 0/2 | Not started | - |
