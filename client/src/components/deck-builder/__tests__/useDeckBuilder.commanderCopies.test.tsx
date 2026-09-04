import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";

import type { GameFormat } from "../../../adapter/types";
import {
  evaluateDeckCompatibility,
  type DeckCompatibilityResult,
} from "../../../services/deckCompatibility";

const eligible = new Set<string>();
const partnerCandidates = vi.fn(async () => [] as string[]);
const cardDataCache = new Map<string, { name: string; cmc: number; color_identity: string[] }>();

vi.mock("../../../services/engineRuntime", () => ({
  isCardCommanderEligibleForFormat: vi.fn(async (name: string) => eligible.has(name)),
  commanderPartnerCandidates: (...args: unknown[]) =>
    (partnerCandidates as unknown as (...a: unknown[]) => Promise<string[]>)(...args),
  companionCandidates: vi.fn(async () => [] as string[]),
  isCardCommanderEligible: vi.fn(async () => false),
  maxDeckCopies: vi.fn(async () => ({ type: "Limited", data: 1 })),
  signatureSpellSelectionPolicy: vi.fn(async () => null),
}));

vi.mock("../../../hooks/useDeckCardData", () => ({
  useDeckCardData: () => ({ cardDataCache, cacheCards: vi.fn() }),
}));

vi.mock("../../../hooks/useBracketEstimate", () => ({
  useBracketEstimate: () => ({ estimate: null, unsupported: true }),
}));

vi.mock("../../../hooks/useDecks", () => ({
  loadPreconDeckMap: vi.fn(async () => ({})),
}));

vi.mock("../../../services/deckCompatibility", () => ({
  evaluateDeckCompatibility: vi.fn(async () => null),
}));

vi.mock("../../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({}),
}));

import { useDeckBuilder } from "../useDeckBuilder";

/**
 * VM row 7 — CR 903.3. The designation "is not a characteristic of the object
 * ... it is an attribute of the card itself": a LABEL on one card, never a
 * removal of every copy. Designating one copy of a card the deck holds three of
 * must leave two.
 *
 * REVERT-PROBE: restore `prev.main.filter((e) => e.name !== cardName)` in
 * `handleSetCommander` and the first case finds NO `Legend` row in main at all.
 */
describe("useDeckBuilder — CR 903.3 designation accounting", () => {
  const LEGEND = "Legend";
  const OTHER = "Other Legend";

  function setup() {
    return renderHook(() =>
      useDeckBuilder({
        format: "Commander" as GameFormat,
        onFormatChange: vi.fn(),
        searchFilters: {} as never,
      }),
    );
  }

  /** Install a main deck and wait for the engine's eligibility scan to land. */
  async function withMain(
    result: { current: ReturnType<typeof useDeckBuilder> },
    main: Array<{ count: number; name: string }>,
  ) {
    act(() => {
      result.current.handleImport({ main, sideboard: [] });
    });
    await waitFor(() => {
      // Reach guard: `handleSetCommander` early-returns unless the engine has
      // marked the card eligible, so a test that skipped this wait would pass
      // vacuously against a no-op handler.
      expect(result.current.isCommanderEligible(LEGEND)).toBe(true);
    });
  }

  function mainCount(
    result: { current: ReturnType<typeof useDeckBuilder> },
    name: string,
  ): number | undefined {
    return result.current.deck.main.find((e) => e.name === name)?.count;
  }

  beforeEach(() => {
    eligible.clear();
    eligible.add(LEGEND);
    eligible.add(OTHER);
    partnerCandidates.mockReset();
    partnerCandidates.mockResolvedValue([]);
    cardDataCache.clear();
    localStorage.clear();
  });

  afterEach(() => {
    cleanup();
  });

  it("decrements the main-deck copy count rather than removing every copy", async () => {
    const { result } = setup();
    await withMain(result, [{ count: 3, name: LEGEND }]);

    await act(async () => {
      result.current.handleSetCommander(LEGEND);
    });

    await waitFor(() => {
      expect(result.current.commanders).toEqual([LEGEND]);
    });
    // REVERT-FAILING: the base `filter` leaves `undefined` here.
    expect(mainCount(result, LEGEND)).toBe(2);
  });

  it("removes the row entirely when the last copy is designated", async () => {
    const { result } = setup();
    await withMain(result, [{ count: 1, name: LEGEND }]);

    await act(async () => {
      result.current.handleSetCommander(LEGEND);
    });

    await waitFor(() => {
      expect(result.current.commanders).toEqual([LEGEND]);
    });
    // Hostile sibling: `{count: 1}` must FILTER, not decrement to zero — a
    // naive `count - 1` everywhere leaves a `{count: 0}` ghost row.
    expect(mainCount(result, LEGEND)).toBeUndefined();
    expect(result.current.deck.main.some((e) => e.name === LEGEND)).toBe(false);
  });

  it("adding a partner does not disturb the first commander's accounting", async () => {
    const { result } = setup();
    await withMain(result, [
      { count: 3, name: LEGEND },
      { count: 2, name: OTHER },
    ]);

    await act(async () => {
      result.current.handleSetCommander(LEGEND);
    });
    await waitFor(() => {
      expect(result.current.commanders).toEqual([LEGEND]);
    });

    // CR 702.124h: the engine confirms the pair, so this is an ADD, not a swap.
    partnerCandidates.mockResolvedValue([OTHER]);
    await act(async () => {
      result.current.handleSetCommander(OTHER);
    });
    await waitFor(() => {
      expect(result.current.commanders).toEqual([LEGEND, OTHER]);
    });

    // Multi-authority: the second designation touches only its own card.
    expect(mainCount(result, LEGEND)).toBe(2);
    expect(mainCount(result, OTHER)).toBe(1);
  });

  it("designate-then-remove restores the exact prior multiset", async () => {
    const { result } = setup();
    await withMain(result, [{ count: 3, name: LEGEND }]);

    await act(async () => {
      result.current.handleSetCommander(LEGEND);
    });
    await waitFor(() => {
      expect(mainCount(result, LEGEND)).toBe(2);
    });

    act(() => {
      result.current.handleRemoveCommander(LEGEND);
    });

    // REVERT-FAILING on `handleRemoveCommander`: the base appends a bare
    // `{count: 1}` row, so main holds TWO `Legend` rows (2 and 1) rather than
    // one row of 3 — a duplicate the deduplicating inverse prevents.
    expect(result.current.deck.main.filter((e) => e.name === LEGEND)).toHaveLength(1);
    expect(mainCount(result, LEGEND)).toBe(3);
    expect(result.current.commanders).toEqual([]);
  });

  it("uses the engine distribution even when cached card data is absent or stale", async () => {
    cardDataCache.set("Azorius Pair", {
      name: "Azorius Pair",
      cmc: 2,
      color_identity: ["G"],
    });
    const distribution = [
      { color: "White" as const, count: 2, percentage: 40, display_percentage: 40 },
      { color: "Blue" as const, count: 2, percentage: 40, display_percentage: 40 },
      { color: "Red" as const, count: 1, percentage: 20, display_percentage: 20 },
    ];
    vi.mocked(evaluateDeckCompatibility).mockResolvedValue({
      standard: { compatible: true, reasons: [] },
      commander: { compatible: true, reasons: [] },
      bo3_ready: false,
      unknown_cards: [],
      selected_format_reasons: [],
      color_identity: ["W", "U", "R"],
      color_distribution: distribution,
    } satisfies DeckCompatibilityResult);
    const { result } = setup();

    act(() => {
      result.current.handleImport({
        main: [
          { name: "Azorius Pair", count: 2 },
          { name: "Red Card", count: 1 },
          { name: "Wastes", count: 1 },
          { name: "Uncached Card", count: 1 },
        ],
        sideboard: [],
      });
    });

    await waitFor(() => {
      expect(result.current.colorDistribution).toEqual(distribution);
    });
  });
});
