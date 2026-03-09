---
phase: 17-mtg-specific-ui
plan: 04
subsystem: ui
tags: [react, mana, wubrg, framer-motion, tailwind]

requires:
  - phase: 17-01
    provides: buttonStyles utility (gameButtonClass)
provides:
  - ManaSymbol component for WUBRG shard rendering
  - Smart auto-pay ManaPaymentUI for simple costs
  - Interactive UI for hybrid, phyrexian, and X mana costs
  - ManaBadge with glow ring on non-zero amounts
affects: [17-05, game-ui]

tech-stack:
  added: []
  patterns: [ambiguity-detection-for-conditional-UI, auto-dispatch-via-useEffect]

key-files:
  created:
    - client/src/components/mana/ManaSymbol.tsx
  modified:
    - client/src/components/mana/ManaPaymentUI.tsx
    - client/src/components/mana/ManaBadge.tsx

key-decisions:
  - "Auto-pay via PassPriority useEffect for non-ambiguous costs — no UI shown"
  - "Cost inference from top stack entry source_id object's mana_cost"
  - "Phyrexian toggle between mana icon and heart with 2-life label"

patterns-established:
  - "Ambiguity detection: hasAmbiguousCost checks for / or X shards to decide UI visibility"
  - "Auto-dispatch pattern: useEffect fires PassPriority when no user input needed"

requirements-completed: [MANA-01, MANA-02, MANA-03]

duration: 2min
completed: 2026-03-09
---

# Phase 17 Plan 04: Mana Payment UI Summary

**ManaSymbol component with WUBRG colors, smart auto-pay for simple costs, and interactive toggles for hybrid/phyrexian/X mana**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-09T17:52:10Z
- **Completed:** 2026-03-09T17:54:03Z
- **Tasks:** 2
- **Files modified:** 3

## Accomplishments
- ManaSymbol renders single-color, hybrid (gradient split), phyrexian (with P overlay), generic number, and X shards
- ManaPaymentUI auto-pays silently for non-ambiguous costs, shows interactive UI only for hybrid/phyrexian/X
- X costs use horizontal slider with live total display
- Phyrexian shards toggle between mana payment and 2-life payment
- Hybrid shards toggle between color options
- ManaBadge enhanced with subtle glow ring and increased size

## Task Commits

Each task was committed atomically:

1. **Task 1: Create ManaSymbol component** - `a163173` (feat)
2. **Task 2: Upgrade ManaPaymentUI with smart auto-pay and ambiguous cost handling** - `23bab6c` (feat)

## Files Created/Modified
- `client/src/components/mana/ManaSymbol.tsx` - Individual mana shard renderer with WUBRG colors, hybrid gradients, phyrexian overlay
- `client/src/components/mana/ManaPaymentUI.tsx` - Smart auto-pay + interactive UI for ambiguous costs
- `client/src/components/mana/ManaBadge.tsx` - Added glow ring on non-zero amounts, increased badge size

## Decisions Made
- Auto-pay via PassPriority useEffect for non-ambiguous costs — avoids UI interruption for common case
- Cost inference from top stack entry's source_id mana_cost — reliable because ManaPayment happens during spell casting
- Phyrexian toggle shows heart symbol with "2 life" label for clear affordance

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- ManaSymbol available for reuse in any component needing mana cost display
- ManaBadge glow provides better visual feedback for mana pool state

---
*Phase: 17-mtg-specific-ui*
*Completed: 2026-03-09*
