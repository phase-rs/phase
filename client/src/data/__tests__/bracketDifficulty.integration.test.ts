import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { beforeAll, describe, expect, it } from "vitest";

import init, { getBracketDifficultyTable } from "@wasm/engine";
import { BRACKET_DIFFICULTY_DEFAULT } from "../bracketDifficulty";
import type { AIDifficulty } from "../../constants/ai";
import type { CommanderBracketTier } from "../../types/bracketEstimate";

/** Drift check for the synchronous client mirror. Requires build-wasm.sh. */
async function initWasm() {
  const bytes = await readFile(resolve(__dirname, "../../wasm/engine_wasm_bg.wasm"));
  const module = await WebAssembly.compile(bytes);
  await init({ module_or_path: module });
}

describe("BRACKET_DIFFICULTY_DEFAULT (engine drift check)", () => {
  beforeAll(initWasm);

  it("matches the WASM table", () => {
    expect(
      getBracketDifficultyTable() as Record<CommanderBracketTier, AIDifficulty>,
    ).toEqual(BRACKET_DIFFICULTY_DEFAULT);
  });
});
