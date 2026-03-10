# Requirements: Forge.rs

**Defined:** 2026-03-10
**Core Value:** A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent — with all cards behaving correctly according to MTG comprehensive rules.

## v1.2 Requirements

Requirements for v1.2 milestone. Each maps to roadmap phases.

### Data Pipeline

- [x] **DATA-01**: Engine loads card metadata (name, mana cost, types, P/T, colors, keywords, oracle text, layout) from MTGJSON AllCards AtomicCards.json using custom Rust types
- [x] **DATA-02**: Engine defines a typed JSON ability schema mapping to AbilityDefinition, TriggerDefinition, StaticDefinition, and ReplacementDefinition types
- [x] **DATA-03**: CardDatabase::load_json() merges MTGJSON metadata + ability JSON into CardFace, becoming the primary card loading path
- [x] **DATA-04**: Ability JSON schema exports a JSON Schema definition via schemars for editor autocompletion and build-time validation

### Card Migration

- [x] **MIGR-01**: All engine-supported cards (thousands — every card whose mechanics have registered handlers) are converted from Forge .txt to MTGJSON metadata + ability JSON via automated migration
- [ ] **MIGR-02**: data/cardsfolder/ and data/standard-cards/ are removed from the repository; Forge parser is feature-gated behind forge-compat
- [x] **MIGR-03**: Automated Forge-to-JSON migration tool converts all 32,300+ Forge .txt card definitions to the new ability JSON format, producing ability files for every engine-supported card
- [x] **MIGR-04**: Card data includes MTGJSON scryfallOracleId for reliable frontend image lookups via Scryfall API
- [ ] **MIGR-05**: CI coverage gate updated to validate against JSON card data; all previously supported cards remain supported after migration

### Testing

- [x] **TEST-01**: A self-contained GameScenario test harness provides add_card(), set_phase(), act(), and assertion helpers with no external filesystem dependencies
- [x] **TEST-02**: Scenario-based rules correctness tests cover core mechanics: ETB triggers, combat, stack resolution, state-based actions, layer system, keyword interactions
- [x] **TEST-03**: insta snapshot tests capture GameState after action sequences to detect unintended engine changes across commits
- [x] **TEST-04**: Per-card behavioral parity tests confirm migrated cards produce identical game outcomes as the original Forge format (sampled across mechanics, not exhaustive per-card)

### Licensing & Cleanup

- [ ] **LICN-01**: Project relicensed as MIT/Apache-2.0 dual license after all GPL-coupled data is removed
- [ ] **LICN-02**: PROJECT.md constraints and key decisions updated to reflect MTGJSON + own ability format (removing Forge format dependency)
- [ ] **LICN-03**: Coverage report (coverage.rs) reads JSON format and CI gate (100% Standard coverage) is preserved

## Future Requirements

Deferred to v2+. Tracked but not in current roadmap.

### Testing (Advanced)

- **TEST-05**: Property-based testing with proptest for randomized game state exploration
- **TEST-06**: Comprehensive rules reference tests indexed by MTG Comprehensive Rule number

### Card Expansion

- **EXPN-01**: MTGJSON auto-update script for new set releases

## Out of Scope

| Feature | Reason |
|---------|--------|
| Natural language ability parsing from oracle text | NLP problem far beyond this milestone's scope |
| Runtime migration to Vec\<AbilityDefinition\> on GameObject | Research identified as cleaner long-term, but v1.2 can emit Forge-compatible strings from JSON loader — defer refactor to avoid touching ~13 source files |
| Full git history rewrite to remove GPL files | git filter-branch is destructive; .gitignore + deletion sufficient for licensing purposes |
| Manual ability authoring for unsupported cards | Migration tool handles supported cards; adding new handler coverage is separate work |

## Traceability

Which phases cover which requirements. Updated during roadmap creation.

| Requirement | Phase | Status |
|-------------|-------|--------|
| DATA-01 | Phase 21 | Complete |
| DATA-02 | Phase 21 | **Complete** (21-01) |
| DATA-03 | Phase 23 | Complete |
| DATA-04 | Phase 21 | Complete |
| MIGR-01 | Phase 24 | Complete |
| MIGR-02 | Phase 25 | Pending |
| MIGR-03 | Phase 24 | Complete |
| MIGR-04 | Phase 23 | Complete |
| MIGR-05 | Phase 24 | Pending |
| TEST-01 | Phase 22 | Complete |
| TEST-02 | Phase 22 | Complete |
| TEST-03 | Phase 22 | Complete |
| TEST-04 | Phase 24 | Complete |
| LICN-01 | Phase 25 | Pending |
| LICN-02 | Phase 25 | Pending |
| LICN-03 | Phase 25 | Pending |

**Coverage:**
- v1.2 requirements: 16 total
- Mapped to phases: 16
- Unmapped: 0

---
*Requirements defined: 2026-03-10*
*Last updated: 2026-03-10 after roadmap creation*
