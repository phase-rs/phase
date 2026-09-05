import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  NativeEngineVersionMismatchError,
  PROTOCOL_VERSION,
  WebSocketAdapter,
} from "../ws-adapter";
import { AdapterError, supportsMatchConcede, supportsServerRewind } from "../types";
import type { FormatConfig, GameState } from "../types";
import type {
  InteractionChoiceId,
  InteractionId,
  InteractionPreviewRequest,
  PreviewRequestId,
} from "../generated/interaction";
import type { PhaseSocketTransport } from "../../services/openPhaseSocket";

// Minimal mock WebSocket. Latest-constructed instance is exposed via
// `MockWebSocket.last` so tests can grab it synchronously — the adapter
// now opens the socket through the async `openPhaseSocket` helper, so
// `adapter.ws` is not populated until after the handshake completes.
class MockWebSocket extends EventTarget {
  static OPEN = 1;
  static last: MockWebSocket | null = null;
  readyState = MockWebSocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  send = vi.fn();
  close = vi.fn();
  constructor(public url: string) {
    super();
    MockWebSocket.last = this;
  }
  // Deliver a frame the way production does — which is asymmetric between
  // the two event types, so this routes them differently.
  //
  // `"message"`: `onmessage` ONLY. Both `openPhaseSocket`
  // (`openPhaseSocket.ts:190`) and the adapter (`ws-adapter.ts:528`) assign
  // the `onmessage` IDL attribute, and neither registers an
  // `addEventListener("message", ...)` — `PhaseSocketTransport` types
  // `addEventListener` for `"close"` alone (`openPhaseSocket.ts:20-25`), so
  // a message listener is not even expressible through the interface. This
  // previously also called `dispatchEvent(new MessageEvent(...))` under a
  // comment claiming `openPhaseSocket` registered a message listener; it
  // does not, and happy-dom routes `dispatchEvent` back through the
  // `onmessage` attribute, so every frame in this file was handled TWICE.
  // Harmless for idempotent handlers, but it silently defeats any negative
  // that depends on state the first delivery consumes.
  //
  // `"close"`: both channels, because production genuinely uses both —
  // `openPhaseSocket.ts:366` registers `addEventListener("close", ...)`
  // alongside the `onclose` assignment.
  dispatchSynthetic(type: "message" | "close", data?: string) {
    if (type === "message" && data !== undefined) {
      this.onmessage?.({ data });
    } else if (type === "close") {
      this.onclose?.();
      this.dispatchEvent(new Event("close"));
    }
  }
}

// Replace global WebSocket with mock
vi.stubGlobal("WebSocket", MockWebSocket);

const SERVER_HELLO = JSON.stringify({
  type: "ServerHello",
  data: {
    server_version: "0.0.0-test",
    build_commit: "testhash",
    protocol_version: PROTOCOL_VERSION,
    mode: "Full",
  },
});

/**
 * Drives an adapter through the shared-handshake pipeline to the
 * post-ServerHello state. Returns the adapter's underlying mock ws once
 * the handshake has landed, so tests can then fire game-level frames.
 */
async function completeHandshake(adapter: WebSocketAdapter): Promise<MockWebSocket> {
  // Allow the microtask inside `openPhaseSocket` to install its
  // `onmessage` handler before we deliver the hello frame.
  await Promise.resolve();
  const ws = MockWebSocket.last!;
  ws.dispatchSynthetic("message", SERVER_HELLO);
  // One more tick so the adapter's `attachSocket` re-binds `onmessage`
  // to its post-handshake handler and the `this.ws` assignment settles.
  await Promise.resolve();
  await Promise.resolve();
  return (adapter as unknown as { ws: MockWebSocket }).ws;
}

/**
 * Starts observing `promise` immediately and returns a reader that yields the
 * rejection reason — or the string `"never settled"` if the promise is still
 * pending.
 *
 * Observation has to start before the first `await` so an already-rejected
 * promise is handled in the same tick, and the drain is what turns an orphaned
 * promise — the exact defect these settlement fixes prevent — into a readable
 * assertion failure instead of a suite timeout.
 *
 * The reader yields a MACROTASK turn (`setTimeout(…, 0)`), not a microtask
 * drain: that is strictly more generous than the settlement paths need, since
 * both `dispose()` and `onclose` reject synchronously. The cost is that it
 * couples the reader to real timers — a caller running it under
 * `vi.useFakeTimers()` would hang. No current caller does; the only fake-timer
 * scope in these suites is closed by a `finally { vi.useRealTimers(); }`.
 */
function trackRejection(promise: Promise<unknown>): () => Promise<unknown> {
  let outcome: unknown = "never settled";
  void promise.then(
    (value) => { outcome = { resolvedWith: value }; },
    (error) => { outcome = error; },
  );
  return async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
    return outcome;
  };
}

// Shared session service relies on localStorage in test environments.
vi.stubGlobal("localStorage", {
  getItem: vi.fn(() => null),
  setItem: vi.fn(),
  removeItem: vi.fn(),
});

function createMockState(): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [],
    priority_player: 0,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
  };
}

describe("WebSocketAdapter", () => {
  let adapter: WebSocketAdapter;
  let ws: MockWebSocket;

  beforeEach(async () => {
    MockWebSocket.last = null;
    adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws",
      "host",
      { main_deck: [], sideboard: [] },
    );
    const initPromise = adapter.initialize();
    ws = await completeHandshake(adapter);
    // Simulate GameStarted to resolve init.
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "GameStarted",
        data: { state: createMockState(), your_player: 0 },
      }),
    );
    await initPromise;
  });

  it("sends the payload-free authenticated whole-match concession intent", () => {
    expect(supportsMatchConcede(adapter)).toBe(true);

    adapter.sendMatchConcede();

    expect(ws.send).toHaveBeenLastCalledWith(JSON.stringify({ type: "ConcedeMatch" }));
  });

  it("exports only the trusted snapshot returned by the server", async () => {
    const exported = adapter.exportPersistenceState();

    expect(ws.send).toHaveBeenLastCalledWith(JSON.stringify({ type: "ExportAuthoritativeState" }));

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "AuthoritativeStateExport", data: { state: "{\"state\":{}}" } }),
    );

    await expect(exported).resolves.toBe("{\"state\":{}}");
  });

  it("rejects an authoritative export failure without ending the session", async () => {
    const exported = adapter.exportPersistenceState();
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "AuthoritativeStateExportFailed",
        data: { message: "Only the game host can export authoritative state" },
      }),
    );

    await expect(exported).rejects.toMatchObject({
      code: "WS_ERROR",
      message: "Only the game host can export authoritative state",
      recoverable: false,
    });
  });

  it("rejects a malformed authoritative export and clears the pending request", async () => {
    const malformed = adapter.exportPersistenceState();
    ws.dispatchSynthetic("message", JSON.stringify({ type: "AuthoritativeStateExport" }));

    await expect(malformed).rejects.toMatchObject({
      code: "WS_ERROR",
      message: "Server sent an invalid authoritative-state export.",
      recoverable: false,
    });

    const retried = adapter.exportPersistenceState();
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "AuthoritativeStateExport", data: { state: "{\"state\":{}}" } }),
    );

    await expect(retried).resolves.toBe("{\"state\":{}}");
  });

  it.each([
    ["invalid JSON", "{"],
    ["null", "null"],
    ["scalar", "1"],
    ["typeless object", "{}"],
    ["non-string type", JSON.stringify({ type: 1 })],
    ["unknown type", JSON.stringify({ type: "UnexpectedMessage" })],
  ])("rejects a pending export for a %s frame and permits a retry", async (_label, frame) => {
    const exported = adapter.exportPersistenceState();
    ws.dispatchSynthetic("message", frame);

    await expect(exported).rejects.toMatchObject({
      code: "WS_ERROR",
      recoverable: false,
    });

    const retried = adapter.exportPersistenceState();
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "AuthoritativeStateExport", data: { state: "{\"state\":{}}" } }),
    );
    await expect(retried).resolves.toBe("{\"state\":{}}");
  });

  it("ignores unknown frames when no authoritative export is pending", async () => {
    ws.dispatchSynthetic("message", JSON.stringify({ type: "UnexpectedMessage" }));

    const exported = adapter.exportPersistenceState();
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "AuthoritativeStateExport", data: { state: "{\"state\":{}}" } }),
    );
    await expect(exported).resolves.toBe("{\"state\":{}}");
  });

  it("does not request an authoritative export over cleartext WebSocket", async () => {
    const insecure = new WebSocketAdapter(
      "ws://localhost:9374/ws",
      "host",
      { main_deck: [], sideboard: [] },
    );

    await expect(insecure.exportPersistenceState()).rejects.toMatchObject({
      code: "WS_ERROR",
      message: "Authoritative-state export requires a secure connection",
      recoverable: false,
    });
  });

/* Legacy browser-owned Resolve All transport coverage removed with the transport.
  it("publishes a Resolve All decision state before resolving its acknowledgement", async () => {
    const listener = vi.fn();
    adapter.onEvent(listener);
    const resultPromise = adapter.resolveAll(0, [{ playerId: 1, difficulty: "Medium" }], 5);

    expect(JSON.parse(ws.send.mock.lastCall![0] as string)).toEqual({
      type: "ResolveAll",
      data: { request_id: 1, max_resolutions: 5 },
    });

    const conniveState = {
      ...createMockState(),
      stack: [{ id: 1 }],
      waiting_for: {
        type: "ConniveDiscard",
        data: { player: 0, conniver_id: 4, source_id: 4, cards: [9, 10], count: 1 },
      },
    } as GameState;
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "StateUpdate", data: { state: conniveState, events: [] } }),
    );

    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "stateChanged",
        snapshot: expect.objectContaining({ state: conniveState }),
      }),
    );

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "ResolveAllResult",
        data: {
          request_id: 1,
          items_resolved: 1,
          total: 2,
        },
      }),
    );

    await expect(resultPromise).resolves.toMatchObject({
      waitingFor: conniveState.waiting_for,
      itemsResolved: 1,
      total: 2,
    });
  });

  it("settles Resolve All only from its correlated server response", async () => {
    const resultPromise = adapter.resolveAll(0, [{ playerId: 1, difficulty: "Medium" }], 5);
    const settled = vi.fn();
    void resultPromise.then(settled, settled);

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "Error", data: { message: "batch snapshot rejected" } }),
    );
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "ActionRejected", data: { rejection: { code: "stale_action", disposition: "stale", message: "stale action rejection", related_object_ids: [] } } }),
    );
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "ResolveAllRejected",
        data: { request_id: 2, rejection: { code: "invalid_action", disposition: "invalid", message: "a different batch", related_object_ids: [] } },
      }),
    );

    await Promise.resolve();
    expect(settled).not.toHaveBeenCalled();

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "ResolveAllRejected",
        data: { request_id: 1, rejection: { code: "invalid_action", disposition: "invalid", message: "batch snapshot rejected", related_object_ids: [] } },
      }),
    );

    await expect(resultPromise).rejects.toMatchObject({ message: "batch snapshot rejected" });
  });

  it("keeps correlated Resolve All rejections engine-classified", async () => {
    const stale = adapter.resolveAll(0, [{ playerId: 1, difficulty: "Medium" }], 5);
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "ResolveAllRejected",
        data: { request_id: 1, rejection: { code: "resolve_all_not_ready", disposition: "unavailable", message: "Resolve All is not ready to run.", related_object_ids: [] } },
      }),
    );
    await expect(stale).rejects.toMatchObject({
      code: "ACTION_REJECTED",
      recoverable: true,
    });

    const rejected = adapter.resolveAll(0, [{ playerId: 1, difficulty: "Medium" }], 5);
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "ResolveAllRejected",
        data: { request_id: 2, rejection: { code: "invalid_action", disposition: "invalid", message: "batch snapshot rejected", related_object_ids: [] } },
      }),
    );
    await expect(rejected).rejects.toMatchObject({
      code: "ACTION_REJECTED",
      recoverable: true,
    });
  });

  it("settles operational failures only against their pending game operation", async () => {
    const action = adapter.submitAction({ type: "PassPriority" }, 0);
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "ActionFailed", data: { message: "action persistence failed" } }),
    );
    await expect(action).rejects.toMatchObject({
      code: "WS_ERROR",
      message: "action persistence failed",
      recoverable: false,
    });

    const resolveAll = adapter.resolveAll(0, [{ playerId: 1, difficulty: "Medium" }], 5);
    const resolveAllSettled = vi.fn();
    void resolveAll.then(resolveAllSettled, resolveAllSettled);
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "ResolveAllFailed", data: { request_id: 2, message: "other batch failed" } }),
    );
    await Promise.resolve();
    expect(resolveAllSettled).not.toHaveBeenCalled();
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "ResolveAllFailed", data: { request_id: 1, message: "batch persistence failed" } }),
    );
    await expect(resolveAll).rejects.toMatchObject({
      code: "WS_ERROR",
      message: "batch persistence failed",
      recoverable: false,
    });

    const preview = adapter.previewManaPayment({ type: "PassPriority" }, 0);
    const calls = ws.send.mock.calls;
    const sent = JSON.parse(calls[calls.length - 1][0] as string);
    const previewSettled = vi.fn();
    void preview.then(previewSettled, previewSettled);
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "ManaPaymentPreviewFailed", data: { request_id: sent.data.request_id + 1, message: "other preview failed" } }),
    );
    await Promise.resolve();
    expect(previewSettled).not.toHaveBeenCalled();
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "ManaPaymentPreviewFailed", data: { request_id: sent.data.request_id, message: "preview lookup failed" } }),
    );
    await expect(preview).rejects.toMatchObject({
      code: "WS_ERROR",
      message: "preview lookup failed",
      recoverable: false,
    });
  });

*/
  describe("server rewind capability (F2)", () => {
    it("declares the capability through the standalone type guard", () => {
      expect(supportsServerRewind(adapter)).toBe(true);
    });

    // The reverse-skew guard. The last-action frame must carry NO `data` key —
    // byte-identical to the frame every already-deployed server accepts, and
    // the reason `ClientMessage::RequestTakeback` is a newtype over
    // `Option<RewindTarget>` rather than a struct variant (which would reject
    // this exact frame with `missing field \`data\``).
    it("sends a data-free frame for a last-action undo", () => {
      adapter.sendRequestTakeback();
      expect(JSON.parse(ws.send.mock.lastCall![0] as string)).toEqual({
        type: "RequestTakeback",
      });

      adapter.sendRequestTakeback({ kind: "last_action" });
      expect(JSON.parse(ws.send.mock.lastCall![0] as string)).toEqual({
        type: "RequestTakeback",
      });
    });

    it("sends the data-bearing frame for a turn rewind", () => {
      adapter.sendRequestTakeback({ kind: "turn_start", turn_number: 3 });
      expect(JSON.parse(ws.send.mock.lastCall![0] as string)).toEqual({
        type: "RequestTakeback",
        data: { kind: "turn_start", turn_number: 3 },
      });
    });

    it("emits rewindTargets from a StateUpdate that carries them", () => {
      const listener = vi.fn();
      adapter.onEvent(listener);
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "StateUpdate",
          data: {
            state: createMockState(),
            events: [],
            rewind_targets: [{ turn_number: 3, active_player: 1 }],
          },
        }),
      );
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({
          type: "stateChanged",
          rewindTargets: [{ turn_number: 3, active_player: 1 }],
        }),
      );
    });

    // Forward-skew hostile: an omitted field must become `[]`, never
    // `undefined`. On this transport `undefined` means "does not publish",
    // which is false here — and `dispatch.ts` treats the two differently, so
    // collapsing them would leave a stale list on screen forever.
    it("emits an empty array when a StateUpdate omits rewind_targets", () => {
      const listener = vi.fn();
      adapter.onEvent(listener);
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "StateUpdate",
          data: { state: createMockState(), events: [] },
        }),
      );
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({ type: "stateChanged", rewindTargets: [] }),
      );
    });

    // The reconnect path: a mid-game reattach must see the list immediately
    // rather than waiting for the next action.
    it("emits rewindTargets from a reconnect GameStarted", async () => {
      MockWebSocket.last = null;
      const reconnected = new WebSocketAdapter(
        "ws://localhost:9374/ws",
        "join",
        { main_deck: [], sideboard: [] },
        "ABC123",
      );
      const listener = vi.fn();
      reconnected.onEvent(listener);
      const initPromise = reconnected.initialize();
      const ws2 = await completeHandshake(reconnected);
      // Resolve init with a first GameStarted, then deliver the reconnect one.
      ws2.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: { state: createMockState(), your_player: 0 },
        }),
      );
      await initPromise;
      listener.mockClear();
      ws2.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: {
            state: createMockState(),
            your_player: 0,
            rewind_targets: [{ turn_number: 5, active_player: 0 }],
          },
        }),
      );
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({
          type: "stateChanged",
          rewindTargets: [{ turn_number: 5, active_player: 0 }],
        }),
      );
    });
  });

  describe("native AI transport", () => {
    const nativeAiOptions = (socketFactory: () => PhaseSocketTransport) => ({
      nativeAi: {
        socketFactory,
        aiSeats: [{
          seatIndex: 1,
          difficulty: "Hard",
          deck: { main_deck: ["Lightning Bolt"], sideboard: [] },
        }],
        playerCount: 2,
      },
    });

    it("uses the bridge factory with the full camelCase AI seat wire shape", async () => {
      MockWebSocket.last = null;
      const socketFactory = vi.fn(
        () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
      );
      const nativeAdapter = new WebSocketAdapter(
        "not-a-websocket-url",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Player",
        nativeAiOptions(socketFactory),
      );

      const initPromise = nativeAdapter.initialize();
      const nativeSocket = await completeHandshake(nativeAdapter);
      expect(socketFactory).toHaveBeenCalledWith("not-a-websocket-url");
      expect(nativeSocket.send).toHaveBeenLastCalledWith(
        JSON.stringify({
          type: "CreateGameWithSettings",
          data: {
            deck: { main_deck: [], sideboard: [] },
            display_name: "Player",
            public: false,
            password: null,
            timer_seconds: null,
            player_count: 2,
            match_config: { match_type: "Bo1" },
            ai_seats: [{
              seatIndex: 1,
              difficulty: "Hard",
              deckName: null,
              deck: {
                type: "DeckList",
                data: { main_deck: ["Lightning Bolt"], sideboard: [] },
              },
            }],
            format_config: null,
            room_name: null,
            start_when_full: true,
            ranked: false,
          },
        }),
      );
      const calls = nativeSocket.send.mock.calls;
      const sentFrame = calls[calls.length - 1]?.[0];
      expect(sentFrame).toContain('"seatIndex"');
      expect(sentFrame).toContain('"deckName"');
      expect(sentFrame).not.toContain('"seat_index"');
      expect(sentFrame).not.toContain('"deck_name"');
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: { state: createMockState(), your_player: 0 },
        }),
      );
      await initPromise;
    });

    // A dropped socket used to be instantly fatal for desktop solo:
    // `maxReconnectAttempts` was 0, and `attemptReconnect` compares
    // `reconnectAttempt >= maxReconnectAttempts`, so 0 >= 0 short-circuits to
    // `reconnectFailed` before the `reconnecting` emit at all. The sidecar
    // runs `--single-user`, so its reconnect window is effectively unbounded
    // and the session is still there to reconnect to.
    // The desktop (Tauri) solo route hands the setup screen's edited config to
    // the sidecar through this frame and nowhere else: that route deliberately
    // writes no resume pointer, so if `format_config` were dropped here the
    // native server would fall back to Standard's 20 life with no other copy of
    // the user's choice anywhere in the session.
    it("carries a custom starting life to the native engine", async () => {
      MockWebSocket.last = null;
      const socketFactory = vi.fn(
        () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
      );
      const formatConfig: FormatConfig = {
        format: "Commander",
        starting_life: 25,
        min_players: 2,
        max_players: 4,
        deck_size: { type: "Exactly", data: 100 },
        singleton: true,
        command_zone: true,
        commander_damage_threshold: 21,
        range_of_influence: null,
        team_based: false,
        sideboard_policy: { type: "Forbidden" },
        default_deck_copy_limit: { type: "UpTo", data: 1 },
        uses_commander: true,
        allow_debug_actions: false,
      };
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Player",
        {
          nativeAi: {
            ...nativeAiOptions(socketFactory).nativeAi,
            formatConfig,
          },
        },
      );

      const initPromise = nativeAdapter.initialize();
      const nativeSocket = await completeHandshake(nativeAdapter);
      const calls = nativeSocket.send.mock.calls;
      const frame = JSON.parse(calls[calls.length - 1]![0] as string);
      expect(frame.type).toBe("CreateGameWithSettings");
      expect(frame.data.format_config).toEqual(formatConfig);

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: { state: createMockState(), your_player: 0 },
        }),
      );
      await initPromise;
    });

    it("retries a dropped native-ai socket instead of failing on first drop", async () => {
      MockWebSocket.last = null;
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Player",
        nativeAiOptions(
          () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
        ),
      );

      const initPromise = nativeAdapter.initialize();
      await Promise.resolve();
      const nativeSocket = MockWebSocket.last!;
      nativeSocket.dispatchSynthetic("message", SERVER_HELLO);
      await Promise.resolve();
      await Promise.resolve();
      // A live session is the first branch `attemptReconnect` reaches: with no
      // game code / player token it short-circuits to `reconnectFailed`
      // regardless of the cap, so the fixture must establish one or the test
      // measures the wrong branch.
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameCreated",
          data: { game_code: "ABCD", player_token: "tok", full_key: { game_code: "ABCD", generation: 1 } },
        }),
      );
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: { state: createMockState(), your_player: 0 },
        }),
      );
      await initPromise;

      const listener = vi.fn();
      nativeAdapter.onEvent(listener);
      nativeSocket.dispatchSynthetic("close");

      // Presently unsatisfiable on the old code: `attemptReconnect` returned
      // before the `reconnecting` emit, so this event was NEVER produced for
      // a native-ai adapter.
      expect(listener).toHaveBeenCalledWith({
        type: "reconnecting",
        attempt: 1,
        maxAttempts: 8,
      });
      expect(listener).not.toHaveBeenCalledWith(
        expect.objectContaining({ type: "reconnectFailed" }),
      );

      nativeAdapter.dispose();
    });

    // Scope guard: this asserts a property of the DIFF (the change was scoped
    // to `nativeAi` and did not widen to both options), not that 0 is the
    // right answer for pregame — that path is explicitly not analysed.
    // Desktop hosting from the multiplayer screen: `P2PHostAdapter` builds a
    // `NativeP2PBridge`, which creates the sidecar game through this pregame
    // frame rather than the `nativeAi` one. `HostSetup`'s edited starting life
    // reaches the engine only if it survives here too.
    it("carries a custom starting life to the native pregame host", async () => {
      MockWebSocket.last = null;
      const formatConfig: FormatConfig = {
        format: "Commander",
        starting_life: 25,
        min_players: 2,
        max_players: 4,
        deck_size: { type: "Exactly", data: 100 },
        singleton: true,
        command_zone: true,
        commander_damage_threshold: 21,
        range_of_influence: null,
        team_based: false,
        sideboard_policy: { type: "Forbidden" },
        default_deck_copy_limit: { type: "UpTo", data: 1 },
        uses_commander: true,
        allow_debug_actions: false,
      };
      const pregameAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () =>
              new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 4,
            aiSeats: [],
            formatConfig,
          },
        },
      );

      const attached = pregameAdapter.initializePregame();
      const nativeSocket = await completeHandshake(pregameAdapter);
      const calls = nativeSocket.send.mock.calls;
      const frame = JSON.parse(calls[calls.length - 1]![0] as string);
      expect(frame.type).toBe("CreateGameWithSettings");
      expect(frame.data.format_config).toEqual(formatConfig);

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "SessionAttached",
          data: {
            game_code: "WXYZ",
            player_id: 0,
            player_token: "tok",
            full_key: { game_code: "WXYZ", generation: 1 },
          },
        }),
      );
      await attached;
      pregameAdapter.dispose();
    });

    it("leaves the native pregame transport failing on first drop", async () => {
      MockWebSocket.last = null;
      const pregameAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = pregameAdapter.initializePregame();
      const nativeSocket = await completeHandshake(pregameAdapter);
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "SessionAttached",
          data: { game_code: "WXYZ", player_id: 0, player_token: "tok", full_key: { game_code: "WXYZ", generation: 1 } },
        }),
      );
      await attached;

      const listener = vi.fn();
      pregameAdapter.onEvent(listener);
      nativeSocket.dispatchSynthetic("close");

      expect(listener).toHaveBeenCalledWith({ type: "reconnectFailed" });
      expect(listener).not.toHaveBeenCalledWith(
        expect.objectContaining({ type: "reconnecting" }),
      );

      pregameAdapter.dispose();
    });

    it("rejects a release version mismatch before creating a game", async () => {
      MockWebSocket.last = null;
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Player",
        {
          nativeAi: {
            ...nativeAiOptions(
              () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            ).nativeAi,
            expectedServerVersion: "1.2.3",
          },
        },
      );

      const initPromise = nativeAdapter.initialize();
      await Promise.resolve();
      const nativeSocket = MockWebSocket.last!;
      nativeSocket.dispatchSynthetic("message", SERVER_HELLO);

      await expect(initPromise).rejects.toBeInstanceOf(NativeEngineVersionMismatchError);
      expect(nativeSocket.close).toHaveBeenCalledOnce();
      expect(nativeSocket.send).not.toHaveBeenCalledWith(
        expect.stringContaining("CreateGameWithSettings"),
      );
    });
  });

  describe("native P2P pregame transport", () => {
    const nativeReconnectAdapter = () => new WebSocketAdapter(
      "native-engine",
      "join",
      { main_deck: [], sideboard: [] },
      undefined,
      undefined,
      undefined,
      "Guest",
      {
        nativePregame: {
          kind: "reconnect",
          socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
          gameCode: "NATIVE",
          playerId: 1,
          playerToken: "guest-token",
          fullKey: { game_code: "NATIVE", generation: 1 },
        },
      },
    );

    it("settles an export when native session identity validation fails", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "wss://localhost:9374/ws",
        "join",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Guest",
        {
          nativePregame: {
            kind: "reconnect",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            gameCode: "NATIVE",
            playerId: 1,
            playerToken: "guest-token",
            fullKey: { game_code: "NATIVE", generation: 1 },
          },
        },
      );
      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      const exported = nativeAdapter.exportPersistenceState();
      const slots = nativeAdapter.waitForPlayerSlots();

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "SessionAttached",
          data: {
            game_code: "NATIVE",
            player_id: 1,
            player_token: "guest-token",
            full_key: { game_code: "NATIVE", generation: 2 },
          },
        }),
      );

      await expect(exported).rejects.toThrow("Server changed the Full session identity");
      await expect(attached).rejects.toThrow("Server changed the Full session identity");
  await expect(slots).rejects.toThrow("Server changed the Full session identity");
      await expect(nativeAdapter.exportPersistenceState()).rejects.toThrow("Session identity rejected");
    });

    it("rejects a native seat attachment without a Full session key", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "SessionAttached",
          data: { game_code: "NATIVE", player_id: 0, player_token: "host-token" },
        }),
      );

      await expect(attached).rejects.toMatchObject({ message: "Server omitted a valid Full session identity" });
      nativeAdapter.dispose();
    });

    it("waits for the server-issued seat attachment and slot confirmation", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      expect(nativeSocket.send).toHaveBeenLastCalledWith(
        expect.stringContaining('"start_when_full":false'),
      );

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "SessionAttached",
          data: { game_code: "NATIVE", player_id: 0, player_token: "host-token", full_key: { game_code: "NATIVE", generation: 1 } },
        }),
      );
      await expect(attached).resolves.toEqual({
        gameCode: "NATIVE",
        playerId: 0,
        playerToken: "host-token",
        fullKey: { game_code: "NATIVE", generation: 1 },
      });

      const confirmed = nativeAdapter.sendSeatMutation({ type: "Start" });
      expect(nativeSocket.send).toHaveBeenLastCalledWith(
        JSON.stringify({ type: "SeatMutate", data: { mutation: { type: "Start" } } }),
      );
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({ type: "PlayerSlotsUpdate", data: { slots: [] } }),
      );
      await expect(confirmed).resolves.toBeUndefined();
    });

    it("reconnects a persisted native viewer with its expected seat", async () => {
      const nativeAdapter = nativeReconnectAdapter();

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      expect(nativeSocket.send).toHaveBeenLastCalledWith(
        JSON.stringify({
          type: "Reconnect",
          data: {
            game_code: "NATIVE",
            player_token: "guest-token",
            full_key: { game_code: "NATIVE", generation: 1 },
          },
        }),
      );
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: {
            state_revision: 7,
            state: createMockState(),
            your_player: 1,
            player_token: "guest-token",
            full_key: { game_code: "NATIVE", generation: 1 },
          },
        }),
      );
      await expect(attached).resolves.toEqual({
        gameCode: "NATIVE",
        playerId: 1,
        playerToken: "guest-token",
        fullKey: { game_code: "NATIVE", generation: 1 },
      });
    });

    it("rejects a hostile GameCreated before it can replace native reconnect credentials", async () => {
      const nativeAdapter = nativeReconnectAdapter();
      const listener = vi.fn();
      nativeAdapter.onEvent(listener);
      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      listener.mockClear();

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameCreated",
          data: {
            game_code: "ATTACK",
            player_token: "attacker-token",
            full_key: { game_code: "ATTACK", generation: 9 },
          },
        }),
      );

      await expect(attached).rejects.toThrow("Native reconnect attached game ATTACK, expected NATIVE");
      await expect(nativeAdapter.getSnapshot()).rejects.toThrow("No game state available");
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "StateUpdate",
          data: {
            state_revision: 8,
            state: createMockState(),
            events: [],
          },
        }),
      );
      await expect(nativeAdapter.getSnapshot()).rejects.toThrow("No game state available");
      expect(nativeAdapter.nativeSession).toEqual({
        gameCode: "NATIVE",
        playerId: 1,
        playerToken: "guest-token",
        fullKey: { game_code: "NATIVE", generation: 1 },
      });
      expect(listener).toHaveBeenCalledTimes(1);
      expect(listener).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
      expect(nativeSocket.close).toHaveBeenCalledTimes(1);
    });

    it("rejects a hostile SessionAttached before it can settle native reconnect", async () => {
      const nativeAdapter = nativeReconnectAdapter();
      const listener = vi.fn();
      nativeAdapter.onEvent(listener);
      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      listener.mockClear();

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "SessionAttached",
          data: {
            game_code: "NATIVE",
            player_id: 1,
            player_token: "attacker-token",
            full_key: { game_code: "NATIVE", generation: 1 },
          },
        }),
      );

      await expect(attached).rejects.toThrow("Native reconnect changed the player token");
      await expect(nativeAdapter.getSnapshot()).rejects.toThrow("No game state available");
      expect(nativeAdapter.nativeSession).toEqual({
        gameCode: "NATIVE",
        playerId: 1,
        playerToken: "guest-token",
        fullKey: { game_code: "NATIVE", generation: 1 },
      });
      expect(listener).toHaveBeenCalledTimes(1);
      expect(listener).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    });

    it("seeds a native reconnect before direct initialize attaches the socket", async () => {
      const nativeAdapter = nativeReconnectAdapter();
      const initialized = nativeAdapter.initialize();
      const nativeSocket = await completeHandshake(nativeAdapter);

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: {
            state_revision: 7,
            state: createMockState(),
            your_player: 1,
            full_key: { game_code: "NATIVE", generation: 1 },
          },
        }),
      );

      await expect(initialized).resolves.toBeUndefined();
      expect(nativeAdapter.nativeSession).toEqual({
        gameCode: "NATIVE",
        playerId: 1,
        playerToken: "guest-token",
        fullKey: { game_code: "NATIVE", generation: 1 },
      });
    });

    it("rejects a native reconnect GameStarted for a different player before caching it", async () => {
      const nativeAdapter = nativeReconnectAdapter();
      const listener = vi.fn();
      nativeAdapter.onEvent(listener);
      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      listener.mockClear();

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: {
            state_revision: 7,
            state: createMockState(),
            your_player: 0,
            full_key: { game_code: "NATIVE", generation: 1 },
          },
        }),
      );

      await expect(attached).rejects.toThrow("Native reconnect attached player 0, expected 1");
      await expect(nativeAdapter.getSnapshot()).rejects.toThrow("No game state available");
      expect(listener).toHaveBeenCalledTimes(1);
      expect(listener).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    });

    it("rejects a native reconnect GameStarted with a changed Full session key before caching it", async () => {
      const nativeAdapter = nativeReconnectAdapter();
      const listener = vi.fn();
      nativeAdapter.onEvent(listener);
      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      listener.mockClear();

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: {
            state_revision: 7,
            state: createMockState(),
            your_player: 1,
            full_key: { game_code: "NATIVE", generation: 2 },
          },
        }),
      );

      await expect(attached).rejects.toThrow("Server changed the Full session identity");
      await expect(nativeAdapter.getSnapshot()).rejects.toThrow("No game state available");
      expect(listener).toHaveBeenCalledTimes(1);
      expect(listener).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    });

    it("rejects a native reconnect GameStarted without a Full session key before caching it", async () => {
      const nativeAdapter = nativeReconnectAdapter();
      const listener = vi.fn();
      nativeAdapter.onEvent(listener);
      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      listener.mockClear();

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: { state_revision: 7, state: createMockState(), your_player: 1 },
        }),
      );

      await expect(attached).rejects.toThrow("Server omitted a valid Full session identity");
      await expect(nativeAdapter.getSnapshot()).rejects.toThrow("No game state available");
      expect(listener).toHaveBeenCalledTimes(1);
      expect(listener).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    });

    it("rejects a native reconnect GameStarted token before caching or attaching", async () => {
      const nativeAdapter = nativeReconnectAdapter();
      const listener = vi.fn();
      nativeAdapter.onEvent(listener);
      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      listener.mockClear();

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: {
            state_revision: 7,
            state: createMockState(),
            your_player: 1,
            player_token: "attacker-token",
            full_key: { game_code: "NATIVE", generation: 1 },
          },
        }),
      );

      await expect(attached).rejects.toThrow("Native reconnect changed the player token");
      await expect(nativeAdapter.getSnapshot()).rejects.toThrow("No game state available");
      expect(nativeAdapter.nativeSession).toEqual({
        gameCode: "NATIVE",
        playerId: 1,
        playerToken: "guest-token",
        fullKey: { game_code: "NATIVE", generation: 1 },
      });
      expect(listener).toHaveBeenCalledTimes(1);
      expect(listener).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    });

    it("rejects native pregame attachment when the server returns an error", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({ type: "Error", data: { message: "Native setup failed" } }),
      );

      await expect(attached).rejects.toThrow("Native setup failed");
    });

    it("rejects native lifecycle waiters when disposed before the socket is attached", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const gameStarted = nativeAdapter.waitForGameStarted();
      nativeAdapter.dispose();

      await expect(attached).rejects.toMatchObject({
        code: "WS_CLOSED",
        recoverable: true,
      } satisfies Partial<AdapterError>);
      await expect(gameStarted).rejects.toMatchObject({
        code: "WS_CLOSED",
        recoverable: true,
      } satisfies Partial<AdapterError>);
    });

    it("preserves the typed non-recoverable deck rejection code", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "Error",
          data: { message: "Deck not legal for this format", code: "deck_rejected" },
        }),
      );

      await expect(attached).rejects.toMatchObject({
        code: "DECK_REJECTED",
        recoverable: false,
      } satisfies Partial<AdapterError>);
    });

    it("does not infer deck rejection from matching error text without a code", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Host",
        {
          nativePregame: {
            kind: "host",
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            playerCount: 2,
            aiSeats: [],
          },
        },
      );

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({ type: "Error", data: { message: "Deck not legal for this format" } }),
      );

      await expect(attached).rejects.toMatchObject({
        code: "ACTION_REJECTED",
        recoverable: true,
      } satisfies Partial<AdapterError>);
    });
  });

  describe("Bug C: stateChanged emission", () => {
    it("emits stateChanged event when StateUpdate arrives without pendingResolve", () => {
      const listener = vi.fn();
      adapter.onEvent(listener);

      const mockState = createMockState();
      const mockEvents = [{ type: "DrawCard", data: { player: 0, object_id: 1 } }];
      const mockLogEntries = [{
        seq: 0,
        turn: 1,
        phase: "PreCombatMain",
        category: "Debug",
        segments: [{ type: "Text", value: "AI guesses Land" }],
      }];

      // Simulate an unsolicited StateUpdate (no pending action)
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "StateUpdate",
          data: { state: mockState, events: mockEvents, log_entries: mockLogEntries },
        }),
      );

      // The engine pair now travels as one seq-stamped `EngineSnapshot`.
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({
          type: "stateChanged",
          snapshot: expect.objectContaining({
            state: expect.objectContaining(mockState),
            seq: expect.any(Number),
          }),
          events: mockEvents,
          logEntries: mockLogEntries,
        }),
      );
    });
  });

  describe("GameStarted identity event", () => {
    it("emits playerIdentity when GameStarted arrives", async () => {
      MockWebSocket.last = null;
      const adapter2 = new WebSocketAdapter(
        "ws://localhost:9374/ws",
        "join",
        { main_deck: [], sideboard: [] },
        "ABC123",
      );
      const listener = vi.fn();
      adapter2.onEvent(listener);
      const initPromise2 = adapter2.initialize();
      const ws2 = await completeHandshake(adapter2);
      ws2.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: { state: createMockState(), your_player: 1, opponent_name: "Opponent" },
        }),
      );
      await initPromise2;
      expect(listener).toHaveBeenCalledWith({
        type: "playerIdentity",
        playerId: 1,
        opponentName: "Opponent",
      });
    });
  });

  describe("reconnect flow", () => {
    it("reconnects with the persisted session after socket close", async () => {
      MockWebSocket.last = null;
      const reconnectingAdapter = new WebSocketAdapter(
        "ws://localhost:9374/ws",
        "join",
        { main_deck: [], sideboard: [] },
        "ABC123",
      );
      const initPromise = reconnectingAdapter.initialize();
      const initialWs = await completeHandshake(reconnectingAdapter);
      initialWs.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: {
            state: createMockState(),
            your_player: 1,
            player_token: "player-token",
            full_key: { game_code: "ABC123", generation: 1 },
          },
        }),
      );
      await initPromise;

      vi.useFakeTimers();
      try {
        initialWs.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(1000);
        vi.useRealTimers();

        const reconnectWs = await completeHandshake(reconnectingAdapter);

        // The handshake helper consumes ServerHello and sends ClientHello
        // internally, so after `completeHandshake` the first post-handshake
        // frame the adapter emits is the Reconnect setup frame.
        expect(reconnectWs.send).toHaveBeenCalledWith(
          JSON.stringify({
            type: "Reconnect",
            data: {
              game_code: "ABC123",
              player_token: "player-token",
              full_key: { game_code: "ABC123", generation: 1 },
            },
          }),
        );
      } finally {
        vi.useRealTimers();
      }
    });
  });

  describe("send() error handling", () => {
    it("rejects initialize when the post-handshake setup frame cannot be sent", async () => {
      MockWebSocket.last = null;
      const setupFailingAdapter = new WebSocketAdapter(
        "ws://localhost:9374/ws",
        "host",
        { main_deck: [], sideboard: [] },
      );
      const initPromise = setupFailingAdapter.initialize();
      await Promise.resolve();
      const setupWs = MockWebSocket.last!;
      setupWs.send
        .mockImplementationOnce(() => undefined)
        .mockImplementationOnce(() => {
          throw new Error("InvalidStateError");
        });

      setupWs.dispatchSynthetic("message", SERVER_HELLO);

      await expect(initPromise).rejects.toThrow("Failed to send setup frame");
    });

    // Issue #5913: the engine's stale-ReorderHand verdict must classify the same
    // way no matter which transport delivered it. Before the shared classifier
    // this path built a generic ACTION_REJECTED, so `dispatchAction` — which
    // suppresses only STALE_ACTION — still showed a server-hosted player the red
    // error the local-WASM seat no longer sees.
    it("classifies a stale ReorderHand rejection from the server as STALE_ACTION", async () => {
      const pending = adapter.submitAction(
        { type: "ReorderHand", data: { order: [1, 2, 3] } },
        0,
      );
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ActionRejected",
          data: { rejection: { code: "stale_action", disposition: "stale", message: "That action is based on outdated game state.", related_object_ids: [] } },
        }),
      );
      await expect(pending).rejects.toMatchObject({
        code: "STALE_ACTION",
        recoverable: false,
      });
    });

    it("keeps the Resolve All priority text actionable on an ordinary action rejection", async () => {
      const pending = adapter.submitAction({ type: "PassPriority" }, 0);
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ActionRejected",
          data: { rejection: { code: "resolve_all_not_ready", disposition: "unavailable", message: "Resolve All is not ready to run.", related_object_ids: [] } },
        }),
      );
      await expect(pending).rejects.toMatchObject({
        code: "ACTION_REJECTED",
        recoverable: true,
      });
    });

    it("resolves an accepted no-op without publishing a state transition", async () => {
      const listener = vi.fn();
      adapter.onEvent(listener);
      const pending = adapter.submitAction(
        {
          type: "Debug",
          data: {
            type: "CreateCard",
            data: {
              card_name: "Lightning Bolt",
              owner: 0,
              zone: "Hand",
              run_etb: false,
              nonlegendary: false,
              count: 0,
            },
          },
        },
        0,
      );

      ws.dispatchSynthetic("message", JSON.stringify({ type: "ActionNoOp" }));

      await expect(pending).resolves.toEqual({ events: [], log_entries: [] });
      expect(listener).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
      expect(listener).not.toHaveBeenCalledWith(
        expect.objectContaining({ type: "stateChanged" }),
      );
    });

    // A refused takeback answers a fire-and-forget request, so no promise owns
    // the rejection. Before this branch the whole `if (this.pendingReject)`
    // body was skipped and the refusal was dropped on the floor — which is why
    // the server had been reaching for `ServerMessage::error` instead, the
    // event `handleNativeEvent` treats as terminal.
    it("emits requestRejected for a request-level refusal", () => {
      const listener = vi.fn();
      adapter.onEvent(listener);

      adapter.sendRequestTakeback();
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "RequestRejected",
          data: { reason: "There is no previous action of yours to take back" },
        }),
      );

      expect(listener).toHaveBeenCalledWith({
        type: "requestRejected",
        reason: "There is no previous action of yours to take back",
      });
      // The survivability property: no terminal `error` event, which is what
      // tears down a native session.
      expect(listener).not.toHaveBeenCalledWith(
        expect.objectContaining({ type: "error" }),
      );
    });

    // Guards the new `else` against swallowing the normal path.
    //
    // This test is why the double delivery in `dispatchSynthetic` had to be
    // fixed rather than worked around: a second delivery of the same frame
    // finds `pendingReject` already cleared by the first, takes the `else`,
    // and makes the negative below unpassable no matter what the adapter
    // does. It now goes through `dispatchSynthetic` like every other frame
    // in this file.
    it("does not emit requestRejected when an action IS in flight", async () => {
      const listener = vi.fn();
      adapter.onEvent(listener);

      const pending = adapter.submitAction({ type: "PassPriority" }, 0);
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ActionRejected",
          data: { rejection: { code: "invalid_action", disposition: "invalid", message: "That action is not valid in the current game state.", related_object_ids: [] } },
        }),
      );

      // Reach-guard: the frame really was delivered and handled — without this
      // the negative below would pass for a message that never arrived.
      await expect(pending).rejects.toMatchObject({ code: "ACTION_REJECTED" });
      expect(listener).not.toHaveBeenCalledWith(
        expect.objectContaining({ type: "requestRejected" }),
      );
    });

    it("sends the action frame and keeps the promise pending on a healthy socket", () => {
      ws.send.mockClear();
      void adapter.submitAction({ type: "PassPriority" }, 0);
      expect(ws.send).toHaveBeenCalledWith(
        JSON.stringify({
          type: "Action",
          data: { action: { type: "PassPriority" } },
        }),
      );
    });

    it("resolves a mana-payment preview only for its matching request", async () => {
      ws.send.mockClear();
      const preview = adapter.previewManaPayment({ type: "PassPriority" }, 0);
      expect(ws.send).toHaveBeenCalledWith(
        JSON.stringify({
          type: "PreviewManaPayment",
          data: { request_id: 1, action: { type: "PassPriority" } },
        }),
      );

      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ManaPaymentPreview",
          data: { request_id: 1, source_ids: [12] },
        }),
      );

      await expect(preview).resolves.toEqual([12]);
    });

    it("rejects submitAction and clears pending state when the socket throws on send", async () => {
      const listener = vi.fn();
      adapter.onEvent(listener);
      ws.send.mockImplementationOnce(() => {
        throw new Error("InvalidStateError");
      });

      await expect(
        adapter.submitAction({ type: "PassPriority" }, 0),
      ).rejects.toThrow();

      // The action was un-pended and an error surfaced, rather than the caller
      // hanging forever on a reply that will never come.
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({ type: "actionPendingChanged", pending: false }),
      );
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({ type: "error" }),
      );
    });

    it("rejects an in-flight action when the native AI driver faults", async () => {
      const listener = vi.fn();
      adapter.onEvent(listener);
      const pending = adapter.submitAction({ type: "PassPriority" }, 0);

      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "AiDriverFault",
          data: { fault: { id: 7, after_state_revision: 3, cause: "ActionSafetyCapReached" } },
        }),
      );

      await expect(pending).rejects.toMatchObject({
        code: "WS_ERROR",
        recoverable: false,
      });
      expect(listener).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
      expect(listener).toHaveBeenCalledWith(expect.objectContaining({ type: "aiDriverFault" }));
    });

    // `dispose()` used to null the submission handles instead of rejecting
    // them, so the caller — which holds the module-level dispatch mutex —
    // waited forever on a reply the closed socket could never deliver.
    it("rejects an in-flight submitAction when the adapter is disposed", async () => {
      const pending = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));

      adapter.dispose();

      expect(await pending()).toMatchObject({
        code: "WS_CLOSED",
        message: "Adapter disposed during action",
        recoverable: true,
      });
    });

    // The `sessionIdentityRejected` guard used to sit ABOVE the pending
    // submission block in `onclose`, so a close taken on the identity-rejected
    // path abandoned an in-flight submission.
    it("settles the pending submission on close even after session identity is rejected", async () => {
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameCreated",
          data: {
            game_code: "ABCD",
            player_token: "tok",
            // generation < 1 is rejected by `acceptFullSessionKey`, latching
            // `sessionIdentityRejected`.
            full_key: { game_code: "ABCD", generation: 0 },
          },
        }),
      );
      // Reach-guard: the latch really is set, otherwise this test would pass
      // against the unguarded close path and prove nothing.
      await expect(adapter.exportPersistenceState()).rejects.toThrow("Session identity rejected");

      const pending = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));
      ws.dispatchSynthetic("close");

      expect(await pending()).toMatchObject({
        code: "WS_CLOSED",
        message: "Connection closed during action",
      });
    });

    it("emits an error instead of throwing when a fire-and-forget send hits a closed socket", () => {
      const listener = vi.fn();
      adapter.onEvent(listener);
      ws.readyState = 3; // CLOSED

      expect(() => adapter.sendEmote("wave")).not.toThrow();
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({ type: "error" }),
      );
    });
  });

  describe("interaction preview transport", () => {
    /**
     * An allocation whose segments are UNEQUAL and whose `choice_id` order is
     * NOT the candidate publication order, so a sort or a canonicalisation
     * anywhere in the adapter layer is caught rather than coinciding.
     */
    const cid = (id: string) => id as InteractionChoiceId;
    const request = {
      requestId: "preview-req-1" as PreviewRequestId,
      interactionId: "interaction-1" as InteractionId,
      response: {
        type: "shortcut",
        data: {
          decision: { type: "fixed", data: { iterations: 6 } },
          pins: [{
            group: 0,
            choiceIds: [cid("choice-c"), cid("choice-a"), cid("choice-b")],
            amounts: [
              { choiceId: cid("choice-c"), amount: 3 },
              { choiceId: cid("choice-a"), amount: 1 },
              { choiceId: cid("choice-b"), amount: 2 },
            ],
          }],
        },
      },
    } satisfies InteractionPreviewRequest;

    const answer = (requestId: string) => ({
      requestId,
      interactionId: "interaction-1",
      status: { type: "confirmable" },
      progress: { selected: 3, minimum: 1, maximum: 3, aggregate: 6, confirmable: true },
      outcome: "advanced",
      summaries: ["confirmAvailable", "progress"],
    });

    function sentPreviewFrame(): { type: string; data: { request: InteractionPreviewRequest } } {
      const calls = ws.send.mock.calls;
      for (let i = calls.length - 1; i >= 0; i--) {
        const parsed = JSON.parse(calls[i][0] as string);
        if (parsed.type === "PreviewInteraction") return parsed;
      }
      throw new Error("no PreviewInteraction frame was sent");
    }

    // Row 8. Reshape the request anywhere in the adapter layer — reorder
    // `amounts`, renumber a `choiceId`, rebuild the object field-by-field —
    // and the deep-equal below fails.
    it("sends the authored request verbatim and resolves its own answer", async () => {
      ws.send.mockClear();
      const pending = adapter.previewInteraction(request, 0);

      const frame = sentPreviewFrame();
      expect(frame.type).toBe("PreviewInteraction");
      expect(frame.data.request).toEqual(request);
      const response = frame.data.request.response;
      if (response.type !== "shortcut") throw new Error(`not a shortcut: ${response.type}`);
      const pins = response.data.pins;
      const amounts = pins[0].amounts;
      // `amounts` is OPTIONAL on the wire, so its presence is an assertion rather than a shape
      // the narrow gives for free.
      if (amounts === undefined) throw new Error("the sent pin carries no amounts");
      // Reach guard: the asserted allocation has more than one segment, so an
      // adapter that dropped all but the first could not pass.
      expect(amounts.length).toBeGreaterThan(1);
      expect(amounts).toEqual([
        { choiceId: "choice-c", amount: 3 },
        { choiceId: "choice-a", amount: 1 },
        { choiceId: "choice-b", amount: 2 },
      ]);
      expect(amounts.map((a) => a.choiceId)).toEqual(pins[0].choiceIds);

      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "InteractionPreview",
          data: { preview: answer("preview-req-1") },
        }),
      );
      await expect(pending).resolves.toMatchObject({ requestId: "preview-req-1" });
    });

    // An answer whose id is absent from the in-flight map settles nothing and
    // disturbs no other pending entry.
    it("drops an answer for a request it never sent", async () => {
      const settled = vi.fn();
      const pending = adapter.previewInteraction(request, 0);
      void pending.then(settled, settled);

      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "InteractionPreview",
          data: { preview: answer("some-other-request") },
        }),
      );
      // Enough microtask turns for a settlement to reach `settled`: one tick
      // is not, and a one-tick wait would make this negative unfalsifiable.
      for (let i = 0; i < 5; i++) await Promise.resolve();
      expect(settled).not.toHaveBeenCalled();

      // Positive control on the identical path: the correlated answer DOES
      // settle it, so the silence above is correlation and not a dead handler.
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "InteractionPreview",
          data: { preview: answer("preview-req-1") },
        }),
      );
      await expect(pending).resolves.toMatchObject({ requestId: "preview-req-1" });
    });

    it("settles a correlated operational failure", async () => {
      const pending = adapter.previewInteraction(request, 0);
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "InteractionPreviewFailed",
          data: { request_id: "preview-req-1", message: "preview lookup failed" },
        }),
      );
      await expect(pending).rejects.toMatchObject({
        code: "WS_ERROR",
        message: "preview lookup failed",
        recoverable: false,
      });
    });

    // Row 12, close-site leg. Delete the `rejectPendingInteractionPreviews`
    // call in `socket.ws.onclose` and this hangs until the test times out.
    it("rejects every in-flight preview when the socket closes, keeping answered ones", async () => {
      const answered = adapter.previewInteraction(request, 0);
      const unanswered = adapter.previewInteraction(
        { ...request, requestId: "preview-req-2" as PreviewRequestId },
        0,
      );

      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "InteractionPreview",
          data: { preview: answer("preview-req-1") },
        }),
      );
      await expect(answered).resolves.toMatchObject({ requestId: "preview-req-1" });

      ws.dispatchSynthetic("close");
      // The clearing loop walks the map rather than rejecting indiscriminately:
      // the already-answered promise keeps its value.
      await expect(answered).resolves.toMatchObject({ requestId: "preview-req-1" });
      await expect(unanswered).rejects.toMatchObject({
        code: "WS_CLOSED",
        message: "Connection closed during interaction preview",
      });
    });

    // Row 12, dispose-site leg. The site a single-site wiring drops: measured
    // on the mana twin, deleting it alone leaves this promise never settling
    // while the close-site leg above still passes.
    it("rejects every in-flight preview when the adapter is disposed", async () => {
      const pending = adapter.previewInteraction(request, 0);
      adapter.dispose();
      await expect(pending).rejects.toMatchObject({
        code: "WS_CLOSED",
        message: "Adapter disposed during interaction preview",
      });
    });
  });
});
