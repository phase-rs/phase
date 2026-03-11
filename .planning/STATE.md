---
gsd_state_version: 1.0
milestone: v1.2
milestone_name: Migrate Data Source & Add Tests
status: executing
stopped_at: Completed 25-03-PLAN.md
last_updated: "2026-03-11T00:40:04Z"
last_activity: 2026-03-11 — Completed Plan 25-03 (Relicensing & Documentation Scrub)
progress:
  total_phases: 6
  completed_phases: 5
  total_plans: 16
  completed_plans: 16
  percent: 100
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-03-11)

**Core value:** A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent -- with all cards behaving correctly according to MTG comprehensive rules.
**Current focus:** Phase 25 Forge Removal & Relicensing — Complete

## Current Position

Phase: 25 (fifth of 5 in v1.2) — Forge Removal & Relicensing
Plan: 3/3 complete
Status: Phase 25 Complete
Last activity: 2026-03-11 — Completed Plan 25-03 (Relicensing & Documentation Scrub)

Progress: [██████████] 100%

## Performance Metrics

**Velocity:**
- Total plans completed: 16 (v1.2)
- Average duration: 13min
- Total execution time: 3.3 hours

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 21 | 5/5 | 68min | 14min |
| 22 | 3/3 | 35min | 12min |
| 23 | 2/2 | 18min | 9min |
| 24 | 3/3 | 43min | 14min |
| 25 | 3/3 | 31min | 10min |

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
- [24-02]: Keyword parity compares base names (before colon) -- Forge "Ward:1" matches MTGJSON "Ward"
- [24-02]: NoCost and Cost{0,[]} treated as equivalent for basic land mana cost comparison
- [24-02]: Extra MTGJSON-only keywords (Scry, Mill) allowed -- action keywords Forge doesn't track
- [24-02]: normalize_for_match strips all non-alphanumeric chars (fixes apostrophe card matching)
- [24-03]: Benign MTGJSON keyword mismatches stripped in coverage binary (bare parameterized keywords, action keywords like Scry/Mill)
- [24-03]: Coverage manifest filtering applied after analyze_standard_coverage() -- binary-local logic, coverage.rs unchanged
- [24-03]: MTGJSON action keywords (Scry, Mill, Surveil, Fateseal) excluded from coverage since handled as effects, not keywords
- [25-01]: effect_variant_name() as standalone function for production variant-to-string mapping (not a method on Effect)
- [25-01]: Effect::to_params() stays ungated -- legitimate typed-to-HashMap serialization for SubAbility chains
- [25-01]: Test code retains compat bridge methods (api_type, params, from_raw) -- gated in Plan 02
- [25-02]: ResolvedAbility::from_raw() kept ungated -- used in production (triggers.rs empty-effect fallback)
- [25-02]: card-data-export binary gated with required-features (Forge-only tool)
- [25-02]: phase-server migrated from CardDatabase::load() to load_json() (last production consumer)
- [25-02]: Test assertions migrated from api_type() to effect_variant_name() (preferred over feature-gating)
- [25-03]: MIT/Apache-2.0 dual license following Rust ecosystem convention
- [25-03]: forge-compat feature gate name retained as technical identifier in documentation

### Roadmap Evolution

- Phase 26 added: Polish and fix multi-player with lobby and embedded server

### Pending Todos

None yet.

### Blockers/Concerns

- ~~GPL contamination legal analysis has LOW confidence~~ — Resolved: Forge data deleted, parser feature-gated, project relicensed MIT/Apache-2.0

## Session Continuity

Last session: 2026-03-11T00:40:04Z
Stopped at: Completed 25-03-PLAN.md
Resume file: .planning/phases/25-forge-removal-relicensing/25-03-SUMMARY.md
