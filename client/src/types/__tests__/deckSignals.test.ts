import { describe, expect, it } from "vitest";

import { isDeckSignals } from "../deckSignals";

function completePayload() {
  return {
    readings: {
      counterspells: { count: 0, contributing: [] },
      spot_removal: { count: 0, contributing: [] },
      sweepers: { count: 0, contributing: [] },
      card_advantage: { count: 0, contributing: [] },
      free_interaction: { count: 0, contributing: [] },
      mana_producers: { count: 0, contributing: [] },
      land_fetch: { count: 0, contributing: [] },
      rituals: { count: 0, contributing: [] },
      extra_land_drops: { count: 0, contributing: [] },
    },
    nonland_cards: 0,
    average_mana_value_centi: null,
    resolved_cards: 0,
    unresolved_cards: 0,
  };
}

describe("isDeckSignals", () => {
  it("accepts a complete payload with a null average", () => {
    expect(isDeckSignals(completePayload())).toBe(true);
  });

  it("rejects a payload missing a kind key", () => {
    const payload = completePayload();
    const { rituals: _rituals, ...readings } = payload.readings;
    expect(isDeckSignals({ ...payload, readings })).toBe(false);
  });

  it("rejects a payload with the average field absent", () => {
    const { average_mana_value_centi: _average, ...payload } = completePayload();
    expect(isDeckSignals(payload)).toBe(false);
  });
});
