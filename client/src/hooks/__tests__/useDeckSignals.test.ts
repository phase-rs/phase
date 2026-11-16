import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ParsedDeck } from "../../services/deckParser";
import { getDeckSignals, type DeckSignalsOutcome } from "../../services/deckSignals";
import type { DeckSignals } from "../../types/deckSignals";
import { clearDeckSignalsCache, useDeckSignals } from "../useDeckSignals";

vi.mock("../../services/deckSignals", () => ({ getDeckSignals: vi.fn() }));

const signals: DeckSignals = {
  readings: {
    counterspells: { count: 1, contributing: ["Counterspell"] },
    spot_removal: { count: 0, contributing: [] },
    sweepers: { count: 0, contributing: [] },
    card_advantage: { count: 0, contributing: [] },
    free_interaction: { count: 0, contributing: [] },
    mana_producers: { count: 0, contributing: [] },
    land_fetch: { count: 0, contributing: [] },
    rituals: { count: 0, contributing: [] },
    extra_land_drops: { count: 0, contributing: [] },
  },
  nonland_cards: 2,
  average_mana_value_centi: 250,
  resolved_cards: 2,
  unresolved_cards: 0,
};

const deck: ParsedDeck = {
  main: [{ name: "Counterspell", count: 2 }],
  sideboard: [{ name: "Negate", count: 1 }],
  companion: "Lutri, the Spellchaser",
  signature_spell: ["Brainstorm"],
};

const baseOptions = {
  deck,
  commanders: ["Talrand, Sky Summoner"],
  format: "Commander" as const,
};

const otherDeck: ParsedDeck = { ...deck, main: [{ name: "Brainstorm", count: 2 }] };
const otherSignals: DeckSignals = { ...signals, nonland_cards: 7 };

/** Longer than the hook's 200ms debounce. */
const pastDebounce = () => act(() => new Promise<void>((resolve) => setTimeout(resolve, 250)));

/** Renders `deck`, waits for its signals to settle, then edits the deck to `otherDeck`. */
async function settleThenEdit() {
  const hook = renderHook(({ deck: current }) => useDeckSignals({ ...baseOptions, deck: current }), {
    initialProps: { deck },
  });
  await waitFor(() => expect(hook.result.current.signals).toBe(signals));
  hook.rerender({ deck: otherDeck });
  expect(hook.result.current.loading).toBe(true);
  return hook;
}

describe("useDeckSignals", () => {
  afterEach(() => {
    clearDeckSignalsCache();
    vi.clearAllMocks();
  });

  it("returns signals for a Commander deck", async () => {
    vi.mocked(getDeckSignals).mockResolvedValue({ kind: "signals", signals });
    const { result } = renderHook(() => useDeckSignals(baseOptions));

    await waitFor(() => expect(result.current.signals).toBe(signals));
    expect(result.current.outcome).toEqual({ kind: "signals", signals });
  });

  it("returns null for a non-Commander format", async () => {
    vi.mocked(getDeckSignals).mockResolvedValue({ kind: "signals", signals });
    const { result } = renderHook(() =>
      useDeckSignals({ ...baseOptions, format: "Standard" }),
    );

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.signals).toBeNull();
    expect(result.current.outcome).toBeNull();
    expect(getDeckSignals).not.toHaveBeenCalled();
  });

  it("sends combo_declaration undeclared and commander from the argument", async () => {
    vi.mocked(getDeckSignals).mockResolvedValue({ kind: "signals", signals });
    renderHook(() => useDeckSignals(baseOptions));

    await waitFor(() => expect(getDeckSignals).toHaveBeenCalledTimes(1));
    expect(getDeckSignals).toHaveBeenCalledWith({
      commander: ["Talrand, Sky Summoner"],
      main_deck: ["Counterspell", "Counterspell"],
      sideboard: ["Negate"],
      companion: ["Lutri, the Spellchaser"],
      signature_spell: ["Brainstorm"],
      combo_declaration: { kind: "undeclared" },
    });
  });

  it("restores the settled deck when an edit is undone before its debounce fires", async () => {
    vi.mocked(getDeckSignals).mockResolvedValue({ kind: "signals", signals });
    const { result, rerender } = await settleThenEdit();

    rerender({ deck });

    expect(result.current.loading).toBe(false);
    expect(result.current.signals).toBe(signals);
    expect(result.current.outcome).toEqual({ kind: "signals", signals });
    await pastDebounce();
    expect(getDeckSignals).toHaveBeenCalledTimes(1);
    expect(result.current.loading).toBe(false);
  });

  it("drops an undone edit's in-flight result instead of showing it on the settled deck", async () => {
    let resolveOther: (outcome: DeckSignalsOutcome) => void = () => {};
    vi.mocked(getDeckSignals)
      .mockResolvedValueOnce({ kind: "signals", signals })
      .mockReturnValueOnce(new Promise((resolve) => { resolveOther = resolve; }));
    const { result, rerender } = await settleThenEdit();
    await waitFor(() => expect(getDeckSignals).toHaveBeenCalledTimes(2));

    rerender({ deck });
    expect(result.current.loading).toBe(false);
    await act(async () => {
      resolveOther({ kind: "signals", signals: otherSignals });
    });

    expect(result.current.signals).toBe(signals);
    expect(result.current.outcome).toEqual({ kind: "signals", signals });
    expect(result.current.loading).toBe(false);
  });
});
