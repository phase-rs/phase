import { existsSync } from "node:fs";
import { readdir, readFile } from "node:fs/promises";
import { resolve } from "node:path";

/**
 * Location of the committed integration card fixture: a directory of JSON Lines
 * shards (`a.jsonl` .. `z.jsonl`, `_digits.jsonl`, `_other.jsonl`), one
 * `{"key":...,"card":...}` record per line. This module is the one place the
 * client tests learn that layout; the Rust side is
 * `crates/engine/src/database/integration_fixture.rs`.
 */
export const INTEGRATION_CARDS_PATH = resolve(
  __dirname,
  "../../../../crates/engine/tests/fixtures/integration_cards",
);

/** Whether the committed fixture is present in this checkout. */
export function integrationCardsExist(): boolean {
  return existsSync(INTEGRATION_CARDS_PATH);
}

/** The fixture's cards, keyed by lowercase card name. */
export async function readIntegrationCards(): Promise<Record<string, unknown>> {
  const names = (await readdir(INTEGRATION_CARDS_PATH)).filter((name) => name.endsWith(".jsonl")).sort();
  const cards: Record<string, unknown> = {};
  for (const name of names) {
    const text = await readFile(resolve(INTEGRATION_CARDS_PATH, name), "utf8");
    for (const [index, line] of text.split("\n").entries()) {
      if (line.trim() === "") continue;
      const record = JSON.parse(line) as { key: string; card: unknown };
      if (Object.prototype.hasOwnProperty.call(cards, record.key)) throw new Error(`${name}:${index + 1}: duplicate key ${record.key}`);
      cards[record.key] = record.card;
    }
  }
  return cards;
}

/** The fixture as one JSON object text (`{"<key>":<card>,...}`). */
export async function readIntegrationCardsJson(): Promise<string> {
  return JSON.stringify(await readIntegrationCards());
}
