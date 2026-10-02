import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

// The worker module's top level only assigns `self.onmessage` and declares
// `cardDbLoaded`; `canonicalCardNames` and `get_card_face_data` are stubbed
// here. `default` stands in for the wasm-bindgen `init` the worker's own
// `init()` request case invokes.
const wasm = vi.hoisted(() => ({
  canonicalCardNames: vi.fn(),
  get_card_face_data: vi.fn(),
}));
vi.mock("@wasm/engine", () => ({ default: vi.fn(), ...wasm }));

describe("engine worker — canonicalCardNames request", () => {
  let fakeSelf: { postMessage: ReturnType<typeof vi.fn>; onmessage: unknown };

  beforeAll(async () => {
    fakeSelf = { postMessage: vi.fn(), onmessage: null };
    vi.stubGlobal("self", fakeSelf);
    await import("../engine-worker");
  });

  afterAll(() => {
    vi.unstubAllGlobals();
  });

  it("answers a canonical-name request with the engine's list", async () => {
    wasm.canonicalCardNames.mockReturnValue(["Revival // Revenge", null]);

    await (fakeSelf.onmessage as (e: unknown) => unknown)({
      data: { type: "canonicalCardNames", id: 1, names: ["Revival/Revenge", "Not A Card"] },
    });

    expect(wasm.canonicalCardNames).toHaveBeenCalledWith(["Revival/Revenge", "Not A Card"]);
    expect(wasm.get_card_face_data).not.toHaveBeenCalled();
    expect(fakeSelf.postMessage).toHaveBeenCalledWith({
      type: "result",
      id: 1,
      data: ["Revival // Revenge", null],
    });
  });
});
