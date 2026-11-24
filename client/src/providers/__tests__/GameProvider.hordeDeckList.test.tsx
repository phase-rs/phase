import type { TFunction } from "i18next";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { FormatConfig } from "../../adapter/types";
import { buildLegalAiDeckCatalog, type AiDeckCandidate } from "../../services/aiDeckCatalog";
import type { ParsedDeck } from "../../services/deckParser";
import type { PrintingEntry } from "../../services/scryfall";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { UNDECLARED_COMBO } from "../../types/bracketEstimate";
import { buildLocalAiDeckList } from "../GameProvider";

const candidate = (id: string, name: string, card: string): AiDeckCandidate => ({
  id,
  name,
  source: { type: "saved" },
  deck: { main: [{ count: 60, name: card }], sideboard: [] },
  coveragePct: null,
  archetype: null,
  bracket: null,
  declaredBracket: null,
  comboDeclaration: UNDECLARED_COMBO,
});

vi.mock("../../services/aiDeckCatalog", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../services/aiDeckCatalog")>(),
  buildLegalAiDeckCatalog: vi.fn(async () => ({
    candidates: [
      candidate("deck-a", "Deck A", "Plains"),
      candidate("deck-b", "Deck B", "Island"),
    ],
  })),
}));

vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({
    getCardFaceData: async (name: string) => ({ scryfall_oracle_id: name }),
    checkDeckPrintingsBounds: async () => {},
  }),
}));

vi.mock("../../services/scryfall", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../services/scryfall")>(),
  getCardPrintings: async (oracleId: string) => printingsByOracleId[oracleId] ?? [],
}));

const printingsByOracleId: Record<string, PrintingEntry[]> = {
  Forest: [printing("one", "276"), printing("dmu", "277")],
  Plains: [printing("one", "262")],
};

function printing(set: string, collector_number: string): PrintingEntry {
  return {
    id: `${set}-${collector_number}`, set, set_name: set, collector_number,
    released_at: "", border_color: "black", frame_effects: [], full_art: false, faces: [],
  };
}

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
  beforeEach(() => {
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
  });

  it("preserves both imported printings of the human survivor's Forests", async () => {
    const deck: ParsedDeck = {
      main: [
        { count: 4, name: "Forest", sourcePrinting: { setCode: "ONE", collectorNumber: "276" } },
        { count: 3, name: "Forest", sourcePrinting: { setCode: "DMU", collectorNumber: "277" } },
      ],
      sideboard: [],
    };
    const payload = await buildLocalAiDeckList(
      t,
      deck,
      2,
      hordeConfig(),
      undefined,
      null,
      UNDECLARED_COMBO,
      { type: "Default" },
      [],
    );

    expect(payload.player.main_deck).toEqual(Array<string>(7).fill("Forest"));
    expect(payload.player.printings).toEqual([
      { section: "main_deck", name: "Forest", count: 4, printing: { set_code: "one", collector_number: "276" } },
      { section: "main_deck", name: "Forest", count: 3, printing: { set_code: "dmu", collector_number: "277" } },
    ]);
  });

  it("preserves the allied AI survivor's printing and leaves Horde seats empty", async () => {
    const ally = candidate("deck-a", "Deck A", "Plains");
    ally.deck.main[0].sourcePrinting = { setCode: "ONE", collectorNumber: "262" };
    vi.mocked(buildLegalAiDeckCatalog).mockResolvedValueOnce({ candidates: [ally], allCandidates: [ally] });

    const payload = await buildLocalAiDeckList(
      t,
      humanDeck,
      4,
      hordeConfig(["SarumanHorde"]),
      undefined,
      null,
      UNDECLARED_COMBO,
      { type: "Default" },
      [],
    );

    expect(payload.opponent.main_deck).toEqual([]);
    expect(payload.opponent).not.toHaveProperty("printings");
    expect(payload.ai_decks).toHaveLength(2);
    expect(payload.ai_decks[0].main_deck).toEqual([]);
    expect(payload.ai_decks[0]).not.toHaveProperty("printings");
    expect(payload.ai_decks[1].main_deck).toEqual(Array<string>(60).fill("Plains"));
    expect(payload.ai_decks[1].printings).toEqual([
      { section: "main_deck", name: "Plains", count: 60, printing: { set_code: "one", collector_number: "262" } },
    ]);
  });

  it("gives the allied AI survivor a real deck in a three-seat game", async () => {
    const payload = await buildLocalAiDeckList(
      t,
      humanDeck,
      3,
      hordeConfig(),
      undefined,
      null,
      UNDECLARED_COMBO,
      { type: "Default" },
      [],
    );
    // Seat map: player 0 = human survivor, opponent 1 = Horde primary,
    // ai_decks[0] = seat 2 = allied AI survivor.
    expect(payload.player.main_deck).toHaveLength(60);
    expect(payload.opponent.main_deck).toHaveLength(0);
    expect(payload.ai_decks).toHaveLength(1);
    expect(payload.ai_decks[0].main_deck).toHaveLength(60);
    expect(payload.player).not.toHaveProperty("printings");
    expect(payload.ai_decks[0]).not.toHaveProperty("printings");
  });

  it("keeps the co-Horde seat empty but decks later survivors (Two Towers)", async () => {
    const payload = await buildLocalAiDeckList(
      t,
      humanDeck,
      4,
      hordeConfig(["SarumanHorde"]),
      undefined,
      null,
      UNDECLARED_COMBO,
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
      UNDECLARED_COMBO,
      { type: "Default" },
      [],
    );
    expect(payload.player.main_deck).toHaveLength(60);
    expect(payload.opponent.main_deck).toHaveLength(0);
    expect(payload.ai_decks).toHaveLength(0);
  });
});
