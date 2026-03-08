# Phase 11: Tech Debt Cleanup: Constants, Combat UI, WASM Tests, CI Coverage - Research

**Researched:** 2026-03-08
**Domain:** Frontend consolidation, UI components, test infrastructure, CI pipeline
**Confidence:** HIGH

## Summary

Phase 11 is a tech debt cleanup phase with five distinct work areas: (1) constants consolidation removing three duplicate `UNDOABLE_ACTIONS`/`MAX_UNDO_HISTORY` definitions, (2) combat UI overlay for attacker/blocker declaration matching Arena-style interaction, (3) card-data.json missing file modal, (4) documentation fixes in ROADMAP.md and REQUIREMENTS.md, and (5) test coverage expansion with CI enforcement.

The codebase is well-structured for all of these changes. The constants duplication is confirmed across exactly three files. The combat UI can leverage existing patterns from the targeting overlay (`TargetingOverlay.tsx`) which already implements click-to-select, arrow drawing, and confirm/cancel flow. The engine already sends `WaitingFor::DeclareAttackers` and `WaitingFor::DeclareBlockers` with valid IDs. Test infrastructure uses Vitest 3.x with @testing-library/react 16 and jsdom -- currently 5 test files with 50 tests. CI uses GitHub Actions with separate Rust and Frontend jobs.

**Primary recommendation:** Structure as 5 plans: (1) constants consolidation + magic number audit, (2) combat overlay UI, (3) card-data.json modal, (4) doc fixes, (5) test coverage + CI thresholds.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Constants canonical source: `client/src/constants/game.ts` for game logic, `constants/storage.ts` for storage keys
- Delete duplicate `UNDOABLE_ACTIONS` and `MAX_UNDO_HISTORY` from `gameStore.ts` and `useKeyboardShortcuts.ts`; import from `constants/game.ts`
- Organize constants by domain (game.ts, storage.ts, ui.ts if needed), not by type
- Rust crates not in scope for constants
- Combat UI: Arena-style click-to-toggle attacker selection with tilt (~15 deg) + red/orange border glow
- Three combat buttons: "Attack All", "Skip", "Confirm Attackers"
- Blocker assignment: click blocker then click attacker, line/arrow between them
- "Confirm Blockers" button for block submission
- card-data.json missing: blocking modal dialog with explanation + "Continue anyway" escape hatch
- Fix ROADMAP.md Phase 8 status, fix REQUIREMENTS.md PARSE-04 and ABIL-01 traceability
- WASM integration tests: initialize_game, submit_action, get_game_state, restore_game_state through JS bindings
- Frontend component tests: combat overlay + critical user paths (game init, undo, deck loading)
- Coverage reporting: cargo-tarpaulin for Rust, vitest --coverage for TypeScript
- Aspirational coverage thresholds that fail the build

### Claude's Discretion
- Exact coverage threshold percentages
- Which existing components get additional test coverage beyond combat overlay and critical paths
- Loading skeleton or transition design for combat overlay UI
- Specific magic numbers found during constants audit -- how to name and organize them
- Documentation fix wording

### Deferred Ideas (OUT OF SCOPE)
None
</user_constraints>

## Standard Stack

### Core (already in project)
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| Vitest | ^3.0.0 | Test runner | Already configured with jsdom, 50 tests passing |
| @testing-library/react | ^16.3.0 | Component testing | Already installed, idiomatic React testing |
| @testing-library/jest-dom | ^6.6.3 | DOM matchers | Already installed for assertion extensions |
| Framer Motion | ^12.35.1 | Combat animations | Already used for all animations in the project |
| Zustand | ^5.0.11 | State management | Combat selection state extends existing uiStore |

### Supporting (to add)
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| @vitest/coverage-v8 | ^3.0.0 | TS coverage | Vitest's built-in V8 coverage provider |
| cargo-tarpaulin | latest | Rust coverage | Standard Rust coverage tool for CI |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| @vitest/coverage-v8 | @vitest/coverage-istanbul | V8 is faster, no instrumentation overhead, default for Vitest |
| cargo-tarpaulin | llvm-cov | Tarpaulin is simpler CI setup, llvm-cov needs nightly |

**Installation:**
```bash
cd client && pnpm add -D @vitest/coverage-v8
# cargo-tarpaulin installed via cargo install in CI
```

## Architecture Patterns

### Constants Organization
```
client/src/constants/
├── game.ts          # Game logic constants (UNDOABLE_ACTIONS, MAX_UNDO_HISTORY, AI delays, animation timing)
├── storage.ts       # Storage keys (STORAGE_KEY_PREFIX, ACTIVE_DECK_KEY, WS_STORAGE_KEY)
└── ui.ts            # UI constants (if needed for combat glow colors, tilt angles)
```

### Combat UI Component Structure
```
client/src/components/combat/
├── CombatOverlay.tsx       # Main overlay - conditionally rendered on DeclareAttackers/DeclareBlockers
├── AttackerControls.tsx     # "Attack All" / "Skip" / "Confirm Attackers" buttons
├── BlockerControls.tsx      # "Confirm Blockers" button
└── BlockerArrow.tsx         # SVG arrow connecting blocker to attacker (reuse TargetArrow pattern)
```

### Pattern 1: Combat State in uiStore
**What:** Extend `uiStore` with combat selection state, mirroring the targeting pattern.
**When to use:** Combat overlay needs to track selected attackers/blockers without modifying gameStore.
**Example:**
```typescript
// Extends existing uiStore pattern
interface UiStoreState {
  // ... existing fields ...
  combatMode: 'attackers' | 'blockers' | null;
  selectedAttackers: ObjectId[];
  blockerAssignments: Map<ObjectId, ObjectId>; // blocker -> attacker
}
```

### Pattern 2: WaitingFor-Driven Overlay Rendering
**What:** Combat overlay renders based on `waitingFor.type`, same as TargetingOverlay and ManaPaymentUI.
**When to use:** GamePage already conditionally renders overlays based on WaitingFor discriminated union.
**Example:**
```typescript
// In GamePage.tsx - follows established pattern from lines 334-336
{waitingFor?.type === "DeclareAttackers" && <CombatOverlay mode="attackers" />}
{waitingFor?.type === "DeclareBlockers" && <CombatOverlay mode="blockers" />}
```

### Pattern 3: PermanentCard Visual Modifiers
**What:** Add combat-specific visual states (tilt, glow) to PermanentCard using existing glow pattern.
**When to use:** PermanentCard already handles targeting glow via uiStore state.
**Example:**
```typescript
// Extends existing glow logic in PermanentCard.tsx (lines 50-57)
const isAttacking = combatMode === 'attackers' && selectedAttackers.includes(objectId);
if (isAttacking) {
  glowClass = "ring-2 ring-orange-500 shadow-[0_0_12px_3px_rgba(249,115,22,0.7)]";
}
// Tilt via CSS transform
const tiltTransform = isAttacking ? "rotate(15deg)" : undefined;
```

### Anti-Patterns to Avoid
- **Separate combat store:** Don't create a new Zustand store for combat -- extend uiStore (single source of UI state)
- **Inline constants in combat overlay:** All numeric values (tilt degrees, glow colors) should go to constants
- **Custom arrow implementation:** Reuse or extend the existing `TargetArrow` component from targeting

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Coverage reporting | Custom coverage scripts | @vitest/coverage-v8 + cargo-tarpaulin | Industry standard, CI-friendly output |
| SVG arrows | New arrow component from scratch | Extend existing `TargetArrow.tsx` | Already handles DOM position lookup via data-object-id |
| Modal dialogs | New modal component | Extend existing `ChoiceModal.tsx` pattern | Established overlay + z-index + backdrop pattern |
| Test setup for stores | Manual store mocking | Vitest's existing mock patterns from `gameStore.test.ts` | Already has adapter mocking, state setup patterns |

## Common Pitfalls

### Pitfall 1: Constants Import Cycles
**What goes wrong:** Moving constants to `constants/game.ts` but creating circular imports if stores import from constants that import from types.
**Why it happens:** TypeScript allows circular imports but they can cause undefined-at-import-time errors.
**How to avoid:** Constants file should only depend on primitive types (string, number, Set), never import from stores or adapters.
**Warning signs:** `undefined` values at runtime, order-dependent initialization.

### Pitfall 2: Combat Overlay Z-Index Conflicts
**What goes wrong:** Combat overlay appears above or below wrong elements (modals, card preview, animation overlay).
**Why it happens:** Existing overlays use z-40 (targeting), z-50 (modals). Combat overlay needs to be below modals but above board.
**How to avoid:** Use z-30 for combat overlay. Review GamePage.tsx overlay stacking order.
**Warning signs:** Clicking through overlays, invisible elements.

### Pitfall 3: Coverage Threshold Ratcheting
**What goes wrong:** Setting thresholds too high causes constant CI failures; too low is meaningless.
**Why it happens:** Current coverage is unknown; aspirational targets need to be based on current + delta.
**How to avoid:** Run coverage locally first, note current %, set threshold at current + 5-10%.
**Warning signs:** CI failures on first merge after adding thresholds.

### Pitfall 4: WASM Test Environment
**What goes wrong:** WASM integration tests need actual WASM bindings but Vitest runs in jsdom.
**Why it happens:** The existing wasm-adapter test mocks all WASM calls -- true integration tests need real bindings.
**How to avoid:** WASM "integration" tests should still mock at the WASM module boundary (matching existing pattern). True end-to-end WASM testing requires wasm-pack test which is a separate concern. The CONTEXT says "through JS bindings" which means testing the adapter layer with mocked WASM, covering initialize_game/submit_action/get_game_state/restore_game_state flows.
**Warning signs:** Tests trying to load actual .wasm files in jsdom.

### Pitfall 5: Framer Motion Layout Conflicts with Tilt
**What goes wrong:** Adding `rotate(15deg)` transform to a `motion.div` with `layoutId` causes layout animation glitches.
**Why it happens:** Framer Motion's layout system and manual transforms can conflict.
**How to avoid:** Apply tilt via Framer Motion's `animate` prop (`animate={{ rotate: isAttacking ? 15 : 0 }}`) rather than inline CSS `transform`.
**Warning signs:** Cards jumping or flickering during combat phase transitions.

## Code Examples

### Constants Consolidation
```typescript
// client/src/constants/game.ts - expand existing file
/** Action types that don't reveal hidden information and are safe to undo. */
export const UNDOABLE_ACTIONS = new Set([
  "PassPriority",
  "DeclareAttackers",
  "DeclareBlockers",
  "ActivateAbility",
]);

/** Maximum number of undo history entries. */
export const MAX_UNDO_HISTORY = 5;

/** AI opponent player ID (always player 1 in WASM mode). */
export const AI_PLAYER_ID = 1;

/** Delay in ms before AI takes action (base + random variance). */
export const AI_BASE_DELAY_MS = 800;
export const AI_DELAY_VARIANCE_MS = 400;
```

### Combat Overlay Dispatch
```typescript
// Dispatching attacker declaration - follows existing adapter/types.ts GameAction
const handleConfirmAttackers = () => {
  dispatch({
    type: "DeclareAttackers",
    data: { attacker_ids: selectedAttackers },
  });
  clearCombatSelection();
};

// Dispatching blocker assignments
const handleConfirmBlockers = () => {
  const assignments: [ObjectId, ObjectId][] = Array.from(
    blockerAssignments.entries()
  );
  dispatch({
    type: "DeclareBlockers",
    data: { assignments },
  });
  clearCombatSelection();
};
```

### card-data.json Missing Modal
```typescript
// Blocking modal pattern from existing ChoiceModal
function CardDataMissingModal({ onContinue }: { onContinue: () => void }) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/70" />
      <div className="relative z-10 max-w-md rounded-xl bg-gray-900 p-8 text-center shadow-2xl ring-1 ring-gray-700">
        <h2 className="mb-4 text-xl font-bold text-white">Card Data Missing</h2>
        <p className="mb-2 text-sm text-gray-300">
          card-data.json was not found. Generate it with:
        </p>
        <code className="mb-4 block rounded bg-gray-800 px-3 py-2 text-sm text-emerald-400">
          cargo run --bin card_data_export
        </code>
        <button onClick={onContinue} className="mt-4 text-xs text-gray-500 underline hover:text-gray-400">
          Continue anyway
        </button>
      </div>
    </div>
  );
}
```

### CI Coverage Configuration
```yaml
# In .github/workflows/ci.yml - Rust coverage step
- name: Install cargo-tarpaulin
  run: cargo install cargo-tarpaulin

- name: Run Rust coverage
  run: cargo tarpaulin --all --out xml --skip-clean

# Frontend coverage step
- name: Run tests with coverage
  run: cd client && pnpm test -- --run --coverage
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| vitest/coverage-istanbul | @vitest/coverage-v8 | Vitest 1.0+ | V8 is default, faster, no babel |
| cargo-tarpaulin XML | cargo-tarpaulin lcov | Recent | lcov integrates better with GitHub Actions |

## Open Questions

1. **Exact current coverage percentages**
   - What we know: 24 Rust tests, 50 TS tests, all passing
   - What's unclear: What percentage of lines/branches these cover
   - Recommendation: Run coverage locally in first task of coverage plan, then set thresholds at current + 5-10%

2. **WASM integration test scope**
   - What we know: Context says "through JS bindings" -- existing wasm-adapter.test.ts mocks all WASM calls
   - What's unclear: Whether "integration" means real WASM or mocked WASM
   - Recommendation: Expand mocked adapter tests to cover restore_game_state flow and multi-action sequences. True WASM integration (wasm-pack test) is a separate CI concern and not explicitly requested.

3. **Magic number audit scope**
   - What we know: Found candidates: `AI_BASE_DELAY = 800`, `AI_DELAY_VARIANCE = 400`, `SCRYFALL_DELAY_MS = 75`, `DEBOUNCE_MS = 300`, `DEFAULT_DURATION = 200`, `aiPlayer = 1`
   - What's unclear: Which of these warrant consolidation vs being fine where they are
   - Recommendation: Consolidate AI constants (delay, player ID) and animation defaults. Leave `DEBOUNCE_MS` and `SCRYFALL_DELAY_MS` in their respective files (single-use, domain-specific).

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework (TS) | Vitest 3.x + @testing-library/react 16 + jsdom |
| Framework (Rust) | cargo test (built-in) |
| Config file (TS) | `client/vitest.config.ts` |
| Quick run command (TS) | `cd client && pnpm test -- --run` |
| Quick run command (Rust) | `cargo test --all` |
| Full suite command | `cargo test --all && cd client && pnpm test -- --run` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| TD-01 | Constants consolidation (no duplicates) | unit | `cd client && pnpm test -- --run src/stores/__tests__/gameStore.test.ts` | Existing (update imports) |
| TD-02 | Combat attacker selection UI | component | `cd client && pnpm test -- --run src/components/combat/__tests__/CombatOverlay.test.tsx` | Wave 0 |
| TD-03 | Combat blocker assignment UI | component | `cd client && pnpm test -- --run src/components/combat/__tests__/CombatOverlay.test.tsx` | Wave 0 |
| TD-04 | card-data.json missing modal | component | `cd client && pnpm test -- --run src/components/modal/__tests__/CardDataMissingModal.test.tsx` | Wave 0 |
| TD-05 | WASM adapter restore_game_state | unit | `cd client && pnpm test -- --run src/adapter/__tests__/wasm-adapter.test.ts` | Existing (extend) |
| TD-06 | Coverage thresholds pass | CI | Full CI pipeline | Wave 0 (CI config) |

### Sampling Rate
- **Per task commit:** `cd client && pnpm test -- --run`
- **Per wave merge:** `cargo test --all && cd client && pnpm test -- --run`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `client/src/components/combat/__tests__/CombatOverlay.test.tsx` -- combat overlay tests
- [ ] `client/src/components/modal/__tests__/CardDataMissingModal.test.tsx` -- missing card data modal tests
- [ ] `@vitest/coverage-v8` dev dependency -- install for coverage reporting
- [ ] `client/vitest.config.ts` -- add coverage configuration block

## Sources

### Primary (HIGH confidence)
- Codebase analysis: direct inspection of all relevant files
- `client/src/constants/game.ts` and `constants/storage.ts` -- existing constant patterns
- `client/src/components/targeting/TargetingOverlay.tsx` -- reference for overlay/arrow pattern
- `client/src/adapter/types.ts` -- GameAction types for DeclareAttackers/DeclareBlockers
- `client/vitest.config.ts` -- current test configuration
- `.github/workflows/ci.yml` -- current CI pipeline

### Secondary (MEDIUM confidence)
- Vitest coverage-v8 is the default/recommended provider (verified by Vitest docs convention)
- cargo-tarpaulin is standard Rust coverage tool for CI

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - all libraries already in project, only adding coverage provider
- Architecture: HIGH - extending well-established patterns (targeting overlay, uiStore, constants)
- Pitfalls: HIGH - identified from direct codebase analysis, known Framer Motion behaviors

**Research date:** 2026-03-08
**Valid until:** 2026-04-07 (stable codebase, tech debt cleanup)
