import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ParsedDeck } from "../../services/deckParser";
import { getDeckSignals } from "../../services/deckSignals";
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
});
