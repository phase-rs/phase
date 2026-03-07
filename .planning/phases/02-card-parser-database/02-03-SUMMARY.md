---
phase: 02-card-parser-database
plan: 03
subsystem: database
tags: [rust, database, card-lookup, walkdir, hashmap]

requires:
  - phase: 02-card-parser-database
    provides: parse_card_file() for parsing .txt card definitions, CardRules/CardFace/CardLayout types
provides:
  - CardDatabase struct with load(), get_by_name(), get_face_by_name(), card_count(), errors()
  - Recursive directory loading of all .txt card files with lenient error handling
  - Case-insensitive O(1) lookup by card name and individual face name
affects: [03-game-state-engine, 04-ability-effect-system]

tech-stack:
  added: [walkdir]
  patterns: [face_index for individual face lookup across multi-face cards]

key-files:
  created:
    - crates/engine/src/database/mod.rs
    - crates/engine/src/database/card_db.rs
  modified:
    - crates/engine/Cargo.toml
    - crates/engine/src/lib.rs

key-decisions:
  - "Clone CardFace for face_index -- simpler than lifetime references, CardFace structs are small"
  - "filter_entry depth==0 bypass -- root directory not filtered by dotfile check (tempdir names can start with dot)"

patterns-established:
  - "layout_faces() helper extracts face references from any CardLayout variant"
  - "Lenient loading: parse errors collected as (PathBuf, String), don't prevent other cards from loading"

requirements-completed: [PARSE-03]

duration: 3min
completed: 2026-03-07
---

# Phase 2 Plan 3: Card Database Summary

**CardDatabase with walkdir recursive .txt loading, case-insensitive HashMap lookup by card name and individual face name, and lenient error collection**

## Performance

- **Duration:** 3 min
- **Started:** 2026-03-07T21:41:11Z
- **Completed:** 2026-03-07T21:44:16Z
- **Tasks:** 1
- **Files modified:** 4

## Accomplishments
- CardDatabase.load() recursively finds and parses all .txt files from a directory tree via walkdir
- Case-insensitive O(1) lookup by card name (get_by_name) and individual face name (get_face_by_name)
- Parse errors collected as warnings -- malformed files don't prevent other cards from loading
- 9 new tests, 91 total engine tests pass (+ 1 ignored real-data test)

## Task Commits

Each task was committed atomically:

1. **Task 1: CardDatabase (RED)** - `bc80f84` (test)
2. **Task 1: CardDatabase (GREEN)** - `f010b90` (feat)

## Files Created/Modified
- `crates/engine/src/database/card_db.rs` - CardDatabase struct with load, lookup, face indexing, and layout_faces helper
- `crates/engine/src/database/mod.rs` - Module declaration and re-export of CardDatabase
- `crates/engine/Cargo.toml` - Added walkdir and tempfile dependencies
- `crates/engine/src/lib.rs` - Added database module declaration

## Decisions Made
- Clone CardFace for face_index rather than using references -- simpler ownership, CardFace is small
- Root directory bypasses dotfile filter (depth==0 check) to handle temp dirs starting with dots

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed dotfile filter excluding root directory**
- **Found during:** Task 1 (GREEN phase)
- **Issue:** filter_entry was filtering the root directory when tempdir name started with '.' (e.g., `.tmpXXXXXX`)
- **Fix:** Added `e.depth() == 0` check to bypass dot-filter for root entry
- **Files modified:** crates/engine/src/database/card_db.rs
- **Verification:** All 9 tests pass including skips_dotfiles_and_dot_directories
- **Committed in:** f010b90 (Task 1 GREEN commit)

---

**Total deviations:** 1 auto-fixed (1 bug)
**Impact on plan:** Essential fix for correctness on macOS where tempfile creates dot-prefixed directories. No scope creep.

## Issues Encountered
None beyond the auto-fixed dotfile filter issue.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- CardDatabase ready for game engine to load card definitions at startup
- All 3 plans in Phase 2 (Card Parser & Database) are complete
- Phase 3 (Game State Engine) can begin

---
## Self-Check: PASSED

All created files verified on disk. All 2 commits verified in git log.

---
*Phase: 02-card-parser-database*
*Completed: 2026-03-07*
