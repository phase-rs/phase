---
phase: 25-forge-removal-relicensing
plan: 02
subsystem: engine
tags: [feature-gate, cargo-features, forge-compat, ci, coverage]

requires:
  - phase: 25-01
    provides: "Typed dispatch migration (all production code uses effect_variant_name instead of api_type)"
provides:
  - "forge-compat feature gate on all Forge-specific code"
  - "JSON-only coverage binary and CI gate"
  - "Forge .txt data files removed from repository"
  - "Engine compiles without Forge code by default"
affects: [25-03-relicensing]

tech-stack:
  added: []
  patterns: ["cargo feature gating for legacy code isolation"]

key-files:
  created: []
  modified:
    - crates/engine/Cargo.toml
    - crates/engine/src/parser/mod.rs
    - crates/engine/src/database/card_db.rs
    - crates/engine/src/types/ability.rs
    - crates/engine/src/bin/coverage_report.rs
    - .github/workflows/ci.yml
    - .gitignore
    - crates/phase-ai/src/card_hints.rs
    - crates/phase-server/src/main.rs

key-decisions:
  - "ResolvedAbility::from_raw() kept ungated -- used in production code (triggers.rs) for empty-effect fallback"
  - "card-data-export binary gated with required-features (Forge-only tool)"
  - "phase-server migrated from CardDatabase::load() to load_json() (was the last production consumer)"
  - "Test assertions migrated from api_type() to effect_variant_name() (preferred over feature-gating)"
  - "server-core gains forge-compat pass-through feature for its Forge-dependent tests"

patterns-established:
  - "Feature gating: #[cfg(feature = \"forge-compat\")] on methods, modules, and test functions"
  - "Production code uses effect_variant_name() for variant-to-string mapping (never api_type())"

requirements-completed: [MIGR-02, LICN-03]

duration: 14min
completed: 2026-03-11
---

# Phase 25 Plan 02: Feature Gate & Data Deletion Summary

**Forge parser, compat methods, and .txt data gated behind forge-compat feature; engine compiles Forge-free by default with JSON-only coverage CI**

## Performance

- **Duration:** 14 min
- **Started:** 2026-03-11T00:18:16Z
- **Completed:** 2026-03-11T00:32:17Z
- **Tasks:** 2
- **Files modified:** 98

## Accomplishments

- Engine compiles without any Forge code by default (no forge-compat feature)
- All compat bridge methods (api_type, params, mode_str, event_str) gated behind forge-compat
- 78 tracked .txt files in data/standard-cards/ deleted from repository
- CI consolidated to single JSON coverage gate (100% Standard coverage preserved)
- WASM build succeeds without forge-compat
- All 630+ engine tests pass

## Task Commits

1. **Task 1: Feature-gate Forge code and compat bridge methods** - `28ba7b53` (feat)
2. **Task 2: Delete Forge data, update CI** - `21d7ca1e` (chore)

## Files Created/Modified

- `crates/engine/Cargo.toml` - Added [features] forge-compat, required-features on migrate and card-data-export
- `crates/engine/src/parser/mod.rs` - Gated card_parser, card_type, mana_cost behind forge-compat
- `crates/engine/src/database/card_db.rs` - Gated load(), layout_faces, walkdir import behind forge-compat
- `crates/engine/src/types/ability.rs` - Gated api_type, params, mode_str, event_str methods
- `crates/engine/src/bin/coverage_report.rs` - Simplified to JSON-only (removed text mode and --json flag)
- `crates/engine/src/game/coverage.rs` - Gated Forge-dependent tests
- `crates/engine/src/game/effects/copy_spell.rs` - Migrated api_type() to effect_variant_name()
- `crates/engine/src/game/engine.rs` - Migrated api_type() to effect_variant_name()
- `crates/engine/src/game/scenario.rs` - Migrated api_type() to effect_variant_name()
- `crates/engine/src/game/transform.rs` - Migrated api_type() to effect_variant_name()
- `crates/engine/src/game/triggers.rs` - Migrated api_type() to effect_variant_name()
- `crates/engine/src/parser/ability.rs` - Migrated test assertions from api_type()/params() to typed APIs
- `crates/phase-ai/src/card_hints.rs` - Migrated from api_type() to effect_variant_name()
- `crates/phase-server/src/main.rs` - Migrated from CardDatabase::load() to load_json()
- `crates/server-core/Cargo.toml` - Added forge-compat pass-through feature
- `crates/server-core/src/deck_resolve.rs` - Gated Forge-dependent tests
- `crates/engine/tests/parity.rs` - Deleted (parity validation complete)
- `data/standard-cards/` - Deleted 78 .txt files
- `.gitignore` - Removed data/standard-cards/ exemption
- `.github/workflows/ci.yml` - Single JSON coverage gate

## Decisions Made

- **ResolvedAbility::from_raw() kept ungated:** Production code in triggers.rs uses from_raw() for empty-effect fallback when SVar parsing fails. Cannot gate without breaking production.
- **card-data-export binary gated:** This tool only makes sense with Forge .txt data, so it gets required-features = ["forge-compat"].
- **phase-server migrated to load_json():** The server was the last production consumer of CardDatabase::load(). Switched to JSON loading via PHASE_DATA_DIR env var.
- **Test assertions refactored over gated:** Preferred migrating test code from api_type() to effect_variant_name() rather than adding cfg(feature) everywhere -- reduces future maintenance.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] card-data-export binary also uses CardDatabase::load()**
- **Found during:** Task 1 (Step 7 - compilation verification)
- **Issue:** The plan didn't mention card_data_export.rs, which also calls CardDatabase::load()
- **Fix:** Added required-features = ["forge-compat"] to the card-data-export binary
- **Files modified:** crates/engine/Cargo.toml
- **Committed in:** 28ba7b53

**2. [Rule 3 - Blocking] phase-server and server-core use CardDatabase::load()**
- **Found during:** Task 1 (Step 7 - test compilation)
- **Issue:** phase-server/main.rs and server-core/deck_resolve.rs tests call CardDatabase::load()
- **Fix:** Migrated phase-server to load_json(), added forge-compat feature to server-core, gated its tests
- **Files modified:** crates/phase-server/src/main.rs, crates/server-core/Cargo.toml, crates/server-core/src/deck_resolve.rs
- **Committed in:** 28ba7b53

**3. [Rule 3 - Blocking] phase-ai production code uses api_type()**
- **Found during:** Task 1 (Step 7 - test compilation)
- **Issue:** card_hints.rs in phase-ai crate calls api_type() on AbilityDefinition -- production code, not just tests
- **Fix:** Migrated to effect_variant_name(&a.effect)
- **Files modified:** crates/phase-ai/src/card_hints.rs
- **Committed in:** 28ba7b53

---

**Total deviations:** 3 auto-fixed (all Rule 3 - blocking)
**Impact on plan:** All auto-fixes necessary to achieve clean compilation. No scope creep.

## Issues Encountered

None beyond the blocking issues documented above.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- Engine compiles Forge-free by default, ready for Plan 03 (license header updates and NOTICE file)
- All Forge code preserved behind forge-compat feature for potential future reference
- 78 .txt files removed, reducing repo size

---
*Phase: 25-forge-removal-relicensing*
*Completed: 2026-03-11*

## Self-Check: PASSED
