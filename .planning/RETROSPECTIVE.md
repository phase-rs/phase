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

## Cross-Milestone Trends

### Process Evolution

| Milestone | Commits | Phases | Key Change |
|-----------|---------|--------|------------|
| v1.0 | 207 | 12 | Bottom-up build, audit-driven gap closure |

### Cumulative Quality

| Milestone | LOC | Requirements | Coverage Threshold |
|-----------|-----|-------------|-------------------|
| v1.0 | ~29,700 | 63/63 | 10% (baseline enforcement) |

### Top Lessons (Verified Across Milestones)

1. Systematic milestone audits catch integration gaps that are invisible during phase-level development
2. Pure reducer architecture scales well — from simple card games to complex MTG rules with 202 effect types
