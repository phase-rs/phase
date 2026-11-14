import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { beforeAll, beforeEach, describe, expect, it } from "vitest";

import init, { export_game_state_json, initialize_game, load_card_database } from "@wasm/engine";

import { STORAGE_KEY_PREFIX } from "../../constants/storage";
import { formatMetadata } from "../../data/formatRegistry";
import type { GameFormat } from "../../adapter/types";
import { emptySeatDeck, pileSeatDeck, type PileSource } from "../pileSource";

/**
 * The host's pile reaches the real engine through `pileSeatDeck`.
 *
 * Integration lane — no CI job runs this file. Run it by hand with
 * `cd client && npx vitest run --config vitest.integration.config.ts
 * --coverage.enabled=false src/services/__tests__/pileSource.integration.test.ts`.
 * Both inputs are gitignored build outputs, so the suite self-skips when absent.
 */

const WASM_PATH = resolve(__dirname, "../../wasm/engine_wasm_bg.wasm");
const CARD_DATA_PATH = resolve(__dirname, "../../../public/card-data.json");
const INPUTS_PRESENT = existsSync(WASM_PATH) && existsSync(CARD_DATA_PATH);

const PILE_ENTRIES = [
  { name: "Forest", count: 40 },
  { name: "Island", count: 20 },
  { name: "Llanowar Elves", count: 10 },
  { name: "Grizzly Bears", count: 10 },
];
const NAMED: PileSource = { type: "SavedDeck", name: "Pile A" };

type InitResult = { error?: boolean; reasons?: string[] };

function start(format: GameFormat, player: ReturnType<typeof emptySeatDeck>): InitResult {
  return initialize_game(
    { player, opponent: emptySeatDeck(), ai_decks: [] },
    7,
    formatMetadata(format)!.default_config,
    { match_type: "Bo1" },
    2,
    0,
  ) as InitResult;
}

/** Every game object's name, from the unprojected state (the projection hides library names). */
function objectNames(): string[] {
  const { state } = JSON.parse(export_game_state_json()) as { state: { objects: Record<string, { name: string }> } };
  return Object.values(state.objects).map(({ name }) => name);
}

function counts(names: string[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const name of names) out[name] = (out[name] ?? 0) + 1;
  return out;
}

describe.skipIf(!INPUTS_PRESENT)("pileSeatDeck over the real engine", () => {
  beforeAll(async () => {
    const module = await WebAssembly.compile(await readFile(WASM_PATH));
    await init({ module_or_path: module });
    load_card_database(await readFile(CARD_DATA_PATH, "utf8"));
  }, 300_000);

  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem(`${STORAGE_KEY_PREFIX}Pile A`, JSON.stringify({ main: PILE_ENTRIES, sideboard: [] }));
  });

  it("plays a named Dandan pile as the shared library", async () => {
    const pile = await pileSeatDeck("Dandan", NAMED);
    const result = start("Dandan", pile!);
    expect(result.error, result.reasons?.join("; ")).not.toBe(true);
    const names = objectNames();
    expect(counts(names)).toEqual(Object.fromEntries(PILE_ENTRIES.map(({ name, count }) => [name, count])));
    expect(names).not.toContain("Dandân");
  }, 300_000);

  it("plays the engine's default pile for the default source", async () => {
    const result = start("Dandan", (await pileSeatDeck("Dandan", { type: "Default" }))!);
    expect(result.error, result.reasons?.join("; ")).not.toBe(true);
    expect(objectNames()).toContain("Dandân");
  }, 300_000);

  it("submits nothing for Momir even with a named source, which Momir would refuse", async () => {
    const result = start("Momir", (await pileSeatDeck("Momir", NAMED))!);
    expect(result.error, result.reasons?.join("; ")).not.toBe(true);

    const refused = start("Momir", (await pileSeatDeck("Dandan", NAMED))!);
    expect(refused.error).toBe(true);
    expect(refused.reasons?.length).toBeGreaterThan(0);
  }, 300_000);
});
