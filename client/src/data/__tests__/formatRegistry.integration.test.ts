import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { describe, it, expect, beforeAll } from "vitest";

import init, {
  getFormatRegistry,
  maxDeckCopies,
  sideboardPolicyForFormat,
} from "@wasm/engine";
import { FORMAT_REGISTRY } from "../formatRegistry";
import type { FormatMetadata } from "../../adapter/types";

/**
 * Drift-detection test: the TS `FORMAT_REGISTRY` is a hand-authored mirror of
 * the Rust `GameFormat::registry()`. This test loads the real WASM binary,
 * calls the engine's `getFormatRegistry` export, and verifies the shapes match
 * exactly. If this test fails, either the TS mirror or the Rust registry has
 * been updated without the other.
 *
 * Requires: ./scripts/build-wasm.sh to have been run.
 */

async function initWasm() {
  const wasmPath = resolve(__dirname, "../../wasm/engine_wasm_bg.wasm");
  const bytes = await readFile(wasmPath);
  const module = await WebAssembly.compile(bytes);
  await init({ module_or_path: module });
}

describe("FORMAT_REGISTRY (engine drift check)", () => {
  beforeAll(async () => {
    await initWasm();
  });

  it("TS mirror matches the Rust registry exactly", () => {
    const fromEngine = getFormatRegistry() as FormatMetadata[];

    // Same length: catches accidental omissions in either direction.
    expect(fromEngine.length).toBe(FORMAT_REGISTRY.length);

    // Same order: order matters because the frontend iterates the list to
    // render format pickers; reordering would shuffle the UI.
    for (let i = 0; i < FORMAT_REGISTRY.length; i++) {
      expect(fromEngine[i]).toEqual(FORMAT_REGISTRY[i]);
    }
  });

  it("sideboardPolicyForFormat resolves from a full FormatConfig, not a bare GameFormat", () => {
    const standard = FORMAT_REGISTRY.find((m) => m.format === "Standard")!;
    const commander = FORMAT_REGISTRY.find((m) => m.format === "Commander")!;
    const tinyLeaders = FORMAT_REGISTRY.find((m) => m.format === "TinyLeaders")!;

    expect(sideboardPolicyForFormat(standard.default_config)).toEqual({
      type: "Limited",
      data: 15,
    });
    expect(sideboardPolicyForFormat(commander.default_config)).toEqual({
      type: "Forbidden",
    });
    expect(sideboardPolicyForFormat(tinyLeaders.default_config)).toEqual({
      type: "Limited",
      data: 10,
    });

    expect(() => sideboardPolicyForFormat("Standard" as never)).toThrow();

    expect(() => maxDeckCopies("Anything", commander.default_config)).not.toThrow();
  });
});
