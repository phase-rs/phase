# Project Retrospective

*A living document updated after each milestone. Lessons feed forward into future planning.*

## Milestone: v1.0 — MVP

**Shipped:** 2026-03-08
**Phases:** 12 | **Plans:** 40 | **Commits:** 207

### What Was Built
- Complete MTG rules engine in Rust: 202 effect handlers, 137 trigger modes, 45 replacement effects, seven-layer continuous effects
- Full combat system with first strike, double strike, trample, deathtouch, lifelink, flying/reach, menace
- Card parser for Forge's 32,300+ card definitions with all multi-face types
- React game UI: battlefield, hand, stack, targeting, mana payment, animations, deck builder
- AI opponent with alpha-beta search, board evaluation, 5 difficulty levels
- WebSocket multiplayer server with hidden information, reconnection, server-side deck resolution
- Tauri desktop app + PWA/WASM browser build from same codebase

### What Worked
- Bottom-up phase ordering (types → parser → engine → abilities → triggers → layers → UI → AI) meant each phase built cleanly on the previous
- fn pointer registries built per apply() call — cheap, avoids global state, no trait object complexity
- Pure `apply(state, action) -> ActionResult` reducer pattern kept the engine testable and predictable
- EngineAdapter abstraction made adding WASM, Tauri, and WebSocket transports trivial
- Milestone audit caught 4 integration gaps (phases 9-12) that would have been missed without systematic verification
- Discriminated unions across WASM boundary (serde tag/content + tsify) gave type-safe TypeScript without manual mapping

### What Was Inefficient
- Phase 7 (UI) had 9 plans — too many for one phase, could have been split into 2 phases for cleaner scope
- Some decisions accumulated in STATE.md that belonged directly in code comments or CLAUDE.md
- Nyquist validation was only fully compliant for 1 of 12 phases — validation overhead wasn't justified for rapid development

### Patterns Established
- Constants in `constants/game.ts` (logic) and `constants/ui.ts` (presentation) — zero duplication
- Build-time pre-computation pattern for data requiring filesystem (coverage-data.json via CLI binary)
- ChaCha20Rng for cross-platform deterministic RNG (WASM + native produce identical sequences)
- HashMap<ObjectId, GameObject> central store with zones as Vec<ObjectId>
- Combat click delegation via handler function on uiStore (not inline event handlers)

### Key Lessons
1. Gap closure phases (9-12) are best identified through systematic audit, not intuition — the audit caught 4 real integration gaps
2. fn pointer registries are the sweet spot for handler dispatch in Rust — simpler than traits, cheaper than dyn, buildable per-call
3. WASM binary size was a non-issue (19 KB) — premature optimization of wasm-opt was unnecessary
4. Forge's card format is remarkably stable and well-structured — lenient parsing (skip unknowns) matches upstream behavior perfectly

### Cost Observations
- Timeline: 2 days from project init to milestone complete
- 40 plans averaging ~4 min each
- Notable: Phase ordering eliminated rework — zero architectural changes needed after initial design

---

## Milestone: v1.1 — Arena UI

**Shipped:** 2026-03-10
**Phases:** 8 | **Plans:** 43 | **Commits:** 305

### What Was Built
- MTGA-quality game board with responsive layout, hand fan, player HUD, zone viewers, and game log
- Canvas particle VFX engine (9+ presets) with step-based animation queue, screen shake, floating damage numbers, death shatter
- AI auto-play game loop with auto-pass heuristics, phase stop preferences, and opponent controller abstraction
- Web Audio API audio system: 39 SFX mapped to game events, WUBRG-themed background music, iOS AudioContext warm-up
- Stack visualization, smart mana auto-pay with hybrid/phyrexian/X cost UI, combat assignment, and priority controls
- 20+ new engine mechanics: combat evasion (Fear/Intimidate/Skulk/Horsemanship), Ward, Protection, Wither/Infect, Prowess
- MTGA-faithful art-crop cards, golden targeting arcs, cinematic turn banners, mode-first menu with deck gallery
- Complete engine: mana abilities (Rule 605), planeswalkers, DFC transform, day/night, morph/manifest — 100% Standard coverage

### What Worked
- Phase ordering (foundation → animation → loop → audio → MTG UI → mechanics → visual fidelity → engine completeness) built cleanly on previous work
- EngineAdapter abstraction held perfectly — Arena UI wired to WASM/Tauri/WebSocket without any adapter changes
- Parallel execution of phases 15+16 (independent subsystems) saved time
- BackFaceData pattern discovered in DFC transform was reused directly for morph/manifest face-down mechanics
- Art-crop + normal dual image strategy gave MTGA fidelity on battlefield while preserving full card images in hand/stack
- Step-based animation with event normalizer grouping made complex multi-event turns play smoothly

### What Was Inefficient
- Phases 18-20 were added mid-milestone (scope expansion from 5 to 8 phases) — should have been planned upfront or deferred to v1.2
- Summary one-liner extraction failed (null results) — SUMMARY.md files may lack the expected frontmatter field
- Phase 19 traceability entries marked "Planned" instead of "Complete" — traceability updates lagged behind actual completion

### Patterns Established
- AudioManager as plain TypeScript singleton (no React coupling), with module-level store subscription for volume updates
- Module-level boolean mutex for dispatch pipeline (avoids useRef re-render cascades)
- Art crop aspect ratio 0.75, token detection via card_id === 0
- Skip-confirm guard pattern with armed timer for destructive actions (No Attacks / No Blocks)
- RAF polling with 10-frame stabilization to prevent infinite animation loops
- Derived display fields via skip_deserializing + WASM-side computation

### Key Lessons
1. Scope expansion (3 phases added mid-milestone) worked because the new phases had clear dependency chains and didn't disrupt already-completed work
2. Dual Scryfall image strategy (art_crop + normal) is essential — a single size can't serve both battlefield compactness and hand/preview detail
3. Standard card curation by name (not set code) is pragmatic when upstream data lacks set metadata — CI gate prevents coverage regressions
4. Animation pipeline design (event normalizer → grouped steps → sequential playback) is the right abstraction for turn-based card games

### Cost Observations
- Timeline: 2 days from v1.0 ship to v1.1 complete
- 43 plans averaging ~3 min each
- Notable: Phases 18-20 (engine mechanics) took longer per plan (5-10 min) due to Rust type complexity

---

## Cross-Milestone Trends

### Process Evolution

| Milestone | Commits | Phases | Key Change |
|-----------|---------|--------|------------|
| v1.0 | 207 | 12 | Bottom-up build, audit-driven gap closure |
| v1.1 | 305 | 8 | Arena UI port, mid-milestone scope expansion (5→8 phases), parallel phase execution |

### Cumulative Quality

| Milestone | LOC | Requirements | Coverage Threshold |
|-----------|-----|-------------|-------------------|
| v1.0 | ~29,700 | 63/63 | 10% (baseline enforcement) |
| v1.1 | ~51,500 | 87/87 | 100% Standard-legal (CI gate) |

### Top Lessons (Verified Across Milestones)

1. Systematic milestone audits catch integration gaps that are invisible during phase-level development
2. Pure reducer architecture scales well — from simple card games to complex MTG rules with 202 effect types
3. EngineAdapter abstraction proves its value repeatedly — v1.1's complete UI replacement required zero adapter changes
4. Mid-milestone scope expansion works when new phases have clear dependency chains and don't disrupt completed work
