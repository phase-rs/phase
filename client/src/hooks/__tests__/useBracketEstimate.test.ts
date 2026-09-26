import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { estimateDeckBracket } from "../../services/bracketEstimate";
import type { ParsedDeck } from "../../services/deckParser";
import type { BracketEstimate, CommanderBracketTier } from "../../types/bracket";
import { clearBracketEstimateCache, useBracketEstimate } from "../useBracketEstimate";

vi.mock("../../services/bracketEstimate", () => ({ estimateDeckBracket: vi.fn() }));

const mockEstimate: BracketEstimate = {
  tier: "upgraded",
  axes: {
    game_changers: { count: 1, contributing: ["Smothering Tithe"] },
    mass_land_denial: { count: 0, contributing: [] },
    extra_turns: { count: 0, contributing: [] },
    efficient_tutors: { count: 2, contributing: ["Demonic Tutor", "Vampiric Tutor"] },
  },
  checks: [],
  coverage: { counted: 4, resolved: 4, unresolved: [], confidence: "complete" },
  data_version: "test-1",
  declaration: null,
};

const deck: ParsedDeck = {
  main: [
    { name: "Smothering Tithe", count: 1 },
    { name: "Forest", count: 2 },
  ],
  sideboard: [{ name: "Pyroblast", count: 1 }],
  companion: "Lutri, the Spellchaser",
  signature_spell: ["Lightning Bolt"],
};

const baseOptions = {
  deck,
  commanders: ["Krenko, Mob Boss"],
  format: "Commander" as const,
  declaredTier: null,
};

function mockEstimateOutcome(estimate: BracketEstimate = mockEstimate): void {
  vi.mocked(estimateDeckBracket).mockResolvedValue({ kind: "estimate", estimate });
}

describe("useBracketEstimate", () => {
  afterEach(() => {
    clearBracketEstimateCache();
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  it("returns null without calling the service when the format is not Commander", async () => {
    mockEstimateOutcome();
    const { result } = renderHook(() =>
      useBracketEstimate({ ...baseOptions, format: "Standard" }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.estimate).toBeNull();
    expect(result.current.outcome).toBeNull();
    expect(estimateDeckBracket).not.toHaveBeenCalled();
  });

  it("returns null without calling the service when no commander is selected", async () => {
    mockEstimateOutcome();
    const { result } = renderHook(() =>
      useBracketEstimate({ ...baseOptions, commanders: [] }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.estimate).toBeNull();
    expect(estimateDeckBracket).not.toHaveBeenCalled();
  });

  it("sends all five deck sections and the declared tier", async () => {
    mockEstimateOutcome();
    const { result } = renderHook(() =>
      useBracketEstimate({ ...baseOptions, declaredTier: "optimized" }),
    );

    await waitFor(() => expect(result.current.estimate).toBe(mockEstimate));
    expect(estimateDeckBracket).toHaveBeenCalledWith({
      deck: {
        commander: ["Krenko, Mob Boss"],
        main_deck: ["Smothering Tithe", "Forest", "Forest"],
        sideboard: ["Pyroblast"],
        companion: ["Lutri, the Spellchaser"],
        signature_spell: ["Lightning Bolt"],
      },
      declared_tier: "optimized",
    });
  });

  it("debounces rapid deck updates into one service call", async () => {
    vi.useFakeTimers();
    mockEstimateOutcome();
    const { rerender } = renderHook(
      ({ currentDeck }) => useBracketEstimate({ ...baseOptions, deck: currentDeck }),
      { initialProps: { currentDeck: deck } },
    );
    rerender({ currentDeck: { ...deck, main: [{ name: "Island", count: 1 }] } });
    rerender({ currentDeck: { ...deck, main: [{ name: "Plains", count: 1 }] } });

    await act(async () => vi.advanceTimersByTime(200));
    expect(estimateDeckBracket).toHaveBeenCalledTimes(1);
  });

  it("uses the cache when identical input is rendered again", async () => {
    mockEstimateOutcome();
    const { rerender } = renderHook((options) => useBracketEstimate(options), {
      initialProps: baseOptions,
    });
    await waitFor(() => expect(estimateDeckBracket).toHaveBeenCalledTimes(1));
    rerender(baseOptions);
    await new Promise((resolve) => setTimeout(resolve, 250));
    expect(estimateDeckBracket).toHaveBeenCalledTimes(1);
  });

  it("discards a stale result when a newer request resolves first", async () => {
    const firstEstimate = { ...mockEstimate, tier: "core" as const };
    const secondEstimate = { ...mockEstimate, tier: "optimized" as const };
    let resolveFirst!: (value: { kind: "estimate"; estimate: BracketEstimate }) => void;
    const first = new Promise<{ kind: "estimate"; estimate: BracketEstimate }>((resolve) => {
      resolveFirst = resolve;
    });
    vi.mocked(estimateDeckBracket)
      .mockReturnValueOnce(first)
      .mockResolvedValueOnce({ kind: "estimate", estimate: secondEstimate });

    const { result, rerender } = renderHook(
      ({ currentDeck }) => useBracketEstimate({ ...baseOptions, deck: currentDeck }),
      { initialProps: { currentDeck: { ...deck, main: [{ name: "A", count: 1 }] } } },
    );
    await waitFor(() => expect(estimateDeckBracket).toHaveBeenCalledTimes(1));
    rerender({ currentDeck: { ...deck, main: [{ name: "B", count: 1 }] } });
    await waitFor(() => expect(result.current.estimate?.tier).toBe("optimized"));

    resolveFirst({ kind: "estimate", estimate: firstEstimate });
    await act(async () => Promise.resolve());
    expect(result.current.estimate?.tier).toBe("optimized");
  });

  it("propagates card-data-unavailable and leaves the estimate null", async () => {
    vi.mocked(estimateDeckBracket).mockResolvedValue({
      kind: "card-data-unavailable",
      reason: "card database failed",
    });
    const { result } = renderHook(() => useBracketEstimate(baseOptions));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.outcome).toEqual({
      kind: "card-data-unavailable",
      reason: "card database failed",
    });
    expect(result.current.estimate).toBeNull();
  });

  it("changing only declaredTier refires the service exactly once", async () => {
    mockEstimateOutcome();
    const { rerender } = renderHook(
      ({ declaredTier }) => useBracketEstimate({ ...baseOptions, declaredTier }),
      { initialProps: { declaredTier: "core" as CommanderBracketTier } },
    );
    await waitFor(() => expect(estimateDeckBracket).toHaveBeenCalledTimes(1));

    rerender({ declaredTier: "optimized" as const });
    await waitFor(() => expect(estimateDeckBracket).toHaveBeenCalledTimes(2));
    expect(estimateDeckBracket).toHaveBeenLastCalledWith(
      expect.objectContaining({ declared_tier: "optimized" }),
    );
  });

  it("shares an in-flight result across hook instances", async () => {
    mockEstimateOutcome();
    renderHook(() => useBracketEstimate(baseOptions));
    renderHook(() => useBracketEstimate(baseOptions));

    await waitFor(() => expect(estimateDeckBracket).toHaveBeenCalledTimes(1));
  });
});
