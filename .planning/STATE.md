---
gsd_state_version: 1.0
milestone: v1.2
milestone_name: Migrate Data Source & Add Tests
status: in-progress
stopped_at: Completed 24-01-PLAN.md
last_updated: "2026-03-10T22:30:00.000Z"
last_activity: 2026-03-10 — Completed Plan 24-01 (Migration Tool & Cost Parser)
progress:
  total_phases: 5
  completed_phases: 3
  total_plans: 12
  completed_plans: 10
  percent: 83
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-03-10)

**Core value:** A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent -- with all cards behaving correctly according to MTG comprehensive rules.
**Current focus:** Phase 24 Card Migration — Plan 01 complete, Plans 02-03 pending

## Current Position

Phase: 24 (fourth of 5 in v1.2) — Card Migration
Plan: 1/3 complete
Status: Plan 24-01 complete, Plan 24-02 pending
Last activity: 2026-03-10 — Completed Plan 24-01 (Migration Tool & Cost Parser)

Progress: [████████░░] 83%

## Performance Metrics

**Velocity:**
- Total plans completed: 11 (v1.2)
- Average duration: 12min
- Total execution time: 2.2 hours

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 21 | 5/5 | 68min | 14min |
| 22 | 3/3 | 35min | 12min |
| 23 | 2/2 | 18min | 9min |
| 24 | 1/3 | 12min | 12min |

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table.
Recent decisions affecting current work:

- [v1.2 init]: Custom MTGJSON types (~50 lines) instead of mtgjson crate (missing defense field, unnecessary transitive deps)
- [v1.2 init]: Author ability definitions from oracle text/rules, use Forge output only for validation (GPL contamination avoidance)
- [Phase 21 context]: REVERSED prior decision — Vec<AbilityDefinition> refactor included in Phase 21 (not deferred). Emitting Forge-compatible strings perpetuates licensing risk. Typed enums for all four definition types (Effect, TriggerMode, StaticMode, ReplacementEvent)
- [Phase 21 context]: AtomicCards.json committed to repo, one ability JSON per card in data/abilities/, schemars for schema generation
- [21-01]: remaining_params field on AbilityDefinition preserves unconsumed parser params for compat
- [21-01]: Display impl on TriggerMode uses Debug formatting for known variants (simple, correct)
- [21-01]: ResolvedAbility left unchanged per plan — transitional approach for Plan 02
- [21-01]: Compat methods (api_type(), params(), mode_str(), event_str()) bridge typed enums to string consumers
- [21-03]: 7-card test fixture instead of full 50MB AtomicCards.json for fast CI
- [21-03]: Schema generation via test keeps schema.json in sync with Rust types
- [21-03]: Relative $schema path in ability JSON files for co-located schema
- [Phase 21-02]: Kept compat bridge methods (api_type, params) for transitional dispatch rather than full pattern-matching migration
- [Phase 21-02]: parse_test_ability() helpers in test modules for readable typed test data construction
- [21-04]: Kept api_type/params on ResolvedAbility for backward compat; typed dispatch via match on effect field
- [21-04]: from_raw() wraps in Effect::Other for test compat; new() builds from typed Effect for production code
- [22-01]: CardBuilder borrows &mut GameState (not &mut GameScenario) to avoid borrow checker conflicts
- [22-01]: scenario.rs not #[cfg(test)] gated -- integration tests compile crate as dependency, can't access cfg(test) modules
- [22-01]: #[path] attributes for integration test module resolution in Cargo test binaries
- [22-02]: ChangesZone triggers test Hand->Stack zone transitions (not just ETB) -- engine fires on all zone changes
- [22-02]: Deathtouch SBA test uses direct GameState construction since GameRunner doesn't expose &mut state for dealt_deathtouch_damage
- [22-02]: Explicit act(PassPriority) loop for stack drain when triggers add entries during resolution
- [22-03]: CardBuilder must push keywords to both keywords and base_keywords to survive layer evaluation (bug fix)
- [22-03]: Combat integration tests use run_combat() helper driving full engine pipeline (PassPriority -> DeclareAttackers -> DeclareBlockers)
- [22-03]: Layer tests trigger evaluation via PassPriority (SBAs run, which evaluate layers when layers_dirty=true)
- [22-03]: GameRunner::snapshot() for step-by-step snapshot tests
- [23-01]: Case-insensitive MTGJSON name matching for filename-to-card lookup (handles title-cased prepositions)
- [23-01]: FaceAbilities struct with flat #[serde(default)] fields mirroring AbilityFile for multi-face cards
- [23-01]: Equip synthesis uses Effect::Attach with TargetSpec::Filtered for Creature.YouCtrl
- [23-01]: CardDatabase fields made pub(crate) for cross-module construction by json_loader
- [23-02]: normalize_for_match() strips punctuation for card name matching (handles comma in "Jace, the Mind Sculptor")
- [23-02]: Smoke game tests use direct mana pool injection for spell casting, PassPriority loop for combat phases
- [23-02]: Ability JSON files use Effect::Other for complex card-specific effects not yet covered by typed variants
- [24-01]: Unknown cost components (PayLife, Discard, tapXType, exert) preserved as AbilityCost::Mana fallback (matches Effect::Other pattern)
- [24-01]: Migration overwrites 8 hand-authored JSON files from Phase 23 for consistency across all 32,274 cards
- [24-01]: json_smoke_test adapted: error-type checking (not zero errors) and fixture-centric cross-validation
- [24-01]: 26 Forge files with missing Name field skipped as errors (Specialize variants with alternate format)

### Pending Todos

None yet.

### Blockers/Concerns

- GPL contamination legal analysis has LOW confidence -- consider clean-room authoring approach for complex multi-ability cards

## Session Continuity

Last session: 2026-03-10T22:30:00.000Z
Stopped at: Completed 24-01-PLAN.md
Resume file: .planning/phases/24-card-migration/24-01-SUMMARY.md
