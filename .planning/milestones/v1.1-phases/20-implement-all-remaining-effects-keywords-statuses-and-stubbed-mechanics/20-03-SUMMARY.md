---
phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics
plan: 03
subsystem: engine
tags: [scry, dig, surveil, waiting-for, card-choice, ai-evaluation, framer-motion]

# Dependency graph
requires:
  - phase: 18-select-candidates-to-support-and-implement-stubbed-mechanics
    provides: "Simplified scry/dig effect handlers with TODO markers"
provides:
  - "WaitingFor::ScryChoice, DigChoice, SurveilChoice variants"
  - "Surveil effect handler"
  - "CardChoiceModal frontend component (Scry/Dig/Surveil)"
  - "AI evaluation-based decisions for Scry/Dig/Surveil"
affects: [game-engine, frontend-modals, ai-opponent]

# Tech tracking
tech-stack:
  added: []
  patterns: [WaitingFor-driven card choice modals, per-card toggle UI pattern]

key-files:
  created:
    - crates/engine/src/game/effects/surveil.rs
    - client/src/components/modal/CardChoiceModal.tsx
  modified:
    - crates/engine/src/types/game_state.rs
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/effects/scry.rs
    - crates/engine/src/game/effects/dig.rs
    - crates/engine/src/game/effects/mod.rs
    - crates/forge-ai/src/search.rs
    - crates/forge-ai/src/legal_actions.rs
    - crates/server-core/src/session.rs
    - client/src/adapter/types.ts
    - client/src/pages/GamePage.tsx

key-decisions:
  - "Scry top_cards sent via SelectCards = cards to put on top; rest go to bottom"
  - "Dig SelectCards = cards to keep in hand; rest to graveyard (validated count)"
  - "Surveil SelectCards = cards to send to graveyard; rest stay on top of library"
  - "AI uses card-value heuristic (creature P/T, land value, mana cost proxy) for choices"
  - "ScryModal uses per-card Top/Bottom toggle buttons (MTGA-style)"
  - "SurveilModal uses per-card Keep/Graveyard toggle buttons"
  - "DigModal uses selection-count-limited card picking"

patterns-established:
  - "WaitingFor choice pattern: effect sets WaitingFor, engine.rs match arm processes SelectCards response"
  - "CardChoiceModal pattern: single component dispatches to sub-modals based on WaitingFor type"
  - "evaluate_card_value heuristic for AI card selection decisions"

requirements-completed: [ENG-05, ENG-06, ENG-07]

# Metrics
duration: 8min
completed: 2026-03-09
---

# Phase 20 Plan 03: Scry/Dig/Surveil Interactive Choices Summary

**Replaced auto-resolving Scry/Dig with proper WaitingFor interactive choices, added Surveil handler, created MTGA-style CardChoiceModal, and wired AI evaluation for all three choice types**

## Performance

- **Duration:** 8 min
- **Started:** 2026-03-09T23:54:34Z
- **Completed:** 2026-03-10T00:02:34Z
- **Tasks:** 2
- **Files modified:** 12

## Accomplishments
- Scry/Dig/Surveil now emit proper WaitingFor variants requiring player decisions instead of auto-resolving
- CardChoiceModal provides three specialized sub-modals with Framer Motion animations and card previews
- AI makes evaluation-based decisions using card-value heuristics (creature stats, land value, mana cost)
- Full exhaustive match coverage across engine, forge-ai, server-core, and engine-wasm

## Task Commits

Each task was committed atomically:

1. **Task 1: Add WaitingFor variants and update Scry/Dig/Surveil engine handlers** - `3cd6bf1` (feat)
2. **Task 2: WASM bridge, TS types, CardChoiceModal, and AI evaluation** - `0c4efec` (feat)

## Files Created/Modified
- `crates/engine/src/types/game_state.rs` - Added ScryChoice, DigChoice, SurveilChoice WaitingFor variants
- `crates/engine/src/game/effects/scry.rs` - Changed from auto-bottom to WaitingFor::ScryChoice
- `crates/engine/src/game/effects/dig.rs` - Changed from auto-take to WaitingFor::DigChoice
- `crates/engine/src/game/effects/surveil.rs` - New surveil handler with WaitingFor::SurveilChoice
- `crates/engine/src/game/effects/mod.rs` - Registered Surveil effect (25 entries)
- `crates/engine/src/game/engine.rs` - Three SelectCards match arms for choice resolution
- `crates/forge-ai/src/search.rs` - AI heuristic handlers + evaluate_card_value function
- `crates/forge-ai/src/legal_actions.rs` - Legal action generation for new WaitingFor variants
- `crates/server-core/src/session.rs` - Exhaustive match coverage for new variants
- `client/src/adapter/types.ts` - Added ScryChoice, DigChoice, SurveilChoice, EquipTarget to WaitingFor union
- `client/src/components/modal/CardChoiceModal.tsx` - MTGA-style card choice UI
- `client/src/pages/GamePage.tsx` - Wired CardChoiceModal into render tree

## Decisions Made
- Scry: SelectCards contains cards to keep on top; remainder goes to bottom of library
- Dig: SelectCards must contain exactly keep_count cards; validated server-side
- Surveil: SelectCards contains cards to send to graveyard; remainder stays on top
- AI evaluate_card_value uses creature P/T (1.5*P + T), land bonus (3.0), and mana cost proxy (0.5 per mana)
- ScryModal uses per-card Top/Bottom toggle buttons per MTGA design decision in CONTEXT.md
- CardChoiceModal component renders internally based on WaitingFor type, no separate rendering logic in GamePage

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Fixed exhaustive match in server-core and forge-ai**
- **Found during:** Task 1
- **Issue:** New WaitingFor variants caused non-exhaustive match errors in legal_actions.rs and session.rs
- **Fix:** Added match arms for EquipTarget, ScryChoice, DigChoice, SurveilChoice in both files
- **Files modified:** crates/forge-ai/src/legal_actions.rs, crates/server-core/src/session.rs
- **Verification:** cargo check --all passes
- **Committed in:** 3cd6bf1

**2. [Rule 1 - Bug] Fixed clippy manual_div_ceil warning**
- **Found during:** Task 1
- **Issue:** `(scored.len() + 1) / 2` triggered clippy manual_div_ceil lint
- **Fix:** Changed to `scored.len().div_ceil(2)`
- **Files modified:** crates/forge-ai/src/search.rs
- **Verification:** cargo clippy --all-targets -- -D warnings passes
- **Committed in:** 3cd6bf1

---

**Total deviations:** 2 auto-fixed (1 blocking, 1 bug)
**Impact on plan:** Both auto-fixes necessary for compilation and lint compliance. No scope creep.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Scry/Dig/Surveil now fully interactive with proper WaitingFor flow
- Frontend CardChoiceModal ready for visual verification when game triggers these effects
- AI properly handles all three choice types with evaluation-based decisions

---
*Phase: 20-implement-all-remaining-effects-keywords-statuses-and-stubbed-mechanics*
*Completed: 2026-03-09*
