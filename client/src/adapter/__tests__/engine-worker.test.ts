import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

// The worker module's top level only assigns `self.onmessage` and declares
// `cardDbLoaded`; `canonicalCardNames` and `get_card_face_data` are stubbed
// here. `default` stands in for the wasm-bindgen `init` the worker's own
// `init()` request case invokes.
const wasm = vi.hoisted(() => ({
  canonicalCardNames: vi.fn(),
  get_card_face_data: vi.fn(),
  get_game_state: vi.fn(),
  get_legal_actions_js: vi.fn(),
  get_legal_actions_for_viewer_js: vi.fn(),
  resume_restored_game_state: vi.fn(),
  resume_multiplayer_host_state: vi.fn(),
}));
vi.mock("@wasm/engine", () => ({ default: vi.fn(), ...wasm }));

describe("engine worker — requests", () => {
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

  describe("local-seat legal actions come from the viewer-scoped entry", () => {
    const send = async (data: Record<string, unknown>) =>
      (fakeSelf.onmessage as (e: unknown) => unknown)({ data });
    const resultOf = (id: number) =>
      fakeSelf.postMessage.mock.calls.map(([m]) => m).find((m) => m.id === id && m.type === "result");

    beforeEach(() => {
      wasm.get_game_state.mockReturnValue({ turn: 1 });
      wasm.get_legal_actions_for_viewer_js.mockImplementation((viewer: number) => ({ viewer }));
      wasm.resume_restored_game_state.mockReturnValue({ restored: true });
      wasm.resume_multiplayer_host_state.mockReturnValue({ restored: true });
    });

    afterEach(() => {
      Object.values(wasm).forEach((fn) => fn.mockReset());
    });

    it.each([
      [901, "getLegalActions", {}, (r: unknown) => r],
      [902, "getSnapshot", {}, (r: { legalResult: unknown }) => r.legalResult],
      [903, "resumeRestoredGameState", {}, (r: { snapshot: { legalResult: unknown } }) => r.snapshot.legalResult],
      [
        904,
        "resumeMultiplayerHostState",
        { stateJson: "{}" },
        (r: { snapshot: { legalResult: unknown } }) => r.snapshot.legalResult,
      ],
    ])("%#: %s answers with the list for the requested viewer", async (id, type, extra, pick) => {
      await send({ type, id, viewerId: 3, ...extra });

      expect(wasm.get_legal_actions_for_viewer_js).toHaveBeenCalledWith(3);
      expect(wasm.get_legal_actions_js).not.toHaveBeenCalled();
      expect(pick(resultOf(id)!.data)).toEqual({ viewer: 3 });
    });
  });
});
