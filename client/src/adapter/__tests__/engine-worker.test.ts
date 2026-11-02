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
  load_card_database: vi.fn(),
  classify_deck_js: vi.fn(),
  deckSupplyForFormat: vi.fn(),
  submit_action: vi.fn(),
  request_takeback_js: vi.fn(),
  respond_takeback_js: vi.fn(),
  cancel_takeback_js: vi.fn(),
  pending_takeback_js: vi.fn(),
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

  describe("browser-hosted takeback", () => {
    const send = async (data: Record<string, unknown>) =>
      (fakeSelf.onmessage as (e: unknown) => unknown)({ data });
    const responseTo = (id: number) =>
      fakeSelf.postMessage.mock.calls.map(([m]) => m).find((m) => m.id === id);

    afterEach(() => {
      Object.values(wasm).forEach((fn) => fn.mockReset());
    });

    it("answers an applied action that ended a vote with its notice", async () => {
      wasm.submit_action.mockReturnValue({
        status: "applied",
        result: { events: [], log_entries: [] },
        takeback: { resolution: "withdrawn", resolvedBy: 1 },
      });

      await send({ type: "submitAction", id: 951, actor: 1, action: { type: "Concede", data: { player_id: 1 } } });

      expect(responseTo(951)).toEqual({
        type: "result",
        id: 951,
        data: { events: [], log_entries: [], takeback: { resolution: "withdrawn", resolvedBy: 1 } },
      });
    });

    it("refuses an applied outcome whose notice names no resolution", async () => {
      wasm.submit_action.mockReturnValue({
        status: "applied",
        result: { events: [], log_entries: [] },
        takeback: { resolution: "maybe", resolvedBy: 1 },
      });

      await send({ type: "submitAction", id: 952, actor: 1, action: { type: "PassPriority" } });

      expect(responseTo(952)).toMatchObject({ type: "error", id: 952, message: "The engine rejected that action." });
    });

    it("routes each takeback request to its export", async () => {
      wasm.request_takeback_js.mockReturnValue({ status: "requested", requester: 1, voters: [0, 2] });
      wasm.respond_takeback_js.mockReturnValue({ status: "recorded" });
      wasm.cancel_takeback_js.mockReturnValue({ status: "resolved", resolution: "cancelled", resolvedBy: 1 });
      wasm.pending_takeback_js.mockReturnValueOnce(null).mockReturnValueOnce({ requester: 1, voters: [0, 2] });

      await send({ type: "requestTakeback", id: 961, actor: 1, aiPlayers: [3] });
      await send({ type: "respondTakeback", id: 962, actor: 2, approve: true, aiPlayers: [3] });
      await send({ type: "cancelTakeback", id: 963, actor: 1 });
      await send({ type: "pendingTakeback", id: 964 });
      await send({ type: "pendingTakeback", id: 965 });

      expect(wasm.request_takeback_js).toHaveBeenCalledWith(1, Uint8Array.from([3]));
      expect(wasm.respond_takeback_js).toHaveBeenCalledWith(2, true, Uint8Array.from([3]));
      expect(wasm.cancel_takeback_js).toHaveBeenCalledWith(1);
      expect(responseTo(961)).toEqual({ type: "result", id: 961, data: { status: "requested", requester: 1, voters: [0, 2] } });
      expect(responseTo(962)).toEqual({ type: "result", id: 962, data: { status: "recorded" } });
      expect(responseTo(963)).toEqual({
        type: "result",
        id: 963,
        data: { status: "resolved", resolution: "cancelled", resolvedBy: 1 },
      });
      expect(responseTo(964)).toEqual({ type: "result", id: 964, data: null });
      expect(responseTo(965)).toEqual({ type: "result", id: 965, data: { requester: 1, voters: [0, 2] } });
    });
  });

  // Runs last: a successful load leaves the module-level database flag set.
  describe("stateless queries", () => {
    const send = async (data: Record<string, unknown>) =>
      (fakeSelf.onmessage as (e: unknown) => unknown)({ data });
    const responseTo = (id: number) =>
      fakeSelf.postMessage.mock.calls.map(([m]) => m).find((m) => m.id === id);

    afterEach(() => {
      Object.values(wasm).forEach((fn) => fn.mockReset());
    });

    it("answers a query that reads no card database without one loaded", async () => {
      wasm.deckSupplyForFormat.mockReturnValue({ kind: "PlayerDecks" });

      await send({ type: "query", id: 1001, name: "deckSupplyForFormat", args: ["Standard"] });

      expect(wasm.deckSupplyForFormat).toHaveBeenCalledWith("Standard");
      expect(responseTo(1001)).toEqual({ type: "result", id: 1001, data: { kind: "PlayerDecks" } });
    });

    it("refuses a card-database query until a load returns cards", async () => {
      vi.stubGlobal("fetch", vi.fn(async () => new Response("{}")));
      const classify = { type: "query", name: "classifyDeck", args: [["Forest"]] };

      await send({ ...classify, id: 1002 });
      wasm.load_card_database.mockReturnValueOnce(0);
      await send({ type: "loadCardDbFromUrl", id: 1003 });
      await send({ ...classify, id: 1004 });

      expect(responseTo(1002)?.type).toBe("error");
      expect(responseTo(1003)).toMatchObject({ type: "error", message: expect.stringContaining("no cards loaded") });
      expect(responseTo(1004)?.type).toBe("error");
      expect(wasm.classify_deck_js).not.toHaveBeenCalled();

      wasm.load_card_database.mockReturnValueOnce(5);
      wasm.classify_deck_js.mockReturnValue({ archetype: "Ramp" });
      await send({ type: "loadCardDbFromUrl", id: 1005 });
      await send({ ...classify, id: 1006 });

      expect(responseTo(1005)).toEqual({ type: "result", id: 1005, data: 5 });
      expect(wasm.classify_deck_js).toHaveBeenCalledWith(["Forest"]);
      expect(responseTo(1006)).toEqual({ type: "result", id: 1006, data: { archetype: "Ramp" } });
    });
  });
});
