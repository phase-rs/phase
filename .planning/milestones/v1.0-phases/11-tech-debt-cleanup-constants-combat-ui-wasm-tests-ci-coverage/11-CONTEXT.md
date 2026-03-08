# Phase 11: Tech Debt Cleanup: Constants, Combat UI, WASM Tests, CI Coverage - Context

**Gathered:** 2026-03-08
**Status:** Ready for planning

<domain>
## Phase Boundary

Clean up accumulated tech debt from v1.0 milestone phases 1-10. All 57 requirements are satisfied — this phase improves code quality, adds missing UI for combat, consolidates duplicated constants, fixes documentation inconsistencies, and establishes test coverage enforcement in CI. No new gameplay features.

</domain>

<decisions>
## Implementation Decisions

### Constants consolidation
- Canonical source: `client/src/constants/game.ts` for game logic constants, `constants/storage.ts` for storage keys
- Delete duplicate `UNDOABLE_ACTIONS` and `MAX_UNDO_HISTORY` from `gameStore.ts` and `useKeyboardShortcuts.ts`; import from `constants/game.ts`
- Audit all TypeScript client code (`client/src/`) for other duplicated constants or magic numbers; consolidate into domain-organized constant files
- Rust crates not in scope — they already use named constants internally
- Organize constants by domain (game.ts, storage.ts, ui.ts if needed), not by type

### Combat UI overlay
- Attacker selection: click-to-toggle on creatures, Arena-style
- Attacking creatures get tilt forward (~15°) + red/orange border glow visual indicator
- Three action buttons: "Attack All" (selects all valid attackers), "Skip" (no attackers), "Confirm Attackers"
- Blocker assignment: click blocker creature → click attacker it blocks, with line/arrow drawn between them
- "Confirm Blockers" button to submit blocking assignments
- Overall interaction pattern mimics MTG Arena combat flow

### Missing warnings
- card-data.json missing: show blocking modal dialog explaining the issue and how to generate it (`cargo run --bin card_data_export`)
- Include small "Continue anyway" escape hatch link for developers/testing
- Game should not silently start with empty libraries

### Documentation fixes
- Fix ROADMAP.md Phase 8 status from "In Progress" to "Complete"
- Update REQUIREMENTS.md traceability: PARSE-04 and ABIL-01 from "Pending" to "Complete"

### Tests & CI coverage
- Add WASM integration tests: verify initialize_game, submit_action, get_game_state, restore_game_state through JS bindings
- Add frontend component tests: prioritize new combat overlay + critical user paths (game initialization, undo, deck loading)
- Add coverage reporting to CI: cargo-tarpaulin for Rust, vitest --coverage for TypeScript
- Enforce aspirational coverage thresholds (higher than current levels) that fail the build if not met
- Write tests in this phase to meet the aspirational thresholds

### Claude's Discretion
- Exact coverage threshold percentages (based on current levels + aspirational target)
- Which existing components get additional test coverage beyond combat overlay and critical paths
- Loading skeleton or transition design for combat overlay UI
- Specific magic numbers found during constants audit — how to name and organize them
- Documentation fix wording

</decisions>

<specifics>
## Specific Ideas

- "Mimic MTGA logic" for combat UI — Arena is the reference point for attacker/blocker interaction
- Blocking modal for missing card-data.json because "they can't do anything with an empty card library"
- Aspirational coverage targets, not just ratcheting current levels

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `constants/game.ts`: Already has UNDOABLE_ACTIONS and MAX_UNDO_HISTORY — canonical source for consolidation
- `constants/storage.ts`: Established pattern for storage key constants (STORAGE_KEY_PREFIX, ACTIVE_DECK_KEY)
- Targeting glow system: `uiStore` has `validTargetIds`/`sourceObjectId` for glow state — reusable for combat selection highlighting
- `PermanentCard` component: Has `data-object-id` attribute for DOM position lookups — blocker-to-attacker lines can use this
- Framer Motion: Already used for animations — combat tilt/glow animations can use existing motion infrastructure
- `PhaseTracker.tsx`: Already has DeclareAttackers/DeclareBlockers phase entries

### Established Patterns
- Click-based interaction: Targeting system uses click-to-select, similar pattern extends to combat
- Modal pattern: Game already has choice modals in `components/modal/` — reusable for card-data.json warning
- Store-driven UI state: `uiStore` manages UI state separate from game state — combat selection state fits here

### Integration Points
- `WaitingFor::DeclareAttackers` / `WaitingFor::DeclareBlockers` — engine already sends valid attacker/blocker IDs
- `GamePage.tsx:76` — card-data.json fetch location where blocking modal should be triggered
- `.github/workflows/ci.yml` — CI pipeline where coverage reporting and thresholds need to be added

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 11-tech-debt-cleanup-constants-combat-ui-wasm-tests-ci-coverage*
*Context gathered: 2026-03-08*
