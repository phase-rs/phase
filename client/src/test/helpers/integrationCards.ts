import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { gunzipSync } from "node:zlib";

/**
 * Location of the committed integration card fixture. This module is the one
 * place the client tests learn its on-disk layout; the Rust side is
 * `crates/engine/src/database/integration_fixture.rs`.
 */
export const INTEGRATION_CARDS_PATH = resolve(
  __dirname,
  "../../../../crates/engine/tests/fixtures/integration_cards.json.gz",
);

/** Whether the committed fixture is present in this checkout. */
export function integrationCardsExist(): boolean {
  return existsSync(INTEGRATION_CARDS_PATH);
}

/** The fixture as one JSON object text (`{"<key>":<card>,...}`). */
export async function readIntegrationCardsJson(): Promise<string> {
  return gunzipSync(await readFile(INTEGRATION_CARDS_PATH)).toString("utf8");
}
