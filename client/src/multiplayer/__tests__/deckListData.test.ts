import { describe, expect, it } from "vitest";

import echoWire from "../../test/fixtures/seat-deck-echo-wire.json";
import { expandParsedDeck } from "../../services/deckParser";
import { DECK_LIST_FIELDS, deckListIdentityKey } from "../deckListData";
import type { DeckChoice, DeckListData, SeatKind } from "../seatTypes";

type DeckListChoice = Extract<DeckChoice, { type: "DeckList" }>;

/** The `DeckList` an engine-echoed AI seat holds, narrowed at runtime. */
function echoedDeckList(seat: SeatKind): DeckListChoice {
  if (seat.type !== "Ai" || seat.data.deck.type !== "DeckList") {
    throw new Error("the golden entry is not an AI seat holding a DeckList");
  }
  return seat.data.deck;
}

const echoes = {
  plain: echoedDeckList(echoWire.plain as SeatKind).data,
  companion: echoedDeckList(echoWire.companion as SeatKind).data,
  declaredTier: echoedDeckList(echoWire.declaredTier as SeatKind).data,
  forestIsland: echoedDeckList(echoWire.forestIsland as SeatKind).data,
  islandForest: echoedDeckList(echoWire.islandForest as SeatKind).data,
};

const islandDeck = expandParsedDeck({ main: [{ count: 1, name: "Island" }], sideboard: [] });

describe("DECK_LIST_FIELDS", () => {
  it("is the engine's DeckData list fields in declaration order", () => {
    const echoedKeys = Object.keys(echoes.plain);
    // Reach guard: the golden really is the engine's serialized deck.
    expect(echoedKeys).toContain("main_deck");
    expect(echoedKeys).toContain("bracket_tier");
    expect(echoedKeys.filter((key) => key !== "bracket_tier")).toEqual([...DECK_LIST_FIELDS]);
  });
});

describe("deckListIdentityKey", () => {
  it("equals the engine's echo for the client's own spelling of the same deck", () => {
    expect(deckListIdentityKey(islandDeck)).toBe(deckListIdentityKey(echoes.plain));
  });

  it("ignores object key order and the declared bracket tier", () => {
    const reordered: DeckListData = {
      bracket_tier: echoes.plain.bracket_tier,
      signature_spell: echoes.plain.signature_spell,
      sticker_sheets: echoes.plain.sticker_sheets,
      contraption_deck: echoes.plain.contraption_deck,
      scheme_deck: echoes.plain.scheme_deck,
      planar_deck: echoes.plain.planar_deck,
      attraction_deck: echoes.plain.attraction_deck,
      companion: echoes.plain.companion,
      commander: echoes.plain.commander,
      sideboard: echoes.plain.sideboard,
      main_deck: echoes.plain.main_deck,
    };
    expect(Object.keys(reordered)[0]).toBe("bracket_tier");
    expect(deckListIdentityKey(reordered)).toBe(deckListIdentityKey(echoes.plain));
    expect(echoes.plain.bracket_tier).toBe("core");
    expect(echoes.declaredTier.bracket_tier).toBe("optimized");
    expect(deckListIdentityKey(echoes.declaredTier)).toBe(deckListIdentityKey(echoes.plain));
  });

  it("reads an absent list as empty", () => {
    const { companion: _companion, ...withoutCompanion } = echoes.plain;
    expect(withoutCompanion).not.toHaveProperty("companion");
    expect(deckListIdentityKey(withoutCompanion)).toBe(deckListIdentityKey(echoes.plain));
  });

  it.each(DECK_LIST_FIELDS)("tells decks apart that differ only in %s", (field) => {
    const changed: DeckListData = { ...echoes.plain, [field]: ["Sentinel Card"] };
    expect(deckListIdentityKey(changed)).not.toBe(deckListIdentityKey(echoes.plain));
  });

  it("tells a deck with a companion from the same deck without one", () => {
    expect(echoes.companion.companion).toEqual(["Lurrus of the Dream-Den"]);
    expect(deckListIdentityKey(echoes.companion)).not.toBe(deckListIdentityKey(echoes.plain));
  });

  it("keeps list order significant", () => {
    expect(echoes.islandForest.main_deck).toEqual(["Island", "Forest"]);
    expect(deckListIdentityKey(echoes.forestIsland)).not.toBe(
      deckListIdentityKey(echoes.islandForest),
    );
  });

  it("counts duplicates", () => {
    const one: DeckListData = { ...echoes.plain, main_deck: ["Island"] };
    const two: DeckListData = { ...echoes.plain, main_deck: ["Island", "Island"] };
    expect(deckListIdentityKey(one)).not.toBe(deckListIdentityKey(two));
  });
});
