import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { beforeAll, describe, expect, it, vi } from "vitest";

import init, { canonicalCardNames, load_card_database } from "@wasm/engine";

/**
 * The engine's `canonical_name` query end to end, over the real card
 * database — no CI job runs this file. Run it by hand with
 * `cd client && npx vitest run --config vitest.integration.config.ts
 * --coverage.enabled=false
 * src/services/__tests__/canonicalCardNames.integration.test.ts`. Both inputs
 * it needs are gitignored build outputs, so the suite self-skips when they
 * are absent.
 */

const WASM_PATH = resolve(__dirname, "../../wasm/engine_wasm_bg.wasm");
const CARD_DATA_PATH = resolve(__dirname, "../../../public/card-data.json");
const INPUTS_PRESENT = existsSync(WASM_PATH) && existsSync(CARD_DATA_PATH);

const adapterStub = vi.hoisted(() => ({
  canonicalCardNames: vi.fn(),
}));

vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => adapterStub,
}));

describe.skipIf(!INPUTS_PRESENT)("canonicalCardNames — over the real engine", () => {
  beforeAll(async () => {
    const bytes = await readFile(WASM_PATH);
    const module = await WebAssembly.compile(bytes);
    await init({ module_or_path: module });
    load_card_database(await readFile(CARD_DATA_PATH, "utf8"));
    adapterStub.canonicalCardNames.mockImplementation(
      async (names: string[]) => canonicalCardNames(names),
    );
  }, 300_000);

  it("canonicalizes every bare-slash and legacy spaced spelling", async () => {
    const answer = canonicalCardNames([
      "Summon: Choco // Mog",
      "Revival/Revenge",
      "Revival",
      "Not A Real Card",
    ]);
    expect(answer).toEqual([
      "Summon: Choco/Mog",
      "Revival // Revenge",
      "Revival",
      null,
    ]);
  }, 300_000);

  it("canonicalizes a parsed deck's names over the real engine", async () => {
    const { canonicalizeDeckNames } = await import("../canonicalCardNames");
    const { parseMtgaDeck } = await import("../deckParser");

    const deck = parseMtgaDeck(
      "2 Revival/Revenge\n2 Revival // Revenge\n1 Summon: Choco // Mog",
    );
    const result = await canonicalizeDeckNames(deck);

    expect(result.main).toEqual([
      { count: 4, name: "Revival // Revenge" },
      { count: 1, name: "Summon: Choco/Mog" },
    ]);
  }, 300_000);
});
