/**
 * Export the TypeScript-authored bundled cEDH demos for Rust consumers.
 *
 * Usage:
 *   node --experimental-strip-types scripts/export-bundled-cedh-decks.mjs
 */

import { writeFile } from "node:fs/promises";

import { BUNDLED_CEDH_DECKS } from "../client/src/data/cedhDecks.ts";

const OUTPUT_URL = new URL("../data/bundled_cedh_decks.json", import.meta.url);
const NOTE =
  "The curated portions are demo-quality (not real tournament lists), and the remainders are padded with basic lands so the picker always has well-formed 100-card decks to hand to the engine.";

function sortObjectKeys(value) {
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

const decks = Object.entries(BUNDLED_CEDH_DECKS)
  .sort(([left], [right]) => left.localeCompare(right))
  .map(([id, deck]) => ({
    id,
    display_name: deck.name,
    bracket: 5,
    commander: deck.commander ?? [],
    main_deck: deck.mainBoard,
  }));

const artifact = sortObjectKeys({ decks, note: NOTE });
await writeFile(OUTPUT_URL, `${JSON.stringify(artifact, null, 2)}\n`, "utf8");
