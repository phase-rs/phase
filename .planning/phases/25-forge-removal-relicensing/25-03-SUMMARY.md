---
phase: 25-forge-removal-relicensing
plan: 03
subsystem: licensing
tags: [mit, apache-2.0, dual-license, mtgjson, cargo-toml, documentation]

# Dependency graph
requires:
  - phase: 25-02
    provides: "Feature-gated Forge code, deleted Forge data, production JSON pipeline"
provides:
  - "MIT/Apache-2.0 dual license files (LICENSE-MIT, LICENSE-APACHE)"
  - "NOTICE file with MTGJSON attribution"
  - "Workspace-level license declaration in all Cargo.toml files"
  - "Forge-free project documentation (CLAUDE.md, PROJECT.md)"
affects: [all-future-phases, open-source-distribution]

# Tech tracking
tech-stack:
  added: []
  patterns: ["workspace.package license inheritance for all crates"]

key-files:
  created:
    - LICENSE-MIT
    - LICENSE-APACHE
    - NOTICE
  modified:
    - Cargo.toml
    - crates/engine/Cargo.toml
    - crates/engine-wasm/Cargo.toml
    - crates/phase-ai/Cargo.toml
    - crates/server-core/Cargo.toml
    - crates/phase-server/Cargo.toml
    - client/src-tauri/Cargo.toml
    - CLAUDE.md
    - .planning/PROJECT.md

key-decisions:
  - "MIT/Apache-2.0 dual license following Rust ecosystem convention"
  - "forge-compat feature gate name retained as technical identifier in documentation"

patterns-established:
  - "Workspace license inheritance: workspace.package.license declared once, crates use license.workspace = true"

requirements-completed: [LICN-01, LICN-02]

# Metrics
duration: 4min
completed: 2026-03-11
---

# Phase 25 Plan 03: Relicensing & Documentation Scrub Summary

**MIT/Apache-2.0 dual license with MTGJSON attribution, all 7 Cargo.toml files declaring workspace license, and Forge-free project documentation**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-11T00:35:53Z
- **Completed:** 2026-03-11T00:40:04Z
- **Tasks:** 2
- **Files modified:** 12

## Accomplishments
- Created LICENSE-MIT, LICENSE-APACHE, and NOTICE files at repository root
- Updated all 7 Cargo.toml files with MIT/Apache-2.0 license via workspace inheritance
- Scrubbed Forge heritage language from CLAUDE.md and PROJECT.md
- Updated PROJECT.md: title, "What This Is", crate names, card format, license constraint, key decisions table, completed Active requirements

## Task Commits

Each task was committed atomically:

1. **Task 1: Create license files and update all Cargo.toml manifests** - `570a9f31` (chore)
2. **Task 2: Scrub Forge references from project documentation** - `82446244` (docs)

## Files Created/Modified
- `LICENSE-MIT` - MIT license text, copyright phase.rs contributors
- `LICENSE-APACHE` - Apache License 2.0 full text
- `NOTICE` - MTGJSON third-party attribution
- `Cargo.toml` - Added workspace.package license = "MIT OR Apache-2.0"
- `crates/engine/Cargo.toml` - Added license.workspace = true
- `crates/engine-wasm/Cargo.toml` - Added license.workspace = true
- `crates/phase-ai/Cargo.toml` - Added license.workspace = true
- `crates/server-core/Cargo.toml` - Added license.workspace = true
- `crates/phase-server/Cargo.toml` - Added license.workspace = true
- `client/src-tauri/Cargo.toml` - Added license.workspace = true
- `CLAUDE.md` - Removed Forge heritage language, updated parser/database descriptions
- `.planning/PROJECT.md` - Updated title, description, crate names, constraints, key decisions

## Decisions Made
- MIT/Apache-2.0 dual license following Rust ecosystem convention (e.g., tokio, serde, axum)
- The `forge-compat` feature gate name is retained in documentation as a technical identifier (it's the actual Cargo feature name)
- Completed Active requirements in PROJECT.md marked with [x] and version tag rather than moved to Validated (preserves v1.2 scope visibility)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Phase 25 (Forge Removal & Relicensing) is now complete
- Project is fully MIT/Apache-2.0 licensed with clean IP chain
- Ready for Phase 26 (Polish and fix multi-player with lobby and embedded server)
- Test suite requirement (XMage MIT reference) remains as the last Active item in PROJECT.md

---
*Phase: 25-forge-removal-relicensing*
*Completed: 2026-03-11*
