---
phase: 29-support-n-players
plan: 07
subsystem: ui
tags: [react, typescript, board-layout, n-player, commander]

requires:
  - phase: 29-01
    provides: N-player engine types (seat_order, eliminated_players, FormatConfig, CommanderDamageEntry)
  - phase: 29-06
    provides: AI N-player seat awareness and combat AI with attack targets
provides:
  - PlayerArea component with full/focused/compact modes
  - CompactStrip for condensed opponent view
  - CommanderDisplay and CommanderDamage components
  - N-player GameBoard layout (dynamic opponent rendering)
  - Frontend types matching engine N-player state
  - DeclareAttackers action using AttackTarget tuples
affects: [29-08-attack-ui, 29-10-lobby, 29-12-deckbuilder]

tech-stack:
  added: []
  patterns: [PlayerArea mode-based rendering, buildAttacks utility for attack target construction]

key-files:
  created:
    - client/src/components/board/PlayerArea.tsx
    - client/src/components/board/CompactStrip.tsx
    - client/src/components/board/CommanderDisplay.tsx
    - client/src/components/board/CommanderDamage.tsx
    - client/src/utils/combat.ts
  modified:
    - client/src/adapter/types.ts
    - client/src/components/board/GameBoard.tsx
    - client/src/components/board/ActionButton.tsx
    - client/src/components/combat/CombatOverlay.tsx

key-decisions:
  - "PlayerArea accepts creatureOverride prop for blocker sorting (keeps sorting logic in GameBoard)"
  - "CompactStrip shows permanent counts grouped by type (creatures/lands/other) instead of individual thumbnails"
  - "buildAttacks utility defaults all attackers to first non-eliminated opponent (N-player target selection in Plan 08)"
  - "AttackTarget uses discriminated union format { type: 'Player', data: id } matching Rust serde output"

patterns-established:
  - "PlayerArea mode prop: full (current player), focused (opponent detail), compact (condensed strip)"
  - "N-player opponent computation from seat_order filtering eliminated_players"

requirements-completed: [NP-BOARD-UI, NP-PLAYERAREA, NP-COMPACT, NP-1V1-PARITY, NP-COMMANDER-UI]

duration: 9min
completed: 2026-03-11
---

# Phase 29 Plan 07: N-Player Board UI Summary

**Unified PlayerArea component with full/focused/compact modes, CompactStrip for multiplayer opponents, Commander display components, and DeclareAttackers updated to use AttackTarget tuples**

## Performance

- **Duration:** 9 min
- **Started:** 2026-03-11T18:40:50Z
- **Completed:** 2026-03-11T19:08:00Z
- **Tasks:** 3 (2 auto + 1 human-verify)
- **Files modified:** 10

## Accomplishments
- Refactored board from hardcoded 2-player layout to N-player dynamic rendering
- 1v1 visual parity maintained (identical three-column layout with lands/creatures/other)
- 3+ player games show compact opponent strips with click-to-focus expansion
- Commander damage tracking and display components ready for Commander format
- Frontend types fully aligned with engine N-player state (seat_order, eliminated_players, format_config, commander_damage)

## Task Commits

Each task was committed atomically:

1. **Task 1: Update frontend types and create PlayerArea/CompactStrip components** - `8d1c82598` (feat)
2. **Task 2: Refactor GameBoard for N-player layout** - `90c89c537` (feat)
3. **Task 3: Visual verification of board layout** - Human-verified approved

## Files Created/Modified
- `client/src/adapter/types.ts` - Added GameFormat, FormatConfig, AttackTarget, CommanderDamageEntry; updated GameState and Player
- `client/src/components/board/PlayerArea.tsx` - Unified player area with full/focused/compact modes
- `client/src/components/board/CompactStrip.tsx` - Condensed opponent view with grouped permanent counts
- `client/src/components/board/CommanderDisplay.tsx` - Commander card display with amber glow highlight
- `client/src/components/board/CommanderDamage.tsx` - Commander damage tracking with lethal/warning colors
- `client/src/components/board/GameBoard.tsx` - Refactored to render N players via PlayerArea components
- `client/src/components/board/ActionButton.tsx` - Updated DeclareAttackers to use attacks format
- `client/src/components/combat/CombatOverlay.tsx` - Updated DeclareAttackers to use attacks format
- `client/src/utils/combat.ts` - buildAttacks utility for constructing attack target tuples

## Decisions Made
- PlayerArea accepts `creatureOverride` prop so GameBoard retains blocker sorting responsibility
- CompactStrip shows permanent counts by type rather than individual card thumbnails (simpler, more scalable)
- buildAttacks utility defaults all selected attackers to the first non-eliminated opponent; explicit per-creature target selection deferred to Plan 29-08
- AttackTarget uses `{ type: "Player", data: id }` discriminated union format matching Rust serde `#[serde(tag = "type", content = "data")]`

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed AttackTarget type format**
- **Found during:** Task 3 (visual verification, external fix)
- **Issue:** AttackTarget initially defined as `{ Player: id }` but engine serializes as `{ type: "Player", data: id }`
- **Fix:** Updated to discriminated union format in types.ts and combat.ts
- **Files modified:** client/src/adapter/types.ts, client/src/utils/combat.ts, client/src/components/combat/__tests__/CombatOverlay.test.tsx
- **Verification:** Type check passes, game runs correctly

**2. [Rule 1 - Bug] Fixed getAiAction missing player_id parameter**
- **Found during:** Task 3 (visual verification, external fix)
- **Issue:** getAiAction needed player_id parameter for N-player AI
- **Fix:** Added player_id to wasm-adapter.ts, types.ts, aiController.ts
- **Files modified:** client/src/adapter/wasm-adapter.ts, client/src/adapter/types.ts, client/src/game/controllers/aiController.ts

---

**Total deviations:** 2 auto-fixed (2 bugs)
**Impact on plan:** Both fixes necessary for correct N-player gameplay. No scope creep.

## Issues Encountered
None beyond the auto-fixed deviations.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Board UI ready for N-player games
- Attack target selection UI needed (Plan 29-08) for choosing which opponent to attack in multiplayer
- Lobby UI (Plan 29-10) can reference PlayerArea/CompactStrip patterns

---
*Phase: 29-support-n-players*
*Completed: 2026-03-11*
