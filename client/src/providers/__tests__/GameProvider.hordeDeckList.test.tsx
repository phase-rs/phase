import type { TFunction } from "i18next";
import { describe, expect, it, vi } from "vitest";

import type { FormatConfig } from "../../adapter/types";
import type { AiDeckCandidate } from "../../services/aiDeckCatalog";
import type { ParsedDeck } from "../../services/deckParser";
import { buildLocalAiDeckList } from "../GameProvider";

const candidate = (id: string, name: string, card: string): AiDeckCandidate => ({
  id,
  name,
  source: { type: "saved" },
  deck: { main: [{ count: 60, name: card }], sideboard: [] },
  coveragePct: null,
  archetype: null,
  bracket: null,
});

vi.mock("../../services/aiDeckCatalog", () => ({
  buildLegalAiDeckCatalog: vi.fn(async () => ({
    candidates: [
      candidate("deck-a", "Deck A", "Plains"),
      candidate("deck-b", "Deck B", "Island"),
    ],
  })),
}));

const t = ((key: string) => key) as unknown as TFunction<["game", "multiplayer"]>;
const humanDeck: ParsedDeck = {
  main: [{ count: 60, name: "Forest" }],
  sideboard: [],
};
const hordeConfig = (coHordes: string[] = []): FormatConfig =>
  ({
    format: "Horde",
    horde_ruleset: { co_horde_decks: coHordes },
  }) as unknown as FormatConfig;

describe("buildLocalAiDeckList Horde branch", () => {
  it("gives the allied AI survivor a real deck in a three-seat game", async () => {
    const payload = await buildLocalAiDeckList(
      t,
      humanDeck,
      3,
      hordeConfig(),
      undefined,
      null,
      { type: "Default" },
      [],
    );
    // Seat map: player 0 = human survivor, opponent 1 = Horde primary,
    // ai_decks[0] = seat 2 = allied AI survivor.
    expect(payload.player.main_deck).toHaveLength(60);
    expect(payload.opponent.main_deck).toHaveLength(0);
    expect(payload.ai_decks).toHaveLength(1);
    expect(payload.ai_decks[0].main_deck).toHaveLength(60);
  });

  it("keeps the co-Horde seat empty but decks later survivors (Two Towers)", async () => {
    const payload = await buildLocalAiDeckList(
      t,
      humanDeck,
      4,
      hordeConfig(["SarumanHorde"]),
      undefined,
      null,
      { type: "Default" },
      [],
    );
    // ai_decks[0] = seat 2 = co-Horde (engine injects the challenge
    // library); ai_decks[1] = seat 3 = allied AI survivor.
    expect(payload.opponent.main_deck).toHaveLength(0);
    expect(payload.ai_decks).toHaveLength(2);
    expect(payload.ai_decks[0].main_deck).toHaveLength(0);
    expect(payload.ai_decks[1].main_deck).toHaveLength(60);
  });

  it("leaves a two-seat game unchanged (no extra AI seats)", async () => {
    const payload = await buildLocalAiDeckList(
      t,
      humanDeck,
      2,
      hordeConfig(),
      undefined,
      null,
      { type: "Default" },
      [],
    );
    expect(payload.player.main_deck).toHaveLength(60);
    expect(payload.opponent.main_deck).toHaveLength(0);
    expect(payload.ai_decks).toHaveLength(0);
  });
});
