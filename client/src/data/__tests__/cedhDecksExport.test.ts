import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import committedExport from "../../../../data/bundled_cedh_decks.json";
import { BUNDLED_CEDH_DECKS } from "../cedhDecks";

const EXPORT_COMMAND =
  "node --experimental-strip-types scripts/export-bundled-cedh-decks.mjs";
const CAVEAT_TEXT =
  "The curated portion is demo-quality (not a real tournament list) and the remainder is padded with Plains so the picker always has a well-formed 100-card deck to hand to the engine.";

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
  const decks = Object.fromEntries(
    Object.entries(BUNDLED_CEDH_DECKS)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([id, deck]) => [
        id,
        {
          code: deck.code,
          name: deck.name,
          type: deck.type,
          commander: deck.commander ?? [],
          mainBoard: deck.mainBoard,
        },
      ]),
  );

  return sortObjectKeys(decks);
}

describe("bundled cEDH deck export", () => {
  it("matches the TypeScript source exactly", () => {
    expect(
      committedExport,
      `Bundled cEDH export drifted; regenerate it with: ${EXPORT_COMMAND}`,
    ).toEqual(projectBundledDecks());
  });

  it("contains the complete non-empty bundled population", () => {
    const decks = Object.entries(committedExport);
    expect(decks).toHaveLength(3);
    for (const [id, deck] of decks) {
      expect(deck.type, `${id} must remain a Commander deck`).toBe("Commander Deck");
      expect(deck.commander.length, `${id} must have a commander`).toBeGreaterThan(0);
      for (const commander of deck.commander) {
        expect(commander.name.trim(), `${id} has an empty commander name`).not.toBe("");
      }
    }
  });

  it("preserves the demo-quality caveat in the TypeScript source", () => {
    const source = readFileSync(resolve("src/data/cedhDecks.ts"), "utf8")
      .replace(/^\s*\*\s?/gm, " ")
      .replace(/\s+/g, " ");

    expect(source).toContain(CAVEAT_TEXT);
  });
});
