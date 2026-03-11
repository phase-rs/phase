---
phase: 29-support-n-players
plan: 09
subsystem: ai
tags: [threat-evaluation, multiplayer-ai, combat-ai, search-scaling, paranoid-search]

requires:
  - phase: 29-06
    provides: "N-player transport and AI seat migration"
provides:
  - "Threat-aware evaluation across N opponents"
  - "Player-count-scaled search budgets (paranoid for 3-4, heuristic for 5-6)"
  - "Per-creature attack target selection in multiplayer"
  - "create_config_for_players() for runtime AI configuration"
affects: [29-10, 29-11, 29-12]

tech-stack:
  added: []
  patterns: ["threat-weighted evaluation", "paranoid minimax search", "alpha-strike detection"]

key-files:
  created: []
  modified:
    - crates/phase-ai/src/eval.rs
    - crates/phase-ai/src/config.rs
    - crates/phase-ai/src/combat_ai.rs
    - crates/phase-ai/src/card_hints.rs
    - crates/phase-ai/src/search.rs
    - crates/phase-ai/src/lib.rs

key-decisions:
  - "threat_level() uses weighted combination: board 40%, life 20%, hand 15%, commander damage 25%"
  - "Multiplayer eval uses threat-weighted opponent scoring; 2-player retains original averaging path"
  - "AiDifficulty derives PartialOrd/Ord for difficulty comparison in scaling logic"
  - "choose_attackers_with_targets() as new primary API; choose_attackers() kept as backward-compatible wrapper"
  - "Alpha-strike detection: allocate smallest attackers first to just exceed lethal threshold"

patterns-established:
  - "Threat-weighted evaluation: opponent scores weighted by threat_level() in multiplayer"
  - "Player-count-aware config: create_config_for_players() applies scaling on top of platform scaling"

requirements-completed: [NP-AI-THREAT, NP-AI-SEARCH, NP-AI-SEAT]

duration: 6min
completed: 2026-03-11
---

# Phase 29 Plan 09: AI N-Player Threat Evaluation and Scaled Search Summary

**Threat-aware multiplayer AI with per-opponent evaluation weighting, player-count search scaling, and per-creature attack target allocation**

## Performance

- **Duration:** 6 min
- **Started:** 2026-03-11T18:32:12Z
- **Completed:** 2026-03-11T18:38:31Z
- **Tasks:** 2
- **Files modified:** 6

## Accomplishments
- Threat-aware evaluation weights opponent scores by board presence, life ratio, hand size, and commander damage
- Search budget scales inversely with player count: depth 2 for 3-4p, depth 1 or disabled for 5-6p
- Per-creature attack targets with alpha-strike detection to finish off weak opponents
- Removal spell scoring in card_hints weighs by controller threat level

## Task Commits

Each task was committed atomically:

1. **Task 1: Threat-aware evaluation and search scaling** - `cee20f6ef` (feat)
2. **Task 2: Multi-opponent combat AI and attack target selection** - `b8acc3cb7` (feat)

## Files Created/Modified
- `crates/phase-ai/src/eval.rs` - Added threat_level(), threat-weighted multiplayer evaluation
- `crates/phase-ai/src/config.rs` - Added player_count field, create_config_for_players(), AiDifficulty Ord
- `crates/phase-ai/src/combat_ai.rs` - Added choose_attackers_with_targets(), alpha-strike detection, assign_attack_targets()
- `crates/phase-ai/src/card_hints.rs` - Threat-weighted removal priority scoring
- `crates/phase-ai/src/search.rs` - Use choose_attackers_with_targets() for DeclareAttackers
- `crates/phase-ai/src/lib.rs` - Export new public API functions

## Decisions Made
- threat_level() returns 0.0-1.0 using weighted factors: board presence (40%), life ratio (20%), hand size (15%), commander damage (25%)
- In multiplayer (3+ opponents), evaluate_state uses threat-weighted opponent scoring instead of simple averaging
- 2-player path remains unchanged for zero overhead in the common case
- Alpha-strike detection sorts attackers by power ascending, allocating smallest first to minimize overkill
- choose_attackers() kept as backward-compat wrapper calling choose_attackers_with_targets()

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- AI engine fully adapted for N-player multiplayer
- Ready for integration with board UI (Plan 10+) and format-specific rules

---
*Phase: 29-support-n-players*
*Completed: 2026-03-11*
