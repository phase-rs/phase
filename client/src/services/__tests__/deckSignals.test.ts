import { beforeEach, describe, expect, it, vi } from "vitest";

import { getSharedAdapter } from "../../adapter/wasm-adapter";
import type { BracketDeckRequest } from "../../types/bracket";
import type { DeckSignals } from "../../types/deckSignals";
import { getDeckSignals } from "../deckSignals";

vi.mock("../../adapter/wasm-adapter", () => ({ getSharedAdapter: vi.fn() }));

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
  nonland_cards: 1,
  average_mana_value_centi: 200,
  resolved_cards: 1,
  unresolved_cards: 0,
};

const deck: BracketDeckRequest = {
  commander: ["Talrand, Sky Summoner"],
  main_deck: ["Counterspell"],
  sideboard: [],
  companion: [],
  signature_spell: [],
  combo_declaration: { kind: "undeclared" },
};

const deckSignals = vi.fn();

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(getSharedAdapter).mockReturnValue({
    deckSignals,
  } as unknown as ReturnType<typeof getSharedAdapter>);
});

describe("getDeckSignals", () => {
  it("maps signals to a signals outcome", async () => {
    deckSignals.mockResolvedValue(signals);
    await expect(getDeckSignals(deck)).resolves.toEqual({ kind: "signals", signals });
    expect(deckSignals).toHaveBeenCalledWith(deck);
  });

  it("maps null to no-commander", async () => {
    deckSignals.mockResolvedValue(null);
    await expect(getDeckSignals(deck)).resolves.toEqual({ kind: "no-commander" });
  });

  it("maps a rejection to card-data-unavailable with its message", async () => {
    deckSignals.mockRejectedValue(new Error("card database unavailable"));
    await expect(getDeckSignals(deck)).resolves.toEqual({
      kind: "card-data-unavailable",
      reason: "card database unavailable",
    });
  });
});
