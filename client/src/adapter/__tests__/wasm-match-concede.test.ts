/**
 * #7920 — the pod bot-match whole-match concede capability on WasmAdapter.
 *
 * The menu's pod-match Concede goes exclusively through the duck-typed
 * `supportsMatchConcede` guard (useConcedeHandler). A bare WasmAdapter
 * must NOT satisfy it (plain AI games keep the engine-dispatch path), and
 * a pod-bound one must run exactly the bound concession.
 */

import { describe, expect, it, vi } from "vitest";

import { WasmAdapter } from "../wasm-adapter";
import { supportsMatchConcede } from "../types";

describe("WasmAdapter match-concede capability (#7920)", () => {
  it("a bare adapter does not satisfy the capability guard", () => {
    const adapter = new WasmAdapter();
    // REVERT DISCRIMINATOR: declaring `supportsMatchConcede = true`
    // unconditionally on the class would turn this row red — and would
    // wrongly route every plain AI game's concede to a missing binding.
    expect(supportsMatchConcede(adapter)).toBe(false);
  });

  it("binding installs the capability and sendMatchConcede runs the bound concession once per call", () => {
    const adapter = new WasmAdapter();
    const run = vi.fn();
    adapter.bindMatchConcede(run);

    expect(supportsMatchConcede(adapter)).toBe(true);
    adapter.sendMatchConcede?.();
    expect(run).toHaveBeenCalledTimes(1);
  });
});
