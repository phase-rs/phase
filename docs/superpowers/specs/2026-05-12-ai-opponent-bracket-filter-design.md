# AI Opponent Bracket Filter — Design

- Date: 2026-05-12
- Branch: `feat/ai-opponent-bracket-filter`
- Status: Design — awaiting implementation plan

## 1. Goal & scope

Add a Commander-bracket filter to the AI opponent picker so the user can constrain the random AI deck pool to specific WotC bracket tiers:

| Bracket | Name |
|---|---|
| 1 | Exhibition |
| 2 | Core |
| 3 | Upgraded |
| 4 | Optimized |
| 5 | cEDH |

The filter:

- Applies **only** when the selected format is Commander.
- Lets the user multi-select any subset of brackets 1–5.
- When **active** (one or more brackets selected), excludes untagged decks from the random AI pool.
- Is **manual-tagging only** — no auto-classifier.
- Is **asymmetric** — it never restricts the human player's deck choice or any other user action.

Bracket is pre-game deck metadata. It never crosses into engine game logic.

## 2. Data model

### 2.1 Shared TypeScript type

New file: `client/src/types/bracket.ts`

```ts
export type CommanderBracket = 1 | 2 | 3 | 4 | 5;

export const COMMANDER_BRACKETS: readonly CommanderBracket[] = [1, 2, 3, 4, 5] as const;

export const BRACKET_LABEL: Record<CommanderBracket, string> = {
  1: "Exhibition",
  2: "Core",
  3: "Upgraded",
  4: "Optimized",
  5: "cEDH",
};
```

### 2.2 Precon manifest

Each bundled Commander precon's manifest entry gains an optional `bracket: CommanderBracket` field. Existing precons are curated by hand and assigned a tier.

### 2.3 Saved decks (user-built)

The saved-deck schema in `localStorage` gains an optional `bracket?: CommanderBracket` field. Decks saved before this change load with `bracket === undefined` and are treated as "Unrated". The field is additive — no data migration is required.

### 2.4 `AiDeckCandidate`

`client/src/services/aiDeckCatalog.ts` extends the candidate type:

```ts
export interface AiDeckCandidate {
  // ...existing fields
  bracket: CommanderBracket | null;   // null = unrated / untagged
}
```

Populated from whichever source (precon manifest or saved-deck record) supplied the deck.

## 3. Preferences store

`client/src/stores/preferencesStore.ts` adds:

```ts
aiBracketFilter: CommanderBracket[];                       // empty = "off / no filter"
setAiBracketFilter: (brackets: CommanderBracket[]) => void;
```

A new persisted-version migration (`v4 → v5`) defaults the new field to `[]` for upgraded users so nobody's pool silently shrinks.

## 4. Deck builder tagging

When the active format in the Deck Builder is Commander:

- Render a small chip row beside the deck name/format: `[1 Exhibition] [2 Core] [3 Upgraded] [4 Optimized] [5 cEDH]` plus an explicit "Unrated" affordance (deselect-all).
- Selecting a chip sets the deck's bracket and saves with the deck record.
- The picker is hidden for non-Commander formats; previously-set bracket values are preserved silently so flipping format back doesn't lose the tag.

The picker is purely informational metadata. It does **not** prevent saving, editing, or playing the deck at any tier.

## 5. AI opponent config UI

`client/src/components/menu/AiOpponentConfig.tsx`:

- Immediately under the existing Archetype dropdown, render a `BracketFilter` chip row.
- **Visible only when `selectedFormat === "Commander"`** (no greyed-out / disabled state outside Commander).
- Chips show `<n> <label>` (e.g., `2 Core`). Multi-select with click-to-toggle.
- Below the chips, a short helper line:
  > *"Random AI picks from these brackets. Untagged decks are excluded when filtering."*

## 6. Filter logic

The existing `filteredDecks` `useMemo` in `AiOpponentConfig.tsx` is the single chokepoint. After the existing archetype + coverage filters, append:

```ts
if (
  bracketFilter.length > 0 &&
  selectedFormat === "Commander"
) {
  if (d.bracket === null) return false;             // untagged excluded
  if (!bracketFilter.includes(d.bracket)) return false;
}
```

Explicit-deck selection (user manually assigns a specific deck to an AI seat) **bypasses** the filter — same precedent as the existing archetype filter.

## 6a. User-side guarantees (asymmetric filter)

The bracket filter is one-directional. It constrains only the AI's random deck pool. Concretely:

1. **The user's own deck slot is never affected.** Whatever deck the user has loaded (precon, saved, freshly built) plays as-is.
2. **Tagging a deck is metadata, never a gate.** A bracket tag on a saved deck does not restrict whether the user can play it, edit it, save it, or load it in any format the deck is otherwise legal in.
3. **The untagged-exclusion rule applies only to the AI random pool.** A user playing an untagged Commander deck is unaffected — the rule narrows only which deck the *AI* may randomly pick.
4. **Explicit AI deck pick bypasses the filter.** If the user manually assigns a specific deck to an AI seat, the filter does not reject it.

## 7. Engine boundary

The Rust engine (`crates/engine`), AI crate (`crates/phase-ai`), and WASM bridge (`crates/engine-wasm`) are **untouched**. Bracket values never enter the engine.

This matches how `archetype` and `coveragePct` are already handled and respects the project's "engine owns game logic" principle without overextending it to pre-game deck metadata. If the in-game HUD ever needs to show an opponent's bracket, the value can be threaded through later; that capability is not required by this change.

## 8. Testing

### 8.1 `client/src/stores/__tests__/preferencesStore.test.ts`

- Default `aiBracketFilter` is `[]`.
- v4 → v5 migration produces `aiBracketFilter: []` for legacy persisted state.
- `setAiBracketFilter([2, 4])` replaces the array; subsequent `setAiBracketFilter([])` clears it.

### 8.2 `client/src/components/menu/__tests__/AiOpponentConfig.test.tsx` (new file)

- Chip row is **not rendered** when `selectedFormat !== "Commander"`.
- Chip row **is rendered** when `selectedFormat === "Commander"`.
- With brackets `{2, 4}` selected, the candidate list is filtered to decks whose `bracket` is 2 or 4.
- A user-saved candidate with `bracket: null` is **excluded** when any bracket is selected, and **included** when no brackets are selected.

### 8.3 `client/src/services/__tests__/aiDeckCatalog.test.ts` (additive)

- Precon candidates surface the curated `bracket` field from the manifest.
- Saved-deck candidates surface `bracket` when the saved record has one and `null` otherwise.

## 9. Out of scope (deliberate)

- **No auto-classifier.** Brackets are not derived from deck contents.
- **No in-game HUD display** of opponent's bracket. The value never reaches the engine and is not rendered during play.
- **No format-agnostic "power level" abstraction** for Standard, Modern, Pioneer, etc. Bracket is strictly the Commander concept.
- **No engine-side `CommanderBracket` enum.** Can be promoted later if needed.
- **No user-side restriction of any kind.** Brackets do not constrain the human player's deck choice, deck building, format legality, or in-game actions.

## 10. Open questions

None at design time. Bracket assignments for shipped precons will be curated during implementation and surfaced for user review before merge.
