// Local render smoke test — no Discord credentials needed.
//   bun scripts/card-bot/smoke.ts "Lightning Bolt"
//   bun scripts/card-bot/smoke.ts "Snapcaster Mage" path/to/coverage-data.json
//
// Loads the local coverage export, renders the embed for a card, and prints the
// description with live ANSI so the terminal shows the same colors Discord will.

import type { CoverageEntry } from "./coverageData";
import { renderCardEmbed } from "./render";
import { lookupScryfall } from "./scryfall";

const name = process.argv[2] ?? "Lightning Bolt";
const file = process.argv[3] ?? "client/public/coverage-data.json";

const data = JSON.parse(await Bun.file(file).text()) as { cards: CoverageEntry[] };
const entry = data.cards.find(
  (c) => c.card_name?.toLowerCase() === name.toLowerCase(),
);
if (!entry) {
  console.error(`"${name}" not found in ${file}`);
  process.exit(1);
}

const scry = await lookupScryfall(entry.card_name);
const embed = renderCardEmbed(entry, scry, "release", {
  commit_short: "abc1234",
  mtgjson_date: "2026-04-18",
});

console.log("=== embed (description elided) ===");
console.log(JSON.stringify({ ...embed, description: "‹see below›" }, null, 2));
console.log("\n=== description, ANSI live ===\n");
console.log(embed.description);
