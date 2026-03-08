---
gsd_state_version: 1.0
milestone: v1.0
milestone_name: milestone
status: in_progress
stopped_at: Completed 06-01-PLAN.md
last_updated: "2026-03-08T04:42:51Z"
last_activity: 2026-03-08 -- Completed 06-01 (Replacement Effects)
progress:
  total_phases: 8
  completed_phases: 5
  total_plans: 17
  completed_plans: 15
  percent: 88
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-03-07)

**Core value:** A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent -- with all cards behaving correctly according to MTG comprehensive rules.
**Current focus:** Phase 6 -- Advanced Rules (Replacement Effects & Layers)

## Current Position

Phase: 6 of 8 (Advanced Rules) -- IN PROGRESS
Plan: 1 of 3 in current phase -- COMPLETE
Status: Phase 06 In Progress
Last activity: 2026-03-08 -- Completed 06-01 (Replacement Effects)

Progress: [████████░░] 88% (15/17 plans across phases)

## Performance Metrics

**Velocity:**
- Total plans completed: 11
- Average duration: 6min
- Total execution time: 0.9 hours

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 01 | 2 | 18min | 9min |
| 02 | 3 | 12min | 4min |
| 03 | 3 | 15min | 5min |

**Recent Trend:**
- Last 5 plans: 3min, 4min, 4min, 7min, 5min, 7min
- Trend: stable

*Updated after each plan completion*
| Phase 01 P01 | 4min | 2 tasks | 19 files |
| Phase 01 P02 | 14min | 3 tasks | 18 files |
| Phase 02 P01 | 5min | 2 tasks | 11 files |
| Phase 02 P02 | 4min | 1 tasks | 2 files |
| Phase 02 P03 | 3min | 1 tasks | 4 files |
| Phase 03 P01 | 4min | 2 tasks | 10 files |
| Phase 03 P02 | 4min | 2 tasks | 7 files |
| Phase 03 P03 | 7min | 2 tasks | 7 files |
| Phase 04 P01 | 5min | 2 tasks | 17 files |
| Phase 04 P02 | 7min | 2 tasks | 11 files |
| Phase 04 P03 | 4min | 2 tasks | 6 files |
| Phase 05 P01 | 5min | 2 tasks | 9 files |
| Phase 05 P02 | 5min | 2 tasks | 7 files |
| Phase 05 P03 | 5min | 2 tasks | 7 files |
| Phase 06 P01 | 5min | 2 tasks | 10 files |

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table.
Recent decisions affecting current work:

- [Roadmap]: 8-phase bottom-up build following rules engine dependency graph (types -> parser -> engine -> abilities -> triggers/combat -> layers/replacements -> UI -> AI)
- [Roadmap]: Engine built as pure Rust with no platform deps before any UI integration
- [Roadmap]: PLAT-03 (EngineAdapter) assigned to Phase 1 as architectural scaffold
- [Phase 01]: Used tsify (not tsify-next) per RUSTSEC-2025-0048 advisory
- [Phase 01]: Newtype wrappers in engine-wasm for tsify (not feature flags on engine types)
- [Phase 01]: Standard Rust collections for Phase 1; rpds deferred to Phase 3 state management
- [Phase 01]: EngineAdapter as simple 4-method interface (initialize, submitAction, getState, dispose)
- [Phase 01]: Async queue in WasmAdapter serializes all WASM access (single-threaded constraint)
- [Phase 01]: AdapterError with code, message, recoverable fields for structured error handling
- [Phase 02]: ManaCost as enum with NoCost/Cost variants (not Option wrapping)
- [Phase 02]: Shared parse_params helper for pipe-delimited Key$ Value format
- [Phase 02]: CardType parser uses FromStr on Supertype/CoreType enums for classification
- [Phase 02]: ManaCostShard::from_str for all 40+ shard token mappings
- [Phase 02]: First-byte dispatch on key character then exact match for card parser line processing
- [Phase 02]: CardFaceBuilder with build() validation -- requires name, defaults ManaCost to zero
- [Phase 02]: Lenient parsing: unknown keys silently skipped matching Forge behavior
- [Phase 02]: Clone CardFace for face_index -- simpler than lifetime references, CardFace structs are small
- [Phase 02]: filter_entry depth==0 bypass for root directory dotfile check
- [Phase 03]: ChaCha20Rng for cross-platform deterministic seeded RNG (not StdRng)
- [Phase 03]: HashMap<ObjectId, GameObject> central object store with zones as Vec<ObjectId>
- [Phase 03]: ManaPool as Vec<ManaUnit> with source tracking and restrictions (not counter fields)
- [Phase 03]: serde(skip) on RNG field with seed-based reconstruction on deserialization
- [Phase 03]: Custom PartialEq on GameState excluding RNG (compared via seed)
- [Phase 03]: Auto-advance loop pattern for phases needing no player input
- [Phase 03]: priority_pass_count on GameState for consecutive-pass tracking
- [Phase 03]: Greedy mana payment: colored first, hybrid prefers more available color, phyrexian life fallback
- [Phase 03]: SBA fixpoint capped at 9 iterations per Forge convention
- [Phase 03]: Action dispatch via (waiting_for, action) tuple match for clean validation
- [Phase 03]: start_game auto-detects libraries for mulligan vs skip-mulligan
- [Phase 04]: EffectHandler as fn pointer (not trait) for HashMap storage simplicity
- [Phase 04]: Each handler emits EffectResolved event for tracking and trigger detection
- [Phase 04]: Token objects use CardId(0) convention
- [Phase 04]: Destroy checks indestructible keyword case-insensitively
- [Phase 04]: Build effect registry per apply() call -- cheap and avoids static patterns
- [Phase 04]: Auto-target when exactly one legal target (skip WaitingFor round-trip)
- [Phase 04]: Effect handler errors don't crash resolution -- graceful degradation
- [Phase 04]: SVar resolution via lazy lookup in ability.svars HashMap at resolve time
- [Phase 04]: Conditions default to true when not present or unrecognized (safe fallback)
- [Phase 04]: Sub-ability inherits svars, source_id, controller from parent
- [Phase 04]: Card filter added to targeting for stack spell targeting (Counterspell)
- [Phase 05]: Keyword FromStr uses Infallible error type (never fails, unknown -> Keyword::Unknown)
- [Phase 05]: TriggerMode FromStr case-sensitive matching Forge's CamelCase conventions
- [Phase 05]: has_keyword uses std::mem::discriminant for parameterized keyword matching
- [Phase 05]: CardFace.keywords stays Vec<String> in parser; conversion via parse_keywords at GameObject creation
- [Phase 05]: Build trigger registry per call (cheap, same pattern as effect registry)
- [Phase 05]: trigger_definitions stored on GameObject at creation time (avoid re-parsing)
- [Phase 05]: APNAP ordering via sort-by-key then reverse for LIFO stack placement
- [Phase 05]: Unimplemented trigger modes return false (recognized but don't fire)
- [Phase 05]: Execute param resolves SVars via existing parse_ability
- [Phase 05]: Auto-order blockers by ObjectId ascending (deterministic default, UI player choice deferred)
- [Phase 05]: CombatState as Option on GameState -- None outside combat, Some during combat phases
- [Phase 05]: SBA: deathtouch flag + damage > 0 = lethal; indestructible prevents destruction
- [Phase 05]: Combat phases auto-skip via has_potential_attackers check
- [Phase 06]: indexmap for deterministic replacement ordering in player choice scenarios
- [Phase 06]: Flat replacement.rs file -- all 14 handlers inline until file grows unwieldy
- [Phase 06]: ReplacementMatcher/ReplacementApplier fn pointer pair per handler type
- [Phase 06]: ProposedEvent carries HashSet<ReplacementId> for once-per-event tracking

### Pending Todos

None yet.

### Blockers/Concerns

- [Research]: Verify rpds API covers all needed persistent data structure operations during Phase 3
- [Resolved]: WASM binary size measured at 19 KB (well under 3 MB target)

## Session Continuity

Last session: 2026-03-08T04:42:51Z
Stopped at: Completed 06-01-PLAN.md
Resume file: .planning/phases/06-advanced-rules/06-01-SUMMARY.md
