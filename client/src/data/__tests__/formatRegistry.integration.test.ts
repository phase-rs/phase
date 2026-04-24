import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { describe, it, expect, beforeAll } from "vitest";

// `getFormatRegistry` is a new WASM export added alongside this test. Until
// `./scripts/build-wasm.sh` runs, the regenerated TS bindings at
// `client/src/wasm/engine.d.ts` may not include it, which would fail
// typecheck. The @ts-expect-error is self-removing: once bindings are
// regenerated, the error directive itself becomes the TS error and forces
// deletion on the next touch.
// @ts-expect-error — regenerated bindings expose getFormatRegistry after WASM build
import init, { getFormatRegistry } from "@wasm/engine";
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
});
