import { describe, expect, it, vi } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useBracketEstimate } from "../useBracketEstimate";
import type { BracketEstimate } from "../../adapter/types";
import type { ParsedDeck } from "../../services/deckParser";

const mockEstimate: BracketEstimate = {
  tier: "upgraded",
  axes: { game_changers: 1, mass_land_denial: 0, extra_turns: 0, efficient_tutors: 2 },
  contributing: {
    game_changers: ["Smothering Tithe"],
    mass_land_denial: [],
    extra_turns: [],
    efficient_tutors: ["Demonic Tutor", "Vampiric Tutor"],
  },
  violations: [],
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
});
