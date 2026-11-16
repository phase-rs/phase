/**
 * Export the TypeScript-authored bundled cEDH demos for Rust consumers.
 *
 * These curated portions are demo-quality (not real tournament lists), and
 * the remainders are padded with basic lands so the picker always has
 * well-formed 100-card decks to hand to the engine.
 *
 * Usage:
 *   node --experimental-strip-types scripts/export-bundled-cedh-decks.mjs
 */

import { writeFile } from "node:fs/promises";

import { BUNDLED_CEDH_DECKS } from "../client/src/data/cedhDecks.ts";

const OUTPUT_URL = new URL("../data/bundled_cedh_decks.json", import.meta.url);

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

// The former top-level note cannot live in the catalog map schema. Its caveat
// is preserved in this header and guarded at its source by the drift test.
const artifact = sortObjectKeys(decks);
await writeFile(OUTPUT_URL, `${JSON.stringify(artifact, null, 2)}\n`, "utf8");
