---
phase: 30-implement-the-required-building-blocks-specified-in-the-summary
plan: 03
subsystem: engine
tags: [adventure, casting, cr-715, exile, game-mechanics]

requires:
  - phase: 30-01
    provides: "CastingPermission::AdventureCreature type, casting_permissions on GameObject"
provides:
  - "Adventure casting choice flow (WaitingFor::AdventureCastChoice, ChooseAdventureFace)"
  - "Adventure spell exile-on-resolve with creature face restoration"
  - "Casting from exile with AdventureCreature permission"
  - "AI candidate action generation for Adventure face choice"
  - "Adventure back_face population via card database rehydration"
affects: [adventure-cards, casting-subsystem, stack-resolution, exile-mechanics]

tech-stack:
  added: []
  patterns:
    - "Adventure face swap via back_face + snapshot_object_face/apply_back_face_to_object"
    - "cast_as_adventure flag on StackEntryKind::Spell for exile routing"
    - "is_adventure_card() detection via back_face core type analysis"

key-files:
  created: []
  modified:
    - crates/engine/src/game/casting.rs
    - crates/engine/src/game/stack.rs
    - crates/engine/src/game/engine.rs
    - crates/engine/src/game/zones.rs
    - crates/engine/src/game/printed_cards.rs
    - crates/engine/src/ai_support/candidates.rs

key-decisions:
  - "Adventure detection uses back_face card types (instant/sorcery + creature front) rather than separate is_adventure flag"
  - "Adventure face data populated via rehydrate_game_from_card_db rather than at deck loading time"
  - "AI face selection delegated to minimax search rather than explicit heuristic"
  - "CastingPermission cleared on zone change from exile (prevents stale permissions)"

patterns-established:
  - "Adventure face swap: snapshot current face, apply back_face, save snapshot as new back_face"
  - "cast_as_adventure on StackEntryKind::Spell controls exile routing at resolution"

requirements-completed: [BB-03]

duration: 40min
completed: 2026-03-17
---

# Phase 30 Plan 03: Adventure Casting Subsystem Summary

**Full CR 715 Adventure casting flow: face choice from hand, exile-on-resolve with AdventureCreature permission, creature casting from exile, and AI support**

## Performance

- **Duration:** 40 min
- **Started:** 2026-03-17T00:42:59Z
- **Completed:** 2026-03-17T01:23:00Z
- **Tasks:** 2
- **Files modified:** 6

## Accomplishments
- Adventure cards in hand prompt face choice (creature vs Adventure spell)
- Adventure spells resolve to exile with creature face restoration + AdventureCreature permission
- Countered Adventure spells go to graveyard normally (CR 715.4 compliance)
- Creature face castable from exile with AdventureCreature permission
- AI generates both face options and evaluates via minimax search
- Adventure face data loaded from card database during rehydration
- 5 unit tests + 1 integration test, 1416 total engine tests passing

## Task Commits

1. **Task 1: Adventure casting flow and exile-on-resolve** - `16d83b0d1` (feat)
2. **Task 2: AI Adventure support + card data loading** - `f438b4001` (feat)

## Files Created/Modified
- `crates/engine/src/game/casting.rs` - Adventure detection, face swap, handle_adventure_choice, exile casting support
- `crates/engine/src/game/stack.rs` - cast_as_adventure extraction, exile routing on resolve, creature face restoration
- `crates/engine/src/game/engine.rs` - AdventureCastChoice + ChooseAdventureFace match arm
- `crates/engine/src/game/zones.rs` - Clear AdventureCreature permission on zone change from exile
- `crates/engine/src/game/printed_cards.rs` - Populate back_face for Adventure cards during rehydration
- `crates/engine/src/ai_support/candidates.rs` - AdventureCastChoice legal action generation + actor matching

## Decisions Made
- Adventure detection uses `back_face` card types (instant/sorcery back + creature front) rather than a separate `is_adventure` boolean -- avoids adding state to GameObject
- Adventure face data populated during `rehydrate_game_from_card_db` rather than at deck loading -- deck loading only has `CardFace`, not `CardRules`, and rehydration already has card database access
- AI face selection delegated to minimax search rather than explicit heuristic -- the candidate action system already generates both options, and the search naturally evaluates which scores higher
- `CastingPermission::AdventureCreature` cleared when an object leaves exile via zone change -- prevents stale permissions if a card is exiled by a different effect later

## Deviations from Plan

None - plan executed as written.

## Issues Encountered
- Pre-existing compilation errors in `oracle_static.rs` from dirty working tree (not related to this plan)
- Pre-existing WASM test failures in `engine-wasm` (serialization issue, not related to this plan)
- Adventure spell test required direct stack push instead of full casting flow because Stomp has targets

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Adventure casting subsystem complete and composable for all Adventure cards
- Ready for Plan 04 (remaining building blocks)
- Integration with specific Adventure cards (Bonecrusher Giant, Brazen Borrower) requires card data pipeline

---
*Phase: 30-implement-the-required-building-blocks-specified-in-the-summary*
*Completed: 2026-03-17*
