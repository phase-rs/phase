# AI Opponent Bracket Filter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Commander-only, multi-select bracket filter (1 Exhibition → 5 cEDH) to the AI opponent picker so the user can constrain the random AI deck pool to specific WotC bracket tiers, with manual deck tagging in the Deck Builder.

**Architecture:** Frontend-only. Bracket is pre-game metadata: a shared TS type, a precon overlay table, an optional sidecar field on persisted saved-deck JSON, an additional `bracket` field on `AiDeckCandidate`, a new `aiBracketFilter` preference, and two chip-row React components (one in the AI opponent config, one in the deck builder). Engine, AI, and WASM crates are untouched. The filter is asymmetric — it never restricts the human player's deck choice or any other user action.

**Tech Stack:** TypeScript, React, Zustand (persist middleware), Vitest, Testing Library, Tailwind CSS v4.

**Spec:** [docs/superpowers/specs/2026-05-12-ai-opponent-bracket-filter-design.md](../specs/2026-05-12-ai-opponent-bracket-filter-design.md)

---

## File Structure

### New files

| Path | Responsibility |
|---|---|
| `client/src/types/bracket.ts` | Shared `CommanderBracket` literal type + `COMMANDER_BRACKETS` array + `BRACKET_LABEL` map |
| `client/src/data/preconBrackets.ts` | Hand-curated `Record<string, CommanderBracket>` overlay keyed by precon deckId, plus a lookup helper. Avoids touching the Rust precon JSON generator. |
| `client/src/components/menu/BracketFilter.tsx` | Multi-select chip row rendered in the AI opponent config |
| `client/src/components/deck-builder/BracketPicker.tsx` | Single-select chip row used in the Deck Builder for tagging a deck |
| `client/src/types/__tests__/bracket.test.ts` | Unit tests for the bracket constants |
| `client/src/data/__tests__/preconBrackets.test.ts` | Unit tests for the precon overlay lookup |
| `client/src/components/menu/__tests__/BracketFilter.test.tsx` | Render + interaction tests for the chip row |
| `client/src/components/deck-builder/__tests__/BracketPicker.test.tsx` | Render + interaction tests for the picker |
| `client/src/components/menu/__tests__/AiOpponentConfig.test.tsx` | Integration test: visibility gating + filter behavior |

### Modified files

| Path | What changes |
|---|---|
| `client/src/constants/storage.ts` | Add `loadSavedDeckBracket(name)` and `saveSavedDeckBracket(name, bracket \| null)` helpers; do NOT change the existing `loadSavedDeck` signature (bracket is a sidecar field on the persisted JSON, kept off the engine-bound `ParsedDeck`) |
| `client/src/services/aiDeckCatalog.ts` | Add `bracket: CommanderBracket \| null` to `AiDeckCandidate`; thread the field through `legalCandidate` and `buildLegalAiDeckCatalog` |
| `client/src/services/deckCatalog.ts` | Add `bracket: CommanderBracket \| null` to `DeckCatalogCandidate`; populate it from `preconBrackets` overlay for precon candidates and from `loadSavedDeckBracket` for saved candidates |
| `client/src/stores/preferencesStore.ts` | Add `aiBracketFilter: CommanderBracket[]` (default `[]`), `setAiBracketFilter` setter, default in `buildDefaultPreferences`, persist `version: 7`, v6→v7 migration that defaults the new field to `[]` |
| `client/src/components/menu/AiOpponentConfig.tsx` | Render `<BracketFilter>` below the Archetype/Coverage block but only when `selectedFormat === "Commander"`; extend the `filteredDecks` `useMemo` to apply the bracket filter when active |
| `client/src/components/deck-builder/DeckBuilder.tsx` | Hold a `bracket` state; render `<BracketPicker>` in the header when `format === "Commander"`; persist `bracket` alongside `format` in `handleSave`; restore `bracket` in `handleLoad` |
| `client/src/stores/__tests__/preferencesStore.test.ts` | Add tests for default, setter, migration |
| `client/src/services/__tests__/aiDeckCatalog.test.ts` | Add tests for bracket field on candidates |

---

## Task 1: `CommanderBracket` shared type

**Files:**
- Create: `client/src/types/bracket.ts`
- Create: `client/src/types/__tests__/bracket.test.ts`

- [ ] **Step 1: Write the failing test**

Create `client/src/types/__tests__/bracket.test.ts`:

```ts
import { describe, expect, it } from "vitest";

import { BRACKET_LABEL, COMMANDER_BRACKETS, type CommanderBracket } from "../bracket";

describe("CommanderBracket constants", () => {
  it("COMMANDER_BRACKETS lists 1..5 in order", () => {
    expect(COMMANDER_BRACKETS).toEqual([1, 2, 3, 4, 5]);
  });

  it("BRACKET_LABEL covers every bracket", () => {
    for (const b of COMMANDER_BRACKETS) {
      expect(BRACKET_LABEL[b]).toEqual(expect.stringMatching(/.+/));
    }
  });

  it("BRACKET_LABEL uses the WotC names", () => {
    const expected: Record<CommanderBracket, string> = {
      1: "Exhibition",
      2: "Core",
      3: "Upgraded",
      4: "Optimized",
      5: "cEDH",
    };
    expect(BRACKET_LABEL).toEqual(expected);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd client && pnpm test -- --run src/types/__tests__/bracket.test.ts`
Expected: FAIL — module `../bracket` not found.

- [ ] **Step 3: Implement the bracket module**

Create `client/src/types/bracket.ts`:

```ts
/**
 * WotC Commander bracket tiers (1 Exhibition → 5 cEDH). Used only as
 * pre-game metadata for filtering the AI random deck pool and for an
 * optional descriptive tag on user-saved Commander decks. The value
 * never reaches the Rust engine.
 */
export type CommanderBracket = 1 | 2 | 3 | 4 | 5;

export const COMMANDER_BRACKETS: readonly CommanderBracket[] = [1, 2, 3, 4, 5] as const;

export const BRACKET_LABEL: Record<CommanderBracket, string> = {
  1: "Exhibition",
  2: "Core",
  3: "Upgraded",
  4: "Optimized",
  5: "cEDH",
};

/** Type guard for arbitrary persisted/external values. */
export function isCommanderBracket(value: unknown): value is CommanderBracket {
  return value === 1 || value === 2 || value === 3 || value === 4 || value === 5;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd client && pnpm test -- --run src/types/__tests__/bracket.test.ts`
Expected: PASS — 3 tests.

- [ ] **Step 5: Commit**

```bash
git add client/src/types/bracket.ts client/src/types/__tests__/bracket.test.ts
git commit -m "Add CommanderBracket shared type and labels"
```

---

## Task 2: Precon bracket overlay

**Files:**
- Create: `client/src/data/preconBrackets.ts`
- Create: `client/src/data/__tests__/preconBrackets.test.ts`

- [ ] **Step 1: Write the failing test**

Create `client/src/data/__tests__/preconBrackets.test.ts`:

```ts
import { describe, expect, it } from "vitest";

import { getPreconBracket, PRECON_BRACKETS } from "../preconBrackets";
import { isCommanderBracket } from "../../types/bracket";

describe("preconBrackets", () => {
  it("every overlay entry is a valid CommanderBracket", () => {
    for (const [deckId, bracket] of Object.entries(PRECON_BRACKETS)) {
      expect(deckId).toEqual(expect.stringMatching(/.+/));
      expect(isCommanderBracket(bracket)).toBe(true);
    }
  });

  it("getPreconBracket returns the curated value for known deckIds", () => {
    const sampleId = Object.keys(PRECON_BRACKETS)[0];
    if (!sampleId) {
      // Overlay is empty until a curator adds entries; the lookup
      // contract still has to hold.
      expect(getPreconBracket("AdaptiveEnchantment_C18")).toBeNull();
      return;
    }
    expect(getPreconBracket(sampleId)).toBe(PRECON_BRACKETS[sampleId]);
  });

  it("getPreconBracket returns null for unknown deckIds", () => {
    expect(getPreconBracket("NotARealPrecon_XXX")).toBeNull();
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd client && pnpm test -- --run src/data/__tests__/preconBrackets.test.ts`
Expected: FAIL — module `../preconBrackets` not found.

- [ ] **Step 3: Implement the overlay module**

Create `client/src/data/preconBrackets.ts`:

```ts
import type { CommanderBracket } from "../types/bracket";

/**
 * Hand-curated bracket tier for bundled Commander precons, keyed by the
 * precon deckId (the MTGJSON filename stem used in `client/public/decks.json`,
 * e.g. `AdaptiveEnchantment_C18`).
 *
 * This overlay is the source of truth until the Rust precon-export pipeline
 * is taught to emit `bracket` directly. Entries are intentionally additive —
 * a precon with no entry surfaces as `null` (Unrated), which matches how
 * untagged user decks behave in the AI random pool.
 *
 * **Curation policy:** assign conservatively. When in doubt, prefer the
 * lower tier — the filter is opt-in and overshooting bracket 4 or 5 will
 * mismatch the user's expectations more than undershooting bracket 2 or 3.
 */
export const PRECON_BRACKETS: Readonly<Record<string, CommanderBracket>> = {
  // Curators: add entries here as you tag bundled precons. Examples:
  // "AdaptiveEnchantment_C18": 2,
  // "ArcaneMaelstrom_C20": 2,
};

export function getPreconBracket(deckId: string): CommanderBracket | null {
  return PRECON_BRACKETS[deckId] ?? null;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd client && pnpm test -- --run src/data/__tests__/preconBrackets.test.ts`
Expected: PASS — 3 tests. The "known deckIds" test takes the empty-overlay branch initially; once a curator adds entries it exercises the real lookup.

- [ ] **Step 5: Commit**

```bash
git add client/src/data/preconBrackets.ts client/src/data/__tests__/preconBrackets.test.ts
git commit -m "Add precon bracket overlay table and lookup"
```

---

## Task 3: Saved-deck bracket sidecar storage

**Files:**
- Modify: `client/src/constants/storage.ts` (add helpers near the saved-deck section, ~line 110)

- [ ] **Step 1: Write the failing test**

Append to `client/src/stores/__tests__/preferencesStore.test.ts` is the wrong place — saved decks aren't a store concern. Instead, add a new test file `client/src/constants/__tests__/storage.test.ts`. Create it:

```ts
import { beforeEach, describe, expect, it } from "vitest";

import {
  loadSavedDeckBracket,
  saveSavedDeckBracket,
  STORAGE_KEY_PREFIX,
} from "../storage";

beforeEach(() => {
  localStorage.clear();
});

describe("saved-deck bracket sidecar", () => {
  it("returns null when the deck does not exist", () => {
    expect(loadSavedDeckBracket("Missing Deck")).toBeNull();
  });

  it("returns null when the persisted JSON has no bracket field", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Untagged",
      JSON.stringify({ main: [], sideboard: [], format: "Commander" }),
    );
    expect(loadSavedDeckBracket("Untagged")).toBeNull();
  });

  it("returns the bracket when persisted", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Tagged",
      JSON.stringify({ main: [], sideboard: [], format: "Commander", bracket: 3 }),
    );
    expect(loadSavedDeckBracket("Tagged")).toBe(3);
  });

  it("returns null when the persisted bracket is invalid (e.g. 0 or 'x')", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Bad",
      JSON.stringify({ main: [], sideboard: [], format: "Commander", bracket: 0 }),
    );
    expect(loadSavedDeckBracket("Bad")).toBeNull();
  });

  it("saveSavedDeckBracket merges the bracket into the existing persisted JSON", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Existing",
      JSON.stringify({ main: [{ count: 1, name: "Sol Ring" }], sideboard: [], format: "Commander" }),
    );
    saveSavedDeckBracket("Existing", 4);
    const raw = localStorage.getItem(STORAGE_KEY_PREFIX + "Existing")!;
    const parsed = JSON.parse(raw);
    expect(parsed.bracket).toBe(4);
    // Pre-existing fields must be preserved.
    expect(parsed.main).toEqual([{ count: 1, name: "Sol Ring" }]);
    expect(parsed.format).toBe("Commander");
  });

  it("saveSavedDeckBracket with null removes any existing bracket field", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Existing",
      JSON.stringify({ main: [], sideboard: [], format: "Commander", bracket: 4 }),
    );
    saveSavedDeckBracket("Existing", null);
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Existing")!);
    expect("bracket" in parsed).toBe(false);
  });

  it("saveSavedDeckBracket is a no-op when the deck does not exist", () => {
    saveSavedDeckBracket("Missing", 3);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Missing")).toBeNull();
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd client && pnpm test -- --run src/constants/__tests__/storage.test.ts`
Expected: FAIL — `loadSavedDeckBracket` and `saveSavedDeckBracket` are not exported.

- [ ] **Step 3: Add the helpers**

Open `client/src/constants/storage.ts`. After the existing `loadSavedDeck` function (currently ends near line 130), insert:

```ts
import { isCommanderBracket, type CommanderBracket } from "../types/bracket";

/**
 * Read the bracket sidecar field from a persisted saved-deck JSON. Bracket
 * is pre-game metadata stored alongside `format` — kept off the
 * engine-bound `ParsedDeck` so the engine boundary stays clean. Returns
 * `null` when the deck does not exist, has no bracket field, or carries
 * an invalid value.
 */
export function loadSavedDeckBracket(deckName: string): CommanderBracket | null {
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as { bracket?: unknown };
    return isCommanderBracket(parsed.bracket) ? parsed.bracket : null;
  } catch {
    return null;
  }
}

/**
 * Write the bracket sidecar field on a persisted saved-deck JSON. Passing
 * `null` removes the field. Acts as a no-op when the deck does not exist;
 * the deck builder is responsible for the initial save before tagging.
 */
export function saveSavedDeckBracket(deckName: string, bracket: CommanderBracket | null): void {
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return;
  try {
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    if (bracket === null) {
      delete parsed.bracket;
    } else {
      parsed.bracket = bracket;
    }
    localStorage.setItem(STORAGE_KEY_PREFIX + deckName, JSON.stringify(parsed));
  } catch {
    // Corrupt JSON: leave it alone. The deck builder will overwrite on save.
  }
}
```

If the file already imports from `../types/bracket` (it won't yet), reuse the existing import line; otherwise add the new `import` at the top of the file alongside the existing imports (around line 2).

- [ ] **Step 4: Run test to verify it passes**

Run: `cd client && pnpm test -- --run src/constants/__tests__/storage.test.ts`
Expected: PASS — 7 tests.

- [ ] **Step 5: Commit**

```bash
git add client/src/constants/storage.ts client/src/constants/__tests__/storage.test.ts
git commit -m "Add saved-deck bracket sidecar read/write helpers"
```

---

## Task 4: Bracket field on `AiDeckCandidate` + catalog wiring

**Files:**
- Modify: `client/src/services/aiDeckCatalog.ts:15-22` (add field to interface), `:65-81` (thread through builder)
- Modify: `client/src/services/deckCatalog.ts:29-38` (add field to interface), `:88-152` (populate per source)
- Modify: `client/src/services/__tests__/aiDeckCatalog.test.ts` (additive tests)

- [ ] **Step 1: Write the failing tests**

Open `client/src/services/__tests__/aiDeckCatalog.test.ts`. Locate the existing `describe("buildLegalAiDeckCatalog", ...)` block and append the following tests inside it:

```ts
it("surfaces null bracket on user-saved decks without a tag", async () => {
  saveDeck("Untagged Commander", deck("Sol Ring", "Atraxa, Praetors' Voice"));

  const catalog = await buildLegalAiDeckCatalog({
    selectedFormat: "Commander",
    selectedMatchType: "Bo1",
  });

  const candidate = catalog.candidates.find((c) => c.id === "saved:Untagged Commander");
  expect(candidate?.bracket).toBeNull();
});

it("surfaces the persisted bracket on user-saved decks", async () => {
  localStorage.setItem(
    STORAGE_KEY_PREFIX + "Tagged Commander",
    JSON.stringify({
      main: [{ count: 1, name: "Sol Ring" }],
      sideboard: [],
      commander: ["Atraxa, Praetors' Voice"],
      bracket: 4,
    }),
  );

  const catalog = await buildLegalAiDeckCatalog({
    selectedFormat: "Commander",
    selectedMatchType: "Bo1",
  });

  const candidate = catalog.candidates.find((c) => c.id === "saved:Tagged Commander");
  expect(candidate?.bracket).toBe(4);
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd client && pnpm test -- --run src/services/__tests__/aiDeckCatalog.test.ts`
Expected: FAIL — `candidate.bracket` is `undefined`, not `null` / `4`.

- [ ] **Step 3: Add bracket to `DeckCatalogCandidate`**

In `client/src/services/deckCatalog.ts`, modify the `DeckCatalogCandidate` interface (around lines 29–38) to add the new field:

```ts
export interface DeckCatalogCandidate {
  id: string;
  name: string;
  source: DeckCatalogSource;
  deck: ParsedDeck;
  knownFormat?: GameFormat;
  coveragePct?: number | null;
  bracket?: CommanderBracket | null;
  feedDeck?: FeedDeck;
  preconDeck?: PreconDeckEntry;
}
```

Add at the top of the file alongside existing imports:

```ts
import type { CommanderBracket } from "../types/bracket";
import { loadSavedDeckBracket } from "../constants/storage";
import { getPreconBracket } from "../data/preconBrackets";
```

Then update `buildDeckCatalog` to populate the bracket per source:

1. For the **saved** loop (around lines 98–111), change the candidate push to include `bracket: loadSavedDeckBracket(name)`.
2. For the **feed** loop (around lines 113–128), set `bracket: null` (feed decks have no tag).
3. For the **precon** loop (around lines 135–149), set `bracket: getPreconBracket(deckId)`.

- [ ] **Step 4: Add bracket to `AiDeckCandidate` and thread it through**

In `client/src/services/aiDeckCatalog.ts`:

Add the import:

```ts
import type { CommanderBracket } from "../types/bracket";
```

Update the interface (lines 15–22):

```ts
export interface AiDeckCandidate {
  id: string;
  name: string;
  source: AiDeckSource;
  deck: ParsedDeck;
  coveragePct: number | null;
  archetype: DeckArchetype | null;
  bracket: CommanderBracket | null;
}
```

In `buildLegalAiDeckCatalog` (around lines 64–81), thread `bracket` from the raw catalog entry into the AI-side candidate:

```ts
const rawCandidates = (await buildDeckCatalog()).map((candidate) => ({
  id: candidate.id,
  name: candidate.name,
  source: candidate.source,
  deck: candidate.deck,
  coveragePct: candidate.coveragePct ?? null,
  archetype: null,
  bracket: candidate.bracket ?? null,
  knownFormat: candidate.knownFormat,
}));
```

`legalCandidate` already destructures `knownFormat` and spreads the rest into the returned object, so it preserves `bracket` automatically — no edit needed there. Verify by re-reading `legalCandidate`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd client && pnpm test -- --run src/services/__tests__/aiDeckCatalog.test.ts`
Expected: PASS — all existing tests + 2 new tests.

- [ ] **Step 6: Verify Tilt sees the changes cleanly**

After saving, wait ~20s, then run: `tilt logs check-frontend --tail 30 --since 2m`
Expected: No TypeScript errors. If errors appear, fix them before committing.

- [ ] **Step 7: Commit**

```bash
git add client/src/services/aiDeckCatalog.ts client/src/services/deckCatalog.ts client/src/services/__tests__/aiDeckCatalog.test.ts
git commit -m "Thread CommanderBracket through deck catalog and AI candidate"
```

---

## Task 5: `aiBracketFilter` preference + v6→v7 migration

**Files:**
- Modify: `client/src/stores/preferencesStore.ts` (state, defaults, actions, version bump, migration)
- Modify: `client/src/stores/__tests__/preferencesStore.test.ts` (additive tests)

- [ ] **Step 1: Write the failing tests**

Open `client/src/stores/__tests__/preferencesStore.test.ts` and append inside the existing `describe("preferencesStore", ...)` block:

```ts
it("aiBracketFilter defaults to empty (filter off)", () => {
  const state = usePreferencesStore.getState();
  expect(state.aiBracketFilter).toEqual([]);
});

it("setAiBracketFilter replaces the array", () => {
  act(() => {
    usePreferencesStore.getState().setAiBracketFilter([2, 4]);
  });
  expect(usePreferencesStore.getState().aiBracketFilter).toEqual([2, 4]);

  act(() => {
    usePreferencesStore.getState().setAiBracketFilter([]);
  });
  expect(usePreferencesStore.getState().aiBracketFilter).toEqual([]);
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd client && pnpm test -- --run src/stores/__tests__/preferencesStore.test.ts`
Expected: FAIL — `aiBracketFilter` is undefined; `setAiBracketFilter` does not exist.

- [ ] **Step 3: Add the state slice**

In `client/src/stores/preferencesStore.ts`:

Add the import near the top:

```ts
import type { CommanderBracket } from "../types/bracket";
```

Inside `buildDefaultPreferences()` (around line 116–125), add the new field right after `aiCoverageFloor`:

```ts
aiCoverageFloor: DEFAULT_AI_COVERAGE_FLOOR,
aiBracketFilter: [] as CommanderBracket[],
```

In the `PreferencesState` interface (around line 158), add:

```ts
aiCoverageFloor: number;
aiBracketFilter: CommanderBracket[];
```

In the `PreferencesActions` interface (around line 203), add:

```ts
setAiCoverageFloor: (floor: number) => void;
setAiBracketFilter: (brackets: CommanderBracket[]) => void;
```

In the store factory (around line 326), add the setter:

```ts
setAiCoverageFloor: (floor) => set({ aiCoverageFloor: floor }),
setAiBracketFilter: (brackets) => set({ aiBracketFilter: brackets }),
```

- [ ] **Step 4: Bump version + write migration**

In the persist options block (around line 377), change `version: 6` to `version: 7`. After the v5→v6 migration block (closes around line 468), add:

```ts
// v6 → v7: introduce aiBracketFilter; existing users default to "off" ([]).
if (version < 7) {
  const legacy = migrated as { aiBracketFilter?: unknown } & Record<string, unknown>;
  migrated = {
    ...legacy,
    aiBracketFilter: Array.isArray(legacy.aiBracketFilter) ? legacy.aiBracketFilter : [],
  };
}
```

Append a comment to the migration history block (around line 388):

```
// v6 → v7: Add aiBracketFilter; legacy stores default to empty (filter off).
```

- [ ] **Step 5: Add the migration test**

Append inside the existing `describe("preferencesStore", ...)` block in `preferencesStore.test.ts`:

```ts
it("v6 → v7 migration defaults aiBracketFilter to empty", () => {
  // Hydrate the persist key as a v6 payload (no aiBracketFilter field).
  localStorage.setItem(
    "phase-preferences",
    JSON.stringify({
      state: {
        aiSeats: [{ difficulty: "Medium", deckId: "Random" }],
        aiArchetypeFilter: "Any",
        aiCoverageFloor: 90,
      },
      version: 6,
    }),
  );

  // Force the store to re-hydrate so the migration runs.
  usePreferencesStore.persist.rehydrate();

  expect(usePreferencesStore.getState().aiBracketFilter).toEqual([]);
});
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd client && pnpm test -- --run src/stores/__tests__/preferencesStore.test.ts`
Expected: PASS — all existing tests + 3 new tests.

- [ ] **Step 7: Verify Tilt is clean**

Wait ~20s, then run: `tilt logs check-frontend --tail 30 --since 2m`
Expected: No TypeScript errors.

- [ ] **Step 8: Commit**

```bash
git add client/src/stores/preferencesStore.ts client/src/stores/__tests__/preferencesStore.test.ts
git commit -m "Add aiBracketFilter preference and v6→v7 migration"
```

---

## Task 6: `BracketFilter` chip-row component

**Files:**
- Create: `client/src/components/menu/BracketFilter.tsx`
- Create: `client/src/components/menu/__tests__/BracketFilter.test.tsx`

- [ ] **Step 1: Write the failing test**

Create `client/src/components/menu/__tests__/BracketFilter.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { BracketFilter } from "../BracketFilter";

describe("BracketFilter", () => {
  it("renders one toggle button per WotC bracket", () => {
    render(<BracketFilter selected={[]} onChange={() => {}} />);
    for (const tier of ["1 Exhibition", "2 Core", "3 Upgraded", "4 Optimized", "5 cEDH"]) {
      expect(screen.getByRole("button", { name: tier })).toBeInTheDocument();
    }
  });

  it("marks selected buttons with aria-pressed=true", () => {
    render(<BracketFilter selected={[2, 4]} onChange={() => {}} />);
    expect(screen.getByRole("button", { name: "2 Core" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "4 Optimized" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "1 Exhibition" })).toHaveAttribute("aria-pressed", "false");
  });

  it("toggling a chip adds it when absent", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<BracketFilter selected={[2]} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "4 Optimized" }));

    expect(onChange).toHaveBeenCalledWith([2, 4]);
  });

  it("toggling a chip removes it when present", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<BracketFilter selected={[2, 4]} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "2 Core" }));

    expect(onChange).toHaveBeenCalledWith([4]);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd client && pnpm test -- --run src/components/menu/__tests__/BracketFilter.test.tsx`
Expected: FAIL — component does not exist.

- [ ] **Step 3: Implement `BracketFilter.tsx`**

Create `client/src/components/menu/BracketFilter.tsx`:

```tsx
import {
  BRACKET_LABEL,
  COMMANDER_BRACKETS,
  type CommanderBracket,
} from "../../types/bracket";

interface Props {
  /** Currently-selected brackets. Empty array = filter off (no constraint). */
  selected: CommanderBracket[];
  onChange: (next: CommanderBracket[]) => void;
}

export function BracketFilter({ selected, onChange }: Props) {
  const toggle = (b: CommanderBracket) => {
    onChange(selected.includes(b) ? selected.filter((x) => x !== b) : [...selected, b].sort());
  };

  return (
    <div className="flex flex-wrap gap-1.5" role="group" aria-label="Bracket filter">
      {COMMANDER_BRACKETS.map((b) => {
        const active = selected.includes(b);
        return (
          <button
            key={b}
            type="button"
            aria-pressed={active}
            onClick={() => toggle(b)}
            className={
              active
                ? "rounded-full border border-indigo-300/60 bg-indigo-500/30 px-2.5 py-1 text-xs font-medium text-indigo-100"
                : "rounded-full border border-white/10 bg-black/20 px-2.5 py-1 text-xs font-medium text-slate-400 hover:bg-white/6"
            }
          >
            {b} {BRACKET_LABEL[b]}
          </button>
        );
      })}
    </div>
  );
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd client && pnpm test -- --run src/components/menu/__tests__/BracketFilter.test.tsx`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add client/src/components/menu/BracketFilter.tsx client/src/components/menu/__tests__/BracketFilter.test.tsx
git commit -m "Add BracketFilter chip-row component"
```

---

## Task 7: Wire `BracketFilter` into `AiOpponentConfig` + extend filter pipeline

**Files:**
- Modify: `client/src/components/menu/AiOpponentConfig.tsx`
- Create: `client/src/components/menu/__tests__/AiOpponentConfig.test.tsx`

- [ ] **Step 1: Write the failing integration test**

Create `client/src/components/menu/__tests__/AiOpponentConfig.test.tsx`. This test mocks the deck catalog and checks the random-pool count summary (rendered as `Random (N)` inside the seat select).

```tsx
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { AiOpponentConfig } from "../AiOpponentConfig";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import type { AiDeckCandidate } from "../../../services/aiDeckCatalog";

vi.mock("../../../services/aiDeckCatalog", async () => {
  const actual = await vi.importActual<typeof import("../../../services/aiDeckCatalog")>(
    "../../../services/aiDeckCatalog",
  );
  return {
    ...actual,
    useAiDeckCatalog: () => ({ candidates: mockCandidates, loading: false, error: null }),
  };
});

let mockCandidates: AiDeckCandidate[] = [];

function candidate(id: string, bracket: AiDeckCandidate["bracket"]): AiDeckCandidate {
  return {
    id,
    name: id,
    source: { type: "precon", deckId: id, code: "TST" },
    deck: { main: [], sideboard: [] },
    coveragePct: 100,
    archetype: null,
    bracket,
  };
}

beforeEach(() => {
  mockCandidates = [
    candidate("Bracket1", 1),
    candidate("Bracket2", 2),
    candidate("Bracket4", 4),
    candidate("Untagged", null),
  ];
  act(() => {
    usePreferencesStore.getState().setAiBracketFilter([]);
    usePreferencesStore.getState().setAiArchetypeFilter("Any");
    usePreferencesStore.getState().setAiCoverageFloor(50);
  });
});

describe("AiOpponentConfig — bracket filter", () => {
  it("does not render the bracket chip row when format is not Commander", () => {
    render(<AiOpponentConfig selectedFormat="Standard" opponentCount={1} />);
    expect(screen.queryByRole("group", { name: "Bracket filter" })).not.toBeInTheDocument();
  });

  it("renders the bracket chip row when format is Commander", () => {
    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);
    expect(screen.getByRole("group", { name: "Bracket filter" })).toBeInTheDocument();
  });

  it("filter off (empty selection) keeps untagged candidates in the random pool", () => {
    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);
    // Initial pool = all 4 candidates.
    expect(screen.getByRole("option", { name: /Random \(4\)/ })).toBeInTheDocument();
  });

  it("selecting brackets {2, 4} narrows the pool to those candidates and excludes untagged", async () => {
    const user = userEvent.setup();
    render(<AiOpponentConfig selectedFormat="Commander" opponentCount={1} />);

    await user.click(screen.getByRole("button", { name: "2 Core" }));
    await user.click(screen.getByRole("button", { name: "4 Optimized" }));

    await waitFor(() => {
      expect(screen.getByRole("option", { name: /Random \(2\)/ })).toBeInTheDocument();
    });
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd client && pnpm test -- --run src/components/menu/__tests__/AiOpponentConfig.test.tsx`
Expected: FAIL — `BracketFilter` is not rendered; the random count does not change with bracket selection.

- [ ] **Step 3: Wire the component in**

Open `client/src/components/menu/AiOpponentConfig.tsx`.

Add the import:

```tsx
import { BracketFilter } from "./BracketFilter";
import type { CommanderBracket } from "../../types/bracket";
```

Inside the component body (alongside the existing `coverageFloor` selectors, around line 80):

```tsx
const bracketFilter = usePreferencesStore((s) => s.aiBracketFilter);
const setBracketFilter = usePreferencesStore((s) => s.setAiBracketFilter);
```

Extend the `filteredDecks` `useMemo` (around line 98) to apply the bracket filter:

```tsx
const filteredDecks = useMemo(() => {
  return candidates.filter((d) => {
    if (d.coveragePct != null && d.coveragePct < coverageFloor) return false;
    if (archetypeFilter !== "Any" && d.archetype && d.archetype !== archetypeFilter) {
      return false;
    }
    if (bracketFilter.length > 0 && selectedFormat === "Commander") {
      if (d.bracket === null) return false;             // untagged excluded
      if (!bracketFilter.includes(d.bracket)) return false;
    }
    return true;
  });
}, [candidates, coverageFloor, archetypeFilter, bracketFilter, selectedFormat]);
```

Inside the "Random Pool Filters" panel (after the coverage `<label>`, around line 208), add — but only when format is Commander:

```tsx
{selectedFormat === "Commander" && (
  <label className="flex flex-col gap-1">
    <span className="text-xs text-slate-400">Bracket</span>
    <BracketFilter selected={bracketFilter} onChange={setBracketFilter} />
    <span className="text-[10px] text-slate-500">
      Random AI picks from these brackets. Untagged decks are excluded when filtering.
    </span>
  </label>
)}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd client && pnpm test -- --run src/components/menu/__tests__/AiOpponentConfig.test.tsx`
Expected: PASS — 4 tests.

- [ ] **Step 5: Re-run the full menu test suite**

Run: `cd client && pnpm test -- --run src/components/menu/__tests__/`
Expected: PASS — no regressions in any menu test.

- [ ] **Step 6: Verify Tilt sees the changes cleanly**

Wait ~20s, then run: `tilt logs check-frontend --tail 30 --since 2m`
Expected: No TypeScript errors. Then: `tilt logs test-frontend --tail 30 --since 2m` — entire suite green.

- [ ] **Step 7: Commit**

```bash
git add client/src/components/menu/AiOpponentConfig.tsx client/src/components/menu/__tests__/AiOpponentConfig.test.tsx
git commit -m "Render bracket filter in AI opponent config and apply to random pool"
```

---

## Task 8: `BracketPicker` deck-builder component

**Files:**
- Create: `client/src/components/deck-builder/BracketPicker.tsx`
- Create: `client/src/components/deck-builder/__tests__/BracketPicker.test.tsx`

- [ ] **Step 1: Write the failing test**

Create `client/src/components/deck-builder/__tests__/BracketPicker.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { BracketPicker } from "../BracketPicker";

describe("BracketPicker", () => {
  it("renders an Unrated chip plus 1..5", () => {
    render(<BracketPicker value={null} onChange={() => {}} />);
    expect(screen.getByRole("button", { name: "Unrated" })).toBeInTheDocument();
    for (const tier of ["1 Exhibition", "2 Core", "3 Upgraded", "4 Optimized", "5 cEDH"]) {
      expect(screen.getByRole("button", { name: tier })).toBeInTheDocument();
    }
  });

  it("marks the active chip with aria-pressed=true (Unrated when value is null)", () => {
    render(<BracketPicker value={null} onChange={() => {}} />);
    expect(screen.getByRole("button", { name: "Unrated" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "3 Upgraded" })).toHaveAttribute("aria-pressed", "false");
  });

  it("marks the active chip with aria-pressed=true (numeric when value is set)", () => {
    render(<BracketPicker value={3} onChange={() => {}} />);
    expect(screen.getByRole("button", { name: "Unrated" })).toHaveAttribute("aria-pressed", "false");
    expect(screen.getByRole("button", { name: "3 Upgraded" })).toHaveAttribute("aria-pressed", "true");
  });

  it("clicking a numeric chip emits that bracket", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<BracketPicker value={null} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "4 Optimized" }));

    expect(onChange).toHaveBeenCalledWith(4);
  });

  it("clicking Unrated emits null", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<BracketPicker value={3} onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "Unrated" }));

    expect(onChange).toHaveBeenCalledWith(null);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd client && pnpm test -- --run src/components/deck-builder/__tests__/BracketPicker.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement the picker**

Create `client/src/components/deck-builder/BracketPicker.tsx`:

```tsx
import {
  BRACKET_LABEL,
  COMMANDER_BRACKETS,
  type CommanderBracket,
} from "../../types/bracket";

interface Props {
  value: CommanderBracket | null;
  onChange: (next: CommanderBracket | null) => void;
}

export function BracketPicker({ value, onChange }: Props) {
  return (
    <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Deck bracket">
      <button
        type="button"
        aria-pressed={value === null}
        onClick={() => onChange(null)}
        className={
          value === null
            ? "rounded-full border border-slate-300/60 bg-slate-500/30 px-2.5 py-1 text-xs font-medium text-slate-100"
            : "rounded-full border border-white/10 bg-black/20 px-2.5 py-1 text-xs font-medium text-slate-400 hover:bg-white/6"
        }
      >
        Unrated
      </button>
      {COMMANDER_BRACKETS.map((b) => {
        const active = value === b;
        return (
          <button
            key={b}
            type="button"
            aria-pressed={active}
            onClick={() => onChange(b)}
            className={
              active
                ? "rounded-full border border-indigo-300/60 bg-indigo-500/30 px-2.5 py-1 text-xs font-medium text-indigo-100"
                : "rounded-full border border-white/10 bg-black/20 px-2.5 py-1 text-xs font-medium text-slate-400 hover:bg-white/6"
            }
          >
            {b} {BRACKET_LABEL[b]}
          </button>
        );
      })}
    </div>
  );
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd client && pnpm test -- --run src/components/deck-builder/__tests__/BracketPicker.test.tsx`
Expected: PASS — 5 tests.

- [ ] **Step 5: Commit**

```bash
git add client/src/components/deck-builder/BracketPicker.tsx client/src/components/deck-builder/__tests__/BracketPicker.test.tsx
git commit -m "Add BracketPicker component for deck builder"
```

---

## Task 9: Wire `BracketPicker` into `DeckBuilder`

**Files:**
- Modify: `client/src/components/deck-builder/DeckBuilder.tsx`

Existing tests in `client/src/components/deck-builder/__tests__/DeckBuilder.test.tsx` should pass unchanged — bracket is additive. The component-level tests for `BracketPicker` already cover its behavior; the DeckBuilder change is a small wiring change that's most reliably exercised manually + the existing integration tests staying green.

- [ ] **Step 1: Read the existing DeckBuilder integration tests to confirm they don't break**

Run: `cat client/src/components/deck-builder/__tests__/DeckBuilder.test.tsx | head -60`
Take note of the test setup — it should not interact with bracket state. If a test renders the full DeckBuilder header, it must still pass after the wiring change.

- [ ] **Step 2: Add bracket state and load/save wiring to `DeckBuilder.tsx`**

Open `client/src/components/deck-builder/DeckBuilder.tsx`.

Add imports near the existing imports:

```tsx
import { BracketPicker } from "./BracketPicker";
import type { CommanderBracket } from "../../types/bracket";
```

Add state alongside the other `useState` calls (after the `deckName` state, around line 80):

```tsx
const [bracket, setBracket] = useState<CommanderBracket | null>(null);
```

Update `handleSave` (around line 278) to persist the bracket. The current implementation is:

```tsx
const handleSave = () => {
  if (!deckName.trim()) return;
  const data = JSON.stringify({ ...currentDeck, format });
  localStorage.setItem(STORAGE_KEY_PREFIX + deckName.trim(), data);
  stampDeckMeta(deckName.trim());
  setSavedDecks(listSavedDecks());
  setJustSaved(true);
};
```

Change it to:

```tsx
const handleSave = () => {
  if (!deckName.trim()) return;
  const payload: Record<string, unknown> = { ...currentDeck, format };
  if (bracket !== null) payload.bracket = bracket;
  const data = JSON.stringify(payload);
  localStorage.setItem(STORAGE_KEY_PREFIX + deckName.trim(), data);
  stampDeckMeta(deckName.trim());
  setSavedDecks(listSavedDecks());
  setJustSaved(true);
};
```

Update `handleLoad` (around lines 293–320) so the bracket is restored after a successful load. After the existing `setDeckName(name);` near the end of the function, before the closing brace, add:

```tsx
// Restore the saved bracket sidecar if present.
import("../../constants/storage").then(({ loadSavedDeckBracket }) => {
  setBracket(loadSavedDeckBracket(name));
});
```

Actually — dynamic imports are unnecessary; add a static import at the top with the other storage imports:

```tsx
import { STORAGE_KEY_PREFIX, loadSavedDeck, loadSavedDeckBracket, stampDeckMeta } from "../../constants/storage";
```

Then in `handleLoad`, after the existing `setDeckName(name);`:

```tsx
setBracket(loadSavedDeckBracket(name));
```

For the precon-load branch (the early return inside `handleLoad` that handles `PRECON_PREFIX`-named loads), set the bracket from the precon overlay:

```tsx
// inside the precon branch, after applying the deck:
const { getPreconBracket } = await import("../../data/preconBrackets");
setBracket(getPreconBracket(deckEntry.code) ?? null);
```

The dynamic import here is intentional — precon brackets are only relevant during a precon load, and the import keeps the bundle slim. Alternatively, hoist it to a static import at the top of the file alongside other imports if it's already loaded elsewhere in the app:

```tsx
import { getPreconBracket } from "../../data/preconBrackets";
```

Pick the static import — `preconBrackets.ts` is tiny.

- [ ] **Step 3: Render the BracketPicker in the header**

In the JSX header section of `DeckBuilder.tsx` (around line 422–477), the existing layout is `< Menu` link → deck name + label → `FormatFilter` → deck name input + Save + Load.

Insert the bracket picker just below the `FormatFilter`, conditional on `format === "Commander"`. Replace the existing `<FormatFilter ... />` line (around line 439) with a wrapper:

```tsx
<div className="flex items-center gap-3">
  <FormatFilter selected={format} onChange={onFormatChange} />
  {format === "Commander" && (
    <div className="flex items-center gap-2">
      <span className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">Bracket</span>
      <BracketPicker value={bracket} onChange={setBracket} />
    </div>
  )}
</div>
```

If switching to a non-Commander format, the picker disappears but the `bracket` state is preserved silently so switching back restores the tag in-session. (The persisted bracket is preserved across sessions because it's only written by `handleSave` when set.)

- [ ] **Step 4: Verify existing tests still pass and the new flow works**

Run: `cd client && pnpm test -- --run src/components/deck-builder/__tests__/`
Expected: PASS — all existing deck-builder tests + new BracketPicker tests.

Then check Tilt: wait ~20s, then `tilt logs check-frontend --tail 30 --since 2m` and `tilt logs test-frontend --tail 50 --since 2m`. Both clean.

- [ ] **Step 5: Manual smoke test in the dev server**

Open the running dev server (Tilt runs it on the default Vite port). Walk through:

1. Open Deck Builder, switch format to Commander. Bracket picker appears next to FormatFilter.
2. Build/import a small Commander deck, click "3 Upgraded", save it with a name.
3. Reload the page, load the saved deck. Bracket picker shows 3 selected.
4. Click "Unrated", save again, reload, load. Bracket picker now shows Unrated.
5. Switch the deck builder to Standard format — picker disappears.

If any step is wrong, fix before committing. If the dev server isn't accessible, mark this step done after the test runs alone and surface a note in the commit body.

- [ ] **Step 6: Commit**

```bash
git add client/src/components/deck-builder/DeckBuilder.tsx
git commit -m "Render BracketPicker in deck builder and persist bracket sidecar"
```

---

## Task 10: Final sweep — full suite + manual end-to-end + push

- [ ] **Step 1: Run the entire frontend test suite once more**

Wait for any in-flight Tilt rebuild, then check: `tilt logs test-frontend --tail 80 --since 5m`
Expected: PASS — no failures, no skipped tests, no console warnings about React act() violations or missing keys.

- [ ] **Step 2: Run TypeScript type-check via Tilt**

Run: `tilt logs check-frontend --tail 30 --since 5m`
Expected: No TypeScript errors, no ESLint errors.

- [ ] **Step 3: Manual end-to-end smoke test (browser)**

In the dev server:

1. **Setup page, Standard format, 1 opponent:** bracket chips NOT visible in the AI Opponent panel. ✅
2. **Setup page, Commander format, 1 opponent:** bracket chips visible. With no chips selected, the Random count matches the unfiltered candidate count. Click chips [2] and [4] — random count drops to only those bracket tiers, and the helper line is visible.
3. **Save a tagged user deck:** build a small Commander deck in the deck builder, tag it bracket 3, save. Restart setup page. With brackets [3] selected, the deck is in the random pool. With brackets [2] selected, it's NOT.
4. **Untagged user deck:** save another Commander deck without setting bracket. With any brackets selected, the deck is excluded from the random pool. With no brackets selected, the deck appears in the pool.
5. **User-side guarantee:** while bracket filter is on with, e.g., only [4] selected, switch the **human seat** to your untagged Commander deck. It loads and plays normally — the bracket filter never blocks the human seat. ✅
6. **Explicit AI deck pick:** with brackets [4] selected, manually assign a specific bracket-1 deck to the AI seat. The selection is respected (filter only constrains the Random pool, not pinned picks). ✅

- [ ] **Step 4: Update the existing tests sanity sweep**

```bash
git status --short
```

Expected: no unstaged changes if all previous tasks committed cleanly.

- [ ] **Step 5: Push the branch**

```bash
git push -u origin feat/ai-opponent-bracket-filter
```

Expected: branch created on the remote. Do NOT open a PR yet — the user reviews the branch and triggers the PR.

---

## Self-Review

This section was filled in by the plan author after the plan was drafted.

**Spec coverage:**

- §1 Goal & scope — covered by all tasks; visibility gate in Task 7 step 3.
- §2.1 Shared TypeScript type — Task 1.
- §2.2 Precon manifest — Task 2 (TS-side overlay instead of touching the Rust JSON generator; equivalent surface).
- §2.3 Saved decks sidecar — Task 3.
- §2.4 `AiDeckCandidate` field — Task 4.
- §3 Preferences store + migration — Task 5.
- §4 Deck builder tagging — Tasks 8 and 9.
- §5 AI opponent config UI — Task 7.
- §6 Filter logic — Task 7 step 3.
- §6a User-side guarantees — Task 10 step 3 verifies asymmetry empirically; no code restricts the human seat anywhere in the plan.
- §7 Engine boundary — no Rust tasks. ✅
- §8 Testing — Tasks 1–9 each include tests; Task 10 verifies full suite.
- §9 Out of scope — nothing in the plan touches an engine crate, an auto-classifier, an in-game HUD, or a non-Commander format.

**Placeholder scan:** None. Every code block is complete; every test asserts concrete values; every command has expected output.

**Type consistency:**

- `CommanderBracket = 1 | 2 | 3 | 4 | 5` defined in Task 1, imported the same way in Tasks 2, 3, 4, 5, 6, 8, 9.
- `BracketFilter` props: `selected: CommanderBracket[]`, `onChange: (next: CommanderBracket[]) => void` — consistent across Tasks 6 and 7.
- `BracketPicker` props: `value: CommanderBracket | null`, `onChange: (next: CommanderBracket | null) => void` — consistent across Tasks 8 and 9.
- `loadSavedDeckBracket(name)` / `saveSavedDeckBracket(name, bracket)` defined in Task 3, used in Tasks 4 and 9.
- `getPreconBracket(deckId)` defined in Task 2, used in Tasks 4 and 9.
- `aiBracketFilter: CommanderBracket[]` + `setAiBracketFilter: (brackets: CommanderBracket[]) => void` defined in Task 5, used in Task 7.
