---
gsd_state_version: 1.0
milestone: v1.1
milestone_name: Arena UI
status: executing
stopped_at: Completed 19-07-PLAN.md
last_updated: "2026-03-09T23:02:38.904Z"
last_activity: 2026-03-09 — Completed 19-02 Battlefield Rendering
progress:
  total_phases: 8
  completed_phases: 6
  total_plans: 33
  completed_plans: 29
  percent: 25
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-03-08)

**Core value:** A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent -- with all cards behaving correctly according to MTG comprehensive rules.
**Current focus:** Phase 19 - Recreate MTGA UI

## Current Position

Phase: 19 of 20 (Recreate MTGA UI)
Plan: 2 of 8 in current phase
Status: In Progress
Last activity: 2026-03-09 — Completed 19-02 Battlefield Rendering

Progress: [███-------] 25%

## Performance Metrics

**Velocity:**
- Total plans completed: 6 (v1.1)
- Average duration: 3min
- Total execution time: 14min

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 13-foundation-board-layout | 4/5 | 12min | 3min |
| Phase 13 P01 | 5min | 2 tasks | 11 files |
| Phase 13 P03 | 2min | 2 tasks | 5 files |
| Phase 13 P04 | 3min | 2 tasks | 6 files |
| Phase 13 P05 | 49min | 3 tasks | 9 files |
| 14-animation-pipeline | 4/4 | 10min | 2.5min |
| Phase 14 P01 | 2min | 2 tasks | 7 files |
| Phase 14 P02 | 3min | 2 tasks | 5 files |
| Phase 14 P03 | 3min | 2 tasks | 8 files |
| Phase 14 P04 | 2min | 2 tasks | 5 files |
| 15-game-loop-controllers | 2/3 | 4min | 2min |
| Phase 15 P01 | 2min | 2 tasks | 7 files |
| Phase 15 P02 | 2min | 2 tasks | 3 files |
| Phase 15 P03 | 5min | 2 tasks | 4 files |
| Phase 18 P01 | 5min | 1 tasks | 4 files |
| Phase 18 P02 | 8min | 2 tasks | 7 files |
| Phase 18 P03 | 7min | 2 tasks | 4 files |
| Phase 18 P04 | 4min | 2 tasks | 6 files |
| Phase 18 P05 | 5min | 2 tasks | 7 files |
| Phase 17 P01 | 3min | 2 tasks | 10 files |
| Phase 17 P02 | 2min | 1 tasks | 3 files |
| Phase 17 P03 | 2min | 2 tasks | 2 files |
| Phase 17 P04 | 2min | 2 tasks | 3 files |
| Phase 17 P05 | 2min | 2 tasks | 3 files |
| Phase 16 P01 | 4min | 2 tasks | 5 files |
| Phase 16 P03 | 2min | 2 tasks | 2 files |
| Phase 16 P02 | 2min | 2 tasks | 3 files |
| Phase 19 P01 | 2min | 2 tasks | 7 files |
| Phase 19 P02 | 2min | 2 tasks | 4 files |
| Phase 19 P07 | 2min | 2 tasks | 5 files |
| Phase 19 P08 | 2min | 2 tasks | 4 files |

## Accumulated Context

### Decisions

Full decision log in PROJECT.md Key Decisions table.

- Port Alchemy UI as Arena-style frontend (pending validation)
- Preserve EngineAdapter abstraction during UI port (pending validation)
- [13-02] Use vw units for card sizing to scale with viewport width across breakpoints
- [13-02] Cap eventHistory at 1000 entries to prevent unbounded memory growth
- [Phase 13-01]: View model functions are pure mappers from GameObject to flat props, no store coupling
- [Phase 13-01]: Permanent grouping requires same name + same tapped state + no attachments + no counters
- [Phase 13-03]: P/T box replaces damage overlay for creatures; non-creatures keep damage overlay
- [Phase 13-03]: Counter badges at top-right to avoid P/T box overlap at bottom-right
- [Phase 13-03]: Attachment tuck uses 15px offset per attachment with marginTop reservation
- [Phase 13-04]: All hand cards highlighted as playable when player has priority (engine legal action filtering deferred)
- [Phase 13-04]: HUD layout toggle between inline and floating via preferencesStore hudLayout
- [Phase 13-05]: GameLogPanel reads eventHistory (cumulative) for full game log
- [Phase 13-05]: WUBRG background gradients use subtle opacity to avoid overwhelming battlefield
- [Phase 13-05]: Module-level empty array constants for Zustand selectors prevent re-render loops
- [Phase 14-01]: Non-visual events defined as set of 12 event types skipped by normalizer
- [Phase 14-01]: Groupable events (DamageDealt, CreatureDestroyed, PermanentSacrificed) merge consecutive same-type into one step
- [Phase 14-01]: Merge types (ZoneChanged, LifeChanged) attach to preceding step
- [Phase 14-02]: captureSnapshot returns local Map, not Zustand state, to avoid re-renders
- [Phase 14-02]: Dispatch mutex uses useRef to prevent re-render cascades
- [Phase 14-02]: currentSnapshot exported as module-level variable for AnimationOverlay
- [Phase 14-03]: VFX quality reads via getState() (non-reactive) in ParticleCanvas for performance
- [Phase 14-03]: ScreenShake is a plain function, not a React component — applies CSS transform via rAF
- [Phase 14-03]: ParticleCanvas halves count internally for reduced quality, centralizing logic
- [Phase 14-04]: Death clones use card name text overlay, not full card images
- [Phase 14-04]: Screen shake only at full VFX quality to avoid motion sickness
- [Phase 14-04]: getObjectPosition checks snapshot first, then live registry
- [Phase 15-01]: Module-level boolean mutex replaces useRef for dispatch pipeline
- [Phase 15-01]: AI controller reads gameStore directly instead of injected callbacks
- [Phase 15-01]: Auto-pass conservative heuristic: stops when player has mana + instants/flash/abilities
- [Phase 15-02]: GameProvider accepts mode/difficulty as props, does not own game initialization
- [Phase 15-02]: Auto-pass uses setTimeout with 200ms beat, re-triggered by store subscription
- [Phase 15-03]: PassButton uses MTGA terminology: Done (empty stack) / Resolve (stack has items)
- [Phase 15-03]: Turn indicator badge with cyan/red color coding for your/opponent turn
- [Phase 18]: test_helpers module is always public (not cfg(test)-gated) for integration test and coverage access
- [Phase 18]: derive_colors_from_mana_cost made pub(crate) in deck_loading for reuse in test_helpers
- [Phase 18]: Scry simplified: all scryed cards go to bottom (TODO: WaitingFor::ScryChoice)
- [Phase 18]: Shared matches_filter() in effects/mod.rs handles Forge Valid patterns (type.controller)
- [Phase 18-03]: CantBeBlocked checked via static_definitions, matching Forge card data format
- [Phase 18-03]: Protection only handles Color variant; CardType/Quality deferred
- [Phase 18-03]: Ward cost enforcement deferred to mana payment UI
- [Phase 18-03]: Prowess uses synthetic trigger injection for keyword-based triggers
- [Phase 18-04]: Dig simplified: first ChangeNum cards to hand (TODO: WaitingFor::DigChoice)
- [Phase 18-04]: Wither/Infect counters applied directly without replacement effects
- [Phase 18-04]: Infect damage to players skips LifeChanged event (no life changes)
- [Phase 18-05]: has_unimplemented_mechanics checks all 4 registry types (keywords, effects, triggers, statics)
- [Phase 18-05]: Derived display fields use skip_deserializing + WASM-side computation pattern
- [Phase 17-01]: usePhaseInfo groups MTG 12 phases into 5 display keys: draw, main1, combat, main2, end
- [Phase 17-01]: LifeTotal accepts size prop for context-aware rendering (lg in HUD, default elsewhere)
- [Phase 17-01]: OpponentHand upgraded from static divs to motion.div with AnimatePresence for fan animation
- [Phase 17]: StackDisplay moved from inline center-divider to fixed right-column overlay for Arena-style presentation
- [Phase 17-03]: ActionButton uses dispatchAction directly for Resolve All async loop (outside React lifecycle)
- [Phase 17-03]: Skip-confirm guard pattern with 1200ms armed timer for No Attacks / No Blocks
- [Phase 17-03]: Old PassButton/CombatOverlay files kept to avoid breaking test imports
- [Phase 17-04]: Auto-pay via PassPriority useEffect for non-ambiguous costs
- [Phase 17-04]: Cost inference from top stack entry source_id mana_cost
- [Phase 17-04]: Phyrexian toggle between mana icon and heart with 2-life label
- [Phase 17-05]: RAF polling stabilizes after 10 identical frames to avoid infinite animation loops
- [Phase 17-05]: DamageAssignmentModal is read-only review with user-triggered open (not auto-shown)
- [Phase 17-05]: BlockAssignmentLines merges UI blockerAssignments with engine blocker_to_attacker for both phases
- [Phase 16-01]: AudioManager is a plain TypeScript singleton, not a React component -- matches dispatch.ts pattern
- [Phase 16-01]: Module-level usePreferencesStore.subscribe() wires real-time volume updates automatically
- [Phase 16-01]: dispose() fully resets AudioManager state for clean test isolation without vi.resetModules()
- [Phase 16-03]: Speaker icon placed left of settings gear for quick access without opening modal
- [Phase 16-03]: Red icon color (text-red-400) for muted state provides clear visual feedback
- [Phase 16-03]: Slider opacity dims when individually muted but remains interactive
- [Phase 16-02]: SFX scheduling uses setTimeout with cumulative offsets to sync with visual animation step timing
- [Phase 16-02]: GameOver triggers music fade-out via audioManager.stopMusic(2.0) after state update
- [Phase 16-02]: Music starts after onReady callback for online games to avoid playing during opponent wait
- [Phase 19-01]: Tokens detected via card_id === 0 (no is_token field on GameObject)
- [Phase 19-01]: Art crop aspect ratio 0.75 (width:height) matches Scryfall art_crop format
- [Phase 19-01]: ImageSize type exported from scryfall.ts for reuse
- [Phase 19]: Splash progress is cosmetic (1.5s rAF timer) since WASM loads on game start, not app start
- [Phase 19]: DeckGallery uses first non-basic-land card name from deck for representative art tile
- [Phase 19]: Difficulty selector is inline segmented control in DeckGallery, not a separate screen

### Roadmap Evolution

- Phase 18 added: Select candidates to support and implement stubbed mechanics
- Phase 19 added: Recreate the MTGA UI as faithfully as possible
- Phase 20 added: Implement all remaining effects, keywords, statuses, and stubbed mechanics

### Blockers/Concerns

None.

### Quick Tasks Completed

| # | Description | Date | Commit | Directory |
|---|-------------|------|--------|-----------|
| 1 | Add card-data generation, cargo script aliases, and README | 2026-03-08 | 1837030 | [1-add-card-data-generation-cargo-script-al](./quick/1-add-card-data-generation-cargo-script-al/) |

## Session Continuity

Last activity: 2026-03-09 - Completed 19-01 Art-Crop Card & Image Infrastructure
Stopped at: Completed 19-07-PLAN.md
Resume file: None
