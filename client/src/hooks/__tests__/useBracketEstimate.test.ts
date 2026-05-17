import { afterEach, describe, expect, it, vi } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useBracketEstimate, clearBracketEstimateCache } from "../useBracketEstimate";
import { AdapterError, AdapterErrorCode } from "../../adapter/types";
import type { BracketEstimate } from "../../types/bracket";
import type { ParsedDeck } from "../../services/deckParser";

const mockEstimate: BracketEstimate = {
  tier: "upgraded",
  axes: { game_changers: 1, mass_land_denial: 0, extra_turns: 0, efficient_tutors: 2 },
  axis_caps_at_tier: { game_changers: 3, mass_land_denial: 0, extra_turns: null, efficient_tutors: null },
  contributing: {
    game_changers: ["Smothering Tithe"],
    mass_land_denial: [],
    extra_turns: [],
    efficient_tutors: ["Demonic Tutor", "Vampiric Tutor"],
  },
  violations: {},
  data_version: "test-1",
};

const makeAdapter = (estimate: BracketEstimate | null = mockEstimate) => ({
  estimateBracket: vi.fn().mockResolvedValue(estimate),
});

const deck: ParsedDeck = {
  main: [
    { name: "Smothering Tithe", count: 1 },
    { name: "Demonic Tutor", count: 1 },
    { name: "Vampiric Tutor", count: 1 },
    { name: "Forest", count: 30 },
  ],
  sideboard: [],
};

describe("useBracketEstimate", () => {
  afterEach(() => clearBracketEstimateCache());

  it("returns null when format is not Commander", async () => {
    const adapter = makeAdapter();
    const { result } = renderHook(() =>
      useBracketEstimate({ deck, commanders: ["Atraxa"], format: "Standard", adapter }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.estimate).toBeNull();
    expect(adapter.estimateBracket).not.toHaveBeenCalled();
  });

  it("returns null when no commander is selected", async () => {
    const adapter = makeAdapter();
    const { result } = renderHook(() =>
      useBracketEstimate({ deck, commanders: [], format: "Commander", adapter }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.estimate).toBeNull();
    expect(adapter.estimateBracket).not.toHaveBeenCalled();
  });

  it("returns an estimate for a Commander deck", async () => {
    const adapter = makeAdapter();
    const { result } = renderHook(() =>
      useBracketEstimate({ deck, commanders: ["Atraxa"], format: "Commander", adapter }),
    );
    await waitFor(() => expect(result.current.estimate).not.toBeNull());
    expect(result.current.estimate?.tier).toBe("upgraded");
    expect(adapter.estimateBracket).toHaveBeenCalledTimes(1);
  });

  it("debounces rapid deck updates into a single call", async () => {
    vi.useFakeTimers();
    const adapter = makeAdapter();
    const { rerender } = renderHook(
      ({ deck }) =>
        useBracketEstimate({ deck, commanders: ["Atraxa"], format: "Commander", adapter }),
      { initialProps: { deck } },
    );
    rerender({ deck: { ...deck, main: [...deck.main, { name: "Island", count: 1 }] } });
    rerender({ deck: { ...deck, main: [...deck.main, { name: "Plains", count: 1 }] } });
    await act(async () => { vi.advanceTimersByTime(200); });
    expect(adapter.estimateBracket).toHaveBeenCalledTimes(1);
    vi.useRealTimers();
  });

  it("memoizes by deck hash + data version (no re-call on identical input)", async () => {
    const adapter = makeAdapter();
    const props = { deck, commanders: ["Atraxa"], format: "Commander" as const, adapter };
    const { rerender } = renderHook((p) => useBracketEstimate(p), { initialProps: props });
    await new Promise((r) => setTimeout(r, 250));
    rerender(props);
    await new Promise((r) => setTimeout(r, 250));
    expect(adapter.estimateBracket).toHaveBeenCalledTimes(1);
  });

  it("discards a stale async result when a newer call supersedes it", async () => {
    // Two adapter calls: first one resolves slowly, second resolves fast.
    // The hook should display the second result, not the first.
    const firstEstimate: BracketEstimate = {
      ...mockEstimate,
      tier: "core",
      contributing: { ...mockEstimate.contributing, game_changers: ["FIRST"] },
    };
    const secondEstimate: BracketEstimate = {
      ...mockEstimate,
      tier: "optimized",
      contributing: { ...mockEstimate.contributing, game_changers: ["SECOND"] },
    };

    let resolveFirst: (v: BracketEstimate) => void = () => {};
    const firstPromise = new Promise<BracketEstimate>((r) => {
      resolveFirst = r;
    });
    const secondPromise = Promise.resolve(secondEstimate);

    const adapter = {
      estimateBracket: vi
        .fn()
        .mockImplementationOnce(() => firstPromise)
        .mockImplementationOnce(() => secondPromise),
    };

    // Render with deck v1.
    const deckV1: ParsedDeck = { main: [{ name: "A", count: 1 }], sideboard: [] };
    const deckV2: ParsedDeck = { main: [{ name: "B", count: 1 }], sideboard: [] };

    const { result, rerender } = renderHook(
      ({ deck: d }: { deck: ParsedDeck }) =>
        useBracketEstimate({
          deck: d,
          commanders: ["Atraxa"],
          format: "Commander",
          adapter,
        }),
      { initialProps: { deck: deckV1 } },
    );

    // Wait past debounce so the first call fires.
    await new Promise((r) => setTimeout(r, 250));
    expect(adapter.estimateBracket).toHaveBeenCalledTimes(1);

    // Now switch to deck v2 before the first call resolves.
    rerender({ deck: deckV2 });
    await new Promise((r) => setTimeout(r, 250));
    // Second call has fired and resolved with the optimized estimate.
    expect(adapter.estimateBracket).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(result.current.estimate?.tier).toBe("optimized"));

    // Now resolve the first promise late. It should be discarded.
    resolveFirst(firstEstimate);
    await new Promise((r) => setTimeout(r, 50));
    // Hook should still show the second (newer) estimate.
    expect(result.current.estimate?.tier).toBe("optimized");
    expect(result.current.estimate?.contributing.game_changers).toEqual(["SECOND"]);
  });

  it("returns an estimate for Brawl format (commander family)", async () => {
    const adapter = makeAdapter();
    renderHook(() =>
      useBracketEstimate({ deck, commanders: ["Atraxa"], format: "Brawl", adapter }),
    );
    await waitFor(() => expect(adapter.estimateBracket).toHaveBeenCalledTimes(1));
  });

  it("returns null for undefined format", async () => {
    const adapter = makeAdapter();
    const { result } = renderHook(() =>
      useBracketEstimate({ deck, commanders: ["Atraxa"], format: undefined, adapter }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.estimate).toBeNull();
    expect(adapter.estimateBracket).not.toHaveBeenCalled();
  });

  it("flags `unsupported` when the adapter throws BRACKET_ESTIMATION_UNSUPPORTED", async () => {
    const adapter = {
      estimateBracket: vi.fn().mockRejectedValue(
        new AdapterError(
          AdapterErrorCode.BRACKET_ESTIMATION_UNSUPPORTED,
          "Not available in this build",
          false,
        ),
      ),
    };
    // Use a unique deck so the module cache doesn't satisfy this from a
    // sibling test's resolved promise.
    const uniqueDeck: ParsedDeck = {
      main: [{ name: `Unsupported-${Date.now()}`, count: 1 }],
      sideboard: [],
    };
    const { result } = renderHook(() =>
      useBracketEstimate({ deck: uniqueDeck, commanders: ["Atraxa"], format: "Commander", adapter }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.unsupported).toBe(true);
    expect(result.current.estimate).toBeNull();
  });

  it("does not flag `unsupported` for generic adapter failures", async () => {
    const adapter = {
      estimateBracket: vi.fn().mockRejectedValue(new Error("boom")),
    };
    const uniqueDeck: ParsedDeck = {
      main: [{ name: `Generic-${Date.now()}`, count: 1 }],
      sideboard: [],
    };
    const { result } = renderHook(() =>
      useBracketEstimate({ deck: uniqueDeck, commanders: ["Atraxa"], format: "Commander", adapter }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.unsupported).toBe(false);
    expect(result.current.estimate).toBeNull();
  });

  it("shares results across hook instances via module-level cache", async () => {
    const adapter = makeAdapter();
    // Use a deck that the cache hasn't seen — a unique fingerprint per test run.
    const uniqueDeck: ParsedDeck = {
      main: [{ name: `Unique-${Date.now()}`, count: 1 }],
      sideboard: [],
    };
    const props = {
      deck: uniqueDeck,
      commanders: ["Atraxa"],
      format: "Commander" as const,
      adapter,
    };
    renderHook(() => useBracketEstimate(props));
    renderHook(() => useBracketEstimate(props));
    await new Promise((r) => setTimeout(r, 250));
    expect(adapter.estimateBracket).toHaveBeenCalledTimes(1);
  });
});
