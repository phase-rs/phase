import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { beforeAll, describe, expect, it } from "vitest";

import init, { commanderCandidates, isCardCommanderEligibleForFormat, load_card_database } from "@wasm/engine";
import { expandParsedDeck } from "../deckParser";

const WASM_PATH = resolve(__dirname, "../../wasm/engine_wasm_bg.wasm");
const CARD_DATA_PATH = resolve(__dirname, "../../../public/card-data.json");
const INPUTS_PRESENT = existsSync(WASM_PATH) && existsSync(CARD_DATA_PATH);

describe.skipIf(!INPUTS_PRESENT)("commander candidates over the real engine", () => {
  beforeAll(async () => {
    const module = await WebAssembly.compile(await readFile(WASM_PATH));
    await init({ module_or_path: module });
    load_card_database(await readFile(CARD_DATA_PATH, "utf8"));
  }, 300_000);

  it("decodes both compositions, returns an empty array and rejects an invalid composition", () => {
    const name = "Rograkh, Son of Rohgahh";
    const request = {
      ...expandParsedDeck({
        main: [
          { name, count: 1, sourcePrinting: { setCode: "cmr", collectorNumber: "197" } },
          { name, count: 1, sourcePrinting: { setCode: "cmr", collectorNumber: "575" } },
        ],
        sideboard: [],
        commander: [name],
      }),
      selected_format: "CommanderDraft",
    };
    expect(commanderCandidates(request, "commanders-inside")).toEqual([name]);
    expect(commanderCandidates(request, "commanders-outside")).toEqual([]);
    expect(() => commanderCandidates(request, "invalid")).toThrow(/Invalid commander deck composition/);
  });

  it("preserves the single-card format eligibility export", () => {
    expect(isCardCommanderEligibleForFormat("Isamaru, Hound of Konda", "Commander")).toBe(true);
    expect(isCardCommanderEligibleForFormat("The One Ring", "Commander")).toBe(false);
  });
});
