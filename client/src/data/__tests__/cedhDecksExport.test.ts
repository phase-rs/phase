import { describe, expect, it } from "vitest";

import committedExport from "../../../../data/bundled_cedh_decks.json";
import { BUNDLED_CEDH_DECKS } from "../cedhDecks";

const EXPORT_COMMAND =
  "node --experimental-strip-types scripts/export-bundled-cedh-decks.mjs";
const NOTE =
  "The curated portions are demo-quality (not real tournament lists), and the remainders are padded with basic lands so the picker always has well-formed 100-card decks to hand to the engine.";

function sortObjectKeys(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(sortObjectKeys);
  }
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value)
        .sort(([left], [right]) => left.localeCompare(right))
        .map(([key, child]) => [key, sortObjectKeys(child)]),
    );
  }
  return value;
}

function projectBundledDecks() {
  const decks = Object.entries(BUNDLED_CEDH_DECKS)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([id, deck]) => ({
      id,
      display_name: deck.name,
      bracket: 5,
      commander: deck.commander ?? [],
      main_deck: deck.mainBoard,
    }));

  return sortObjectKeys({ decks, note: NOTE });
}

describe("bundled cEDH deck export", () => {
  it("matches the TypeScript source exactly", () => {
    expect(
      committedExport,
      `Bundled cEDH export drifted; regenerate it with: ${EXPORT_COMMAND}`,
    ).toEqual(projectBundledDecks());
  });

  it("contains the complete non-empty bundled population", () => {
    expect(committedExport.decks).toHaveLength(3);
    for (const deck of committedExport.decks) {
      expect(deck.commander.length, `${deck.id} must have a commander`).toBeGreaterThan(0);
      for (const commander of deck.commander) {
        expect(commander.name.trim(), `${deck.id} has an empty commander name`).not.toBe("");
      }
    }
  });
});
