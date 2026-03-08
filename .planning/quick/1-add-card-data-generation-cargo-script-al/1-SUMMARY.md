---
phase: quick-1
plan: 01
subsystem: infra
tags: [bash, cargo-aliases, setup, documentation]

requires: []
provides:
  - Card data generation pipeline (scripts/gen-card-data.sh)
  - Full first-run setup script (scripts/setup.sh)
  - Expanded cargo aliases (test-all, clippy-strict, export-cards, serve)
  - Comprehensive README.md
affects: []

tech-stack:
  added: []
  patterns: [sparse-git-checkout for upstream card data]

key-files:
  created:
    - scripts/gen-card-data.sh
    - scripts/setup.sh
    - README.md
  modified:
    - .cargo/config.toml
    - .gitignore

key-decisions:
  - "Cargo aliases only for cargo-native commands; shell scripts for multi-step workflows"
  - "Sparse checkout to avoid cloning full 2GB+ Forge repo"

patterns-established:
  - "Setup scripts in scripts/ directory, matching build-wasm.sh style"

requirements-completed: [QUICK-1]

duration: 4min
completed: 2026-03-08
---

# Quick Task 1: Card Data Generation, Cargo Aliases, and README Summary

**Sparse-checkout card data pipeline, cargo alias task runner, and full project README with setup-to-running docs**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-08T20:43:28Z
- **Completed:** 2026-03-08T20:47:28Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments

- Card data generation script that sparse-checkouts Forge's cardsfolder and runs card_data_export
- Full first-run setup script orchestrating card data, WASM build, and frontend install
- Expanded cargo aliases: test-all, clippy-strict, export-cards, serve
- Comprehensive README with features, quick start, architecture, and development docs

## Task Commits

Each task was committed atomically:

1. **Task 1: Create card-data generation script and expand cargo aliases** - `29954e4` (feat)
2. **Task 2: Create README.md** - `df78623` (docs)

## Files Created/Modified

- `scripts/gen-card-data.sh` - Downloads Forge cards via sparse checkout, runs card_data_export, outputs card-data.json
- `scripts/setup.sh` - Orchestrates full first-run: card data + WASM + pnpm install
- `.cargo/config.toml` - Added test-all, clippy-strict, export-cards, serve aliases
- `.gitignore` - Added data/ directory for downloaded card files
- `README.md` - User-facing showcase and developer documentation

## Decisions Made

- Cargo aliases limited to cargo-native commands only (cargo subcommands). Shell scripts handle multi-step workflows involving git, file copies, etc.
- Used git sparse checkout for Forge repo download to avoid cloning the full 2GB+ repository.
- README structured with user-facing content first (features, quick start), developer docs below (architecture, build commands, cargo aliases).

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- Setup pipeline complete; new contributors can clone and run
- Card data generation ready for use once Forge repo is accessible

---
*Quick Task: 1-add-card-data-generation-cargo-script-al*
*Completed: 2026-03-08*
