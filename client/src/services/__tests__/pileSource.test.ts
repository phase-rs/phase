import { beforeEach, describe, expect, it, vi } from "vitest";

const engine = vi.hoisted(() => ({ deckSupplyForFormat: vi.fn() }));
vi.mock("../engineRuntime", () => engine);

import { STORAGE_KEY_PREFIX } from "../../constants/storage";
import { emptySeatDeck, pileSeatDeck, suppliedAiDeckChoice } from "../pileSource";

const PILE = { main: [{ name: "Island", count: 40 }, { name: "Dandân", count: 40 }], sideboard: [] };

describe("pileSeatDeck", () => {
  beforeEach(() => {
    localStorage.clear();
    engine.deckSupplyForFormat.mockReset();
    localStorage.setItem(`${STORAGE_KEY_PREFIX}Pile A`, JSON.stringify(PILE));
  });

  it("submits the expanded saved deck when the engine says the host supplies the pile", async () => {
    engine.deckSupplyForFormat.mockResolvedValue("HostPile");
    const deck = await pileSeatDeck("Dandan", { type: "SavedDeck", name: "Pile A" });
    expect(deck?.main_deck).toHaveLength(80);
    expect(deck?.main_deck.filter((name) => name === "Island")).toHaveLength(40);
    expect(engine.deckSupplyForFormat).toHaveBeenCalledWith("Dandan");
  });

  it("submits the empty deck for an engine-fixed format even with a named source", async () => {
    engine.deckSupplyForFormat.mockResolvedValue("EngineFixed");
    const deck = await pileSeatDeck("Momir", { type: "SavedDeck", name: "Pile A" });
    expect(deck).toEqual(emptySeatDeck());
    expect(engine.deckSupplyForFormat).toHaveBeenCalledWith("Momir");
  });

  it("submits the empty deck for the default pile without asking the engine", async () => {
    const deck = await pileSeatDeck("Dandan", { type: "Default" });
    expect(deck).toEqual(emptySeatDeck());
    expect(deck?.main_deck).toEqual([]);
    expect(engine.deckSupplyForFormat).not.toHaveBeenCalled();
  });

  it("answers null for a named pile that is no longer saved", async () => {
    engine.deckSupplyForFormat.mockResolvedValue("HostPile");
    expect(await pileSeatDeck("Dandan", { type: "SavedDeck", name: "Gone" })).toBeNull();
  });
});

describe("suppliedAiDeckChoice", () => {
  it("is an empty DeckList", () => {
    expect(suppliedAiDeckChoice().choice).toEqual({ type: "DeckList", data: emptySeatDeck() });
    expect(emptySeatDeck().main_deck).toEqual([]);
  });
});
