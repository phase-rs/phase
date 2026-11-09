import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { diagnosticIdFor, getDiagnosticHistory } from "../../services/troubleshooting";
import { loadDiagnosticHistory } from "../../services/diagnosticHistory";

const lanGate = vi.hoisted(() => ({ supported: false, probe: vi.fn(), authorize: vi.fn() }));
const terminalStore = vi.hoisted(() => ({ commit: vi.fn(async () => true) }));
// The production default for the codec provider reads the tab's shared engine;
// `open` stands a fake engine in only for the default-wiring test below.
const sharedEngine = vi.hoisted(() => ({ open: null as null | (() => Promise<unknown>) }));
vi.mock("../wasm-adapter", async (importOriginal) => {
  const original = await importOriginal<typeof import("../wasm-adapter")>();
  return {
    ...original,
    getSharedAdapter: () => sharedEngine.open
      ? { openStateCodecSession: sharedEngine.open } as unknown as ReturnType<typeof original.getSharedAdapter>
      : original.getSharedAdapter(),
  };
});
vi.mock("../../services/nativeEngineSocket", () => ({ NativeEngineSocket: class { constructor() { return new MockWebSocket("native-lan"); } } }));
vi.mock("../../services/lan", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../services/lan")>(),
  initializeLanCapabilities: lanGate.probe,
  authorizeLanServer: lanGate.authorize,
  canUseLanBridge: () => lanGate.supported,
}));
vi.mock("../../services/fullTerminalResult", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../services/fullTerminalResult")>(),
  commitFullTerminalDelivery: terminalStore.commit,
}));

import {
  NativeEngineVersionMismatchError,
  PROTOCOL_VERSION,
  WebSocketAdapter,
} from "../ws-adapter";
import { AdapterError, AdapterErrorCode, supportsMatchConcede, supportsTakeback, supportsTurnRewind } from "../types";
import { StateCodecRuntime, type StateCodecSession } from "../state-codec-runtime";
import type { EngineModule } from "../engine-queries";
import {
  STATE_CODEC_PREPARATION_TIMEOUT_MS,
  STATE_CODEC_SOCKET_TIMEOUT_MS,
} from "../../services/stateCodecSocket";
import type { FormatConfig, GameAction, GameState, SubmitResult } from "../types";
import type {
  InteractionChoiceId,
  InteractionId,
  InteractionPreviewRequest,
  InteractionSubmission,
  PreviewRequestId,
} from "../generated/interaction";
import type { PhaseSocketTransport } from "../../services/openPhaseSocket";
import type { FullTerminalDelivery } from "../../services/fullTerminalResult";

// Minimal mock WebSocket. Latest-constructed instance is exposed via
// `MockWebSocket.last` so tests can grab it synchronously — the adapter
// now opens the socket through the async `openPhaseSocket` helper, so
// `adapter.ws` is not populated until after the handshake completes.
class MockWebSocket extends EventTarget {
  static OPEN = 1;
  static CLOSED = 3;
  static last: MockWebSocket | null = null;
  readyState = MockWebSocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((event?: CloseEvent) => void) | null = null;
  send = vi.fn();
  close = vi.fn();
  constructor(public url: string) {
    super();
    MockWebSocket.last = this;
  }
  // Deliver raw observers and the IDL handler once each; happy-dom would
  // otherwise invoke the same IDL handler again through dispatchEvent.
  dispatchSynthetic(type: "message" | "close", data?: string, closeEvent = new CloseEvent("close")) {
    if (type === "message" && data !== undefined) {
      const handler = this.onmessage;
      this.onmessage = null;
      this.dispatchEvent(new MessageEvent("message", { data }));
      this.onmessage = handler;
      handler?.({ data });
    } else if (type === "close") {
      this.onclose?.(closeEvent);
      this.dispatchEvent(closeEvent);
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
const HEARTBEAT_HELLO = JSON.stringify({
  ...JSON.parse(SERVER_HELLO),
  data: {
    ...JSON.parse(SERVER_HELLO).data,
    heartbeat: { ping_interval_ms: 5000, silence_budget_ms: 15000 },
  },
});
const CORRELATED_HELLO = JSON.stringify({
  ...JSON.parse(HEARTBEAT_HELLO),
  data: {
    ...JSON.parse(HEARTBEAT_HELLO).data,
    command_correlation: "CommandIdV1",
  },
});

const REPLY_OWNERSHIP_FULL_KEY = { game_code: "GAME01", generation: 7 };

/**
 * Drives an adapter through the shared-handshake pipeline to the
 * post-ServerHello state. Returns the adapter's underlying mock ws once
 * the handshake has landed, so tests can then fire game-level frames.
 */
async function completeHandshake(adapter: WebSocketAdapter, hello = SERVER_HELLO): Promise<MockWebSocket> {
  // Allow the microtask inside `openPhaseSocket` to install its
  // `onmessage` handler before we deliver the hello frame.
  await Promise.resolve();
  const ws = MockWebSocket.last!;
  ws.dispatchSynthetic("message", hello);
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

function interactionSubmission(id: string): InteractionSubmission {
  return {
    interactionId: id as InteractionId,
    response: { type: "choose", data: { choiceId: `choice-${id}` as InteractionChoiceId } },
  };
}

function deliveredEventMarker(observation: unknown): string | null {
  if (typeof observation !== "object" || observation === null || !("resolvedWith" in observation)) return null;
  const { resolvedWith } = observation as { resolvedWith: SubmitResult };
  const first = resolvedWith.events[0] as unknown as { marker?: string } | undefined;
  return first?.marker ?? null;
}

function submissionTypes(ws: MockWebSocket): string[] {
  return ws.send.mock.calls
    .map(([raw]) => JSON.parse(raw as string) as { type?: string })
    .map((frame) => frame.type)
    .filter((type): type is string => type === "Action" || type === "Interaction");
}

function lastSubmissionCommandId(ws: MockWebSocket): unknown {
  for (const [raw] of ws.send.mock.calls.slice().reverse()) {
    if (typeof raw !== "string") continue;
    const frame: unknown = JSON.parse(raw);
    if (typeof frame !== "object" || frame === null || !("type" in frame)) continue;
    if (frame.type !== "Action" && frame.type !== "Interaction") continue;
    return "command_id" in frame ? frame.command_id : undefined;
  }
  throw new Error("No game submission was sent");
}

function deliverOwnershipStateUpdate(
  ws: MockWebSocket,
  marker: string,
  revision: number,
  commandId?: unknown,
): void {
  ws.dispatchSynthetic("message", JSON.stringify({
    type: "StateUpdate",
    command_id: commandId,
    data: {
      state_revision: revision,
      state: { ...createMockState(), turn_number: revision },
      events: [{ marker }],
      log_entries: [],
      full_key: REPLY_OWNERSHIP_FULL_KEY,
    },
  }));
}

function acceptFullReplyOwnershipSession(
  adapter: WebSocketAdapter,
  ws: MockWebSocket,
): void {
  const events = vi.fn();
  adapter.onEvent(events);
  ws.dispatchSynthetic("message", JSON.stringify({
    type: "GameStarted",
    data: {
      state_revision: 1,
      state: createMockState(),
      your_player: 0,
      player_token: "accepted-full-session-token",
      full_key: REPLY_OWNERSHIP_FULL_KEY,
    },
  }));
  expect(events).toHaveBeenCalledWith(expect.objectContaining({
    type: "sessionChanged",
    session: expect.objectContaining({ fullKey: REPLY_OWNERSHIP_FULL_KEY }),
  }));
  ws.send.mockClear();
}

async function exerciseOverlappingSubmissionOwnership(
  ws: MockWebSocket,
  submitFirst: () => Promise<SubmitResult>,
  submitSecond: () => Promise<SubmitResult>,
): Promise<void> {
  const readFirst = trackRejection(submitFirst());
  const readSecond = trackRejection(submitSecond());

  deliverOwnershipStateUpdate(ws, "reply-A", 2);
  const [firstAfterA, secondAfterA] = await Promise.all([readFirst(), readSecond()]);
  const secondWasSent = submissionTypes(ws).length > 1;

  if (secondWasSent) deliverOwnershipStateUpdate(ws, "reply-B", 3);
  const firstAfterB = await readFirst();
  await readSecond();

  const secondErrorCode = typeof secondAfterA === "object" && secondAfterA !== null && "code" in secondAfterA
    ? secondAfterA.code
    : null;
  expect({
    secondWasSent,
    secondErrorCode,
    firstOwnsReplyA: deliveredEventMarker(firstAfterA) === "reply-A",
    firstSettlesWithoutAnOrphan: deliveredEventMarker(firstAfterB) === "reply-A",
  }).toEqual({
    secondWasSent: false,
    secondErrorCode: AdapterErrorCode.ACTION_NOT_SENT,
    firstOwnsReplyA: true,
    firstSettlesWithoutAnOrphan: true,
  });
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
    terminalStore.commit.mockReset();
    terminalStore.commit.mockResolvedValue(true);
    adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws",
      "join",
      { main_deck: [], sideboard: [] },
      "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
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

  const fullFollowUpCases = [
    {
      label: "StateUpdate",
      acceptedEvent: "stateChanged",
      frame: (fullKey?: { game_code: string; generation: number }) => ({
        type: "StateUpdate",
        data: {
          state_revision: 2,
          state: { ...createMockState(), turn_number: 2 },
          events: [],
          ...(fullKey ? { full_key: fullKey } : {}),
        },
      }),
    },
    {
      label: "OpponentDisconnected",
      acceptedEvent: "opponentDisconnected",
      frame: (fullKey?: { game_code: string; generation: number }) => ({
        type: "OpponentDisconnected",
        data: { grace_seconds: 30, ...(fullKey ? { full_key: fullKey } : {}) },
      }),
    },
    {
      label: "OpponentReconnected",
      acceptedEvent: "opponentReconnected",
      frame: (fullKey?: { game_code: string; generation: number }) => ({
        type: "OpponentReconnected",
        data: fullKey ? { full_key: fullKey } : {},
      }),
    },
    {
      label: "ContestProgress",
      acceptedEvent: "contestProgress",
      frame: (fullKey?: { game_code: string; generation: number }) => ({
        type: "ContestProgress",
        data: { contest: 1, clicks: [[0, 3]], ...(fullKey ? { full_key: fullKey } : {}) },
      }),
    },
  ];

  it.each(
    fullFollowUpCases.flatMap(({ label, acceptedEvent, frame }) => [
      {
        label,
        acceptedEvent,
        frame: frame(),
        expectedError: "Server omitted a valid Full session identity",
      },
      {
        label,
        acceptedEvent,
        frame: frame({ game_code: "GAME01", generation: 2 }),
        expectedError: "Server changed the Full session identity",
      },
      {
        label,
        acceptedEvent,
        frame: frame({ game_code: "GAME01", generation: 1 }),
        expectedError: null,
      },
    ]),
  )("fences $label by the established Full identity", ({ frame, acceptedEvent, expectedError }) => {
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "GameCreated",
        data: {
          game_code: "GAME01",
          player_token: "player-token",
          full_key: { game_code: "GAME01", generation: 1 },
        },
      }),
    );
    const listener = vi.fn();
    adapter.onEvent(listener);

    ws.dispatchSynthetic("message", JSON.stringify(frame));

    expect(listener.mock.calls.some(([event]) => event.type === acceptedEvent)).toBe(
      expectedError === null,
    );
    if (expectedError) {
      expect(listener).toHaveBeenCalledWith({ type: "error", message: expectedError });
      expect(ws.close).toHaveBeenCalledOnce();
    } else {
      expect(ws.close).not.toHaveBeenCalled();
    }
  });

  it("stays silent on a late socket error after rejecting the session identity", () => {
    const listener = vi.fn();
    adapter.onEvent(listener);
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "GameCreated",
        data: {
          game_code: "GAME01",
          player_token: "player-token",
          full_key: { game_code: "GAME01", generation: 0 },
        },
      }),
    );
    expect(ws.close).toHaveBeenCalledOnce();

    ws.onerror?.();

    expect(listener.mock.calls.map(([event]) => event).filter((event) => event.type === "error"))
      .toEqual([{ type: "error", message: "Server omitted a valid Full session identity" }]);
  });

  it("emits a live socket error before any identity rejection", () => {
    const listener = vi.fn();
    adapter.onEvent(listener);
    ws.onerror!();
    expect(listener).toHaveBeenCalledWith({ type: "error", message: "WebSocket connection failed" });
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
      code: "REQUEST_FAILED",
      message: "Only the game host can export authoritative state",
      recoverable: true,
    });
  });

  it("rejects a correlated action failure and a mana-preview failure as recoverable request failures", async () => {
    const action = adapter.submitAction({ type: "PassPriority" }, 0);
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "ActionFailed", data: { message: "action persistence failed" } }),
    );
    await expect(action).rejects.toMatchObject({
      code: "REQUEST_FAILED",
      message: "action persistence failed",
      recoverable: true,
    });

    const preview = adapter.previewManaPayment({ type: "PassPriority" }, 0);
    const sent = JSON.parse(ws.send.mock.lastCall![0] as string);
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "ManaPaymentPreviewFailed", data: { request_id: sent.data.request_id, message: "preview lookup failed" } }),
    );
    await expect(preview).rejects.toMatchObject({
      code: "REQUEST_FAILED",
      message: "preview lookup failed",
      recoverable: true,
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
      undefined,
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
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
      expect(supportsTurnRewind(adapter)).toBe(true);
      expect(supportsTakeback(adapter)).toBe(true);
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
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
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

    it("exports authoritative state through the native bridge without a WebSocket URL", async () => {
      const nativeAdapter = new WebSocketAdapter(
        "native-engine",
        "host",
        { main_deck: [], sideboard: [] },
        undefined,
        undefined,
        undefined,
        "Player",
        nativeAiOptions(() => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport),
      );
      const initPromise = nativeAdapter.initialize();
      const nativeSocket = await completeHandshake(nativeAdapter);
      nativeSocket.dispatchSynthetic("message", JSON.stringify({
        type: "GameStarted",
        data: { state: createMockState(), your_player: 0 },
      }));
      await initPromise;

      const exported = nativeAdapter.exportPersistenceState();
      expect(nativeSocket.send).toHaveBeenLastCalledWith(JSON.stringify({ type: "ExportAuthoritativeState" }));
      nativeSocket.dispatchSynthetic("message", JSON.stringify({
        type: "AuthoritativeStateExport", data: { state: "{\"state\":{}}" },
      }));
      await expect(exported).resolves.toBe("{\"state\":{}}");
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
      expect(listener).toHaveBeenCalledWith(expect.objectContaining({
        type: "reconnecting",
        attempt: 1,
      }));
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
            boosterPackPool: ["Cube Card", "Cube Card", "Undealt sentinel"],
          },
        },
      );

      const attached = pregameAdapter.initializePregame();
      const nativeSocket = await completeHandshake(pregameAdapter);
      const calls = nativeSocket.send.mock.calls;
      const frame = JSON.parse(calls[calls.length - 1]![0] as string);
      expect(frame.type).toBe("CreateGameWithSettings");
      expect(frame.data.format_config).toEqual(formatConfig);
      expect(frame.data.booster_pack_pool).toEqual(["Cube Card", "Cube Card", "Undealt sentinel"]);

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

    it("hands serialized terminal authority to native P2P without local commit or early ACK", async () => {
      const nativeAdapter = new WebSocketAdapter(
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
            callerManagedTerminalDelivery: true,
          },
        },
      );
      const events: unknown[] = [];
      nativeAdapter.onEvent((event) => events.push(event));
      const delivery: FullTerminalDelivery = {
        key: { game_code: "NATIVE", generation: 1 },
        terminalRevision: 9,
        deliveryId: "recipient-delivery",
        credential: "recipient-credential",
        display: { winner: 0, reason: "Finished" },
        finalView: {
          players: [],
          objects: {},
          waiting_for: { type: "GameOver", data: { winner: 0 } },
        } as unknown as GameState,
      };

      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);
      nativeSocket.dispatchSynthetic("message", JSON.stringify({
        type: "SessionAttached",
        data: {
          game_code: "NATIVE",
          player_id: 1,
          player_token: "guest-token",
          full_key: { game_code: "NATIVE", generation: 1 },
        },
      }));
      await expect(attached).resolves.toMatchObject({ playerId: 1 });
      nativeSocket.dispatchSynthetic("message", JSON.stringify({ type: "TerminalResult", data: { delivery } }));

      expect(events).toContainEqual({ type: "terminalDelivery", delivery });
      expect(events).toContainEqual({ type: "sessionChanged", session: null });
      expect(events).not.toContainEqual(expect.objectContaining({ type: "terminalUnavailable" }));
      expect(terminalStore.commit).not.toHaveBeenCalled();
      expect(MockWebSocket.last).toBe(nativeSocket);
      nativeAdapter.dispose();
    });

    it("settles an export when native session identity validation fails", async () => {
      const nativeAdapter = new WebSocketAdapter(
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

    it("rejects a GameCreated without a Full session key before it can replace native reconnect credentials", async () => {
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
          data: { game_code: "NATIVE", player_token: "guest-token" },
        }),
      );

      await expect(attached).rejects.toThrow("Server omitted a valid Full session identity");
      expect(nativeAdapter.nativeSession).toEqual({
        gameCode: "NATIVE",
        playerId: 1,
        playerToken: "guest-token",
        fullKey: { game_code: "NATIVE", generation: 1 },
      });
      expect(listener).toHaveBeenCalledTimes(1);
      expect(listener).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
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

    it("reports the Full key before the seat or token when a native reconnect GameStarted is wrong on all three (the shared comparator's order)", async () => {
      const nativeAdapter = nativeReconnectAdapter();
      const attached = nativeAdapter.initializePregame();
      const nativeSocket = await completeHandshake(nativeAdapter);

      nativeSocket.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "GameStarted",
          data: {
            state_revision: 7,
            state: createMockState(),
            your_player: 0,
            player_token: "attacker-token",
            full_key: { game_code: "NATIVE", generation: 2 },
          },
        }),
      );

      await expect(attached).rejects.toThrow("Server changed the Full session identity");
      await expect(nativeAdapter.getSnapshot()).rejects.toThrow("No game state available");
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
    it("keeps reconnect revision 10 when a delayed revision 7 arrives", async () => {
      acceptFullReplyOwnershipSession(adapter, ws);
      ws.dispatchSynthetic("message", JSON.stringify({
        type: "GameStarted",
        data: {
          state_revision: 10, state: { ...createMockState(), turn_number: 10 }, your_player: 0,
          full_key: REPLY_OWNERSHIP_FULL_KEY,
        },
      }));
      const started = await adapter.getSnapshot();
      const events = vi.fn();
      adapter.onEvent(events);
      deliverOwnershipStateUpdate(ws, "stale", 7);
      expect(await adapter.getSnapshot()).toBe(started);
      expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "stateChanged" }));
      deliverOwnershipStateUpdate(ws, "fresh", 11);
      expect((await adapter.getState()).turn_number).toBe(11);
    });

    it("allows GameStarted to reseed the revision baseline", async () => {
      acceptFullReplyOwnershipSession(adapter, ws);
      deliverOwnershipStateUpdate(ws, "previous", 10);
      ws.dispatchSynthetic("message", JSON.stringify({
        type: "GameStarted",
        data: {
          state_revision: 3, state: { ...createMockState(), turn_number: 3 }, your_player: 0,
          full_key: REPLY_OWNERSHIP_FULL_KEY,
        },
      }));
      deliverOwnershipStateUpdate(ws, "reseeded", 4);
      expect((await adapter.getState()).turn_number).toBe(4);
    });

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
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
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
    async function liveReconnectFixture(hello = HEARTBEAT_HELLO) {
      const subject = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
    );
      const initialized = subject.initialize();
      const socket = await completeHandshake(subject, hello);
      socket.dispatchSynthetic("message", JSON.stringify({
        type: "GameStarted",
        data: {
          state: createMockState(), your_player: 0,
          player_token: "accepted-full-session-token", full_key: REPLY_OWNERSHIP_FULL_KEY,
        },
      }));
      await initialized;
      return { subject, socket };
    }

    it("retains one privacy-safe close record and updates it when reconnect resumes", async () => {
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const online = vi.spyOn(navigator, "onLine", "get").mockReturnValue(false);
      const { subject, socket } = await liveReconnectFixture();
      try {
        await vi.advanceTimersByTimeAsync(1000);
        socket.dispatchSynthetic("message", JSON.stringify({ type: "Pong", data: { timestamp: Date.now() } }));
        await vi.advanceTimersByTimeAsync(1000);
        socket.dispatchSynthetic("close", undefined, new CloseEvent("close", {
          code: 1006, reason: "PRIVATE-token@private-host", wasClean: false,
        }));
        const loss = getDiagnosticHistory().filter((entry) => entry.kind === "websocket-loss").slice(-1)[0];
        expect(loss).toMatchObject({
          code: 1006, reason: "redacted", wasClean: false,
          receiveAgeMs: 1000, sendAgeMs: 2000, visibility: "visible", online: false,
          silenceExceeded: false, reconnectAttempt: 0, outcome: "pending",
        });
        expect(loadDiagnosticHistory()).toContainEqual(loss);
        expect(JSON.stringify(loss)).not.toMatch(/PRIVATE|private-host|accepted-full-session-token/);
        await vi.advanceTimersByTimeAsync(1000);
        const recovered = await completeHandshake(subject, HEARTBEAT_HELLO);
        recovered.dispatchSynthetic("message", JSON.stringify({
          type: "GameStarted",
          data: {
            state: createMockState(), your_player: 0,
            player_token: "accepted-full-session-token", full_key: REPLY_OWNERSHIP_FULL_KEY,
          },
        }));
        const retained = getDiagnosticHistory().filter((entry) => entry.kind === "websocket-loss" && entry.diagnosticId === loss.diagnosticId);
        expect(retained).toHaveLength(1);
        expect(retained[0]).toMatchObject({ outcome: "resumed" });
        expect(loadDiagnosticHistory()).toContainEqual(retained[0]);
      } finally {
        subject.dispose();
        online.mockRestore();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it("records an exceeded silence budget on a later loss without closing the socket itself", async () => {
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const { subject, socket } = await liveReconnectFixture();
      try {
        await vi.advanceTimersByTimeAsync(15_000);
        expect(socket.close).not.toHaveBeenCalled();
        socket.dispatchSynthetic("close", undefined, new CloseEvent("close", { code: 1006, wasClean: false }));
        expect(getDiagnosticHistory().filter((entry) => entry.kind === "websocket-loss").slice(-1)[0]).toMatchObject({
          code: 1006, silenceExceeded: true, receiveAgeMs: 15_000,
        });
      } finally {
        subject.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it("captures loss during a reconnect handshake and marks the retry chain gave-up", async () => {
      vi.useFakeTimers();
      const { subject, socket } = await liveReconnectFixture();
      try {
        socket.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(1000);
        const retrySocket = MockWebSocket.last!;
        retrySocket.dispatchSynthetic("close", undefined, new CloseEvent("close", {
          code: 1013, reason: "Server shutdown", wasClean: true,
        }));
        const loss = getDiagnosticHistory().filter((entry) => entry.kind === "websocket-loss").slice(-1)[0];
        expect(loss).toMatchObject({ code: 1013, reason: "server shutdown", reconnectAttempt: 1, outcome: "pending" });
        await vi.advanceTimersByTimeAsync(135_000);
        expect(getDiagnosticHistory().find((entry) => entry.diagnosticId === loss.diagnosticId)).toMatchObject({ outcome: "gave-up" });
      } finally {
        subject.dispose();
        vi.useRealTimers();
      }
    });

    it("still captures native closure when a transport error consumer disposes first", async () => {
      const { subject, socket } = await liveReconnectFixture();
      subject.onEvent((event) => { if (event.type === "error") subject.dispose(); });
      socket.onerror?.();
      socket.dispatchSynthetic("close", undefined, new CloseEvent("close", { code: 1006, wasClean: false }));
      expect(getDiagnosticHistory().find((entry) => entry.diagnosticId === diagnosticIdFor(socket))).toMatchObject({
        kind: "websocket-loss", code: 1006, wasClean: false, silenceExceeded: false, outcome: "not-attempted",
      });
      subject.dispose();
    });

    it("does not close legacy servers during sixty seconds of visible AI silence", async () => {
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const { subject, socket } = await liveReconnectFixture(SERVER_HELLO);
      try {
        await vi.advanceTimersByTimeAsync(60_000);
        expect(socket.close).not.toHaveBeenCalled();
        expect(socket.send).toHaveBeenCalledWith(expect.stringContaining('"type":"Ping"'));
      } finally {
        subject.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it("uses the advertised silence budget for the diagnostic and never closes on silence", async () => {
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const hello = JSON.parse(HEARTBEAT_HELLO);
      hello.data.heartbeat.silence_budget_ms = 30_000;
      // Each probe silences a fresh live socket for `ms`, then drops it and
      // reads the loss record that drop produced.
      const lossAfterSilence = async (ms: number) => {
        const { subject, socket } = await liveReconnectFixture(JSON.stringify(hello));
        try {
          await vi.advanceTimersByTimeAsync(ms);
          expect(socket.close).not.toHaveBeenCalled();
          socket.dispatchSynthetic("close", undefined, new CloseEvent("close", { code: 1006 }));
          return getDiagnosticHistory().filter((entry) => entry.kind === "websocket-loss").slice(-1)[0];
        } finally {
          subject.dispose();
        }
      };
      try {
        expect(await lossAfterSilence(20_000)).toMatchObject({ silenceExceeded: false });
        expect(await lossAfterSilence(120_000)).toMatchObject({ silenceExceeded: true });
      } finally {
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it.each([0, -1, "45000", null])(
      "treats a malformed advertised silence budget (%s) as no budget",
      async (budget) => {
        vi.useFakeTimers();
        const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
        const hello = JSON.parse(HEARTBEAT_HELLO);
        hello.data.heartbeat.silence_budget_ms = budget;
        const { subject, socket } = await liveReconnectFixture(JSON.stringify(hello));
        try {
          await vi.advanceTimersByTimeAsync(60_000);
          expect(socket.close).not.toHaveBeenCalled();
        } finally {
          subject.dispose();
          visibility.mockRestore();
          vi.useRealTimers();
        }
      },
    );

    it.each(["timeout", "ws_error", "closed"] as const)(
      "continues reconnecting after a %s handshake failure",
      async (failure) => {
        acceptFullReplyOwnershipSession(adapter, ws);
        vi.useFakeTimers();
        try {
          ws.dispatchSynthetic("close");
          await vi.advanceTimersByTimeAsync(1000);
          const failedSocket = MockWebSocket.last!;
          expect(failedSocket).not.toBe(ws);
          if (failure === "timeout") {
            await vi.advanceTimersByTimeAsync(5000);
          } else if (failure === "ws_error") {
            failedSocket.onerror?.();
          } else {
            failedSocket.dispatchSynthetic("close");
          }
          await vi.advanceTimersByTimeAsync(6000);
          expect(MockWebSocket.last).not.toBe(failedSocket);
          const recoveredSocket = await completeHandshake(adapter);
          expect(recoveredSocket.send).toHaveBeenCalledWith(expect.stringContaining('"type":"Reconnect"'));
        } finally {
          adapter.dispose();
          vi.useRealTimers();
        }
      },
    );
    it("keeps retrying throughout the server grace and eventually stops", async () => {
      vi.useFakeTimers();
      const { subject, socket } = await liveReconnectFixture();
      const events = vi.fn();
      subject.onEvent(events);
      try {
        socket.dispatchSynthetic("close");
        let previous = socket;
        for (let second = 0; second < 120; second++) {
          await vi.advanceTimersByTimeAsync(1000);
          if (MockWebSocket.last !== previous) {
            previous = await completeHandshake(subject);
            previous.dispatchSynthetic("close");
          }
        }
        expect(events).not.toHaveBeenCalledWith({ type: "reconnectFailed" });
        await vi.advanceTimersByTimeAsync(15000);
        expect(events).toHaveBeenCalledWith({ type: "reconnectFailed" });
      } finally {
        subject.dispose();
        vi.useRealTimers();
      }
    });

    // Fails each redial the moment it is dialed. Returns the elapsed fake time at
    // which `done` first holds. Fixed jitter makes the schedule deterministic.
    async function failEveryRedial(done: () => boolean, limitMs = 200_000): Promise<number> {
      const stepMs = 100;
      let previous = MockWebSocket.last;
      let elapsed = 0;
      while (!done() && elapsed < limitMs) {
        await vi.advanceTimersByTimeAsync(stepMs);
        elapsed += stepMs;
        if (MockWebSocket.last !== previous) {
          previous = MockWebSocket.last;
          previous!.dispatchSynthetic("close");
        }
      }
      return elapsed;
    }

    it("ends a remote reconnect episode without scheduling an attempt that cannot dial", async () => {
      vi.useFakeTimers();
      const random = vi.spyOn(Math, "random").mockReturnValue(1);
      const { subject, socket } = await liveReconnectFixture();
      const events = vi.fn();
      subject.onEvent(events);
      try {
        socket.dispatchSynthetic("close");
        const failedAt = await failEveryRedial(
          () => events.mock.calls.some(([event]) => (event as { type: string }).type === "reconnectFailed"),
        );
        // Redials fire at 1, 3, 7, 12, ... 122 s; the next would fire at 127 s,
        // past the 125 s budget. The episode ends at 122 s, not at the 125 s edge.
        expect(failedAt).toBeGreaterThan(120_000);
        expect(failedAt).toBeLessThan(124_000);
        const attempts = events.mock.calls
          .map(([event]) => event as { type: string; attempt?: number })
          .filter((event) => event.type === "reconnecting");
        expect(attempts).toHaveLength(26);
      } finally {
        random.mockRestore();
        subject.dispose();
        vi.useRealTimers();
      }
    });

    it("reports no attempt denominator for a remote episode bounded by time", async () => {
      vi.useFakeTimers();
      const { subject, socket } = await liveReconnectFixture();
      const events = vi.fn();
      subject.onEvent(events);
      try {
        socket.dispatchSynthetic("close");
        expect(events).toHaveBeenCalledWith({ type: "reconnecting", attempt: 1, maxAttempts: 0 });
      } finally {
        subject.dispose();
        vi.useRealTimers();
      }
    });

    it("keeps the loopback sidecar's original 8-attempt, ~32 s reconnect budget", async () => {
      vi.useFakeTimers();
      const random = vi.spyOn(Math, "random").mockReturnValue(1);
      MockWebSocket.last = null;
      const nativeAdapter = new WebSocketAdapter(
        "native-engine", "host", { main_deck: [], sideboard: [] },
        undefined, undefined, undefined, "Player",
        {
          nativeAi: {
            socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
            aiSeats: [],
            playerCount: 2,
          },
        },
      );
      try {
        const initialized = nativeAdapter.initialize();
        const nativeSocket = await completeHandshake(nativeAdapter);
        nativeSocket.dispatchSynthetic("message", JSON.stringify({
          type: "GameCreated",
          data: { game_code: "ABCD", player_token: "tok", full_key: { game_code: "ABCD", generation: 1 } },
        }));
        nativeSocket.dispatchSynthetic("message", JSON.stringify({
          type: "GameStarted",
          data: { state: createMockState(), your_player: 0 },
        }));
        await initialized;
        const events = vi.fn();
        nativeAdapter.onEvent(events);
        nativeSocket.dispatchSynthetic("close");
        const failedAt = await failEveryRedial(
          () => events.mock.calls.some(([event]) => (event as { type: string }).type === "reconnectFailed"),
        );
        // 1 + 2 + 4 + 5 * 5 = 32 s of backoff across 8 attempts, then give up
        // rather than carry on for the remote server's 120 s grace.
        expect(failedAt).toBeGreaterThanOrEqual(31_000);
        expect(failedAt).toBeLessThanOrEqual(34_000);
        const attempts = events.mock.calls
          .map(([event]) => event as { type: string; attempt?: number; maxAttempts?: number })
          .filter((event) => event.type === "reconnecting");
        expect(attempts).toHaveLength(8);
        expect(attempts.every((event) => event.maxAttempts === 8)).toBe(true);
      } finally {
        random.mockRestore();
        nativeAdapter.dispose();
        vi.useRealTimers();
      }
    });

    it("fully ends the reconnect episode when a reconnect is answered with PasswordRequired", async () => {
      acceptFullReplyOwnershipSession(adapter, ws);
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const events = vi.fn();
      adapter.onEvent(events);
      try {
        ws.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(1000);
        const retry = await completeHandshake(adapter);
        retry.dispatchSynthetic("message", JSON.stringify({
          type: "PasswordRequired", data: { game_code: "GAME01" },
        }));
        expect(events).toHaveBeenCalledWith({ type: "reconnectFailed" });
        // The lifecycle listeners are gone and the budget is reset: neither a
        // network resume nor a later manual reconnect inherits a dead episode.
        const dialed = MockWebSocket.last;
        window.dispatchEvent(new Event("online"));
        document.dispatchEvent(new Event("visibilitychange"));
        await vi.advanceTimersByTimeAsync(10_000);
        expect(MockWebSocket.last).toBe(dialed);
        expect((adapter as unknown as { reconnectStartedAt: number | null }).reconnectStartedAt).toBeNull();
      } finally {
        adapter.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    const BOOTSTRAP_WITHHELD = JSON.stringify({
      type: "StateWithheld",
      data: {
        full_key: REPLY_OWNERSHIP_FULL_KEY, state_revision: 3,
        withheld: { type: "GameStarted", data: { your_player: 0 } },
        reason: "The game state is too large to send.",
      },
    });
    const TAKEBACK_REQUESTED = JSON.stringify({
      type: "TakebackRequested", data: { requester: 1, requester_name: "guest-one", voters: [0] },
    });
    const RESTORED = JSON.stringify({
      type: "StateUpdate",
      data: {
        state_revision: 4, state: { ...createMockState(), turn_number: 4 },
        events: [], full_key: REPLY_OWNERSHIP_FULL_KEY,
      },
    });
    const teardownEvents = [
      { type: "reconnectFailed" },
      { type: "error", message: expect.anything() },
      { type: "sessionChanged", session: null },
    ];

    it("reattaches a withheld reconnect onto the delivered board", async () => {
      acceptFullReplyOwnershipSession(adapter, ws);
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const events = vi.fn();
      adapter.onEvent(events);
      try {
        ws.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(1000);
        const retry = await completeHandshake(adapter);
        retry.dispatchSynthetic("message", BOOTSTRAP_WITHHELD);
        expect(events).toHaveBeenCalledWith({ type: "reconnected", inGame: true });
        expect(events).toHaveBeenCalledWith({
          type: "stateDelivery",
          withheld: { revision: 3, reason: "The game state is too large to send." },
        });
        for (const teardown of teardownEvents) expect(events).not.toHaveBeenCalledWith(teardown);
        expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "operationFailed" }));
        retry.dispatchSynthetic("message", TAKEBACK_REQUESTED);
        expect(events).toHaveBeenCalledWith(expect.objectContaining({ type: "takebackRequested", requester: 1, voters: [0] }));
        retry.dispatchSynthetic("message", RESTORED);
        expect(events).toHaveBeenCalledWith(expect.objectContaining({ type: "stateChanged", serverRevision: 4 }));
        expect(events).toHaveBeenLastCalledWith(expect.objectContaining({ type: "stateChanged" }));
        expect(events).toHaveBeenCalledWith({ type: "stateDelivery", withheld: null });
      } finally {
        adapter.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it("fails a withheld reconnect without a board recoverably, then clears the failure when a state arrives", async () => {
      const subject = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
    );
      const events = vi.fn();
      subject.onEvent(events);
      const session = {
        gameCode: "GAME01", playerToken: "accepted-full-session-token",
        fullKey: REPLY_OWNERSHIP_FULL_KEY, serverUrl: "wss://localhost:9374/ws", timestamp: Date.now(),
      };
      try {
        expect(subject.tryReconnect(session)).toBe(true);
        const socket = await completeHandshake(subject);
        expect(socket.send).toHaveBeenCalledWith(expect.stringContaining('"type":"Reconnect"'));
        const seatOneNotice = JSON.parse(BOOTSTRAP_WITHHELD) as { data: Record<string, unknown> };
        seatOneNotice.data.withheld = { type: "GameStarted", data: { your_player: 1 } };
        socket.dispatchSynthetic("message", JSON.stringify(seatOneNotice));
        const types = events.mock.calls.map(([event]) => (event as { type: string }).type);
        expect(types.indexOf("playerIdentity")).toBeGreaterThan(-1);
        expect(types.indexOf("playerIdentity")).toBeLessThan(types.indexOf("reconnectFailed"));
        expect(events).toHaveBeenCalledWith({ type: "playerIdentity", playerId: 1, opponentName: null });
        expect(subject.playerId).toBe(1);
        expect(events).toHaveBeenCalledWith({ type: "reconnectFailed" });
        expect(events).toHaveBeenCalledWith({ type: "operationFailed", message: "The game state is too large to send." });
        expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
        expect(events).not.toHaveBeenCalledWith({ type: "sessionChanged", session: null });
        expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "reconnected" }));

        socket.dispatchSynthetic("message", JSON.stringify({
          type: "TakebackRequested", data: { requester: 0, requester_name: "host", voters: [1] },
        }));
        const request = events.mock.calls
          .map(([event]) => event as { type: string; requester?: number; voters?: number[] })
          .find((event) => event.type === "takebackRequested");
        // GamePage tests the announced seat's membership in the voters: seat 0's request is approvable here.
        expect(request?.requester).toBe(0);
        expect(request?.voters).toEqual([1]);
        expect(subject.playerId).toBe(1);
        socket.dispatchSynthetic("message", RESTORED);
        expect(events).toHaveBeenCalledWith(expect.objectContaining({ type: "stateChanged", serverRevision: 4 }));
        expect(events).toHaveBeenCalledWith({ type: "reconnected", inGame: true });
      } finally {
        subject.dispose();
      }
    });

    const GAME_STARTED = JSON.stringify({
      type: "GameStarted",
      data: { state_revision: 3, state: createMockState(), your_player: 0, full_key: REPLY_OWNERSHIP_FULL_KEY },
    });
    const reconnectedSocket = async (): Promise<{ subject: WebSocketAdapter; socket: MockWebSocket }> => {
      acceptFullReplyOwnershipSession(adapter, ws);
      ws.dispatchSynthetic("close");
      await vi.advanceTimersByTimeAsync(1000);
      return { subject: adapter, socket: await completeHandshake(adapter) };
    };
    const boardlessReconnectSocket = async (): Promise<{ subject: WebSocketAdapter; socket: MockWebSocket }> => {
      const subject = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
    );
      subject.tryReconnect({
        gameCode: "GAME01", playerToken: "accepted-full-session-token",
        fullKey: REPLY_OWNERSHIP_FULL_KEY, serverUrl: "wss://localhost:9374/ws", timestamp: Date.now(),
      });
      return { subject, socket: await completeHandshake(subject) };
    };

    it.each([
      ["GameStarted during a reconnect", reconnectedSocket, GAME_STARTED],
      ["a withheld GameStarted onto the delivered board", reconnectedSocket, BOOTSTRAP_WITHHELD],
      ["a withheld GameStarted with no board", boardlessReconnectSocket, BOOTSTRAP_WITHHELD],
    ] as const)("marks %s as a bootstrap before the replayed takeback request", async (_leg, connect, bootstrap) => {
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const { subject, socket } = await connect();
      const events = vi.fn();
      subject.onEvent(events);
      try {
        socket.dispatchSynthetic("message", bootstrap);
        socket.dispatchSynthetic("message", TAKEBACK_REQUESTED);
        const types = events.mock.calls
          .map(([event]) => (event as { type: string }).type)
          .filter((type) => type === "bootstrapDelivered" || type === "takebackRequested");
        expect(types).toEqual(["bootstrapDelivered", "takebackRequested"]);
      } finally {
        subject.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it("rejects a withheld initial bootstrap as a recoverable request failure", async () => {
      const subject = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
    );
      try {
        const initialized = trackRejection(subject.initialize());
        const socket = await completeHandshake(subject);
        socket.dispatchSynthetic("message", BOOTSTRAP_WITHHELD);
        expect(await initialized()).toMatchObject({
          code: "REQUEST_FAILED", message: "The game state is too large to send.", recoverable: true,
        });
      } finally {
        subject.dispose();
      }
    });

    it.each(["online", "visible"] as const)("retries a backed-off session immediately on %s", async (resume) => {
      acceptFullReplyOwnershipSession(adapter, ws);
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      try {
        ws.dispatchSynthetic("close");
        if (resume === "online") window.dispatchEvent(new Event("online"));
        else document.dispatchEvent(new Event("visibilitychange"));
        await vi.advanceTimersByTimeAsync(0);
        expect(MockWebSocket.last).not.toBe(ws);
        const recovered = await completeHandshake(adapter);
        expect(recovered.send).toHaveBeenCalledWith(expect.stringContaining('"type":"Reconnect"'));
        adapter.dispose();
        const disposedSocket = MockWebSocket.last;
        window.dispatchEvent(new Event("online"));
        document.dispatchEvent(new Event("visibilitychange"));
        await vi.advanceTimersByTimeAsync(6000);
        expect(MockWebSocket.last).toBe(disposedSocket);
      } finally {
        adapter.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it("does not spend reconnect attempts on resume notifications that never dial", async () => {
      acceptFullReplyOwnershipSession(adapter, ws);
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const events = vi.fn();
      adapter.onEvent(events);
      try {
        ws.dispatchSynthetic("close");
        for (let i = 0; i < 70; i++) window.dispatchEvent(new Event("online"));
        expect(events).not.toHaveBeenCalledWith({ type: "reconnectFailed" });
        await vi.advanceTimersByTimeAsync(0);
        expect(MockWebSocket.last).not.toBe(ws);
        expect(events).toHaveBeenLastCalledWith(expect.objectContaining({ type: "reconnecting", attempt: 1 }));
      } finally {
        adapter.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it("ends the reconnect episode when a hosting reconnect is answered with GameCreated", async () => {
      acceptFullReplyOwnershipSession(adapter, ws);
      vi.useFakeTimers();
      const events = vi.fn();
      adapter.onEvent(events);
      try {
        ws.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(1000);
        const hosting = await completeHandshake(adapter);
        hosting.dispatchSynthetic("message", JSON.stringify({
          type: "GameCreated",
          data: { game_code: "GAME01", player_token: "accepted-full-session-token", full_key: REPLY_OWNERSHIP_FULL_KEY },
        }));
        expect(events).toHaveBeenCalledWith({ type: "reconnected", inGame: false });
        // Waiting in the lobby past the old budget must not doom the next drop.
        await vi.advanceTimersByTimeAsync(130_000);
        hosting.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(1000);
        expect(events).not.toHaveBeenCalledWith({ type: "reconnectFailed" });
        expect(MockWebSocket.last).not.toBe(hosting);
      } finally {
        adapter.dispose();
        vi.useRealTimers();
      }
    });

    it("reports a reconnect answered with GameStarted as back in the game", async () => {
      acceptFullReplyOwnershipSession(adapter, ws);
      vi.useFakeTimers();
      const events = vi.fn();
      adapter.onEvent(events);
      try {
        ws.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(1000);
        const resumed = await completeHandshake(adapter);
        resumed.dispatchSynthetic("message", JSON.stringify({
          type: "GameStarted",
          data: {
            state_revision: 2, state: createMockState(), your_player: 0,
            player_token: "accepted-full-session-token", full_key: REPLY_OWNERSHIP_FULL_KEY,
          },
        }));
        expect(events).toHaveBeenCalledWith({ type: "reconnected", inGame: true });
      } finally {
        adapter.dispose();
        vi.useRealTimers();
      }
    });

    it("ignores a late error from a retired socket once its replacement is live", async () => {
      acceptFullReplyOwnershipSession(adapter, ws);
      vi.useFakeTimers();
      const events = vi.fn();
      adapter.onEvent(events);
      try {
        ws.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(1000);
        const replacement = await completeHandshake(adapter);
        events.mockClear();
        ws.onerror?.();
        expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
        replacement.onerror?.();
        expect(events).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
      } finally {
        adapter.dispose();
        vi.useRealTimers();
      }
    });

    it("keeps a visible silent socket open; a real transport close still reconnects", async () => {
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const { subject, socket } = await liveReconnectFixture();
      const events = vi.fn();
      subject.onEvent(events);
      try {
        await vi.advanceTimersByTimeAsync(120_000);
        expect(socket.close).not.toHaveBeenCalled();
        expect(socket.send).toHaveBeenCalledWith(expect.stringContaining('"type":"Ping"'));
        socket.dispatchSynthetic("close");
        expect(events).toHaveBeenCalledWith(expect.objectContaining({ type: "reconnecting" }));
      } finally {
        subject.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it.each(["Pong", "StateUpdate"] as const)("uses inbound %s as liveness evidence", async (type) => {
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const { subject, socket } = await liveReconnectFixture();
      try {
        await vi.advanceTimersByTimeAsync(10000);
        socket.dispatchSynthetic("message", JSON.stringify({
          type,
          data: type === "Pong" ? { timestamp: Date.now() } : {
            state_revision: 2, state: createMockState(), events: [], full_key: REPLY_OWNERSHIP_FULL_KEY,
          },
        }));
        await vi.advanceTimersByTimeAsync(10000);
        // 20 s of wall time but only 10 s since the inbound frame: within the 15 s budget.
        socket.dispatchSynthetic("close", undefined, new CloseEvent("close", { code: 1006 }));
        expect(getDiagnosticHistory().filter((entry) => entry.kind === "websocket-loss").slice(-1)[0])
          .toMatchObject({ silenceExceeded: false });
        expect(socket.close).not.toHaveBeenCalled();
      } finally {
        subject.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it("does not mistake background throttling or a local clock gap for a dead peer", async () => {
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
      const { subject, socket } = await liveReconnectFixture();
      try {
        await vi.advanceTimersByTimeAsync(30000);
        expect(socket.close).not.toHaveBeenCalled();
        visibility.mockReturnValue("visible");
        document.dispatchEvent(new Event("visibilitychange"));
        vi.setSystemTime(Date.now() + 60000);
        await vi.advanceTimersByTimeAsync(5000);
        expect(socket.close).not.toHaveBeenCalled();
      } finally {
        subject.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it("reconnects with the persisted session after socket close", async () => {
      MockWebSocket.last = null;
      const reconnectingAdapter = new WebSocketAdapter(
        "ws://localhost:9374/ws",
        "join",
        { main_deck: [], sideboard: [] },
        "ABC123",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
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

  it("refuses a server host bring-up outside the lobby without opening a socket", async () => {
    MockWebSocket.last = null;
    const hostAdapter = new WebSocketAdapter(
      "ws://localhost:9374/ws",
      "host",
      { main_deck: [], sideboard: [] },
      undefined,
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
    );

    const outcome = trackRejection(hostAdapter.initialize());

    await expect(outcome()).resolves.toMatchObject({ code: "WS_ERROR", recoverable: false });
    expect(MockWebSocket.last).toBeNull();
  });

  describe("negotiated game-command correlation", () => {
    let subject: WebSocketAdapter;
    let socket: MockWebSocket;

    beforeEach(async () => {
      subject = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
    );
      const initialized = subject.initialize();
      socket = await completeHandshake(subject, CORRELATED_HELLO);
      acceptFullReplyOwnershipSession(subject, socket);
      await initialized;
    });

    afterEach(() => subject.dispose());

    const WITHHELD_REASON = "The game state after this action is too large to send.";

    function deliverWithheld(revision: number, commandId?: unknown, fullKey = REPLY_OWNERSHIP_FULL_KEY): void {
      socket.dispatchSynthetic("message", JSON.stringify({
        type: "StateWithheld",
        command_id: commandId,
        data: { full_key: fullKey, state_revision: revision, withheld: { type: "StateUpdate" }, reason: WITHHELD_REASON },
      }));
    }

    function deliveries(events: ReturnType<typeof vi.fn>): unknown[] {
      return events.mock.calls
        .map(([event]) => event as { type: string; withheld?: unknown })
        .filter((event) => event.type === "stateDelivery")
        .map((event) => event.withheld);
    }

    function latestDelivery(events: ReturnType<typeof vi.fn>): unknown {
      const all = deliveries(events);
      return all[all.length - 1];
    }

    it("settles consecutive withheld commands as applied and keeps the status until a state arrives", async () => {
      deliverOwnershipStateUpdate(socket, "cached", 5);
      const events = vi.fn();
      subject.onEvent(events);
      for (const revision of [6, 7]) {
        const outcome = trackRejection(subject.submitAction({ type: "PassPriority" }, 0));
        deliverWithheld(revision, lastSubmissionCommandId(socket));
        await expect(outcome()).resolves.toEqual({ resolvedWith: { events: [], log_entries: [] } });
      }
      expect(deliveries(events)).toEqual([
        { revision: 6, reason: WITHHELD_REASON },
        { revision: 7, reason: WITHHELD_REASON },
      ]);
      expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "stateChanged" }));
      deliverOwnershipStateUpdate(socket, "resumed", 8);
      expect(events).toHaveBeenCalledWith(expect.objectContaining({ type: "stateChanged", serverRevision: 8 }));
      expect(latestDelivery(events)).toBeNull();

      deliverWithheld(9);
      expect(latestDelivery(events)).toEqual({ revision: 9, reason: WITHHELD_REASON });
      const own = trackRejection(subject.submitAction({ type: "PassPriority" }, 0));
      deliverOwnershipStateUpdate(socket, "own", 10, lastSubmissionCommandId(socket));
      await expect(own()).resolves.toEqual({ resolvedWith: { events: [{ marker: "own" }], log_entries: [] } });
      expect(latestDelivery(events)).toBeNull();
    });

    it("does not settle a stamped command from an id-less withheld notice", async () => {
      deliverOwnershipStateUpdate(socket, "cached", 5);
      const events = vi.fn();
      subject.onEvent(events);
      const outcome = trackRejection(subject.submitAction({ type: "PassPriority" }, 0));
      deliverWithheld(6);
      expect(await outcome()).toBe("never settled");
      expect(deliveries(events)).toEqual([{ revision: 6, reason: WITHHELD_REASON }]);
    });

    it("settles a stale withheld notice's command without reporting the newer board withheld", async () => {
      deliverOwnershipStateUpdate(socket, "cached", 5);
      const events = vi.fn();
      subject.onEvent(events);
      const outcome = trackRejection(subject.submitAction({ type: "PassPriority" }, 0));
      deliverWithheld(4, lastSubmissionCommandId(socket));
      await expect(outcome()).resolves.toEqual({ resolvedWith: { events: [], log_entries: [] } });
      expect(deliveries(events)).toEqual([]);
      deliverWithheld(5);
      expect(deliveries(events)).toEqual([{ revision: 5, reason: WITHHELD_REASON }]);
    });

    it("drops a withheld notice for another Full session", () => {
      const events = vi.fn();
      subject.onEvent(events);
      deliverWithheld(6, undefined, { game_code: "GAME01", generation: 8 });
      expect(events).toHaveBeenCalledWith({ type: "error", message: "Server changed the Full session identity" });
      expect(deliveries(events)).toEqual([]);
    });

    it("clears the withheld status when a takeback's state arrives", async () => {
      deliverOwnershipStateUpdate(socket, "cached", 5);
      const events = vi.fn();
      subject.onEvent(events);
      const outcome = trackRejection(subject.submitAction({ type: "PassPriority" }, 0));
      deliverWithheld(6, lastSubmissionCommandId(socket));
      deliverWithheld(7);
      socket.dispatchSynthetic("message", JSON.stringify({
        type: "TakebackRequested", data: { requester: 1, requester_name: "guest-one", voters: [0] },
      }));
      socket.dispatchSynthetic("message", JSON.stringify({
        type: "StateUpdate",
        data: {
          state_revision: 8, state: { ...createMockState(), turn_number: 8 },
          events: [], full_key: REPLY_OWNERSHIP_FULL_KEY,
        },
      }));
      socket.dispatchSynthetic("message", JSON.stringify({
        type: "TakebackResolved", data: { resolution: "approved", resolved_by: 0 },
      }));
      await expect(outcome()).resolves.toEqual({ resolvedWith: { events: [], log_entries: [] } });
      const sequence = events.mock.calls
        .map(([event]) => event as { type: string; withheld?: { revision: number } | null; serverRevision?: number })
        .filter((event) => ["stateDelivery", "takebackRequested", "stateChanged", "takebackResolved"].includes(event.type))
        .map((event) => event.type === "stateDelivery"
          ? `withheld:${event.withheld?.revision ?? "none"}`
          : event.type === "stateChanged" ? `state:${event.serverRevision}` : event.type);
      expect(sequence).toEqual([
        "withheld:6", "withheld:7", "takebackRequested", "withheld:none", "state:8", "takebackResolved",
      ]);

      expect(events).toHaveBeenCalledWith({ type: "takebackResolved", resolution: "approved", resolvedBy: 0 });

      deliverWithheld(9);
      expect(latestDelivery(events)).toEqual({ revision: 9, reason: WITHHELD_REASON });
    });

    it("emits the resolution a TakebackResolved frame names", () => {
      deliverOwnershipStateUpdate(socket, "cached", 5);
      const events = vi.fn();
      subject.onEvent(events);
      socket.dispatchSynthetic("message", JSON.stringify({
        type: "TakebackResolved", data: { resolution: "withdrawn", resolved_by: 1 },
      }));
      expect(events).toHaveBeenCalledWith({ type: "takebackResolved", resolution: "withdrawn", resolvedBy: 1 });
    });

    it("keeps all four simultaneous mulligan submissions owned when a peer advances", async () => {
      const seats = [{ subject, socket }];
      const keep: GameAction = { type: "MulliganDecision", data: { choice: { type: "Keep" } } };
      const pending = Array.from({ length: 4 }, (_, player) => ({
        player, mulligan_count: 0, phase: { type: "Declare" as const },
      }));
      const mulliganState: GameState = {
        ...createMockState(),
        waiting_for: { type: "MulliganDecision", data: { pending, free_first_mulligan: true } },
      };
      try {
        for (let player = 0; player < 4; player++) {
          let initialized: Promise<void> | undefined;
          if (player !== 0) {
            const next = new WebSocketAdapter(
              "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
    );
            initialized = next.initialize();
            seats.push({ subject: next, socket: await completeHandshake(next, CORRELATED_HELLO) });
          }
          seats[player].socket.dispatchSynthetic("message", JSON.stringify({
            type: "GameStarted",
            data: {
              state_revision: 1, state: mulliganState, your_player: player,
              legal_actions: [keep], full_key: REPLY_OWNERSHIP_FULL_KEY,
              player_token: player === 0 ? "accepted-full-session-token" : `seat-token-${player}`,
            },
          }));
          if (initialized) await initialized;
        }
        const outcomes = seats.map((seat, player) => trackRejection(seat.subject.submitAction(keep, player)));
        const ids = seats.map((seat) => lastSubmissionCommandId(seat.socket));
        const publishKeep = (owner: number) => {
          seats.forEach((seat, player) => seat.socket.dispatchSynthetic("message", JSON.stringify({
            type: "StateUpdate",
            command_id: player === owner ? ids[owner] : undefined,
            data: {
              state_revision: owner + 2,
              state: {
                ...mulliganState,
                waiting_for: {
                  type: "MulliganDecision",
                  data: { pending: pending.slice(owner + 1), free_first_mulligan: true },
                },
              },
              events: [{ marker: `keep-${owner}` }], log_entries: [],
              legal_actions: player > owner ? [keep] : [],
              full_key: REPLY_OWNERSHIP_FULL_KEY,
            },
          })));
        };
        publishKeep(0);
        const first = await Promise.all(outcomes.map((read) => read()));
        expect(deliveredEventMarker(first[0])).toBe("keep-0");
        // This is the old any-StateUpdate ACK failure: three peers must remain pending.
        expect(first.slice(1)).toEqual(["never settled", "never settled", "never settled"]);
        expect(ids).toEqual([expect.any(Number), expect.any(Number), expect.any(Number), expect.any(Number)]);
        for (let player = 1; player < 4; player++) {
          await expect(seats[player].subject.submitAction(keep, player)).rejects.toMatchObject({
            code: AdapterErrorCode.ACTION_NOT_SENT,
          });
          expect(submissionTypes(seats[player].socket)).toEqual(["Action"]);
          publishKeep(player);
          expect(deliveredEventMarker(await outcomes[player]())).toBe(`keep-${player}`);
        }
      } finally {
        seats.forEach((seat) => seat.subject.dispose());
      }
    });

    it("renders peer updates without settling on missing or mismatched completion IDs", async () => {
      const events = vi.fn();
      subject.onEvent(events);
      const outcome = trackRejection(subject.submitAction({ type: "PassPriority" }, 0));
      const commandId = lastSubmissionCommandId(socket);
      deliverOwnershipStateUpdate(socket, "peer-update", 2);
      expect(await outcome()).toBe("never settled");
      expect(events).toHaveBeenCalledWith(expect.objectContaining({
        type: "stateChanged", serverRevision: 2,
      }));
      if (typeof commandId !== "number") throw new Error("Expected a numeric command ID");
      const data = {
        message: "Another request failed",
        rejection: { code: "invalid_action", disposition: "invalid", message: "Other rejection", related_object_ids: [] },
      };
      const surfaced = (type: string) => events.mock.calls.filter(([event]) => event.type === type).length;
      for (const type of ["ActionNoOp", "ActionRejected", "ActionFailed"]) {
        // An id-less frame is not an answer to this command, so it never settles it.
        socket.dispatchSynthetic("message", JSON.stringify({ type, data }));
      }
      // ...but an id-less failure still surfaces, exactly once each, without releasing the slot.
      expect(surfaced("requestRejected")).toBe(1);
      expect(surfaced("operationFailed")).toBe(1);
      expect(events).toHaveBeenCalledWith({ type: "requestRejected", reason: "Other rejection" });
      expect(events).toHaveBeenCalledWith({ type: "operationFailed", message: "Another request failed" });
      for (const type of ["ActionNoOp", "ActionRejected", "ActionFailed"]) {
        // A frame carrying another command's id belongs to that command: swallowed entirely.
        socket.dispatchSynthetic("message", JSON.stringify({ type, data, command_id: commandId + 1 }));
      }
      expect(surfaced("requestRejected")).toBe(1);
      expect(surfaced("operationFailed")).toBe(1);
      expect(await outcome()).toBe("never settled");
      expect(events).not.toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
      socket.dispatchSynthetic("message", JSON.stringify({ type: "ActionNoOp", command_id: commandId }));
      await expect(outcome()).resolves.toEqual({ resolvedWith: { events: [], log_entries: [] } });
    });

    it("settles its own correlated completion even when that frame's revision is stale", async () => {
      const events = vi.fn();
      subject.onEvent(events);
      deliverOwnershipStateUpdate(socket, "newer-peer-update", 5);
      const outcome = trackRejection(subject.submitAction({ type: "PassPriority" }, 0));
      const commandId = lastSubmissionCommandId(socket);
      events.mockClear();
      deliverOwnershipStateUpdate(socket, "own-stale-completion", 4, commandId);
      await expect(outcome()).resolves.toEqual({
        resolvedWith: { events: [{ marker: "own-stale-completion" }], log_entries: [] },
      });
      expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
      // The stale snapshot itself is never applied.
      expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "stateChanged" }));
    });

    it.each(["ActionRejected", "ActionFailed"] as const)(
      "owns a matching %s but ignores its late echo during the next interaction",
      async (type) => {
        const first = subject.submitAction({ type: "PassPriority" }, 0);
        const commandId = lastSubmissionCommandId(socket);
        socket.dispatchSynthetic("message", JSON.stringify({
          type, command_id: commandId,
          data: {
            message: "Command failed",
            rejection: { code: "invalid_action", disposition: "invalid", message: "Command rejected", related_object_ids: [] },
          },
        }));
        await expect(first).rejects.toMatchObject({
          code: type === "ActionRejected" ? "ACTION_REJECTED" : "REQUEST_FAILED",
        });
        const next = trackRejection(subject.submitInteraction(interactionSubmission("correlated-next"), 0));
        const nextId = lastSubmissionCommandId(socket);
        socket.dispatchSynthetic("message", JSON.stringify({ type: "ActionNoOp", command_id: commandId }));
        expect(await next()).toBe("never settled");
        expect(nextId).not.toEqual(commandId);
        socket.dispatchSynthetic("message", JSON.stringify({ type: "ActionNoOp", command_id: nextId }));
        await expect(next()).resolves.toEqual({ resolvedWith: { events: [], log_entries: [] } });
      },
    );

    it("keeps the AI fault latch across matching late replies and a same-game bootstrap", async () => {
      const pendingAction = subject.submitAction({ type: "PassPriority" }, 0);
      const commandId = lastSubmissionCommandId(socket);
      socket.dispatchSynthetic("message", JSON.stringify({
        type: "AiDriverFault",
        data: { fault: { id: 7, after_state_revision: 3, cause: "ActionSafetyCapReached" } },
      }));
      await expect(pendingAction).rejects.toMatchObject({ code: "WS_ERROR", recoverable: false });
      acceptFullReplyOwnershipSession(subject, socket);
      deliverOwnershipStateUpdate(socket, "late-before-fault-replay", 4, commandId);
      await expect(subject.submitInteraction(interactionSubmission("fault-remains"), 0)).rejects.toMatchObject({
        code: "WS_ERROR", recoverable: false,
      });
      expect(submissionTypes(socket)).toEqual([]);
    });

    it("ignores a completion carrying a pre-resume command id after resume", async () => {
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const events = vi.fn();
      subject.onEvent(events);
      try {
        const oldAction = subject.submitAction({ type: "PassPriority" }, 0);
        const oldId = lastSubmissionCommandId(socket);
        const closed = expect(oldAction).rejects.toMatchObject({ code: "WS_CLOSED" });
        socket.dispatchSynthetic("close");
        await closed;
        window.dispatchEvent(new Event("online"));
        await vi.advanceTimersByTimeAsync(0);
        const resumed = await completeHandshake(subject, CORRELATED_HELLO);
        acceptFullReplyOwnershipSession(subject, resumed);
        expect(events).toHaveBeenCalledWith({ type: "reconnected", inGame: true });
        let settled = false;
        const next = subject.submitInteraction(interactionSubmission("after-resume"), 0);
        void next.then(() => { settled = true; }, () => { settled = true; });
        const nextId = lastSubmissionCommandId(resumed);
        deliverOwnershipStateUpdate(resumed, "old-command-reply", 2, oldId);
        await vi.advanceTimersByTimeAsync(0);
        expect(settled).toBe(false);
        expect(nextId).not.toEqual(oldId);
        deliverOwnershipStateUpdate(resumed, "resumed-own-reply", 3, nextId);
        await expect(next).resolves.toMatchObject({ events: [{ marker: "resumed-own-reply" }] });
      } finally {
        subject.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });

    it("records silence without closing or acknowledging an outstanding correlated command", async () => {
      vi.useFakeTimers();
      const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
      const silent = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
    );
      const initialized = silent.initialize();
      const silentSocket = await completeHandshake(silent, CORRELATED_HELLO);
      acceptFullReplyOwnershipSession(silent, silentSocket);
      await initialized;
      try {
        let settled = false;
        const pendingAction = silent.submitAction({ type: "PassPriority" }, 0);
        void pendingAction.then(() => { settled = true; }, () => { settled = true; });
        const closed = expect(pendingAction).rejects.toMatchObject({ code: "WS_CLOSED" });
        await vi.advanceTimersByTimeAsync(15_000);
        expect(settled).toBe(false);
        expect(silentSocket.close).not.toHaveBeenCalled();
        silentSocket.dispatchSynthetic("close", undefined, new CloseEvent("close", { code: 1006 }));
        await closed;
        expect(getDiagnosticHistory().find((entry) => entry.diagnosticId === diagnosticIdFor(silentSocket))).toMatchObject({
          kind: "websocket-loss", silenceExceeded: true,
        });
      } finally {
        silent.dispose();
        visibility.mockRestore();
        vi.useRealTimers();
      }
    });
  });

  describe("pending-indicator bookkeeping", () => {
    let subject: WebSocketAdapter;
    let socket: MockWebSocket;
    let events: Array<{ type: string; pending?: boolean }>;

    beforeEach(async () => {
      subject = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
    );
      const initialized = subject.initialize();
      socket = await completeHandshake(subject, CORRELATED_HELLO);
      acceptFullReplyOwnershipSession(subject, socket);
      await initialized;
      events = [];
      subject.onEvent((event) => { events.push(event as { type: string; pending?: boolean }); });
    });

    afterEach(() => subject.dispose());

    it("frees the slot when a listener throws while the submission is announced", async () => {
      let armed = true;
      subject.onEvent((event) => {
        if (event.type === "actionPendingChanged" && event.pending && armed) {
          armed = false;
          throw new Error("listener failed");
        }
      });
      await expect(subject.submitAction({ type: "PassPriority" }, 0)).rejects.toThrow("listener failed");
      expect(submissionTypes(socket)).toEqual([]);

      const next = trackRejection(subject.submitAction({ type: "PassPriority" }, 0));
      expect(submissionTypes(socket)).toEqual(["Action"]);
      deliverOwnershipStateUpdate(socket, "after-throw", 2, lastSubmissionCommandId(socket));
      expect(deliveredEventMarker(await next())).toBe("after-throw");
    });

    it.each(["close", "dispose"] as const)(
      "sends nothing when a listener ends the connection with %s while the submission is announced",
      async (ending) => {
        subject.onEvent((event) => {
          if (event.type !== "actionPendingChanged" || !event.pending) return;
          if (ending === "close") {
            socket.readyState = 3; // CLOSED
            socket.dispatchSynthetic("close");
          } else {
            subject.dispose();
          }
        });
        const outcome = trackRejection(subject.submitAction({ type: "PassPriority" }, 0));
        expect(await outcome()).toMatchObject({ code: "WS_CLOSED" });
        expect(submissionTypes(socket)).toEqual([]);
        // A send attempted against the ended connection would also report a transport error.
        expect(events).not.toContainEqual(expect.objectContaining({ type: "error" }));
      },
    );

    it("announces the released indicator when the socket closes under a pending submission", async () => {
      const pending = trackRejection(subject.submitAction({ type: "PassPriority" }, 0));
      expect(events).toContainEqual({ type: "actionPendingChanged", pending: true });
      events.length = 0;
      socket.readyState = 3; // CLOSED
      socket.dispatchSynthetic("close");
      expect(await pending()).toMatchObject({ code: "WS_CLOSED" });
      expect(events).toContainEqual({ type: "actionPendingChanged", pending: false });
    });

    it("clears the slot when the AI driver faults", async () => {
      // The fault latch refuses every later submit for this game before the slot is
      // consulted, so the slot itself is the only observable proof it was cleared.
      const slot = (subject as unknown as { submission: { occupied: boolean } }).submission;
      const pending = trackRejection(subject.submitAction({ type: "PassPriority" }, 0));
      expect(slot.occupied).toBe(true);
      socket.dispatchSynthetic("message", JSON.stringify({
        type: "AiDriverFault",
        data: { fault: { id: 7, after_state_revision: 3, cause: "ActionSafetyCapReached" } },
      }));
      expect(await pending()).toMatchObject({ code: "WS_ERROR", recoverable: false });
      expect(slot.occupied).toBe(false);
      expect(events).toContainEqual({ type: "actionPendingChanged", pending: false });
    });
  });

  describe("servers without command correlation", () => {
    it("sends no command_id and keeps settling on the first completion frame", async () => {
      const legacy = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
    );
      const initialized = legacy.initialize();
      const legacySocket = await completeHandshake(legacy, HEARTBEAT_HELLO);
      acceptFullReplyOwnershipSession(legacy, legacySocket);
      await initialized;
      try {
        const events = vi.fn();
        legacy.onEvent(events);
        const outcome = trackRejection(legacy.submitAction({ type: "PassPriority" }, 0));
        expect(lastSubmissionCommandId(legacySocket)).toBeUndefined();
        // A completion frame with an unrelated ID is still the legacy ACK.
        legacySocket.dispatchSynthetic("message", JSON.stringify({ type: "ActionNoOp", command_id: 99 }));
        await expect(outcome()).resolves.toEqual({ resolvedWith: { events: [], log_entries: [] } });
        expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
        // With nothing pending, legacy still releases the indicator.
        events.mockClear();
        legacySocket.dispatchSynthetic("message", JSON.stringify({ type: "ActionNoOp" }));
        expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
        // An ActionRejected with nothing pending releases the indicator and surfaces too.
        events.mockClear();
        legacySocket.dispatchSynthetic("message", JSON.stringify({
          type: "ActionRejected",
          data: { rejection: { code: "invalid_action", disposition: "invalid", message: "Nothing pending", related_object_ids: [] } },
        }));
        expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
        expect(events).toHaveBeenCalledWith({ type: "requestRejected", reason: "Nothing pending" });
      } finally {
        legacy.dispose();
      }
    });
  });

  describe("Issue #9319 pending submission reply ownership", () => {
    beforeEach(() => {
      acceptFullReplyOwnershipSession(adapter, ws);
    });

    it("issue #9319 Action then Interaction keeps the first StateUpdate with its owner", async () => {
      await exerciseOverlappingSubmissionOwnership(
        ws,
        () => adapter.submitAction({ type: "PassPriority" }, 0),
        () => adapter.submitInteraction(interactionSubmission("overlap-interaction"), 0),
      );
    });

    it("issue #9319 Interaction then Interaction keeps the first StateUpdate with its owner", async () => {
      await exerciseOverlappingSubmissionOwnership(
        ws,
        () => adapter.submitInteraction(interactionSubmission("overlap-first"), 0),
        () => adapter.submitInteraction(interactionSubmission("overlap-second"), 0),
      );
    });

    it("claims the submission slot before pending notifications can re-enter", async () => {
      let reentrant: Promise<SubmitResult> | undefined;
      let didReenter = false;
      adapter.onEvent((event) => {
        if (event.type === "actionPendingChanged" && event.pending && !didReenter) {
          didReenter = true;
          reentrant = adapter.submitInteraction(interactionSubmission("reentrant"), 0);
        }
      });

      const first = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));
      const second = trackRejection(reentrant!);
      deliverOwnershipStateUpdate(ws, "reply-A", 2);
      const [firstResult, secondResult] = await Promise.all([first(), second()]);

      expect(submissionTypes(ws)).toEqual(["Action"]);
      expect(deliveredEventMarker(firstResult)).toBe("reply-A");
      expect(secondResult).toMatchObject({ code: AdapterErrorCode.ACTION_NOT_SENT });
    });

    it("releases the slot after an engine rejection", async () => {
      const first = adapter.submitAction({ type: "PassPriority" }, 0);
      ws.dispatchSynthetic("message", JSON.stringify({
        type: "ActionRejected",
        data: { rejection: { code: "invalid_action", disposition: "invalid", message: "Action rejected", related_object_ids: [] } },
      }));
      await expect(first).rejects.toMatchObject({ code: "ACTION_REJECTED" });

      const next = trackRejection(adapter.submitInteraction(interactionSubmission("after-rejection"), 0));
      expect(submissionTypes(ws)).toEqual(["Action", "Interaction"]);
      deliverOwnershipStateUpdate(ws, "reply-after-rejection", 2);
      await expect(next()).resolves.toMatchObject({
        resolvedWith: { events: [{ marker: "reply-after-rejection" }] },
      });
    });

    it("issue #9319 sequential Action and Interaction submissions remain usable", async () => {
      const action = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));
      deliverOwnershipStateUpdate(ws, "reply-A", 2);
      await expect(action()).resolves.toMatchObject({
        resolvedWith: { events: [{ marker: "reply-A" }] },
      });

      const interaction = trackRejection(adapter.submitInteraction(interactionSubmission("sequential-one"), 0));
      deliverOwnershipStateUpdate(ws, "reply-B", 3);
      await expect(interaction()).resolves.toMatchObject({
        resolvedWith: { events: [{ marker: "reply-B" }] },
      });

      const nextInteraction = trackRejection(adapter.submitInteraction(interactionSubmission("sequential-two"), 0));
      deliverOwnershipStateUpdate(ws, "reply-C", 4);
      await expect(nextInteraction()).resolves.toMatchObject({
        resolvedWith: { events: [{ marker: "reply-C" }] },
      });
    });
  });

  describe("send() error handling", () => {
    it.each(["ActionFailed", "Error"] as const)("keeps unsolicited %s off the transport error channel", async (type) => {
      acceptFullReplyOwnershipSession(adapter, ws);
      const events = vi.fn();
      adapter.onEvent(events);
      ws.dispatchSynthetic("message", JSON.stringify({ type, data: { message: "Operation declined" } }));
      expect(events).toHaveBeenCalledWith({ type: "operationFailed", message: "Operation declined" });
      expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
      const next = adapter.submitAction({ type: "PassPriority" }, 0);
      ws.dispatchSynthetic("message", JSON.stringify({ type: "ActionNoOp", data: { full_key: REPLY_OWNERSHIP_FULL_KEY } }));
      await expect(next).resolves.toEqual({ events: [], log_entries: [] });
    });

    it("reports an AI driver fault, then a terminal error so the game cannot continue", () => {
      const events = vi.fn();
      adapter.onEvent(events);
      ws.dispatchSynthetic("message", JSON.stringify({
        type: "AiDriverFault",
        data: { fault: { id: 4, after_state_revision: 10, cause: {} } },
      }));
      const types = events.mock.calls.map(([event]) => (event as { type: string }).type);
      expect(types.filter((type) => type === "aiDriverFault")).toHaveLength(1);
      expect(types.filter((type) => type === "error")).toHaveLength(1);
      expect(types.indexOf("aiDriverFault")).toBeLessThan(types.indexOf("error"));
      expect(events).toHaveBeenCalledWith({
        type: "error",
        message: "Native AI driver stopped; this game can no longer advance.",
      });
      expect(ws.close).not.toHaveBeenCalled();
    });

    it("rejects initialize when the post-handshake setup frame cannot be sent", async () => {
      MockWebSocket.last = null;
      const setupFailingAdapter = new WebSocketAdapter(
        "ws://localhost:9374/ws",
        "join",
        { main_deck: [], sideboard: [] },
        "ABC123",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: null },
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
              creation_kind: "Card",
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

    it("sends only the compact replacement selector and settles ActionNoOp", async () => {
      const selector = `r${"a".repeat(64)}`;
      const action: GameAction = { type: "SetReplacementAutoChoice", data: { selector } };
      ws.send.mockClear();
      const pending = adapter.submitAction(action, 0);
      expect(JSON.parse(ws.send.mock.lastCall![0] as string)).toEqual({
        type: "Action",
        data: { action: { type: "SetReplacementAutoChoice", data: { selector } } },
      });
      ws.dispatchSynthetic("message", JSON.stringify({ type: "ActionNoOp" }));
      await pending;
    });

    it.each(["Card", "Token"] as const)(
      "preserves the %s creation kind in a nonzero debug CreateCard action frame",
      async (creationKind) => {
        const action: GameAction = {
          type: "Debug",
          data: {
            type: "CreateCard",
            data: {
              card_name: "Lightning Bolt",
              owner: 0,
              zone: "Battlefield",
              run_etb: false,
              nonlegendary: false,
              creation_kind: creationKind,
              count: 1,
            },
          },
        };

        ws.send.mockClear();
        const pending = adapter.submitAction(action, 0);

        expect(JSON.parse(ws.send.mock.lastCall![0] as string)).toEqual({
          type: "Action",
          data: { action },
        });

        // Settle the promise after inspecting the outgoing frame; ActionNoOp
        // is not the source of truth for this transport assertion.
        ws.dispatchSynthetic("message", JSON.stringify({ type: "ActionNoOp" }));
        await pending;
      },
    );

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

    it.each(["missing", "closed"] as const)(
      "classifies submitAction before send when the socket is %s",
      async (socketState) => {
        const internal = adapter as unknown as { ws: MockWebSocket | null };
        const original = internal.ws;
        if (socketState === "missing") internal.ws = null;
        else ws.readyState = 3;
        ws.send.mockClear();

        await expect(
          adapter.submitAction({ type: "PassPriority" }, 0),
        ).rejects.toMatchObject({ code: AdapterErrorCode.ACTION_NOT_SENT });
        expect(ws.send).not.toHaveBeenCalled();

        internal.ws = original;
      },
    );

    it("classifies a synchronous send failure as definitely not sent and clears pending state", async () => {
      const listener = vi.fn();
      adapter.onEvent(listener);
      ws.send.mockImplementationOnce(() => {
        throw new Error("InvalidStateError");
      });

      await expect(
        adapter.submitAction({ type: "PassPriority" }, 0),
      ).rejects.toMatchObject({ code: AdapterErrorCode.ACTION_NOT_SENT });

      // The action was un-pended and an error surfaced, rather than the caller
      // hanging forever on a reply that will never come.
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({ type: "actionPendingChanged", pending: false }),
      );
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({ type: "error" }),
      );

      const next = trackRejection(adapter.submitInteraction(interactionSubmission("after-send-failure"), 0));
      deliverOwnershipStateUpdate(ws, "after-send-failure", 2);
      await expect(next()).resolves.toMatchObject({
        resolvedWith: { events: [{ marker: "after-send-failure" }] },
      });
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

    it("fails later submissions fast once the native AI driver has faulted", async () => {
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "AiDriverFault",
          data: { fault: { id: 7, after_state_revision: 3, cause: "ActionSafetyCapReached" } },
        }),
      );
      ws.send.mockClear();

      await expect(adapter.submitAction({ type: "PassPriority" }, 0)).rejects.toMatchObject({
        code: "WS_ERROR",
        recoverable: false,
      });
      await expect(adapter.submitInteraction(interactionSubmission("after-fault"), 0)).rejects.toMatchObject({
        code: "WS_ERROR",
      });
      expect(ws.send).not.toHaveBeenCalledWith(expect.stringContaining('"type":"Action"'));
    });

    it("keeps a fault fence across a same-game GameStarted, before the replayed fault arrives", async () => {
      acceptFullReplyOwnershipSession(adapter, ws);
      ws.dispatchSynthetic("message", JSON.stringify({
        type: "AiDriverFault",
        data: { fault: { id: 7, after_state_revision: 3, cause: "ActionSafetyCapReached" } },
      }));
      // A reconnect snapshot precedes the server's fault replay; nothing may slip through between them.
      ws.dispatchSynthetic("message", JSON.stringify({
        type: "GameStarted",
        data: {
          state_revision: 4, state: createMockState(), your_player: 0,
          player_token: "accepted-full-session-token", full_key: REPLY_OWNERSHIP_FULL_KEY,
        },
      }));
      ws.send.mockClear();

      await expect(adapter.submitAction({ type: "PassPriority" }, 0)).rejects.toMatchObject({ code: "WS_ERROR" });
      expect(ws.send).not.toHaveBeenCalledWith(expect.stringContaining('"type":"Action"'));
    });

    it("does not fence a different game with an earlier game's fault", async () => {
      ws.dispatchSynthetic("message", JSON.stringify({
        type: "AiDriverFault",
        data: { fault: { id: 7, after_state_revision: 3, cause: "ActionSafetyCapReached" } },
      }));
      ws.dispatchSynthetic("message", JSON.stringify({
        type: "GameCreated",
        data: { game_code: "GAME02", player_token: "next-game-token" },
      }));
      ws.send.mockClear();

      void adapter.submitAction({ type: "PassPriority" }, 0).catch(() => undefined);
      expect(ws.send).toHaveBeenCalledWith(expect.stringContaining('"type":"Action"'));
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
      await expect(adapter.submitInteraction(interactionSubmission("after-dispose"), 0)).rejects.toMatchObject({
        code: "WS_ERROR",
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

      ws.send.mockClear();
      const pending = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));
      expect(ws.send).toHaveBeenCalledWith(JSON.stringify({
        type: "Action",
        data: { action: { type: "PassPriority" } },
      }));
      ws.readyState = 3; // CLOSED
      ws.dispatchSynthetic("close");

      expect(await pending()).toMatchObject({
        code: "WS_CLOSED",
        message: "Connection closed during action",
      });
      await expect(adapter.submitInteraction(interactionSubmission("after-close"), 0)).rejects.toMatchObject({
        code: "WS_ERROR",
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
        code: "REQUEST_FAILED",
        message: "preview lookup failed",
        recoverable: true,
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


it("waits for the first LAN capability probe before rejecting an HTTPS manual join", async () => {
  const originalLocation = window.location;
  Object.defineProperty(window, "location", { configurable: true, value: { ...originalLocation, protocol: "https:" } });
  let release!: () => void;
  lanGate.supported = false;
  lanGate.authorize.mockReset().mockResolvedValue(undefined);
  lanGate.probe.mockImplementation(() => new Promise<boolean>((resolve) => {
    release = () => { lanGate.supported = true; resolve(true); };
  }));
  const manual = new WebSocketAdapter("ws://192.168.1.2:9374/ws", "join", { main_deck: [], sideboard: [] }, "ABC123", undefined, undefined, undefined, { stateCodecSessionProvider: null });
  MockWebSocket.last = null;
  const initialized = manual.initialize();
  // The adapter must not reject on mixed content while capability is unknown.
  release();
  // The transport's probe shares the resolved capability in production.
  lanGate.probe.mockResolvedValue(true);
  try {
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
    expect(lanGate.authorize).toHaveBeenCalledExactlyOnceWith("ws://192.168.1.2:9374/ws");
    const socket = MockWebSocket.last!;
    socket.dispatchSynthetic("message", SERVER_HELLO);
    await vi.waitFor(() => expect(socket.send).toHaveBeenCalledWith(expect.stringContaining('"type":"JoinGameWithPassword"')));
    socket.dispatchSynthetic("message", JSON.stringify({ type: "GameStarted", data: { state: createMockState(), your_player: 0 } }));
    await expect(initialized).resolves.toBeUndefined();
  } finally {
    manual.dispose(); lanGate.supported = false;
    Object.defineProperty(window, "location", { configurable: true, value: originalLocation });
  }
});


it.each(["resolve", "reject"] as const)("disposes during a pending LAN probe before its late %s", async (completion) => {
  let complete!: () => void;
  const probe = new Promise<boolean>((resolve, reject) => {
    complete = () => completion === "resolve" ? resolve(true) : reject(new Error("Late probe failure"));
  });
  lanGate.probe.mockReset().mockReturnValue(probe);
  lanGate.authorize.mockReset();
  const manual = new WebSocketAdapter("ws://192.168.1.2:9374/ws", "join", { main_deck: [], sideboard: [] }, "ABC123", undefined, undefined, undefined, { stateCodecSessionProvider: null });
  MockWebSocket.last = null;
  const rejection = trackRejection(manual.initialize());
  manual.dispose();
  try {
    expect(await rejection()).toMatchObject({
      code: "WS_CLOSED",
      message: "Adapter disposed before initialization completed",
    });
    complete();
    await probe.catch(() => {});
    expect(MockWebSocket.last).toBeNull();
    expect(lanGate.authorize).not.toHaveBeenCalled();
    expect(lanGate.probe).toHaveBeenCalledOnce();
  } finally {
    lanGate.probe.mockReset().mockResolvedValue(false);
  }
});

describe("WebSocketAdapter dispose during the handshake", () => {
  it.each(["before ServerHello", "after ServerHello settles"] as const)(
    "sends nothing after dispose and closes the socket (%s)",
    async (order) => {
      MockWebSocket.last = null;
      const adapter = new WebSocketAdapter("wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01", undefined, undefined, undefined, { stateCodecSessionProvider: null });
      void adapter.initialize().catch(() => {});
      await Promise.resolve();
      const socket = MockWebSocket.last!;
      let sentAtDispose: number;
      if (order === "before ServerHello") {
        sentAtDispose = socket.send.mock.calls.length;
        adapter.dispose();
        socket.dispatchSynthetic("message", SERVER_HELLO);
      } else {
        socket.dispatchSynthetic("message", SERVER_HELLO);
        sentAtDispose = socket.send.mock.calls.length;
        adapter.dispose();
      }
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(socket.send.mock.calls.slice(sentAtDispose)).toEqual([]);
      expect(socket.close).toHaveBeenCalledOnce();
    },
  );
});

// ── Negotiated StateCodecV1 delivery ────────────────────────────────────
//
// The decoder is substituted (orchestration is this file's subject); exact
// reconstruction with the real codec lives in `stateCodec.integration.test.ts`.

const CODEC_SESSION_HELLO = JSON.stringify({
  type: "ServerHello",
  data: {
    server_version: "0.0.0-test",
    build_commit: "testhash",
    protocol_version: PROTOCOL_VERSION,
    mode: "Full",
    command_correlation: "CommandIdV1",
    state_codecs: ["StateCodecV1"],
  },
});
const CODEC_CONFIRMATION = JSON.stringify({
  type: "ServerHello",
  data: { server_version: "0.0.0-test", selected_state_codec: "StateCodecV1" },
});

type CodecDecodeResult =
  | { kind: "decoded"; json: string }
  | { kind: "resyncRequired" };

function codecSession(handler: () => CodecDecodeResult): {
  session: StateCodecSession;
  decode: ReturnType<typeof vi.fn>;
  close: ReturnType<typeof vi.fn>;
} {
  const decode = vi.fn(async () => handler());
  const close = vi.fn();
  return { session: { decode, close }, decode, close };
}

async function startCodecAdapter(session: StateCodecSession, hello = CODEC_SESSION_HELLO) {
  MockWebSocket.last = null;
  const adapter = new WebSocketAdapter(
    "wss://localhost:9374/ws",
    "join",
    { main_deck: [], sideboard: [] },
    "GAME01",
    undefined,
    undefined,
    undefined,
    { stateCodecSessionProvider: () => Promise.resolve(session) },
  );
  const init = adapter.initialize();
  await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
  const raw = MockWebSocket.last!;
  raw.dispatchSynthetic("message", hello);
  await vi.waitFor(() => expect(raw.send).toHaveBeenCalledWith(
    expect.stringContaining('"type":"JoinGameWithPassword"'),
  ));
  raw.dispatchSynthetic("message", CODEC_CONFIRMATION);
  return { adapter, raw, init };
}

function fullFrame(envelope: string): string {
  return JSON.stringify({ type: "Full", data: { state: JSON.parse(envelope), revision: 1 } });
}

function deltaFrame(envelope: string): string {
  return JSON.stringify({ type: "Delta", data: { state: JSON.parse(envelope), revision: 2 } });
}

describe("WebSocketAdapter — negotiated StateCodecV1", () => {
  it("checks snapshot and codec state, metadata, removals and command completion against explicit envelopes", async () => {
    const initial = createMockState();
    const changed = {
      ...initial,
      turn_number: 4,
      objects: { "12": { name: "Éowyn 雪 🧙" } },
      battlefield: [12],
      derived: { marker: "old-derived" },
    };
    const removed = { ...initial, turn_number: 5 };
    const started = {
      type: "GameStarted",
      data: { state: initial, your_player: 0, player_token: "codec-token",
        full_key: REPLY_OWNERSHIP_FULL_KEY, state_revision: 7 },
    };
    const enriched = {
      type: "StateUpdate",
      data: {
        state: changed, state_revision: 8, full_key: REPLY_OWNERSHIP_FULL_KEY,
        legal_actions: [{ type: "PassPriority" }], auto_pass_recommended: true,
        end_continuous_effect_offers: [{ marker: "offer" }],
        mana_payment_shortcut_actions: [{ type: "PassPriority" }],
        spell_costs: { "12": { generic: 2 } },
        legal_actions_by_object: { "12": [{ marker: "object-action" }] },
        activation_block_reasons: { "12": [{ marker: "block" }] },
        viewer_interaction: { marker: "viewer" }, derived: { marker: "envelope-derived" },
        events: [{ marker: "peer-event" }], log_entries: [{ marker: "peer-log" }],
        rewind_targets: [{ turn_number: 2, active_player: 1 }],
      },
    };
    const lean = {
      type: "StateUpdate",
      data: { state: removed, state_revision: 8, full_key: REPLY_OWNERSHIP_FULL_KEY,
        events: [] },
    };
    const expectedLegal = {
      actions: [], autoPassRecommended: false, endContinuousEffectOffers: [],
      manaPaymentShortcutActions: [], spellCosts: undefined, legalActionsByObject: undefined,
      activationBlockReasons: undefined, viewerInteraction: undefined,
    };
    for (const useCodec of [false, true]) {
      MockWebSocket.last = null;
      const decoder = codecSession(() => ({ kind: "resyncRequired" }));
      decoder.decode.mockImplementation(async (frame: Uint8Array) => ({
        kind: "decoded",
        json: JSON.stringify(JSON.parse(new TextDecoder().decode(frame)).data.state),
      }));
      const adapter = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
        undefined, undefined, undefined,
        { stateCodecSessionProvider: useCodec ? () => Promise.resolve(decoder.session) : null },
      );
      const states: unknown[] = [];
      adapter.onEvent((event) => { if (event.type === "stateChanged") states.push(event); });
      const init = adapter.initialize();
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
      const raw = MockWebSocket.last!;
      raw.dispatchSynthetic("message", CODEC_SESSION_HELLO);
      await vi.waitFor(() => expect(raw.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"JoinGameWithPassword"'),
      ));
      if (useCodec) raw.dispatchSynthetic("message", CODEC_CONFIRMATION);
      const deliver = (envelope: unknown, full = false) => {
        const json = JSON.stringify(envelope);
        raw.dispatchSynthetic("message", useCodec ? (full ? fullFrame(json) : deltaFrame(json)) : json);
      };
      try {
        deliver(started, true);
        await init;
        expect((await adapter.getSnapshot()).state).toEqual({ ...initial, derived: undefined });
        expect(states).toEqual([]); // Initial state is consumed through getSnapshot.
        deliver(enriched, true); // GameStarted is FullOnly; the first update is Full.
        await vi.waitFor(() => expect(states).toHaveLength(1));
        expect(states[0]).toEqual({
          type: "stateChanged", snapshot: {
            seq: expect.any(Number),
            state: { ...changed, derived: enriched.data.derived },
            legalResult: {
              actions: enriched.data.legal_actions, autoPassRecommended: true,
              endContinuousEffectOffers: enriched.data.end_continuous_effect_offers,
              manaPaymentShortcutActions: enriched.data.mana_payment_shortcut_actions,
              spellCosts: enriched.data.spell_costs,
              legalActionsByObject: enriched.data.legal_actions_by_object,
              activationBlockReasons: enriched.data.activation_block_reasons,
              viewerInteraction: enriched.data.viewer_interaction,
            },
          },
          events: enriched.data.events, logEntries: enriched.data.log_entries,
          serverRevision: 8, rewindTargets: enriched.data.rewind_targets,
        });
        // Equal revision is accepted; absent optional fields and removed objects
        // must disappear, rather than inheriting from the prior decoded state.
        deliver(lean);
        await vi.waitFor(() => expect(states).toHaveLength(2));
        expect(states[1]).toEqual({
          type: "stateChanged", snapshot: { seq: expect.any(Number),
            state: { ...removed, derived: undefined }, legalResult: expectedLegal },
          events: [], logEntries: undefined, serverRevision: 8, rewindTargets: [],
        });
        const pending = adapter.submitAction({ type: "PassPriority" }, 0);
        const id = lastSubmissionCommandId(raw);
        deliver({ ...lean, command_id: id, data: { ...lean.data, state_revision: 9,
          events: [{ marker: "own" }], log_entries: [{ marker: "own-log" }] } });
        await expect(pending).resolves.toEqual({
          events: [{ marker: "own" }], log_entries: [{ marker: "own-log" }],
        });
        expect(states).toHaveLength(2);
        expect((await adapter.getSnapshot()).legalResult).toEqual(expectedLegal);
      } finally {
        adapter.dispose();
      }
    }
  });

  it.each([true, false])("retains the wire baseline across a strictly stale update (own=%s)", async (own) => {
    const session = codecSession(() => ({ kind: "resyncRequired" }));
    session.decode.mockImplementation(async (frame: Uint8Array) => ({
      kind: "decoded", json: JSON.stringify(JSON.parse(new TextDecoder().decode(frame)).data.state),
    }));
    const { adapter, raw, init } = await startCodecAdapter(session.session);
    const events = vi.fn();
    adapter.onEvent(events);
    const started = { type: "GameStarted", data: { state: createMockState(),
      state_revision: 10, your_player: 0, player_token: "codec-token", full_key: REPLY_OWNERSHIP_FULL_KEY } };
    raw.dispatchSynthetic("message", fullFrame(JSON.stringify(started)));
    await init;
    const pending = adapter.submitAction({ type: "PassPriority" }, 0);
    const settled = vi.fn();
    void pending.then(settled);
    const id = lastSubmissionCommandId(raw);
    const stale = { type: "StateUpdate", ...(own ? { command_id: id } : {}),
      data: { state: { ...createMockState(), turn_number: 9 }, state_revision: 9,
        full_key: REPLY_OWNERSHIP_FULL_KEY, events: [{ marker: "stale-completion" }] } };
    raw.dispatchSynthetic("message", fullFrame(JSON.stringify(stale)));
    await vi.waitFor(() => expect(session.decode).toHaveBeenCalledTimes(2));
    // A later queued control frame is the observable drain barrier.
    raw.dispatchSynthetic("message", JSON.stringify({ type: "Pong", data: { timestamp: Date.now() } }));
    await vi.waitFor(() => expect(events).toHaveBeenCalledWith(
      expect.objectContaining({ type: "latencyChanged" })));
    expect((await adapter.getSnapshot()).state.turn_number).toBe(1);
    expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "stateChanged" }));
    if (own) {
      await expect(pending).resolves.toEqual({ events: [{ marker: "stale-completion" }], log_entries: undefined });
      expect(settled).toHaveBeenCalledOnce();
      expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
    } else {
      expect(settled).not.toHaveBeenCalled();
    }
    const newer = { type: "StateUpdate", data: { ...stale.data,
      state: { ...createMockState(), turn_number: 11 }, state_revision: 11 } };
    raw.dispatchSynthetic("message", deltaFrame(JSON.stringify(newer)));
    await vi.waitFor(() => expect(events).toHaveBeenCalledWith(expect.objectContaining({
      type: "stateChanged", serverRevision: 11,
    })));
    expect((await adapter.getSnapshot()).state.turn_number).toBe(11);
    expect(raw.close).not.toHaveBeenCalled();
    expect(raw.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');
    if (!own) {
      raw.dispatchSynthetic("message", JSON.stringify({ type: "ActionNoOp", command_id: id }));
      await expect(pending).resolves.toEqual({ events: [], log_entries: [] });
    }
    adapter.dispose();
  });

  it.each(["stale update", "throwing application"] as const)(
    "does not reopen submissions after a recovery Full is declined by a %s",
    async (decline) => {
      const accepted = { ...createMockState(), turn_number: 10 };
      const started = JSON.stringify({
        type: "GameStarted",
        data: { state: accepted, state_revision: 10, your_player: 0,
          player_token: "codec-token", full_key: REPLY_OWNERSHIP_FULL_KEY },
      });
      const stale = JSON.stringify({
        type: "StateUpdate",
        data: { state: { ...accepted, turn_number: 9 }, state_revision: 9,
          full_key: REPLY_OWNERSHIP_FULL_KEY, events: [] },
      });
      const rejected = decline === "stale update" ? stale : JSON.stringify({
        type: "GameStarted",
        data: { state: { ...accepted, turn_number: 11 }, state_revision: 11,
          your_player: 0, player_token: "codec-token", full_key: REPLY_OWNERSHIP_FULL_KEY },
      });
      const results: CodecDecodeResult[] = [
        { kind: "decoded", json: started },
        { kind: "decoded", json: stale },
        { kind: "resyncRequired" },
        { kind: "decoded", json: rejected },
      ];
      const session = codecSession(() => results.shift() ?? { kind: "resyncRequired" });
      const { adapter, raw, init } = await startCodecAdapter(session.session);
      let rejectApplication = false;
      const events = vi.fn();
      adapter.onEvent(events);
      adapter.onEvent((event) => {
        if (rejectApplication && event.type === "bootstrapDelivered") {
          throw new Error("application rejected the delivery before caching it");
        }
      });
      try {
        raw.dispatchSynthetic("message", fullFrame(started));
        await init;
        // The same stale payload remains an ordinary healthy-stream ignore.
        raw.dispatchSynthetic("message", fullFrame(stale));
        raw.dispatchSynthetic("message", JSON.stringify({ type: "Pong", data: { timestamp: Date.now() } }));
        await vi.waitFor(() => expect(events).toHaveBeenCalledWith(
          expect.objectContaining({ type: "latencyChanged" })));
        expect((await adapter.getSnapshot()).state.turn_number).toBe(10);
        expect(raw.close).not.toHaveBeenCalled();
        const pending = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));
        raw.dispatchSynthetic("message", deltaFrame(stale));
        await expect(pending()).resolves.toMatchObject({ code: "WS_CLOSED", recoverable: true });
        await expect(adapter.submitAction({ type: "PassPriority" }, 0))
          .rejects.toMatchObject({ code: AdapterErrorCode.ACTION_NOT_SENT });
        expect(raw.send).toHaveBeenCalledWith('{"type":"ResyncState"}');

        rejectApplication = decline === "throwing application";
        events.mockClear();
        raw.dispatchSynthetic("message", fullFrame(rejected));
        raw.dispatchSynthetic("message", JSON.stringify({ type: "Pong", data: { timestamp: Date.now() } }));
        // Either terminal close or this following control frame proves the
        // recovery delivery finished, including on the unsafe old path.
        await vi.waitFor(() => expect(
          session.close.mock.calls.length > 0
          || events.mock.calls.some(([event]) => event.type === "latencyChanged"),
        ).toBe(true));
        expect(session.decode).toHaveBeenCalledTimes(4);
        const refused = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));
        // On the old implementation this sends a second Action and never settles.
        expect(submissionTypes(raw)).toEqual(["Action"]);
        expect(await refused()).toBeInstanceOf(AdapterError);
        expect((await adapter.getSnapshot()).state.turn_number).toBe(10);
        expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "stateChanged" }));
        expect(raw.close).toHaveBeenCalledOnce();
        expect(session.close).toHaveBeenCalledOnce();
      } finally {
        adapter.dispose();
      }
    },
  );

  it.each([
    ["wrong game", { game_code: "OTHER", generation: 1 }],
    ["changed generation", { game_code: "GAME01", generation: 2 }],
    ["invalid generation", { game_code: "GAME01", generation: 0 }],
    ["missing key", undefined],
  ] as const)("retires a successfully decoded terminal %s Full and fences the queued Delta", async (_label, key) => {
    const session = codecSession(() => ({ kind: "resyncRequired" }));
    session.decode.mockImplementation(async (frame: Uint8Array) => ({
      kind: "decoded", json: JSON.stringify(JSON.parse(new TextDecoder().decode(frame)).data.state),
    }));
    const { adapter, raw, init } = await startCodecAdapter(session.session);
    const started = { type: "GameStarted", data: { state: createMockState(),
      your_player: 0, player_token: "codec-token", full_key: REPLY_OWNERSHIP_FULL_KEY } };
    raw.dispatchSynthetic("message", fullFrame(JSON.stringify(started)));
    await init;
    const events = vi.fn();
    adapter.onEvent(events);
    const pending = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));
    const rejected = { type: "StateUpdate", data: { state: { ...createMockState(), turn_number: 99 },
      state_revision: 2, full_key: key } };
    raw.dispatchSynthetic("message", fullFrame(JSON.stringify(rejected)));
    raw.dispatchSynthetic("message", deltaFrame(JSON.stringify(rejected)));
    await expect(pending()).resolves.toMatchObject({ code: "WS_CLOSED" });
    expect(session.decode).toHaveBeenCalledTimes(2); // The rejecting Full decoded successfully.
    expect(events).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "stateChanged" }));
    expect((await adapter.getSnapshot()).state.turn_number).toBe(1);
    expect(raw.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');
    expect(session.close).toHaveBeenCalledOnce();
    expect(raw.close).toHaveBeenCalledOnce();
    adapter.dispose();
  });

  it.each([
    ["Action", true], ["Interaction", true], ["Action", false], ["Interaction", false],
  ] as const)("rejects stranded %s (correlated=%s), gates synchronous submissions, then resumes", async (kind, correlated) => {
    const started = JSON.stringify({
      type: "GameStarted",
      data: {
        state_revision: 2,
        state: createMockState(),
        your_player: 0,
        player_token: "codec-token",
        full_key: REPLY_OWNERSHIP_FULL_KEY,
      },
    });
    const update = JSON.stringify({
      type: "StateUpdate",
      data: {
        state_revision: 3,
        state: { ...createMockState(), turn_number: 2 },
        events: [{ type: "TurnStarted" }],
        full_key: REPLY_OWNERSHIP_FULL_KEY,
      },
    });
    const results: CodecDecodeResult[] = [
      { kind: "decoded", json: started },
      { kind: "decoded", json: update },
      { kind: "resyncRequired" },
      { kind: "decoded", json: update },
      { kind: "decoded", json: update },
    ];
    const session = codecSession(() => results.shift() ?? { kind: "resyncRequired" });
    const hello = correlated ? CODEC_SESSION_HELLO : JSON.stringify({
      ...JSON.parse(CODEC_SESSION_HELLO),
      data: { ...JSON.parse(CODEC_SESSION_HELLO).data, command_correlation: undefined },
    });
    const { adapter, raw, init } = await startCodecAdapter(session.session, hello);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));

    raw.dispatchSynthetic("message", fullFrame(started));
    await init;
    expect((await adapter.getSnapshot()).state.turn_number).toBe(1);
    raw.dispatchSynthetic("message", fullFrame(update));
    await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({ type: "stateChanged" })));
    events.length = 0;
    const attempted: Promise<unknown>[] = [];
    const stopReentrant = adapter.onEvent((event) => {
      if ((event.type === "actionPendingChanged" && !event.pending)
        || event.type === "stateChanged") {
        attempted.push(adapter.submitInteraction(interactionSubmission("reentrant"), 0)
          .catch((error: unknown) => error));
      }
    });
    const pending = kind === "Action"
      ? adapter.submitAction({ type: "PassPriority" }, 0)
      : adapter.submitInteraction(interactionSubmission("initial"), 0);
    const observed = trackRejection(pending);
    const commandId = lastSubmissionCommandId(raw);

    raw.dispatchSynthetic("message", deltaFrame(update));
    await expect(observed()).resolves.toMatchObject({
      code: "WS_CLOSED",
      recoverable: true,
    });
    expect(events).toContainEqual({ type: "actionPendingChanged", pending: false });
    expect(raw.send).toHaveBeenCalledWith('{"type":"ResyncState"}');

    const sentBefore = submissionTypes(raw).length;
    await expect(adapter.submitAction({ type: "PassPriority" } as GameAction, 0)).rejects
      .toMatchObject({ code: "ACTION_NOT_SENT" });
    expect(submissionTypes(raw)).toHaveLength(sentBefore);

    raw.dispatchSynthetic("message", fullFrame(update));
    await vi.waitFor(() => expect(events).toContainEqual(
      expect.objectContaining({ type: "stateChanged" }),
    ));
    expect(await Promise.all(attempted)).toEqual([
      expect.objectContaining({ code: "ACTION_NOT_SENT" }),
      expect.objectContaining({ code: "ACTION_NOT_SENT" }),
    ]);
    stopReentrant();

    const resumed = adapter.submitAction({ type: "PassPriority" } as GameAction, 0);
    await vi.waitFor(() => expect(submissionTypes(raw)).toHaveLength(sentBefore + 1));
    const resumedId = lastSubmissionCommandId(raw);
    if (correlated) {
      expect(resumedId).not.toBe(commandId);
      const settled = vi.fn();
      void resumed.then(settled);
      const countStates = () => events.filter((event) =>
        typeof event === "object" && event !== null && "type" in event && event.type === "stateChanged").length;
      const beforeEcho = countStates();
      raw.dispatchSynthetic("message", JSON.stringify({
        type: "StateUpdate", command_id: commandId,
        data: { state_revision: 3, state: createMockState(), events: [], full_key: REPLY_OWNERSHIP_FULL_KEY },
      }));
      await vi.waitFor(() => expect(countStates()).toBe(beforeEcho + 1));
      expect(settled).not.toHaveBeenCalled();
    }
    raw.dispatchSynthetic("message", JSON.stringify({
      type: "StateUpdate",
      data: {
        state_revision: 4,
        state: { ...createMockState(), turn_number: 3 },
        events: [],
        full_key: REPLY_OWNERSHIP_FULL_KEY,
      },
      command_id: resumedId,
    }));
    await expect(resumed).resolves.toMatchObject({ events: [] });
    adapter.dispose();
  });

  it("requests one resync and closes when a rejection precedes bootstrap, then completes the same join on one snapshot-only fallback", async () => {
    const started = JSON.stringify({
      type: "GameStarted",
      data: {
        state_revision: 1,
        state: createMockState(),
        your_player: 0,
        player_token: "codec-token",
        full_key: REPLY_OWNERSHIP_FULL_KEY,
      },
    });
    // Only the first frame is rejected; every later frame WOULD decode, so a
    // fenced late frame is the wrapper's doing, not the decoder's.
    const codec = scriptedCodec();
    codec.rejectNext(1);
    const { adapter, raw, init } = await startCodecAdapter(codec.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    const rejection = trackRejection(init);

    raw.dispatchSynthetic("message", fullFrame(started));
    await vi.waitFor(() => expect(codec.close).toHaveBeenCalledOnce());
    expect(raw.send).toHaveBeenCalledWith('{"type":"ResyncState"}');
    expect(
      raw.send.mock.calls.filter(([value]) => value === '{"type":"ResyncState"}'),
    ).toHaveLength(1);
    expect(events.some((event) => (event as { type?: string }).type === "playerIdentity")).toBe(false);
    expect(raw.close).toHaveBeenCalled();
    expect(codec.decode).toHaveBeenCalledOnce();
    // The rejection no longer ends the join: the credentials on the rejected
    // frame start one snapshot-only fallback inside the same initialization.
    expect(await rejection()).toBe("never settled");

    // A late, valid resync StateUpdate Full + Delta, then a control frame, still
    // reach the transport (the wrapper's handler is installed). The receive
    // queue is microtask-only, so a macrotask boundary proves it drained them.
    const late = vi.fn();
    adapter.onEvent(late);
    expect(raw.onmessage).toBeTypeOf("function");
    raw.dispatchSynthetic("message", fullFrame(JSON.stringify(codecStateUpdate(2, 2))));
    raw.dispatchSynthetic("message", deltaFrame(JSON.stringify(codecStateUpdate(3, 3))));
    raw.dispatchSynthetic("message", JSON.stringify({ type: "Pong", data: { timestamp: Date.now() } }));
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(codec.decode).toHaveBeenCalledOnce();
    expect(late).not.toHaveBeenCalled();
    await expect(adapter.getSnapshot()).rejects.toBeDefined();

    const fallback = await snapshotOnlyHandshake("Reconnect", raw);
    expect(offeredCodec(fallback)).toBe(false);
    fallback.dispatchSynthetic("message", JSON.stringify({
      type: "GameStarted",
      data: { state_revision: 4, state: { ...createMockState(), turn_number: 4 }, your_player: 0, full_key: REPLY_OWNERSHIP_FULL_KEY },
    }));
    await init;
    expect((await adapter.getSnapshot()).state.turn_number).toBe(4);
    adapter.dispose();
  });

  it.each(["nativeAi", "nativePregame"] as const)("keeps %s snapshot-only with same-turn startup", async (nativeKind) => {
    MockWebSocket.last = null;
    const adapter = new WebSocketAdapter(
      "native-engine",
      "host",
      { main_deck: [], sideboard: [] },
      undefined,
      undefined,
      undefined,
      "Player",
      nativeKind === "nativeAi" ? {
        nativeAi: {
          socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
          aiSeats: [{ seatIndex: 1, difficulty: "Hard",
            deck: { main_deck: ["Lightning Bolt"], sideboard: [] } }],
          playerCount: 2,
        },
      } : {
        nativePregame: { kind: "host", aiSeats: [], playerCount: 2,
          socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport },
      },
    );
    const init = adapter.initialize();
    const raw = MockWebSocket.last!;
    expect(raw).not.toBeNull();
    raw.dispatchSynthetic("message", CODEC_SESSION_HELLO);
    await vi.waitFor(() => expect(raw.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"ClientHello"'),
    ));
    expect(raw.send).toHaveBeenCalledWith(expect.stringContaining('"type":"ClientHello"'));
    expect(raw.send).not.toHaveBeenCalledWith(expect.stringContaining("state_codecs"));
    raw.dispatchSynthetic("message", JSON.stringify({
      type: "GameStarted", data: { state: createMockState(), your_player: 0 },
    }));
    await init;
    expect((await adapter.getSnapshot()).state.turn_number).toBe(1);
    adapter.dispose();
  });
});

/** Reading the static through a function keeps assignment narrowing out. */
function latestSocket(): MockWebSocket {
  const socket = MockWebSocket.last;
  if (!socket) throw new Error("no socket was created");
  return socket;
}

describe("WebSocketAdapter — bounded codec preparation", () => {
  const joinArgs = (): [
    string,
    "join",
    { main_deck: string[]; sideboard: string[] },
    string,
    string | undefined,
    string | undefined,
    string | undefined,
  ] => [
    "wss://localhost:9374/ws",
    "join",
    { main_deck: [], sideboard: [] },
    "GAME01",
    undefined,
    undefined,
    undefined,
  ];

  it("falls back to snapshot delivery after the preparation deadline and closes the late lease", async () => {
    vi.useFakeTimers();
    try {
      MockWebSocket.last = null;
      const late = codecSession(() => ({ kind: "decoded", json: "{}" }));
      let resolveProvider!: (session: StateCodecSession) => void;
      const provider = () => new Promise<StateCodecSession | null>((resolve) => {
        resolveProvider = resolve;
      });
      const adapter = new WebSocketAdapter(
        ...joinArgs(),
        { stateCodecSessionProvider: provider },
      );
      const init = adapter.initialize();

      await vi.advanceTimersByTimeAsync(STATE_CODEC_PREPARATION_TIMEOUT_MS);
      const raw = latestSocket();
      raw.dispatchSynthetic("message", CODEC_SESSION_HELLO);
      await vi.advanceTimersByTimeAsync(0);

      expect((adapter as unknown as { ws: unknown }).ws).toBe(raw);
      expect(raw.send).toHaveBeenCalledWith(expect.stringContaining('"type":"ClientHello"'));
      expect(raw.send).not.toHaveBeenCalledWith(expect.stringContaining("state_codecs"));

      const socketBefore = MockWebSocket.last;
      resolveProvider(late.session);
      await vi.advanceTimersByTimeAsync(0);
      expect(late.close).toHaveBeenCalledOnce();
      expect(MockWebSocket.last).toBe(socketBefore);

      raw.dispatchSynthetic("message", JSON.stringify({
        type: "GameStarted",
        data: { state: createMockState(), your_player: 0 },
      }));
      await init;
      adapter.dispose();
    } finally {
      vi.useRealTimers();
    }
  });

  it("abandons the attachment when disposed while preparing, opening no socket", async () => {
    MockWebSocket.last = null;
    const adapter = new WebSocketAdapter(
      ...joinArgs(),
      { stateCodecSessionProvider: () => new Promise<StateCodecSession | null>(() => undefined) },
    );
    const outcome = trackRejection(adapter.initialize());
    adapter.dispose();

    await expect(outcome()).resolves.toMatchObject({ code: "WS_CLOSED" });
    expect(MockWebSocket.last).toBeNull();
  });

  it("explicit null keeps the handshake in the same turn", async () => {
    MockWebSocket.last = null;
    const adapter = new WebSocketAdapter(
      ...joinArgs(),
      { stateCodecSessionProvider: null },
    );
    void adapter.initialize().catch(() => undefined);
    expect(MockWebSocket.last).not.toBeNull();
    adapter.dispose();
  });
});

// ── Negotiated StateCodecV1: reconnect, recovery and teardown ───────────

/**
 * A decoder that unwraps each frame's embedded envelope verbatim and rejects
 * (resync) exactly as many leading frames as the test asks for.
 */
function scriptedCodec(): {
  session: StateCodecSession;
  decode: ReturnType<typeof vi.fn>;
  close: ReturnType<typeof vi.fn>;
  rejectNext: (count?: number) => void;
} {
  let rejecting = 0;
  const decode = vi.fn(async (frame: Uint8Array): Promise<CodecDecodeResult> => {
    if (rejecting > 0) {
      rejecting -= 1;
      return { kind: "resyncRequired" };
    }
    const envelope = JSON.parse(new TextDecoder().decode(frame)) as { data: { state: unknown } };
    return { kind: "decoded", json: JSON.stringify(envelope.data.state) };
  });
  const close = vi.fn();
  return {
    session: { decode, close },
    decode,
    close,
    rejectNext: (count = 1) => { rejecting = count; },
  };
}

function codecGameStarted(turn: number, revision: number, overrides: Record<string, unknown> = {}) {
  return {
    type: "GameStarted",
    data: {
      state: { ...createMockState(), turn_number: turn },
      state_revision: revision,
      your_player: 0,
      player_token: "codec-token",
      full_key: REPLY_OWNERSHIP_FULL_KEY,
      ...overrides,
    },
  };
}

function codecStateUpdate(
  turn: number,
  revision: number,
  topLevel: Record<string, unknown> = {},
  dataOverrides: Record<string, unknown> = {},
) {
  return {
    type: "StateUpdate",
    ...topLevel,
    data: {
      state: { ...createMockState(), turn_number: turn },
      state_revision: revision,
      events: [],
      full_key: REPLY_OWNERSHIP_FULL_KEY,
      ...dataOverrides,
    },
  };
}

function deliverCodec(raw: MockWebSocket, kind: "Full" | "Delta", envelope: unknown): void {
  const text = JSON.stringify(envelope);
  raw.dispatchSynthetic("message", kind === "Full" ? fullFrame(text) : deltaFrame(text));
}

const RESYNC_REQUEST = '{"type":"ResyncState"}';

function resyncRequests(raw: MockWebSocket): number {
  return raw.send.mock.calls.filter(([value]) => value === RESYNC_REQUEST).length;
}

/** Completes the codec handshake of the socket created after `previous`. */
async function codecHandshake(setupType: string, previous: MockWebSocket | null): Promise<MockWebSocket> {
  await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(previous));
  const raw = latestSocket();
  raw.dispatchSynthetic("message", CODEC_SESSION_HELLO);
  await vi.waitFor(() => expect(raw.send).toHaveBeenCalledWith(
    expect.stringContaining(`"type":"${setupType}"`),
  ));
  raw.dispatchSynthetic("message", CODEC_CONFIRMATION);
  return raw;
}

/** A joined, bootstrapped codec stream: GameStarted Full accepted. */
async function establishCodecAdapter(provider: () => Promise<StateCodecSession | null>) {
  MockWebSocket.last = null;
  const adapter = new WebSocketAdapter(
    "wss://localhost:9374/ws",
    "join",
    { main_deck: [], sideboard: [] },
    "GAME01",
    undefined,
    undefined,
    undefined,
    { stateCodecSessionProvider: provider },
  );
  const events: unknown[] = [];
  adapter.onEvent((event) => events.push(event));
  const init = adapter.initialize();
  const raw = await codecHandshake("JoinGameWithPassword", null);
  deliverCodec(raw, "Full", codecGameStarted(1, 1));
  await init;
  return { adapter, raw, events };
}

describe("WebSocketAdapter — negotiated StateCodecV1 reconnect", () => {
  /** Established stream A, lost, then a reconnect attachment B with its own session. */
  async function reconnectedCodecAdapter() {
    const first = scriptedCodec();
    const second = scriptedCodec();
    const sessions = [first.session, second.session];
    const established = await establishCodecAdapter(() => Promise.resolve(sessions.shift() ?? null));
    expect((await established.adapter.getSnapshot()).state.turn_number).toBe(1);
    established.raw.dispatchSynthetic("close");
    await vi.advanceTimersByTimeAsync(1000);
    const reconnect = await codecHandshake("Reconnect", established.raw);
    return { ...established, first, second, reconnect };
  }

  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  it("treats a rejection on a reconnect attachment as pre-bootstrap despite the cached snapshot and token", async () => {
    const { adapter, events, first, second, reconnect } = await reconnectedCodecAdapter();
    try {
      // Reach guard: the previous attachment's lease was released by its close,
      // and this attachment really is a reconnect that kept the old snapshot.
      expect(first.close).toHaveBeenCalledOnce();
      expect(second.close).not.toHaveBeenCalled();
      expect((await adapter.getSnapshot()).state.turn_number).toBe(1);
      events.length = 0;

      second.rejectNext(1);
      deliverCodec(reconnect, "Full", codecGameStarted(5, 5));
      await vi.advanceTimersByTimeAsync(0);
      expect(second.decode).toHaveBeenCalledOnce();
      expect(resyncRequests(reconnect)).toBe(1);
      expect(reconnect.close).toHaveBeenCalled();
      expect(second.close).toHaveBeenCalledOnce();

      // Late valid resync StateUpdate Full + Delta, then a control frame. The
      // decoder would accept them: only the fence keeps them out.
      deliverCodec(reconnect, "Full", codecStateUpdate(6, 6));
      deliverCodec(reconnect, "Delta", codecStateUpdate(7, 7));
      reconnect.dispatchSynthetic("message", JSON.stringify({ type: "Pong", data: { timestamp: 1 } }));
      await vi.advanceTimersByTimeAsync(0);
      expect(second.decode).toHaveBeenCalledOnce();
      expect(events).not.toContainEqual(expect.objectContaining({ type: "stateChanged" }));
      expect(events).not.toContainEqual(expect.objectContaining({ type: "reconnected" }));
      expect((await adapter.getSnapshot()).state.turn_number).toBe(1);

      // The bootstrap-fatal close still reaches the reconnect boundary: the
      // reconnect socket is the current one until the backoff elapses, and then
      // a third socket is created.
      expect(MockWebSocket.last).toBe(reconnect);
      await vi.advanceTimersByTimeAsync(10000);
      expect(MockWebSocket.last).not.toBe(reconnect);
    } finally {
      adapter.dispose();
    }
  });

  it("accepts a fresh reconnect session's GameStarted Full without a redundant resync, then restores Full and Delta", async () => {
    const { adapter, events, first, second, reconnect } = await reconnectedCodecAdapter();
    try {
      events.length = 0;
      deliverCodec(reconnect, "Full", codecGameStarted(5, 5));
      await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
      expect((await adapter.getSnapshot()).state.turn_number).toBe(5);
      expect(resyncRequests(reconnect)).toBe(0);

      // GameStarted is FullOnly: the first update is a Full, the next a Delta.
      deliverCodec(reconnect, "Full", codecStateUpdate(6, 6));
      deliverCodec(reconnect, "Delta", codecStateUpdate(7, 7));
      await vi.waitFor(() => expect(
        events.filter((event) => (event as { type?: string }).type === "stateChanged"),
      ).toHaveLength(3));
      expect((await adapter.getSnapshot()).state.turn_number).toBe(7);
      expect(resyncRequests(reconnect)).toBe(0);
      expect(reconnect.close).not.toHaveBeenCalled();
      // No old session can authorize the new one: each decoded only its own stream.
      expect(first.decode).toHaveBeenCalledOnce();
      expect(second.decode).toHaveBeenCalledTimes(3);
      expect(second.close).not.toHaveBeenCalled();
    } finally {
      adapter.dispose();
    }
  });

  it("recovers an established reconnect stream through one resync without closing", async () => {
    const { adapter, events, second, reconnect } = await reconnectedCodecAdapter();
    try {
      deliverCodec(reconnect, "Full", codecGameStarted(5, 5));
      await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
      deliverCodec(reconnect, "Full", codecStateUpdate(6, 6));
      await vi.waitFor(() => expect((adapter as unknown as { snapshot: { state: GameState } }).snapshot.state.turn_number).toBe(6));

      second.rejectNext(1);
      deliverCodec(reconnect, "Delta", codecStateUpdate(7, 7));
      await vi.waitFor(() => expect(resyncRequests(reconnect)).toBe(1));
      await expect(adapter.submitAction({ type: "PassPriority" } as GameAction, 0)).rejects
        .toMatchObject({ code: "ACTION_NOT_SENT" });
      expect(reconnect.close).not.toHaveBeenCalled();
      expect(second.close).not.toHaveBeenCalled();

      deliverCodec(reconnect, "Full", codecStateUpdate(8, 8));
      await vi.waitFor(() => expect((adapter as unknown as { snapshot: { state: GameState } }).snapshot.state.turn_number).toBe(8));
      const resumed = adapter.submitAction({ type: "PassPriority" } as GameAction, 0);
      await vi.waitFor(() => expect(submissionTypes(reconnect)).toHaveLength(1));
      deliverCodec(reconnect, "Delta", codecStateUpdate(9, 9, { command_id: lastSubmissionCommandId(reconnect) }));
      await expect(resumed).resolves.toMatchObject({ events: [] });
      expect(resyncRequests(reconnect)).toBe(1);
    } finally {
      adapter.dispose();
    }
  });

  it("keeps the still-current socket on the recovery path while a replacement attachment starts", async () => {
    const first = scriptedCodec();
    let releasePreparation!: (session: StateCodecSession | null) => void;
    const providers = [
      () => Promise.resolve<StateCodecSession | null>(first.session),
      () => new Promise<StateCodecSession | null>((resolve) => { releasePreparation = resolve; }),
    ];
    const { adapter, raw } = await establishCodecAdapter(() => providers.shift()!());
    try {
      const pending = adapter.submitAction({ type: "PassPriority" } as GameAction, 0);
      const outcome = pending.then(() => "resolved", (error: unknown) => error);
      // The replacement attachment is still preparing; A is the current socket.
      expect(adapter.tryReconnect({
        gameCode: "GAME01", playerToken: "codec-token", fullKey: REPLY_OWNERSHIP_FULL_KEY,
        serverUrl: "wss://localhost:9374/ws", timestamp: Date.now(),
      })).toBe(true);
      expect(releasePreparation).toBeTypeOf("function");

      first.rejectNext(1);
      deliverCodec(raw, "Delta", codecStateUpdate(2, 2));
      await vi.waitFor(() => expect(resyncRequests(raw)).toBe(1));
      // Recovery path, not the pre-bootstrap fatal path.
      await expect(outcome).resolves.toMatchObject({
        code: "WS_CLOSED", recoverable: true,
      });
      expect(raw.close).not.toHaveBeenCalled();
      expect(first.close).not.toHaveBeenCalled();
      await expect(adapter.submitAction({ type: "PassPriority" } as GameAction, 0)).rejects
        .toMatchObject({ code: "ACTION_NOT_SENT" });

      deliverCodec(raw, "Full", codecStateUpdate(3, 3));
      await vi.waitFor(() => expect((adapter as unknown as { snapshot: { state: GameState } }).snapshot.state.turn_number).toBe(3));
      void adapter.submitAction({ type: "PassPriority" } as GameAction, 0).catch(() => undefined);
      await vi.waitFor(() => expect(submissionTypes(raw)).toHaveLength(2));
    } finally {
      adapter.dispose();
      releasePreparation?.(null);
    }
  });
});

describe("WebSocketAdapter — negotiated StateCodecV1 identity and teardown", () => {
  const injectedNativeReconnect = (codec: ReturnType<typeof scriptedCodec>) => new WebSocketAdapter(
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
      // Native sessions are snapshot-only by default; an explicit provider opts in.
      stateCodecSessionProvider: () => Promise.resolve(codec.session),
    },
  );
  const sessionAttached = (fullKey: unknown) => JSON.stringify({
    type: "SessionAttached",
    data: { game_code: "NATIVE", player_id: 1, player_token: "guest-token", full_key: fullKey },
  });

  it("accepts a matching SessionAttached on an injected native reconnect codec stream", async () => {
    // Reach guard for the terminal rows below: the same route attaches on a valid key.
    MockWebSocket.last = null;
    const codec = scriptedCodec();
    const adapter = injectedNativeReconnect(codec);
    const attached = adapter.initializePregame();
    const raw = await codecHandshake("Reconnect", null);
    raw.dispatchSynthetic("message", sessionAttached({ game_code: "NATIVE", generation: 1 }));
    await expect(attached).resolves.toMatchObject({ gameCode: "NATIVE", playerId: 1 });
    expect(raw.close).not.toHaveBeenCalled();
    expect(codec.close).not.toHaveBeenCalled();
    adapter.dispose();
  });

  it.each([
    ["a changed generation", { game_code: "NATIVE", generation: 2 }],
    ["a wrong game code", { game_code: "OTHER", generation: 1 }],
    ["a missing key", undefined],
  ] as const)("retires the codec session on a SessionAttached with %s", async (_label, fullKey) => {
    MockWebSocket.last = null;
    const codec = scriptedCodec();
    const adapter = injectedNativeReconnect(codec);
    const attached = trackRejection(adapter.initializePregame());
    const raw = await codecHandshake("Reconnect", null);
    const events = vi.fn();
    adapter.onEvent(events);
    raw.dispatchSynthetic("message", sessionAttached(fullKey));

    await expect(attached()).resolves.toMatchObject({ code: "WS_ERROR" });
    expect(events).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    expect(raw.close).toHaveBeenCalledOnce();
    expect(codec.close).toHaveBeenCalledOnce();
    expect(resyncRequests(raw)).toBe(0);
    adapter.dispose();
  });

  it("retires the codec session on a native reconnect GameStarted Full for the wrong seat and fences the queued Delta", async () => {
    MockWebSocket.last = null;
    const codec = scriptedCodec();
    const adapter = injectedNativeReconnect(codec);
    const attached = adapter.initializePregame();
    const raw = await codecHandshake("Reconnect", null);
    raw.dispatchSynthetic("message", sessionAttached({ game_code: "NATIVE", generation: 1 }));
    await attached;
    const events = vi.fn();
    adapter.onEvent(events);

    // Seat 0 was attached, not the expected seat 1: the decoded Full is
    // terminal, so the queued dependent Delta must never be decoded.
    const wrongSeat = codecGameStarted(4, 4, {
      your_player: 0, player_token: "guest-token", full_key: { game_code: "NATIVE", generation: 1 },
    });
    deliverCodec(raw, "Full", wrongSeat);
    deliverCodec(raw, "Delta", codecStateUpdate(5, 5));
    await vi.waitFor(() => expect(codec.close).toHaveBeenCalledOnce());
    expect(codec.decode).toHaveBeenCalledOnce();
    expect(events).toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "stateChanged" }));
    expect(raw.close).toHaveBeenCalledOnce();
    expect(resyncRequests(raw)).toBe(0);
    adapter.dispose();
  });

  it("closes instead of publishing ready when a decoded wrong-key Full arrives during recovery", async () => {
    const codec = scriptedCodec();
    MockWebSocket.last = null;
    const { adapter, raw, events } = await establishCodecAdapter(() => Promise.resolve(codec.session));
    try {
      events.length = 0;
      codec.rejectNext(1);
      deliverCodec(raw, "Delta", codecStateUpdate(2, 2));
      await vi.waitFor(() => expect(resyncRequests(raw)).toBe(1));
      // Reach guard: the stream is in recovery and gates submissions.
      await expect(adapter.submitAction({ type: "PassPriority" } as GameAction, 0)).rejects
        .toMatchObject({ code: "ACTION_NOT_SENT" });
      expect(raw.close).not.toHaveBeenCalled();

      const wrongKey = { game_code: "GAME01", generation: 8 };
      deliverCodec(raw, "Full", codecStateUpdate(9, 9, {}, { full_key: wrongKey }));
      deliverCodec(raw, "Delta", codecStateUpdate(10, 10, {}, { full_key: wrongKey }));
      await vi.waitFor(() => expect(codec.close).toHaveBeenCalledOnce());
      // started + rejected Delta + the decoded wrong-key Full; the Delta is fenced.
      expect(codec.decode).toHaveBeenCalledTimes(3);
      expect(events).toContainEqual(expect.objectContaining({ type: "error" }));
      expect(events).not.toContainEqual(expect.objectContaining({ type: "stateChanged" }));
      expect(raw.close).toHaveBeenCalledOnce();
      // The identity failure adds no second resync request.
      expect(resyncRequests(raw)).toBe(1);
      expect((await adapter.getSnapshot()).state.turn_number).toBe(1);
    } finally {
      adapter.dispose();
    }
  });

  it("reports an adapter dispose, not a lost connection, to a pending action in codec mode", async () => {
    const codec = scriptedCodec();
    const { adapter, raw } = await establishCodecAdapter(() => Promise.resolve(codec.session));
    const pending = trackRejection(adapter.submitAction({ type: "PassPriority" } as GameAction, 0));
    expect(submissionTypes(raw)).toHaveLength(1);

    adapter.dispose();
    await expect(pending()).resolves.toMatchObject({
      code: "WS_CLOSED", message: "Adapter disposed during action", recoverable: true,
    });
    expect(codec.close).toHaveBeenCalledOnce();
    expect(raw.close).toHaveBeenCalled();
  });

  it("reports a failed setup send as such in codec mode", async () => {
    MockWebSocket.last = null;
    const codec = scriptedCodec();
    const adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined,
      { stateCodecSessionProvider: () => Promise.resolve(codec.session) },
    );
    const init = trackRejection(adapter.initialize());
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
    const raw = latestSocket();
    raw.send.mockImplementation((text: string) => {
      if (text.includes('"type":"JoinGameWithPassword"')) throw new Error("socket write failed");
    });
    raw.dispatchSynthetic("message", CODEC_SESSION_HELLO);

    await expect(init()).resolves.toMatchObject({
      code: "WS_CLOSED", message: "Failed to send setup frame", recoverable: true,
    });
    expect(codec.close).toHaveBeenCalledOnce();
    expect(raw.close).toHaveBeenCalled();
    adapter.dispose();
  });
});

// ── Superseded attachments ───────────────────────────────────────────────
//
// `initialize()` can start a newer attachment while an older reconnect
// attachment is already inside its handshake. The opening latch, the pending
// initialization and the reconnect episode belong to the newer attachment.

describe("WebSocketAdapter — superseded attachment handshake", () => {
  const reconnectSession = {
    gameCode: "GAME01", playerToken: "codec-token", fullKey: REPLY_OWNERSHIP_FULL_KEY,
    serverUrl: "wss://localhost:9374/ws", timestamp: Date.now(),
  };
  const GAME_STARTED = JSON.stringify({
    type: "GameStarted", data: { state: createMockState(), your_player: 0 },
  });
  const internals = (adapter: WebSocketAdapter) => adapter as unknown as {
    socketOpening: boolean;
    reconnectTimer: unknown;
  };

  /** Reconnect attachment A is mid-handshake when `initialize()` starts attachment B. */
  async function overlappingAttachments() {
    MockWebSocket.last = null;
    const adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined,
      { stateCodecSessionProvider: () => Promise.resolve(null) },
    );
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    expect(adapter.tryReconnect(reconnectSession)).toBe(true);
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
    const socketA = latestSocket();
    const init = trackRejection(adapter.initialize());
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(socketA));
    const socketB = latestSocket();
    // Reach guard: both handshakes are in flight and B owns the opening latch.
    expect(internals(adapter).socketOpening).toBe(true);
    return { adapter, events, socketA, socketB, init };
  }

  async function finishAttachmentB(socketB: MockWebSocket) {
    socketB.dispatchSynthetic("message", SERVER_HELLO);
    await vi.waitFor(() => expect(socketB.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"JoinGameWithPassword"'),
    ));
    socketB.dispatchSynthetic("message", GAME_STARTED);
  }

  it("keeps the newer attachment's opening latch and init when the older handshake settles", async () => {
    const { adapter, socketA, socketB, init } = await overlappingAttachments();
    try {
      socketA.dispatchSynthetic("message", SERVER_HELLO);
      // Reach guard: the older handshake settled and its socket was abandoned.
      await vi.waitFor(() => expect(socketA.close).toHaveBeenCalled());

      expect(internals(adapter).socketOpening).toBe(true);
      expect(adapter.tryReconnect(reconnectSession)).toBe(false);
      expect(await init()).toBe("never settled");

      await finishAttachmentB(socketB);
      await expect(init()).resolves.toMatchObject({ resolvedWith: undefined });
      // The newer attachment's own settlement releases the latch.
      expect(internals(adapter).socketOpening).toBe(false);
    } finally {
      adapter.dispose();
    }
  });

  it("leaves the newer attachment's init and the reconnect schedule untouched when the older handshake fails", async () => {
    const { adapter, events, socketA, socketB, init } = await overlappingAttachments();
    try {
      socketA.dispatchSynthetic("close");
      await new Promise((resolve) => setTimeout(resolve, 0));

      expect(internals(adapter).socketOpening).toBe(true);
      expect(internals(adapter).reconnectTimer).toBeNull();
      expect(events).not.toContainEqual(expect.objectContaining({ type: "reconnecting" }));
      expect(await init()).toBe("never settled");

      await finishAttachmentB(socketB);
      await expect(init()).resolves.toMatchObject({ resolvedWith: undefined });
      expect(internals(adapter).socketOpening).toBe(false);
    } finally {
      adapter.dispose();
    }
  });

  it("still retries when the failing reconnect handshake is the current attachment", async () => {
    // Positive control for the test above: the same failure with no newer
    // attachment releases the latch and schedules the retry.
    MockWebSocket.last = null;
    const adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined,
      { stateCodecSessionProvider: () => Promise.resolve(null) },
    );
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    try {
      expect(adapter.tryReconnect(reconnectSession)).toBe(true);
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
      latestSocket().dispatchSynthetic("close");
      await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({ type: "reconnecting" })));
      expect(internals(adapter).socketOpening).toBe(false);
      expect(internals(adapter).reconnectTimer).not.toBeNull();
    } finally {
      adapter.dispose();
    }
  });
});

describe("WebSocketAdapter — production codec provider default", () => {
  afterEach(() => { sharedEngine.open = null; });

  it("advertises StateCodecV1 and decodes through the shared engine's session when no provider is injected", async () => {
    const codec = scriptedCodec();
    const open = vi.fn(() => Promise.resolve(codec.session));
    sharedEngine.open = open;
    MockWebSocket.last = null;
    // Non-native options and no `stateCodecSessionProvider`: the default applies.
    const adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
    );
    try {
      const init = adapter.initialize();
      // Observe teardown rejection immediately; awaiting the original still exposes init failures.
      void init.catch(() => undefined);
      // Hold capability discovery until the default lease and its handshake observer are ready.
      await vi.waitFor(() => expect(open).toHaveBeenCalledOnce());
      await open.mock.results[0]!.value;
      await Promise.resolve();
      const raw = await codecHandshake("JoinGameWithPassword", null);
      const hello = raw.send.mock.calls
        .map(([text]) => JSON.parse(text as string) as { type?: string; data?: { state_codecs?: unknown } })
        .find((frame) => frame.type === "ClientHello");
      expect(hello?.data?.state_codecs).toEqual(["StateCodecV1"]);

      deliverCodec(raw, "Full", codecGameStarted(1, 1));
      await init;
      // The Full frame reached the adapter only through the shared engine's session.
      expect(codec.decode).toHaveBeenCalledOnce();
      expect((await adapter.getSnapshot()).state.turn_number).toBe(1);
    } finally {
      adapter.dispose();
    }
  });
});

describe("WebSocketAdapter — negotiated StateCodecV1 spectate", () => {
  it("bootstraps a spectator on GameStarted Full, then recovers a rejected Delta through one resync", async () => {
    MockWebSocket.last = null;
    const codec = scriptedCodec();
    const adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws", "spectate", { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined,
      { stateCodecSessionProvider: () => Promise.resolve(codec.session) },
    );
    const turn = () => (adapter as unknown as { snapshot: { state: GameState } }).snapshot.state.turn_number;
    try {
      const init = adapter.initialize();
      const raw = await codecHandshake("SpectatorJoin", null);
      // The spectator bootstrap: a non-seat viewer and no player token.
      deliverCodec(raw, "Full", codecGameStarted(1, 1, { your_player: 255, player_token: undefined }));
      await init;
      expect(adapter.playerId).toBe(255);
      expect(turn()).toBe(1);

      deliverCodec(raw, "Full", codecStateUpdate(2, 2));
      await vi.waitFor(() => expect(turn()).toBe(2));
      codec.rejectNext(1);
      deliverCodec(raw, "Delta", codecStateUpdate(3, 3));
      await vi.waitFor(() => expect(resyncRequests(raw)).toBe(1));
      // Reach guard: the rejected Delta was decoded and never applied.
      expect(codec.decode).toHaveBeenCalledTimes(3);
      expect(turn()).toBe(2);

      deliverCodec(raw, "Full", codecStateUpdate(4, 4));
      await vi.waitFor(() => expect(turn()).toBe(4));
      deliverCodec(raw, "Delta", codecStateUpdate(5, 5));
      await vi.waitFor(() => expect(turn()).toBe(5));
      // One resync recovered the stream; the spectator socket stayed open.
      expect(resyncRequests(raw)).toBe(1);
      expect(raw.close).not.toHaveBeenCalled();
      expect(codec.close).not.toHaveBeenCalled();
    } finally {
      adapter.dispose();
    }
  });
});

// ── Codec degradation policy ────────────────────────────────────────────
//
// A terminal codec failure makes the adapter's later attachments snapshot-only
// (the provider is not consulted and no codec is advertised); a decoder trap
// additionally refuses codec sessions on the shared runtime, so a fresh adapter
// does not negotiate either. Mock decoding proves orchestration; the real-WASM
// halves live in `stateCodec.integration.test.ts`.

/**
 * Completes the handshake of the socket created after `previous` against a
 * server that OFFERS StateCodecV1: a client that is snapshot-only must decline
 * it, so "no state_codecs in the ClientHello" is a real policy outcome.
 */
async function snapshotOnlyHandshake(setupType: string, previous: MockWebSocket | null): Promise<MockWebSocket> {
  await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(previous));
  const raw = latestSocket();
  raw.dispatchSynthetic("message", CODEC_SESSION_HELLO);
  await vi.waitFor(() => expect(raw.send).toHaveBeenCalledWith(
    expect.stringContaining(`"type":"${setupType}"`),
  ));
  return raw;
}

/** Whether this socket's ClientHello offered StateCodecV1. */
function offeredCodec(raw: MockWebSocket): boolean {
  const hello = raw.send.mock.calls
    .map(([text]) => JSON.parse(text as string) as { type?: string; data?: { state_codecs?: unknown } })
    .find((frame) => frame.type === "ClientHello");
  if (!hello) throw new Error("no ClientHello was sent");
  return hello.data?.state_codecs !== undefined;
}

const RECONNECT_SESSION = {
  gameCode: "GAME01",
  playerToken: "codec-token",
  fullKey: REPLY_OWNERSHIP_FULL_KEY,
  serverUrl: "wss://localhost:9374/ws",
};

/** The adapter's delivery policy, observed only to discriminate when it was decided. */
function policyOf(adapter: WebSocketAdapter): unknown {
  return (adapter as unknown as { stateCodec: { deliveryPolicy: unknown } }).stateCodec.deliveryPolicy;
}

/**
 * A real `StateCodecRuntime` over a scripted engine, exposed as the lease
 * provider the shared WasmAdapter facade hands out. The poison verdict is the
 * runtime's own and is not mocked: `script.trapNext` makes the engine's next
 * decode raise a genuine `WebAssembly.RuntimeError`, which the runtime latches
 * before it refuses every later open and decode, whichever session asks.
 */
function realRuntimeCodec() {
  interface FakeHandle { json(): string; free(): void; clone_handle(): FakeHandle }
  const script = { trapNext: false };
  const handle = (json: string): FakeHandle => ({
    json: () => json,
    free: vi.fn(),
    clone_handle: () => handle(json),
  });
  const decodedHandles: FakeHandle[] = [];
  const leases: StateCodecSession[] = [];
  const engine = {
    state_codec_decode: vi.fn((frame: Uint8Array) => {
      if (script.trapNext) {
        script.trapNext = false;
        throw new WebAssembly.RuntimeError("unreachable");
      }
      const parsed = JSON.parse(new TextDecoder().decode(frame)) as { data: { state: unknown } };
      const decoded = handle(JSON.stringify(parsed.data.state));
      decodedHandles.push(decoded);
      return decoded;
    }),
  };
  const runtime = new StateCodecRuntime(engine as unknown as EngineModule);
  const provider = vi.fn(async (): Promise<StateCodecSession | null> => {
    const sessionId = runtime.open();
    const session: StateCodecSession = {
      decode: async (frame) => runtime.decode(sessionId, frame),
      close: vi.fn(() => runtime.close(sessionId)),
    };
    leases.push(session);
    return session;
  });
  const latestBaseline = () => {
    const baseline = decodedHandles[decodedHandles.length - 1];
    if (!baseline) throw new Error("No codec baseline was decoded");
    return baseline;
  };
  return { engine, runtime, script, provider, leases, latestBaseline };
}

describe("WebSocketAdapter — acquired socket ownership", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });
  const session = () => ({
    gameCode: "GAME01", playerToken: "codec-token", fullKey: REPLY_OWNERSHIP_FULL_KEY,
    serverUrl: "wss://localhost:9374/ws", timestamp: Date.now(),
  });

  interface PendingObservation {
    settled: (outcome: unknown) => void;
    value: () => unknown;
  }

  function observe(promise: Promise<unknown>): PendingObservation {
    let value: unknown = "never settled";
    const settled = vi.fn((outcome: unknown) => { value = outcome; });
    void promise.then((result) => settled({ resolvedWith: result }), settled);
    return { settled, value: () => value };
  }

  it.each(["Action", "Interaction"] as const)(
    "settles A's pending correlated %s before a live handoff and never consumes B's command",
    async (kind) => {
      const codec = realRuntimeCodec();
      const { adapter, raw, events } = await establishCodecAdapter(codec.provider);
      const baseline = codec.latestBaseline();
      const old = observe(kind === "Action"
        ? adapter.submitAction({ type: "PassPriority" }, 0)
        : adapter.submitInteraction(interactionSubmission("pending-A"), 0));
      const oldId = lastSubmissionCommandId(raw);
      try {
        expect(submissionTypes(raw)).toEqual([kind]);
        expect(raw.readyState).toBe(MockWebSocket.OPEN);
        expect(adapter.tryReconnect(session())).toBe(true);
        const successor = await codecHandshake("Reconnect", raw);
        deliverCodec(successor, "Full", codecGameStarted(5, 5));
        await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
        expect(old.value()).toMatchObject({ code: "WS_CLOSED", recoverable: true });
        expect(old.settled).toHaveBeenCalledOnce();
        expect(raw.close).toHaveBeenCalledOnce();
        expect(codec.leases[0].close).toHaveBeenCalledOnce();
        expect(baseline.free).toHaveBeenCalledOnce();

        const next = observe(adapter.submitInteraction(interactionSubmission("pending-B"), 0));
        const nextId = lastSubmissionCommandId(successor);
        expect(nextId).not.toBe(oldId);
        // Completion is addressed to the submitting A, not broadcast to B.
        // A's server-side connection is never closed by this fixture.
        deliverCodec(raw, "Delta", codecStateUpdate(99, 99, { command_id: oldId }));
        raw.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(0);
        expect(next.value()).toBe("never settled");
        expect(old.settled).toHaveBeenCalledOnce();
        expect((await adapter.getSnapshot()).state.turn_number).toBe(5);
        deliverCodec(successor, "Delta", codecStateUpdate(6, 6, { command_id: nextId }));
        await vi.waitFor(() => expect(next.value()).toMatchObject({ resolvedWith: { events: [] } }));
        expect(next.settled).toHaveBeenCalledOnce();
        adapter.dispose();
        expect(old.settled).toHaveBeenCalledOnce();
        expect(raw.close).toHaveBeenCalledOnce();
        expect(codec.leases[0].close).toHaveBeenCalledOnce();
        expect(baseline.free).toHaveBeenCalledOnce();
      } finally {
        adapter.dispose();
        codec.runtime.dispose();
      }
    },
  );

  it("clears every A preview/export owner before a pending listener claims B work, including a reused engine preview ID", async () => {
    const codec = realRuntimeCodec();
    const { adapter, raw, events } = await establishCodecAdapter(codec.provider);
    const request: InteractionPreviewRequest = {
      requestId: "handoff-preview" as PreviewRequestId,
      ...interactionSubmission("handoff-preview-interaction"),
    };
    const old = [
      observe(adapter.submitAction({ type: "PassPriority" }, 0)),
      observe(adapter.previewManaPayment({ type: "PassPriority" }, 0)),
      observe(adapter.previewInteraction(request, 0)),
      observe(adapter.exportPersistenceState()),
    ];
    let next: PendingObservation[] = [];
    let armed = true;
    adapter.onEvent((event) => {
      if (event.type !== "actionPendingChanged" || event.pending || !armed) return;
      armed = false;
      next = [
        observe(adapter.submitInteraction(interactionSubmission("listener-B"), 0)),
        observe(adapter.previewManaPayment({ type: "PassPriority" }, 0)),
        observe(adapter.previewInteraction(request, 0)),
        observe(adapter.exportPersistenceState()),
      ];
    });
    try {
      expect(adapter.tryReconnect(session())).toBe(true);
      const successor = await codecHandshake("Reconnect", raw);
      deliverCodec(successor, "Full", codecGameStarted(5, 5));
      await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
      expect(next).toHaveLength(4);
      for (const outcome of old) {
        expect(outcome.value()).toMatchObject({ code: "WS_CLOSED", recoverable: true });
        expect(outcome.settled).toHaveBeenCalledOnce();
      }
      for (const outcome of next) expect(outcome.value()).toBe("never settled");
      const manaRequest = successor.send.mock.calls
        .map(([text]) => JSON.parse(text as string) as { type: string; data?: { request_id: number } })
        .find((frame) => frame.type === "PreviewManaPayment");
      expect(manaRequest?.data?.request_id).toBe(2);
      raw.dispatchSynthetic("message", JSON.stringify({
        type: "InteractionPreviewFailed", data: { request_id: request.requestId, message: "late A preview" },
      }));
      raw.dispatchSynthetic("message", JSON.stringify({
        type: "AuthoritativeStateExportFailed", data: { message: "late A export" },
      }));
      raw.dispatchSynthetic("close");
      await vi.advanceTimersByTimeAsync(0);
      for (const outcome of next) expect(outcome.value()).toBe("never settled");
      successor.dispatchSynthetic("message", JSON.stringify({
        type: "ManaPaymentPreview", data: { request_id: manaRequest!.data!.request_id, source_ids: [12] },
      }));
      successor.dispatchSynthetic("message", JSON.stringify({
        type: "InteractionPreviewFailed", data: { request_id: request.requestId, message: "B preview answered" },
      }));
      successor.dispatchSynthetic("message", JSON.stringify({ type: "AuthoritativeStateExport", data: { state: "B envelope" } }));
      deliverCodec(successor, "Delta", codecStateUpdate(6, 6, { command_id: lastSubmissionCommandId(successor) }));
      await vi.waitFor(() => expect(next[0].value()).toMatchObject({ resolvedWith: { events: [] } }));
      expect(next[1].value()).toEqual({ resolvedWith: [12] });
      expect(next[2].value()).toMatchObject({ code: "REQUEST_FAILED", message: "B preview answered" });
      expect(next[3].value()).toEqual({ resolvedWith: "B envelope" });
      for (const outcome of [...old, ...next]) expect(outcome.settled).toHaveBeenCalledOnce();
    } finally {
      adapter.dispose();
      codec.runtime.dispose();
    }
  });

  it.each([
    ["actionPendingChanged", "dispose"], ["actionPendingChanged", "throw"],
    ["actionPendingChanged", "supersede"], ["actionPendingChanged", "supersede then throw"],
    ["serverHello", "throw"], ["serverHello", "supersede then throw"],
  ] as const)(
    "releases A once and settles abandoned B requests when %s listeners %s",
    async (notification, ending) => {
      const codec = realRuntimeCodec();
      const { adapter, raw } = await establishCodecAdapter(codec.provider);
      const baseline = codec.latestBaseline();
      const old = observe(adapter.submitAction({ type: "PassPriority" }, 0));
      let next: PendingObservation | undefined;
      let secondary: PendingObservation[] = [];
      let initialization: PendingObservation | undefined;
      let armed = true;
      adapter.onEvent((event) => {
        if (!armed || event.type !== notification) return;
        if (event.type === "actionPendingChanged" && event.pending) return;
        armed = false;
        next = observe(adapter.submitInteraction(interactionSubmission("abandoned-B"), 0));
        secondary = [
          observe(adapter.previewManaPayment({ type: "PassPriority" }, 0)),
          observe(adapter.previewInteraction({
            requestId: "abandoned-B-preview" as PreviewRequestId,
            ...interactionSubmission("abandoned-B-preview"),
          }, 0)),
          observe(adapter.exportPersistenceState()),
        ];
        if (ending === "dispose") adapter.dispose();
        if (ending.startsWith("supersede")) {
          // Permit an explicit C attachment: this listener is deliberately
          // replacing B before its Reconnect setup has been sent.
          initialization = observe(adapter.initialize());
        }
        if (ending.includes("throw")) throw new Error("pending listener failed");
      });
      try {
        expect(adapter.tryReconnect(session())).toBe(true);
        await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(raw));
        const abandoned = latestSocket();
        abandoned.dispatchSynthetic("message", CODEC_SESSION_HELLO);
        await vi.advanceTimersByTimeAsync(0);
        expect(next).toBeDefined();
        expect(old.value()).toMatchObject({ code: "WS_CLOSED" });
        expect(next!.value()).toMatchObject({ code: "WS_CLOSED" });
        expect(old.settled).toHaveBeenCalledOnce();
        expect(next!.settled).toHaveBeenCalledOnce();
        for (const outcome of secondary) {
          expect(outcome.value()).toMatchObject({ code: "WS_CLOSED" });
          expect(outcome.settled).toHaveBeenCalledOnce();
        }
        expect(raw.close).toHaveBeenCalledOnce();
        expect(codec.leases[0].close).toHaveBeenCalledOnce();
        expect(baseline.free).toHaveBeenCalledOnce();
        expect(abandoned.close).toHaveBeenCalledOnce();
        expect(codec.leases[1].close).toHaveBeenCalledOnce();
        expect(abandoned.send).not.toHaveBeenCalledWith(expect.stringContaining('"type":"Reconnect"'));
        if (ending.startsWith("supersede")) {
          const current = await codecHandshake("JoinGameWithPassword", abandoned);
          deliverCodec(current, "Full", codecGameStarted(5, 5));
          await vi.waitFor(() => expect(initialization!.value()).toEqual({ resolvedWith: undefined }));
          const command = observe(adapter.submitAction({ type: "PassPriority" }, 0));
          deliverCodec(current, "Delta", codecStateUpdate(6, 6, { command_id: lastSubmissionCommandId(current) }));
          await vi.waitFor(() => expect(command.value()).toMatchObject({ resolvedWith: { events: [] } }));
          expect(current.close).not.toHaveBeenCalled();
          expect(codec.leases[2].close).not.toHaveBeenCalled();
        }
      } finally {
        armed = false;
        adapter.dispose();
        codec.runtime.dispose();
      }
    },
  );

  it("rejects A's pregame mutation, slot waiter and abandon without rejecting the handoff's game-start waiter", async () => {
    const codec = realRuntimeCodec();
    MockWebSocket.last = null;
    const adapter = new WebSocketAdapter(
      "native-engine", "host", { main_deck: [], sideboard: [] }, undefined, undefined, undefined, "Host",
      {
        nativePregame: {
          kind: "host", playerCount: 2, aiSeats: [],
          socketFactory: () => new MockWebSocket("native-engine") as unknown as PhaseSocketTransport,
        },
        stateCodecSessionProvider: codec.provider,
      },
    );
    const attached = adapter.initializePregame();
    const raw = await codecHandshake("CreateGameWithSettings", null);
    raw.dispatchSynthetic("message", JSON.stringify({
      type: "SessionAttached",
      data: { game_code: "GAME01", player_id: 0, player_token: "codec-token", full_key: REPLY_OWNERSHIP_FULL_KEY },
    }));
    await attached;
    const started = observe(adapter.waitForGameStarted());
    const old = [
      observe(adapter.sendSeatMutation({ type: "Start" })),
      observe(adapter.waitForPlayerSlots()),
      observe(adapter.sendAbandonGame()),
    ];
    try {
      expect(adapter.tryReconnect(session())).toBe(true);
      const successor = await codecHandshake("Reconnect", raw);
      await vi.advanceTimersByTimeAsync(0);
      for (const outcome of old) {
        expect(outcome.value()).toMatchObject({ code: "WS_CLOSED", recoverable: true });
        expect(outcome.settled).toHaveBeenCalledOnce();
      }
      expect(started.value()).toBe("never settled");
      raw.dispatchSynthetic("message", JSON.stringify({ type: "PlayerSlotsUpdate", data: { slots: [] } }));
      raw.dispatchSynthetic("message", JSON.stringify({ type: "GameAbandoned" }));
      deliverCodec(successor, "Full", codecGameStarted(5, 5));
      await vi.waitFor(() => expect(started.value()).toEqual({ resolvedWith: undefined }));
      expect(started.settled).toHaveBeenCalledOnce();
      for (const outcome of old) expect(outcome.settled).toHaveBeenCalledOnce();
      expect(codec.leases[0].close).toHaveBeenCalledOnce();
    } finally {
      adapter.dispose();
      codec.runtime.dispose();
    }
  });

  it("retires a healthy predecessor and its live baseline while the successor still completes commands", async () => {
    const codec = realRuntimeCodec();
    const { adapter, raw, events } = await establishCodecAdapter(codec.provider);
    const baseline = codec.latestBaseline();
    try {
      expect(raw.readyState).toBe(MockWebSocket.OPEN);
      expect(codec.leases[0].close).not.toHaveBeenCalled();
      expect(baseline.free).not.toHaveBeenCalled();
      // The server never closes A. Its local close can synchronously reenter
      // the consumers, which must already have lost authority over B.
      raw.close.mockImplementation(() => {
        raw.dispatchSynthetic("close");
        raw.onerror?.();
      });
      expect(adapter.tryReconnect(session())).toBe(true);
      const successor = await codecHandshake("Reconnect", raw);
      expect.soft(raw.close).toHaveBeenCalledOnce();
      expect.soft(codec.leases[0].close).toHaveBeenCalledOnce();
      expect.soft(baseline.free).toHaveBeenCalledOnce();
      expect(successor.close).not.toHaveBeenCalled();
      expect(codec.leases[1].close).not.toHaveBeenCalled();

      deliverCodec(successor, "Full", codecGameStarted(5, 5));
      await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
      const command = adapter.submitAction({ type: "PassPriority" }, 0);
      deliverCodec(successor, "Delta", codecStateUpdate(6, 6, {
        command_id: lastSubmissionCommandId(successor),
      }));
      await expect(command).resolves.toMatchObject({ events: [] });
      expect((await adapter.getSnapshot()).state.turn_number).toBe(6);
      expect(events).not.toContainEqual({ type: "reconnectFailed" });
      expect(events).not.toContainEqual(expect.objectContaining({ type: "error" }));

      const currentBaseline = codec.latestBaseline();
      adapter.dispose();
      adapter.dispose();
      expect(raw.close).toHaveBeenCalledOnce();
      expect(codec.leases[0].close).toHaveBeenCalledOnce();
      expect(baseline.free).toHaveBeenCalledOnce();
      expect(successor.close).toHaveBeenCalledOnce();
      expect(codec.leases[1].close).toHaveBeenCalledOnce();
      expect(currentBaseline.free).toHaveBeenCalledOnce();
    } finally {
      adapter.dispose();
      codec.runtime.dispose();
    }
  });

  it.each(["throw", "dispose then throw", "supersede then throw"] as const)(
    "releases a live acquired decoder when serverHello listeners %s without failing a newer attachment",
    async (ending) => {
      const codec = realRuntimeCodec();
      const prepared = await codec.provider();
      if (!prepared) throw new Error("The runtime did not open a codec lease");
      // The public decoder lease already retains an actual runtime baseline
      // when handed to the socket; a close-only mock cannot prove its release.
      await prepared.decode(new TextEncoder().encode(fullFrame(JSON.stringify(codecGameStarted(1, 1)))));
      const baseline = codec.latestBaseline();
      let first = true;
      MockWebSocket.last = null;
      const adapter = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
        undefined, undefined, undefined,
        { stateCodecSessionProvider: () => {
          if (!first) return codec.provider();
          first = false;
          return Promise.resolve(prepared);
        } },
      );
      const failure = new Error("serverHello listener failed");
      const events: unknown[] = [];
      let armed = true;
      adapter.onEvent((event) => {
        events.push(event);
        if (event.type !== "serverHello" || !armed) return;
        armed = false;
        expect(baseline.free).not.toHaveBeenCalled();
        if (ending === "dispose then throw") adapter.dispose();
        if (ending === "supersede then throw") expect(adapter.tryReconnect(session())).toBe(true);
        throw failure;
      });
      let initialization: unknown = "never settled";
      void adapter.initialize().then(
        (value) => { initialization = { resolvedWith: value }; },
        (error: unknown) => { initialization = error; },
      );
      const initialized = async () => {
        await vi.advanceTimersByTimeAsync(0);
        return initialization;
      };
      try {
        await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
        const abandoned = latestSocket();
        abandoned.dispatchSynthetic("message", CODEC_SESSION_HELLO);
        await vi.advanceTimersByTimeAsync(0);
        expect.soft(abandoned.close).toHaveBeenCalledOnce();
        expect.soft(prepared.close).toHaveBeenCalledOnce();
        expect.soft(baseline.free).toHaveBeenCalledOnce();
        expect(abandoned.send).not.toHaveBeenCalledWith(expect.stringContaining('"type":"JoinGameWithPassword"'));
        abandoned.dispatchSynthetic("message", CODEC_CONFIRMATION);
        deliverCodec(abandoned, "Full", codecGameStarted(99, 99));
        expect(codec.engine.state_codec_decode).toHaveBeenCalledOnce();

        if (ending === "supersede then throw") {
          expect(await initialized()).toBe("never settled");
          const successor = await codecHandshake("Reconnect", abandoned);
          deliverCodec(successor, "Full", codecGameStarted(5, 5));
          await expect(initialized()).resolves.toEqual({ resolvedWith: undefined });
          expect((await adapter.getSnapshot()).state.turn_number).toBe(5);
          expect(successor.close).not.toHaveBeenCalled();
          expect(codec.leases[1].close).not.toHaveBeenCalled();
          expect(events).not.toContainEqual({ type: "reconnectFailed" });
          expect(events).not.toContainEqual(expect.objectContaining({ type: "error" }));
          const command = adapter.submitAction({ type: "PassPriority" }, 0);
          deliverCodec(successor, "Delta", codecStateUpdate(6, 6, {
            command_id: lastSubmissionCommandId(successor),
          }));
          await expect(command).resolves.toMatchObject({ events: [] });
        } else if (ending === "dispose then throw") {
          await expect(initialized()).resolves.toMatchObject({
            code: "WS_CLOSED", message: "Adapter disposed before initialization completed",
          });
        } else {
          expect(await initialized()).toBe(failure);
        }
        adapter.dispose();
        expect(abandoned.close).toHaveBeenCalledOnce();
        expect(prepared.close).toHaveBeenCalledOnce();
        expect(baseline.free).toHaveBeenCalledOnce();
      } finally {
        adapter.dispose();
        codec.runtime.dispose();
      }
    },
  );
});

describe("WebSocketAdapter — codec degradation policy", () => {
  const action = { type: "PassPriority" } as GameAction;
  const outcomeOf = (promise: Promise<unknown>) => promise.then(() => "resolved" as const, (error: unknown) => error);

  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  /** A snapshot-only reconnect attachment that restores play from plain frames. */
  async function restoreFromPlainFrames(
    adapter: WebSocketAdapter,
    events: unknown[],
    previous: MockWebSocket,
  ) {
    await vi.advanceTimersByTimeAsync(1000);
    const reconnect = await snapshotOnlyHandshake("Reconnect", previous);
    expect(offeredCodec(reconnect)).toBe(false);
    reconnect.dispatchSynthetic("message", JSON.stringify(codecGameStarted(5, 5)));
    await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
    const resumed = adapter.submitAction(action, 0);
    await vi.waitFor(() => expect(submissionTypes(reconnect)).toHaveLength(1));
    reconnect.dispatchSynthetic("message", JSON.stringify(
      codecStateUpdate(6, 6, { command_id: lastSubmissionCommandId(reconnect) }),
    ));
    await expect(resumed).resolves.toMatchObject({ events: [] });
    return reconnect;
  }

  // Confirmation has no deadline; heartbeat does not close a silent socket.
  // A deadline would fail an initial join AFTER JoinGameWithPassword was sent,
  // and the joiner's player_token only arrives inside the GameStarted that a
  // failed attachment never delivers, orphaning the seat.
  it("completes an initial join whose confirmation hello arrives after the former deadline, leaving the policy negotiating", async () => {
    const codec = scriptedCodec();
    const provider = vi.fn(() => Promise.resolve<StateCodecSession | null>(codec.session));
    MockWebSocket.last = null;
    const adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined, { stateCodecSessionProvider: provider },
    );
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    const init = adapter.initialize();
    const initOutcome = init.then(() => "resolved" as const, (error: unknown) => error);
    try {
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
      const raw = MockWebSocket.last!;
      raw.dispatchSynthetic("message", CODEC_SESSION_HELLO);
      await vi.waitFor(() => expect(raw.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"JoinGameWithPassword"'),
      ));
      // Reach guard: the codec was offered and the join request is on the wire.
      expect(offeredCodec(raw)).toBe(true);

      await vi.advanceTimersByTimeAsync(STATE_CODEC_SOCKET_TIMEOUT_MS + 1000);
      expect(raw.close).not.toHaveBeenCalled();
      expect(codec.close).not.toHaveBeenCalled();
      expect(policyOf(adapter)).toEqual({ kind: "negotiate" });

      raw.dispatchSynthetic("message", CODEC_CONFIRMATION);
      deliverCodec(raw, "Full", codecGameStarted(1, 1));
      // The owner is settled by the GameStarted, not rejected by the attachment.
      expect(await initOutcome).toBe("resolved");
      expect((await adapter.getSnapshot()).state.turn_number).toBe(1);
      expect(codec.decode).toHaveBeenCalledOnce();
      expect(raw.close).not.toHaveBeenCalled();
      expect(codec.close).not.toHaveBeenCalled();
      expect(policyOf(adapter)).toEqual({ kind: "negotiate" });

      // The next attachment still negotiates the codec.
      raw.dispatchSynthetic("close");
      await vi.advanceTimersByTimeAsync(1000);
      const reconnect = await codecHandshake("Reconnect", raw);
      expect(offeredCodec(reconnect)).toBe(true);
      expect(provider).toHaveBeenCalledTimes(2);
    } finally {
      adapter.dispose();
    }
  });

  it("reconnects snapshot-only after a decode failure, rejecting the pending command once and not consulting the provider again", async () => {
    const first = scriptedCodec();
    const spare = scriptedCodec();
    const sessions = [first.session, spare.session];
    const provider = vi.fn(() => Promise.resolve(sessions.shift() ?? null));
    const { adapter, raw, events } = await establishCodecAdapter(provider);
    try {
      // Reach guard: the initial attachment negotiated and consulted the provider.
      expect(provider).toHaveBeenCalledOnce();
      expect(offeredCodec(raw)).toBe(true);
      const outcome = outcomeOf(adapter.submitAction(action, 0));
      await vi.waitFor(() => expect(submissionTypes(raw)).toHaveLength(1));

      first.decode.mockImplementationOnce(async () => { throw new Error("State codec decode timed out after 60000ms"); });
      deliverCodec(raw, "Delta", codecStateUpdate(2, 2));
      await expect(outcome).resolves.toMatchObject({
        code: "WS_CLOSED", message: "Connection closed during action", recoverable: true,
      });
      expect(first.close).toHaveBeenCalledOnce();
      expect(resyncRequests(raw)).toBe(0);

      await restoreFromPlainFrames(adapter, events, raw);
      expect(provider).toHaveBeenCalledOnce();
      expect(spare.close).not.toHaveBeenCalled();
      expect(spare.decode).not.toHaveBeenCalled();
    } finally {
      adapter.dispose();
    }
  });

  it("tolerates one completed recovery, delivers the second recovery's Full without ready, then reconnects snapshot-only; a fresh adapter negotiates again", async () => {
    const first = scriptedCodec();
    const provider = vi.fn(() => Promise.resolve<StateCodecSession | null>(first.session));
    const { adapter, raw, events } = await establishCodecAdapter(provider);
    const probes: string[] = [];
    let probing = false;
    adapter.onEvent((event) => {
      if ((event as { type?: string }).type !== "stateChanged" || !probing) return;
      // A listener that submits during the Full's delivery must be refused.
      adapter.submitAction(action, 0).then(() => probes.push("sent"), (error: AdapterError) => probes.push(error.code));
    });
    try {
      // Recovery one: refused while recovering, admitted after ready.
      first.rejectNext(1);
      deliverCodec(raw, "Delta", codecStateUpdate(2, 2));
      await vi.waitFor(() => expect(resyncRequests(raw)).toBe(1));
      probing = true;
      deliverCodec(raw, "Full", codecStateUpdate(3, 3));
      await vi.waitFor(() => expect(probes).toEqual([AdapterErrorCode.ACTION_NOT_SENT]));
      probing = false;
      const resumed = adapter.submitAction(action, 0);
      await vi.waitFor(() => expect(submissionTypes(raw)).toHaveLength(1));
      deliverCodec(raw, "Delta", codecStateUpdate(4, 4, { command_id: lastSubmissionCommandId(raw) }));
      await expect(resumed).resolves.toMatchObject({ events: [] });
      expect(raw.close).not.toHaveBeenCalled();

      // Recovery two: its Full is delivered, submissions stay refused, then the stream closes.
      first.rejectNext(1);
      deliverCodec(raw, "Delta", codecStateUpdate(5, 5));
      await vi.waitFor(() => expect(resyncRequests(raw)).toBe(2));
      expect(raw.close).not.toHaveBeenCalled();
      probing = true;
      deliverCodec(raw, "Full", codecStateUpdate(6, 6));
      await vi.waitFor(() => expect(raw.close).toHaveBeenCalled());
      expect(probes).toEqual([
        AdapterErrorCode.ACTION_NOT_SENT,
        AdapterErrorCode.ACTION_NOT_SENT,
      ]);
      expect((adapter as unknown as { snapshot: { state: GameState } }).snapshot.state.turn_number).toBe(6);
      expect(first.close).toHaveBeenCalledOnce();
      probing = false;

      await restoreFromPlainFrames(adapter, events, raw);
      expect(provider).toHaveBeenCalledOnce();
    } finally {
      adapter.dispose();
    }

    // The policy died with the adapter: a fresh adapter on the same healthy
    // provider offers the codec and decodes its bootstrap.
    const next = scriptedCodec();
    const fresh = await establishCodecAdapter(() => Promise.resolve(next.session));
    try {
      expect(offeredCodec(fresh.raw)).toBe(true);
      expect(next.decode).toHaveBeenCalledOnce();
    } finally {
      fresh.adapter.dispose();
    }
  });

  it("keeps negotiating across a recovery, a network reconnect and a second recovery on the replacement", async () => {
    const first = scriptedCodec();
    const second = scriptedCodec();
    const sessions = [first.session, second.session];
    const provider = vi.fn(() => Promise.resolve(sessions.shift() ?? null));
    const { adapter, raw, events } = await establishCodecAdapter(provider);
    const turn = () => (adapter as unknown as { snapshot: { state: GameState } }).snapshot.state.turn_number;
    try {
      first.rejectNext(1);
      deliverCodec(raw, "Delta", codecStateUpdate(2, 2));
      await vi.waitFor(() => expect(resyncRequests(raw)).toBe(1));
      deliverCodec(raw, "Full", codecStateUpdate(3, 3));
      await vi.waitFor(() => expect(turn()).toBe(3));
      expect(raw.close).not.toHaveBeenCalled();

      raw.dispatchSynthetic("close");
      await vi.advanceTimersByTimeAsync(1000);
      const reconnect = await codecHandshake("Reconnect", raw);
      // Reach guard: the first recovery completed without degrading, so the
      // following attachment still advertises the codec.
      expect(offeredCodec(reconnect)).toBe(true);
      deliverCodec(reconnect, "Full", codecGameStarted(5, 5));
      await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));

      second.rejectNext(1);
      deliverCodec(reconnect, "Delta", codecStateUpdate(6, 6));
      await vi.waitFor(() => expect(resyncRequests(reconnect)).toBe(1));
      deliverCodec(reconnect, "Full", codecStateUpdate(7, 7));
      await vi.waitFor(() => expect(turn()).toBe(7));
      const resumed = adapter.submitAction(action, 0);
      await vi.waitFor(() => expect(submissionTypes(reconnect)).toHaveLength(1));
      deliverCodec(reconnect, "Delta", codecStateUpdate(8, 8, { command_id: lastSubmissionCommandId(reconnect) }));
      await expect(resumed).resolves.toMatchObject({ events: [] });
      expect(reconnect.close).not.toHaveBeenCalled();
      expect(second.close).not.toHaveBeenCalled();
      expect(provider).toHaveBeenCalledTimes(2);
    } finally {
      adapter.dispose();
    }
  });

  it("closes a lease that arrives after the adapter degraded and attaches snapshot-only, settling the reconnect", async () => {
    const first = scriptedCodec();
    const late = scriptedCodec();
    let releasePreparation!: (session: StateCodecSession | null) => void;
    const providers = [
      () => Promise.resolve<StateCodecSession | null>(first.session),
      () => new Promise<StateCodecSession | null>((resolve) => { releasePreparation = resolve; }),
    ];
    const { adapter, raw, events } = await establishCodecAdapter(() => providers.shift()!());
    try {
      // The replacement attachment is still preparing; A is current.
      expect(adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
      expect(releasePreparation).toBeTypeOf("function");

      first.decode.mockImplementationOnce(async () => { throw new Error("decoder trapped"); });
      deliverCodec(raw, "Delta", codecStateUpdate(2, 2));
      await vi.waitFor(() => expect(first.close).toHaveBeenCalledOnce());
      // Reach guard: A is closed and the preparation is still outstanding.
      expect(late.close).not.toHaveBeenCalled();

      releasePreparation(late.session);
      const reconnect = await snapshotOnlyHandshake("Reconnect", raw);
      expect(late.close).toHaveBeenCalledOnce();
      expect(offeredCodec(reconnect)).toBe(false);
      reconnect.dispatchSynthetic("message", JSON.stringify(codecGameStarted(5, 5)));
      await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
      expect(late.decode).not.toHaveBeenCalled();
    } finally {
      adapter.dispose();
    }
  });

  it("adopts an already-negotiated candidate when the current attachment degrades, then attaches snapshot-only", async () => {
    const first = scriptedCodec();
    const second = scriptedCodec();
    const sessions = [first.session, second.session];
    const provider = vi.fn(() => Promise.resolve(sessions.shift() ?? null));
    const { adapter, raw, events } = await establishCodecAdapter(provider);
    try {
      // B holds its lease and is mid-handshake while A is still current.
      expect(adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(raw));
      const candidate = latestSocket();
      expect(provider).toHaveBeenCalledTimes(2);

      first.decode.mockImplementationOnce(async () => { throw new Error("decoder trapped"); });
      deliverCodec(raw, "Delta", codecStateUpdate(2, 2));
      await vi.waitFor(() => expect(first.close).toHaveBeenCalledOnce());

      // B is not retired for A's failure: it negotiates, bootstraps and serves a command.
      candidate.dispatchSynthetic("message", CODEC_SESSION_HELLO);
      await vi.waitFor(() => expect(candidate.send).toHaveBeenCalledWith(expect.stringContaining('"type":"Reconnect"')));
      expect(offeredCodec(candidate)).toBe(true);
      candidate.dispatchSynthetic("message", CODEC_CONFIRMATION);
      deliverCodec(candidate, "Full", codecGameStarted(5, 5));
      await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
      const resumed = adapter.submitAction(action, 0);
      await vi.waitFor(() => expect(submissionTypes(candidate)).toHaveLength(1));
      deliverCodec(candidate, "Full", codecStateUpdate(6, 6, { command_id: lastSubmissionCommandId(candidate) }));
      await expect(resumed).resolves.toMatchObject({ events: [] });
      expect(second.close).not.toHaveBeenCalled();

      // The next attachment (C) is plain.
      candidate.dispatchSynthetic("close");
      await vi.advanceTimersByTimeAsync(10_000);
      const third = await snapshotOnlyHandshake("Reconnect", candidate);
      expect(offeredCodec(third)).toBe(false);
      expect(provider).toHaveBeenCalledTimes(2);
    } finally {
      adapter.dispose();
    }
  });

  it("captures a confirmation failure that lands before adoption, rejects the join once and attaches the next attempt snapshot-only", async () => {
    const early = scriptedCodec();
    const spare = scriptedCodec();
    const sessions = [early.session, spare.session];
    const provider = vi.fn(() => Promise.resolve(sessions.shift() ?? null));
    MockWebSocket.last = null;
    const adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined, { stateCodecSessionProvider: provider },
    );
    const sendSpy = vi.spyOn(adapter as unknown as { send(message: unknown): boolean }, "send");
    const events: unknown[] = [];
    const policyAtServerHello: unknown[] = [];
    adapter.onEvent((event) => {
      events.push(event);
      // The reason must already be consumed when a serverHello listener runs.
      if ((event as { type?: string }).type === "serverHello") policyAtServerHello.push(policyOf(adapter));
    });
    try {
      const joined = outcomeOf(adapter.initialize());
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
      const raw = latestSocket();
      const adoptedAtFailure: boolean[] = [];
      // The contradictory confirmation is delivered from inside the ClientHello
      // send, i.e. after the wrapper is installed and before openPhaseSocket
      // resolves: the failure is published before the adapter adopts the socket.
      raw.send.mockImplementation((text: unknown) => {
        if (typeof text !== "string" || !text.includes('"type":"ClientHello"')) return;
        adoptedAtFailure.push((adapter as unknown as { ws: unknown }).ws !== null);
        raw.dispatchSynthetic("message", JSON.stringify({
          type: "ServerHello",
          data: { server_version: "0.0.0-test", selected_state_codec: "SomeOtherCodec" },
        }));
      });
      raw.dispatchSynthetic("message", CODEC_SESSION_HELLO);

      await expect(joined).resolves.toMatchObject({
        code: "WS_CLOSED", message: "Connection closed before game started", recoverable: true,
      });
      // Reach guard: the codec really was offered, and no socket was adopted yet.
      expect(offeredCodec(raw)).toBe(true);
      expect(adoptedAtFailure).toEqual([false]);
      expect(provider).toHaveBeenCalledOnce();
      // One wrapper failure: lease released once, no resync, one serverHello, no retry of the join.
      expect(early.close).toHaveBeenCalledOnce();
      expect(resyncRequests(raw)).toBe(0);
      expect(policyAtServerHello).toEqual([{ kind: "snapshotOnly", reason: "confirmationFailure" }]);
      expect(events.filter((event) => (event as { type?: string }).type === "serverHello")).toHaveLength(1);
      expect(events).not.toContainEqual(expect.objectContaining({ type: "reconnecting" }));
      // The retained close had already settled the join, so the continuation is
      // fenced: no setup frame is attempted on the closed attachment and the
      // obsolete "Cannot send message" feedback is not emitted.
      expect(sendSpy.mock.calls.filter(([message]) =>
        (message as { type?: string }).type === "JoinGameWithPassword")).toHaveLength(0);
      expect(events).not.toContainEqual(expect.objectContaining({ type: "error" }));

      // The next eligible attachment of the same adapter is snapshot-only.
      expect(adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
      const next = await snapshotOnlyHandshake("Reconnect", raw);
      expect(offeredCodec(next)).toBe(false);
      expect(provider).toHaveBeenCalledOnce();
      next.dispatchSynthetic("message", JSON.stringify(codecGameStarted(5, 5)));
      await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
      expect(spare.decode).not.toHaveBeenCalled();
    } finally {
      adapter.dispose();
    }
  });

  it("creates the degraded attachment's socket in the same turn, while a negotiating attachment still awaits its lease", async () => {
    const first = scriptedCodec();
    const second = scriptedCodec();
    const sessions = [first.session, second.session];
    const provider = vi.fn(() => Promise.resolve(sessions.shift() ?? null));
    const { adapter, raw, events } = await establishCodecAdapter(provider);
    try {
      // Positive provider case: a healthy loss re-negotiates, so the socket is
      // not created until the lease has been prepared.
      raw.dispatchSynthetic("close");
      await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({ type: "reconnecting" })));
      vi.advanceTimersByTime(1000);
      expect(provider).toHaveBeenCalledTimes(2);
      expect(MockWebSocket.last).toBe(raw);
      const candidate = await codecHandshake("Reconnect", raw);
      expect(offeredCodec(candidate)).toBe(true);
      deliverCodec(candidate, "Full", codecGameStarted(5, 5));
      await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));

      // A real qualifying failure on the live attachment degrades the adapter.
      second.decode.mockImplementationOnce(async () => { throw new Error("decoder trapped"); });
      deliverCodec(candidate, "Delta", codecStateUpdate(6, 6));
      await vi.waitFor(() => expect(second.close).toHaveBeenCalledOnce());
      expect(policyOf(adapter)).toEqual({ kind: "snapshotOnly", reason: "decodeFailure" });
      expect(MockWebSocket.last).toBe(candidate);

      // The reconnect timer opens the next socket without any await in between.
      vi.advanceTimersByTime(1000);
      expect(MockWebSocket.last).not.toBe(candidate);
      expect(provider).toHaveBeenCalledTimes(2);
      const plain = latestSocket();
      plain.dispatchSynthetic("message", CODEC_SESSION_HELLO);
      await vi.waitFor(() => expect(plain.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"Reconnect"'),
      ));
      expect(offeredCodec(plain)).toBe(false);
    } finally {
      adapter.dispose();
    }
  });

  describe("after a decoder trap on the shared runtime", () => {
    afterEach(() => { sharedEngine.open = null; });

    it("settles an adopted candidate normally when the shared runtime traps on another live adapter before the candidate's next decode", async () => {
      const { runtime, script, provider } = realRuntimeCodec();
      const { adapter, raw, events } = await establishCodecAdapter(provider);
      let trapAdapter: WebSocketAdapter | null = null;
      try {
        expect(runtime.state).toEqual({ kind: "ready" });
        // B bootstraps on its own session after A has been retired and closed.
        expect(adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
        const candidate = await codecHandshake("Reconnect", raw);
        expect(offeredCodec(candidate)).toBe(true);
        deliverCodec(candidate, "Full", codecGameStarted(5, 5));
        await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
        const outcome = outcomeOf(adapter.submitAction(action, 0));
        await vi.waitFor(() => expect(submissionTypes(candidate)).toHaveLength(1));

        // Another genuinely live adapter shares the runtime. Its trap poisons
        // B's already-open lease without relying on impossible traffic from A.
        const trapSource = await establishCodecAdapter(provider);
        trapAdapter = trapSource.adapter;
        expect((await trapSource.adapter.getSnapshot()).state.turn_number).toBe(1);
        script.trapNext = true;
        deliverCodec(trapSource.raw, "Delta", codecStateUpdate(6, 6));
        await vi.waitFor(() => expect(runtime.state).toEqual({ kind: "poisoned", reason: "wasmTrap" }));
        await vi.waitFor(() => expect(trapSource.raw.close).toHaveBeenCalled());
        trapSource.adapter.dispose();
        // Reach guard: B already held a session and was neither closed nor degraded by the other adapter's trap.
        expect(candidate.close).not.toHaveBeenCalled();
        expect(policyOf(adapter)).toEqual({ kind: "negotiate" });

        // B's next decode meets the poison: its owner settles, it is not abandoned.
        events.length = 0;
        deliverCodec(candidate, "Delta", codecStateUpdate(7, 7, { command_id: lastSubmissionCommandId(candidate) }));
        await expect(outcome).resolves.toMatchObject({
          code: "WS_CLOSED", message: "Connection closed during action", recoverable: true,
        });
        expect(candidate.close).toHaveBeenCalledOnce();
        expect(policyOf(adapter)).toEqual({ kind: "snapshotOnly", reason: "decodeFailure" });
        expect(events).toContainEqual(expect.objectContaining({ type: "reconnecting" }));

        // The existing reconnect path continues on plain frames without the provider.
        await restoreFromPlainFrames(adapter, events, candidate);
      } finally {
        adapter.dispose();
        trapAdapter?.dispose();
      }
    });

    it("completes a correlated action on a default-constructed fresh adapter after the trap, through the shared facade", async () => {
      const { engine, runtime, script, provider } = realRuntimeCodec();
      sharedEngine.open = provider;
      const create = () => new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      );
      MockWebSocket.last = null;
      const first = create();
      const firstInit = first.initialize();
      void firstInit.catch(() => undefined);
      let firstFallback: MockWebSocket;
      try {
        // The shared default races discovery, so send the hello only after its lease is ready.
        await vi.waitFor(() => expect(provider).toHaveBeenCalledOnce());
        await provider.mock.results[0]!.value;
        await Promise.resolve();
        const trapped = await codecHandshake("JoinGameWithPassword", null);
        expect(offeredCodec(trapped)).toBe(true);
        script.trapNext = true;
        deliverCodec(trapped, "Full", codecGameStarted(1, 1));
        await vi.waitFor(() => expect(runtime.state).toEqual({ kind: "poisoned", reason: "wasmTrap" }));
        // The first adapter finishes its own join on a snapshot-only fallback.
        firstFallback = await snapshotOnlyHandshake("Reconnect", trapped);
        expect(offeredCodec(firstFallback)).toBe(false);
        firstFallback.dispatchSynthetic("message", JSON.stringify(codecGameStarted(1, 1, { player_token: undefined })));
        await firstInit;
      } finally {
        first.dispose();
      }

      const second = create();
      try {
        const init = second.initialize();
        void init.catch(() => undefined);
        const plain = await snapshotOnlyHandshake("JoinGameWithPassword", firstFallback);
        // The default route consulted the shared facade; the runtime itself refused the lease.
        expect(provider).toHaveBeenCalledTimes(2);
        expect(offeredCodec(plain)).toBe(false);
        plain.dispatchSynthetic("message", JSON.stringify(codecGameStarted(1, 1)));
        await init;

        const completion = second.submitAction(action, 0);
        void completion.catch(() => undefined);
        await vi.waitFor(() => expect(submissionTypes(plain)).toHaveLength(1));
        plain.dispatchSynthetic("message", JSON.stringify(
          codecStateUpdate(2, 2, { command_id: lastSubmissionCommandId(plain) }),
        ));
        await expect(completion).resolves.toMatchObject({ events: [] });
        expect((await second.getSnapshot()).state.turn_number).toBe(2);
        expect(engine.state_codec_decode).toHaveBeenCalledOnce();
      } finally {
        second.dispose();
      }
    });

    function trappingRuntime() {
      const engine = {
        state_codec_decode: vi.fn(() => { throw new WebAssembly.RuntimeError("unreachable"); }),
      };
      const runtime = new StateCodecRuntime(engine as unknown as EngineModule);
      // The production lease shape over the real runtime: the poison verdict is not mocked.
      const provider = vi.fn(async (): Promise<StateCodecSession | null> => {
        const sessionId = runtime.open();
        return {
          decode: async (frame) => runtime.decode(sessionId, frame),
          close: () => runtime.close(sessionId),
        };
      });
      return { engine, runtime, provider };
    }

    const newJoin = (provider: () => Promise<StateCodecSession | null>) => new WebSocketAdapter(
      "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined, { stateCodecSessionProvider: provider },
    );

    it("offers no codec from a fresh adapter on the same runtime, which joins from plain frames", async () => {
      const { engine, runtime, provider } = trappingRuntime();
      MockWebSocket.last = null;
      const first = newJoin(provider);
      const firstInit = first.initialize();
      const raw = await codecHandshake("JoinGameWithPassword", null);
      expect(offeredCodec(raw)).toBe(true);
      deliverCodec(raw, "Full", codecGameStarted(1, 1));
      await vi.waitFor(() => expect(runtime.state).toEqual({ kind: "poisoned", reason: "wasmTrap" }));
      expect(engine.state_codec_decode).toHaveBeenCalledOnce();
      // The first adapter finishes its own join on a snapshot-only fallback.
      const firstFallback = await snapshotOnlyHandshake("Reconnect", raw);
      expect(offeredCodec(firstFallback)).toBe(false);
      firstFallback.dispatchSynthetic("message", JSON.stringify(codecGameStarted(1, 1, { player_token: undefined })));
      await firstInit;
      first.dispose();

      const second = newJoin(provider);
      try {
        const init = second.initialize();
        const plain = await snapshotOnlyHandshake("JoinGameWithPassword", firstFallback);
        // The provider was consulted and refused by the runtime itself.
        expect(provider).toHaveBeenCalledTimes(2);
        expect(offeredCodec(plain)).toBe(false);
        plain.dispatchSynthetic("message", JSON.stringify(codecGameStarted(1, 1)));
        await init;
        expect((await second.getSnapshot()).state.turn_number).toBe(1);
        expect(engine.state_codec_decode).toHaveBeenCalledOnce();
      } finally {
        second.dispose();
      }
    });
  });
});

// ── Ordered codec errors and the attach continuation fence ──────────────
//
// A raw transport error is ordered behind the frames the codec wrapper has
// already received, so a GameStarted held in the decoder still completes the
// join before the error becomes established-socket feedback. A retained close
// (or a reentrant attachment) can end an attachment while `attachSocket` is
// still installing its consumers: the setup frame must then not be sent.

describe("WebSocketAdapter — ordered codec errors", () => {
  /** A decoder whose first decode stays inside the decoder until released. */
  function heldCodec() {
    let resolveDecode: ((result: CodecDecodeResult) => void) | null = null;
    const decode = vi.fn(() => new Promise<CodecDecodeResult>((resolve) => { resolveDecode = resolve; }));
    const close = vi.fn();
    const session: StateCodecSession = { decode, close };
    return {
      session, decode, close,
      release: (turn: number) => {
        if (resolveDecode === null) throw new Error("the decoder was never entered");
        resolveDecode({ kind: "decoded", json: JSON.stringify(codecGameStarted(turn, turn)) });
      },
    };
  }

  it("resolves the join from a GameStarted Full held in the decoder before the raw error that follows it, which only becomes established-socket feedback", async () => {
    const held = heldCodec();
    const { adapter, raw, init } = await startCodecAdapter(held.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    try {
      deliverCodec(raw, "Full", codecGameStarted(1, 1));
      // Reach guard: the frame is inside the decoder, so the wrapper received it.
      await vi.waitFor(() => expect(held.decode).toHaveBeenCalledOnce());
      raw.onerror?.();
      await new Promise((resolve) => setTimeout(resolve, 0));
      // The error is still queued behind the held frame.
      expect(events).toEqual([]);

      held.release(1);
      await init;
      await vi.waitFor(() => expect(events).toContainEqual(
        { type: "error", message: "WebSocket connection failed" },
      ));
      const types = events.map((event) => (event as { type: string }).type);
      expect(types.indexOf("bootstrapDelivered")).toBeGreaterThanOrEqual(0);
      expect(types.indexOf("bootstrapDelivered")).toBeLessThan(types.indexOf("error"));
      expect((await adapter.getSnapshot()).state.turn_number).toBe(1);
      // An ordinary error is not a codec failure: the lease is untouched and
      // the policy keeps negotiating.
      expect(held.close).not.toHaveBeenCalled();
      expect(policyOf(adapter)).toEqual({ kind: "negotiate" });
    } finally {
      adapter.dispose();
    }
  });

  it("rejects a pending join once on an error that precedes every frame, and a following frame cannot undo it", async () => {
    const codec = scriptedCodec();
    const { adapter, raw, init } = await startCodecAdapter(codec.session);
    const settled = trackRejection(init);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    try {
      raw.onerror?.();
      await expect(settled()).resolves.toMatchObject({
        code: "WS_ERROR", message: "WebSocket connection failed", recoverable: true,
      });
      // Reach guard: the following Full really is decoded by the live wrapper.
      deliverCodec(raw, "Full", codecGameStarted(1, 1));
      await vi.waitFor(() => expect(codec.decode).toHaveBeenCalledOnce());
      await new Promise((resolve) => setTimeout(resolve, 0));
      await expect(settled()).resolves.toMatchObject({ code: "WS_ERROR", message: "WebSocket connection failed" });
      expect(events.filter((event) => (event as { type: string }).type === "error")).toEqual([]);
    } finally {
      adapter.dispose();
    }
  });

  it.each(["rejects before the drain deadline", "never settles"] as const)(
    "settles an in-flight submission and reconnects with codec capability when a naturally closed decode %s",
    async (outcome) => {
      const first = scriptedCodec();
      const second = scriptedCodec();
      const sessions = [first.session, second.session];
      const provider = vi.fn(() => Promise.resolve(sessions.shift() ?? null));
      const { adapter, raw, events } = await establishCodecAdapter(provider);
      let rejectDecode!: (error: Error) => void;
      let signalEntered!: () => void;
      const entered = new Promise<void>((resolve) => { signalEntered = resolve; });
      first.decode.mockImplementationOnce(() => new Promise<CodecDecodeResult>((_resolve, reject) => {
        rejectDecode = reject;
        signalEntered();
      }));
      const pendingOutcomes: unknown[] = [];
      void adapter.submitAction({ type: "PassPriority" }, 0).then(
        (result) => { pendingOutcomes.push({ resolvedWith: result }); },
        (error: unknown) => { pendingOutcomes.push(error); },
      );
      const commandId = lastSubmissionCommandId(raw);
      deliverCodec(raw, "Delta", codecStateUpdate(2, 2, { command_id: commandId }));
      await entered;
      // Reach guard: a genuine accepted game and an actual Action are waiting
      // on a completion held inside the second decode, not on a mock promise.
      expect(submissionTypes(raw)).toEqual(["Action"]);
      expect(first.decode).toHaveBeenCalledTimes(2);
      vi.useFakeTimers();
      try {
        raw.readyState = MockWebSocket.CLOSED;
        raw.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(0);
        expect(pendingOutcomes).toEqual([]);
        if (outcome === "rejects before the drain deadline") {
          rejectDecode(new Error("RPC rejection after transport death"));
          await vi.advanceTimersByTimeAsync(0);
        } else {
          // The natural-close drain has its own five-second ceiling, even
          // though this fixture never installs a decoder watchdog.
          await vi.advanceTimersByTimeAsync(5_000);
        }
        expect(pendingOutcomes).toEqual([expect.objectContaining({
          code: "WS_CLOSED", message: "Connection closed during action", recoverable: true,
        })]);
        expect(events).toContainEqual({ type: "actionPendingChanged", pending: false });
        expect(events.filter((event) => event !== null && typeof event === "object"
          && "type" in event && event.type === "reconnecting")).toHaveLength(1);
        expect(first.close).toHaveBeenCalledOnce();
        expect(raw.close).not.toHaveBeenCalled();
        expect(resyncRequests(raw)).toBe(0);
        expect((await adapter.getSnapshot()).state.turn_number).toBe(1);

        await vi.advanceTimersByTimeAsync(1000);
        const replacement = await codecHandshake("Reconnect", raw);
        // The observable policy witness: network death did not poison future
        // attachments into snapshot-only mode or bypass the explicit provider.
        expect(provider).toHaveBeenCalledTimes(2);
        expect(offeredCodec(replacement)).toBe(true);
        deliverCodec(replacement, "Full", codecGameStarted(5, 5));
        await vi.advanceTimersByTimeAsync(0);
        expect(events).toContainEqual({ type: "reconnected", inGame: true });
        expect((await adapter.getSnapshot()).state.turn_number).toBe(5);

        if (outcome === "never settles") {
          rejectDecode(new Error("obsolete decoder rejected after reconnection"));
          await vi.advanceTimersByTimeAsync(0);
        }
        const resumed = adapter.submitAction({ type: "PassPriority" }, 0);
        deliverCodec(replacement, "Delta", codecStateUpdate(6, 6, {
          command_id: lastSubmissionCommandId(replacement),
        }));
        await expect(resumed).resolves.toMatchObject({ events: [] });
        expect(pendingOutcomes).toHaveLength(1);
        expect(first.close).toHaveBeenCalledOnce();
        expect(second.close).not.toHaveBeenCalled();
        expect(resyncRequests(replacement)).toBe(0);
      } finally {
        adapter.dispose();
        vi.useRealTimers();
      }
    },
  );

  describe("an attachment superseded while it installs its consumers", () => {
    const setupFrames = (send: { mock: { calls: unknown[][] } }) => send.mock.calls
      .map(([message]) => (message as { type?: string }).type)
      .filter((type) => type === "JoinGameWithPassword" || type === "Reconnect");

    it.each([true, false])("closes the retired socket without a setup frame and leaves the join to its successor (reentrant=%s)", async (reentrant) => {
      MockWebSocket.last = null;
      const first = scriptedCodec();
      const second = scriptedCodec();
      const sessions = [first.session, second.session];
      const adapter = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
        undefined, undefined, undefined,
        { stateCodecSessionProvider: () => Promise.resolve(sessions.shift() ?? null) },
      );
      const sendSpy = vi.spyOn(adapter as unknown as { send(message: unknown): boolean }, "send");
      let reentered = false;
      adapter.onEvent((event) => {
        if (reentrant && event.type === "serverHello" && !reentered) {
          reentered = true;
          expect(adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
        }
      });
      try {
        const init = trackRejection(adapter.initialize());
        await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
        const retired = latestSocket();
        retired.dispatchSynthetic("message", CODEC_SESSION_HELLO);

        if (!reentrant) {
          // Positive control: a still-current attachment does send its setup.
          await vi.waitFor(() => expect(setupFrames(sendSpy)).toEqual(["JoinGameWithPassword"]));
          expect(first.close).not.toHaveBeenCalled();
          expect(retired.close).not.toHaveBeenCalled();
          expect(await init()).toBe("never settled");
          return;
        }

        // The superseded attachment's socket is closed through the normal path,
        // releasing its lease, and no setup frame goes out on it.
        await vi.waitFor(() => expect(first.close).toHaveBeenCalledOnce());
        expect(retired.close).toHaveBeenCalled();
        expect(setupFrames(sendSpy)).toEqual([]);
        // The join is not settled by the retired attachment.
        expect(await init()).toBe("never settled");

        // The successor owns the join and completes it on its own socket.
        const successor = await codecHandshake("Reconnect", retired);
        expect(successor).not.toBe(retired);
        expect(setupFrames(sendSpy)).toEqual(["Reconnect"]);
        deliverCodec(successor, "Full", codecGameStarted(5, 5));
        await vi.waitFor(() => expect(second.decode).toHaveBeenCalledOnce());
        await expect(init()).resolves.toEqual({ resolvedWith: undefined });
        expect((await adapter.getSnapshot()).state.turn_number).toBe(5);
        expect(second.close).not.toHaveBeenCalled();
        expect(first.close).toHaveBeenCalledOnce();
      } finally {
        adapter.dispose();
      }
    });
  });
});

describe("WebSocketAdapter — retained close and early credential frames", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  const contradictoryConfirmation = JSON.stringify({
    type: "ServerHello",
    data: { server_version: "0.0.0-test", selected_state_codec: "SomeOtherCodec" },
  });
  const setupTypes = (raw: MockWebSocket) => raw.send.mock.calls
    .map(([text]) => (JSON.parse(text as string) as { type?: string }).type)
    .filter((type) => type === "JoinGameWithPassword" || type === "Reconnect");
  const privateFailure = (adapter: WebSocketAdapter) => (adapter as unknown as {
    stateCodec: { current: { failure: unknown } | null };
  }).stateCodec.current?.failure;

  /** The delivery of `frame` from inside the ClientHello send: before adoption. */
  function deliverFromClientHello(raw: MockWebSocket, frame: string): void {
    raw.send.mockImplementation((text: unknown) => {
      if (typeof text === "string" && text.includes('"type":"ClientHello"')) {
        raw.dispatchSynthetic("message", frame);
      }
    });
  }

  it("schedules the next reconnect attempt without the obsolete send error when a retained close lands during an established reconnect attachment", async () => {
    const first = scriptedCodec();
    const second = scriptedCodec();
    const sessions = [first.session, second.session];
    const { adapter, raw, events } = await establishCodecAdapter(() => Promise.resolve(sessions.shift() ?? null));
    const sendSpy = vi.spyOn(adapter as unknown as { send(message: unknown): boolean }, "send");
    try {
      raw.dispatchSynthetic("close");
      await vi.advanceTimersByTimeAsync(1000);
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(raw));
      const attempt = latestSocket();
      deliverFromClientHello(attempt, contradictoryConfirmation);
      attempt.dispatchSynthetic("message", CODEC_SESSION_HELLO);

      await vi.waitFor(() => expect(second.close).toHaveBeenCalledOnce());
      // Reach guard: the attempt offered the codec, and the retained close was
      // consumed as an established-game loss, which schedules the next attempt.
      expect(offeredCodec(attempt)).toBe(true);
      await vi.waitFor(() => expect(
        events.filter((event) => (event as { type: string }).type === "reconnecting"),
      ).toHaveLength(2));
      expect(sendSpy.mock.calls.filter(([message]) => (message as { type?: string }).type === "Reconnect"))
        .toHaveLength(0);
      expect(events).not.toContainEqual(expect.objectContaining({ type: "error" }));

      // The next attempt is dialled and, the codec having failed, is snapshot-only.
      await vi.advanceTimersByTimeAsync(2000);
      const next = await snapshotOnlyHandshake("Reconnect", attempt);
      expect(offeredCodec(next)).toBe(false);
      expect(setupTypes(next)).toEqual(["Reconnect"]);
    } finally {
      adapter.dispose();
    }
  });

  it("rejects the join once, records a credential-free first notice and dials no fallback when a credential-bearing Full lands before adoption", async () => {
    const early = scriptedCodec();
    const provider = vi.fn(() => Promise.resolve(early.session));
    MockWebSocket.last = null;
    const adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined, { stateCodecSessionProvider: provider },
    );
    const sendSpy = vi.spyOn(adapter as unknown as { send(message: unknown): boolean }, "send");
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    try {
      const joined = (async () => {
        try { await adapter.initialize(); return "resolved"; } catch (error) { return error; }
      })();
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
      const raw = latestSocket();
      const adoptedAtFailure: boolean[] = [];
      raw.send.mockImplementation((text: unknown) => {
        if (typeof text !== "string" || !text.includes('"type":"ClientHello"')) return;
        adoptedAtFailure.push((adapter as unknown as { ws: unknown }).ws !== null);
        raw.dispatchSynthetic("message", fullFrame(JSON.stringify(codecGameStarted(1, 1))));
      });
      raw.dispatchSynthetic("message", CODEC_SESSION_HELLO);

      await expect(joined).resolves.toMatchObject({
        code: "WS_CLOSED", message: "Connection closed before game started", recoverable: true,
      });
      // Reach guard: the codec was offered and the failure preceded adoption.
      expect(offeredCodec(raw)).toBe(true);
      expect(adoptedAtFailure).toEqual([false]);
      expect(early.close).toHaveBeenCalledOnce();
      expect(early.decode).not.toHaveBeenCalled();
      expect(privateFailure(adapter)).toEqual({
        kind: "failed", reason: "confirmationFailure", joinCredentials: null,
      });
      expect(policyOf(adapter)).toEqual({ kind: "snapshotOnly", reason: "confirmationFailure" });

      // No fallback is dialled, and no setup frame ever follows the rejection.
      await vi.advanceTimersByTimeAsync(130_000);
      expect(MockWebSocket.last).toBe(raw);
      expect(provider).toHaveBeenCalledOnce();
      expect(sendSpy).not.toHaveBeenCalled();
      expect(events).not.toContainEqual(expect.objectContaining({ type: "reconnecting" }));
      expect(events).not.toContainEqual(expect.objectContaining({ type: "error" }));
    } finally {
      adapter.dispose();
    }
  });

  it("records the credentials of a Full that precedes the confirmation after adoption on the first notice, and completes the same join on one snapshot-only fallback", async () => {
    const codec = scriptedCodec();
    const provider = vi.fn(() => Promise.resolve(codec.session));
    MockWebSocket.last = null;
    const adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined, { stateCodecSessionProvider: provider },
    );
    const sendSpy = vi.spyOn(adapter as unknown as { send(message: unknown): boolean }, "send");
    const outcome: { value: unknown } = { value: "never settled" };
    try {
      adapter.initialize().then(
        (value) => { outcome.value = { resolvedWith: value }; },
        (error) => { outcome.value = error; },
      );
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
      const raw = latestSocket();
      raw.dispatchSynthetic("message", CODEC_SESSION_HELLO);
      await vi.waitFor(() => expect(raw.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"JoinGameWithPassword"'),
      ));
      // Reach guard: the socket is adopted and the join setup was sent.
      expect((adapter as unknown as { ws: unknown }).ws).not.toBeNull();
      raw.dispatchSynthetic("message", fullFrame(JSON.stringify(codecGameStarted(1, 1))));

      await vi.waitFor(() => expect(codec.close).toHaveBeenCalledOnce());
      expect(codec.decode).not.toHaveBeenCalled();
      expect(privateFailure(adapter)).toEqual({
        kind: "failed",
        reason: "confirmationFailure",
        joinCredentials: {
          playerToken: "codec-token",
          yourPlayer: 0,
          fullKey: REPLY_OWNERSHIP_FULL_KEY,
        },
      });
      // The credentials are consumed: the same join continues on one
      // snapshot-only attachment and the original initialization stays pending.
      const fallback = await snapshotOnlyHandshake("Reconnect", raw);
      expect(offeredCodec(fallback)).toBe(false);
      expect(outcome.value).toBe("never settled");
      fallback.dispatchSynthetic("message", JSON.stringify(codecGameStarted(1, 1, { player_token: undefined })));
      await vi.waitFor(() => expect(outcome.value).toEqual({ resolvedWith: undefined }));
      expect(provider).toHaveBeenCalledOnce();
      const setups = sendSpy.mock.calls.map(([message]) => (message as { type?: string }).type)
        .filter((type) => type === "JoinGameWithPassword" || type === "Reconnect");
      expect(setups).toEqual(["JoinGameWithPassword", "Reconnect"]);
    } finally {
      adapter.dispose();
    }
  });
});

// ── Initialization-owned fallback after a failed initial GameStarted Full ──
//
// A remote join whose first GameStarted `Full` fails in the codec carries the
// seat's credentials on the failure notice. The adapter finishes the SAME
// initialization on one new snapshot-only attachment that sends `Reconnect`
// with those credentials: no second `JoinGameWithPassword`, no second
// initialization, no failed state rendered, and the init promise is the single
// failure surface (reconnect lifecycle events and episode errors are
// suppressed). Everything runs against the real initialize/openPhaseSocket/
// handlers; only the decoder is scripted.

describe("WebSocketAdapter — initialization-owned fallback (salvaged and held credentials)", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  const FAILURE_MESSAGE = "Could not finish joining after state delivery failed; reconnect failed.";
  const DISPOSED_MESSAGE = "Adapter disposed before initialization completed";
  /** The reconnect-lifecycle and error events an episode must never announce. */
  const EPISODE_EVENTS = ["reconnecting", "reconnected", "reconnectFailed", "error"];
  const REMOTE_BUDGET_MS = 125_000;

  type Seen = { type: string } & Record<string, unknown>;
  const typesOf = (events: Seen[]) => events.map((event) => event.type);
  const episodeEventsOf = (events: Seen[]) => events.filter((event) => EPISODE_EVENTS.includes(event.type));
  const fallbackOf = (adapter: WebSocketAdapter) => (adapter as unknown as {
    initialFallback: { kind: string };
  }).initialFallback.kind;
  const latchesOf = (adapter: WebSocketAdapter) => {
    const state = adapter as unknown as {
      socketOpening: boolean; reconnectTimer: unknown; reconnectInFlight: boolean;
      snapshot: unknown; cachedStateRevision: unknown; withheldState: unknown;
      withheldBootstrapFailed: boolean; stateCodecPreparation: unknown;
    };
    return {
      socketOpening: state.socketOpening, reconnectTimer: state.reconnectTimer,
      reconnectInFlight: state.reconnectInFlight, snapshot: state.snapshot,
      cachedStateRevision: state.cachedStateRevision, withheldState: state.withheldState,
      withheldBootstrapFailed: state.withheldBootstrapFailed,
      stateCodecPreparation: state.stateCodecPreparation,
    };
  };
  const CLEAN_LATCHES = {
    socketOpening: false, reconnectTimer: null, reconnectInFlight: false, snapshot: null,
    cachedStateRevision: null, withheldState: null, withheldBootstrapFailed: false,
    stateCodecPreparation: null,
  };

  /** A remote join: negotiated codec, original initialization tracked without timers. */
  async function startJoin(mode: "join" | "spectate" = "join") {
    MockWebSocket.last = null;
    const codec = scriptedCodec();
    const provider = vi.fn(() => Promise.resolve(codec.session));
    const adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws", mode, { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined, { stateCodecSessionProvider: provider },
    );
    const events: Seen[] = [];
    adapter.onEvent((event) => events.push(event as Seen));
    const outcome: { value: unknown } = { value: "never settled" };
    adapter.initialize().then(
      (value) => { outcome.value = { resolvedWith: value }; },
      (error) => { outcome.value = error; },
    );
    const raw = await codecHandshake(mode === "join" ? "JoinGameWithPassword" : "SpectatorJoin", null);
    return { adapter, raw, codec, provider, events, outcome };
  }
  type Join = Awaited<ReturnType<typeof startJoin>>;

  type FirstFullFailure = "decodeFailure" | "bootstrapRejection";
  /** Fails the join's first GameStarted Full in the decoder and waits for its lease release. */
  async function failFirstFull(
    join: Join, kind: FirstFullFailure, envelope: unknown = codecGameStarted(1, 1), frame: "Full" | "Delta" = "Full",
  ) {
    if (kind === "decodeFailure") join.codec.decode.mockRejectedValueOnce(new Error("decoder trapped"));
    else join.codec.rejectNext(1);
    deliverCodec(join.raw, frame, envelope);
    await vi.waitFor(() => expect(join.codec.close).toHaveBeenCalledOnce());
    expect(policyOf(join.adapter)).toEqual({ kind: "snapshotOnly", reason: kind });
  }

  /** The reply a reconnect receives: the token is never resent. */
  const plainStarted = (turn = 2, overrides: Record<string, unknown> = {}) =>
    JSON.stringify(codecGameStarted(turn, turn, { player_token: undefined, ...overrides }));

  const sentFrames = (raw: MockWebSocket, type: string) => raw.send.mock.calls
    .map(([text]) => JSON.parse(text as string) as { type?: string; data?: unknown })
    .filter((frame) => frame.type === type);

  const settledAs = async (join: Join) => {
    await vi.waitFor(() => expect(join.outcome.value).not.toBe("never settled"));
    return join.outcome.value;
  };

  /** Waits for the next socket after `previous` and delivers the codec-offering hello, optionally prepared first. */
  async function nextHandshake(previous: MockWebSocket, prepare?: (socket: MockWebSocket) => void) {
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(previous));
    const socket = latestSocket();
    prepare?.(socket);
    socket.dispatchSynthetic("message", CODEC_SESSION_HELLO);
    return socket;
  }

  const GAME_ENDED_MESSAGE = "The game ended before the join could finish after state delivery failed.";
  const TERMINAL_DELIVERY: FullTerminalDelivery = {
    key: REPLY_OWNERSHIP_FULL_KEY,
    terminalRevision: 9,
    deliveryId: "episode-delivery",
    credential: "episode-credential",
    display: { winner: 0, reason: "Finished" },
    finalView: { players: [], objects: {}, waiting_for: { type: "GameOver", data: { winner: 0 } } } as unknown as GameState,
  };
  /**
   * The inbound messages the episode router fails the join on beyond a refusal, with the single
   * failure each yields, and the event the same message publishes through its ordinary handler.
   */
  const ROUTED_TERMINALS = [
    [
      "RequestRejected",
      { type: "RequestRejected", data: { reason: "Too many requests" } },
      { code: "REQUEST_FAILED", message: "Too many requests", recoverable: true },
      "requestRejected",
    ],
    [
      "TerminalResult",
      { type: "TerminalResult", data: { delivery: TERMINAL_DELIVERY } },
      { code: "WS_CLOSED", message: GAME_ENDED_MESSAGE, recoverable: false },
      "terminalDelivery",
    ],
    [
      "GameOver",
      { type: "GameOver", data: { winner: 0, reason: "Finished" } },
      { code: "WS_CLOSED", message: GAME_ENDED_MESSAGE, recoverable: false },
      "gameOver",
    ],
  ] as const;
  /** A native AI driver fault: the router lists it as neutral, a fault of the game's driver and not of the join. */
  const AI_DRIVER_FAULT = { type: "AiDriverFault", data: { fault: { id: 1, after_state_revision: 2, cause: null } } };
  const AI_DRIVER_FAULT_MESSAGE = "Native AI driver stopped; this game can no longer advance.";

  describe("admission and plain completion", () => {
    it.each<FirstFullFailure>(["decodeFailure", "bootstrapRejection"])("finishes the same join on one snapshot-only Reconnect carrying the failed Full's credentials (%s)", async (kind) => {
      const join = await startJoin();
      const { adapter, raw, provider, events, outcome } = join;
      try {
        await failFirstFull(join, kind);
        if (kind === "bootstrapRejection") {
          // One best-effort resync, then an immediate close of the failed attachment.
          expect(resyncRequests(raw)).toBe(1);
          expect(raw.close).toHaveBeenCalled();
        } else {
          expect(resyncRequests(raw)).toBe(0);
        }

        const fallback = await snapshotOnlyHandshake("Reconnect", raw);
        // Reach guards: the codec was offered to the first attachment only, the
        // sticky provider was not consulted again, and the original join waits.
        expect(offeredCodec(raw)).toBe(true);
        expect(offeredCodec(fallback)).toBe(false);
        expect(provider).toHaveBeenCalledOnce();
        expect(fallbackOf(adapter)).toBe("reconnecting");
        expect(outcome.value).toBe("never settled");
        expect(sentFrames(fallback, "Reconnect")).toEqual([{
          type: "Reconnect",
          data: { game_code: "GAME01", player_token: "codec-token", full_key: REPLY_OWNERSHIP_FULL_KEY },
        }]);
        // Nothing is published until the plain reply authenticates the credentials.
        expect(typesOf(events)).toEqual(["serverHello", "serverHello"]);
        await expect(adapter.getSnapshot()).rejects.toBeDefined();

        fallback.dispatchSynthetic("message", plainStarted(2));
        await vi.waitFor(() => expect(outcome.value).toEqual({ resolvedWith: undefined }));
        expect(typesOf(events)).toEqual([
          "serverHello", "serverHello", "bootstrapDelivered", "sessionChanged", "playerIdentity",
        ]);
        expect(events[3]).toMatchObject({
          session: { gameCode: "GAME01", playerToken: "codec-token", fullKey: REPLY_OWNERSHIP_FULL_KEY },
        });
        expect(episodeEventsOf(events)).toEqual([]);
        expect(fallbackOf(adapter)).toBe("finished");
        expect((await adapter.getSnapshot()).state.turn_number).toBe(2);
        // Exactly one join setup went out over the whole initialization.
        expect(sentFrames(raw, "JoinGameWithPassword")).toHaveLength(1);
        expect(sentFrames(fallback, "JoinGameWithPassword")).toHaveLength(0);

        // The recovered seat is usable: a correlated action completes on the new socket.
        const completion = adapter.submitAction({ type: "PassPriority" } as GameAction, 0);
        await vi.waitFor(() => expect(submissionTypes(fallback)).toHaveLength(1));
        fallback.dispatchSynthetic("message", JSON.stringify(
          codecStateUpdate(3, 3, { command_id: lastSubmissionCommandId(fallback) }),
        ));
        await expect(completion).resolves.toMatchObject({ events: [] });
      } finally {
        adapter.dispose();
      }
    });

    it("still carries the credentials and reason when the best-effort resync cannot be sent", async () => {
      const join = await startJoin();
      const { adapter, raw, events, outcome } = join;
      try {
        raw.send.mockImplementation((text: unknown) => {
          if (text === RESYNC_REQUEST) throw new Error("socket is gone");
        });
        await failFirstFull(join, "bootstrapRejection");
        const fallback = await snapshotOnlyHandshake("Reconnect", raw);
        expect(sentFrames(fallback, "Reconnect")).toHaveLength(1);
        fallback.dispatchSynthetic("message", plainStarted(2));
        await vi.waitFor(() => expect(outcome.value).toEqual({ resolvedWith: undefined }));
        expect(episodeEventsOf(events)).toEqual([]);
      } finally {
        adapter.dispose();
      }
    });

    it("keeps a late frame of the failed attachment from bootstrapping the join", async () => {
      const join = await startJoin();
      const { adapter, raw, codec, outcome } = join;
      try {
        await failFirstFull(join, "bootstrapRejection");
        // Every later frame WOULD decode, so a fenced frame is the wrapper's doing.
        deliverCodec(raw, "Full", codecGameStarted(9, 9));
        deliverCodec(raw, "Full", codecStateUpdate(9, 9));
        raw.dispatchSynthetic("message", JSON.stringify({ type: "Pong", data: { timestamp: Date.now() } }));
        await vi.advanceTimersByTimeAsync(0);
        expect(codec.decode).toHaveBeenCalledOnce();
        expect(outcome.value).toBe("never settled");
        await expect(adapter.getSnapshot()).rejects.toBeDefined();
      } finally {
        adapter.dispose();
      }
    });

    it("joins from a healthy decode on the first socket with no fallback", async () => {
      const join = await startJoin();
      const { adapter, raw, provider, events, outcome } = join;
      try {
        deliverCodec(raw, "Full", codecGameStarted(1, 1));
        await vi.waitFor(() => expect(outcome.value).toEqual({ resolvedWith: undefined }));
        expect(fallbackOf(adapter)).toBe("unused");
        expect(MockWebSocket.last).toBe(raw);
        expect(provider).toHaveBeenCalledOnce();
        expect(typesOf(events)).toEqual(["serverHello", "bootstrapDelivered", "sessionChanged", "playerIdentity"]);
        expect(sentFrames(raw, "Reconnect")).toHaveLength(0);
      } finally {
        adapter.dispose();
      }
    });
  });

  describe("identity preflight of the fallback's reply", () => {
    const fallbackWithReply = async (reply: (fallback: MockWebSocket) => string, kind: FirstFullFailure = "decodeFailure") => {
      const join = await startJoin();
      await failFirstFull(join, kind);
      const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
      fallback.dispatchSynthetic("message", reply(fallback));
      return { join, fallback };
    };

    it.each([
      ["a missing Full key", { full_key: undefined }, "Server omitted a valid Full session identity"],
      ["another game", { full_key: { game_code: "OTHER1", generation: 7 } }, "Server changed the Full session identity"],
      ["another generation", { full_key: { game_code: "GAME01", generation: 8 } }, "Server changed the Full session identity"],
      ["another seat", { your_player: 1 }, "Reconnect attached player 1, expected 0"],
      ["a missing seat", { your_player: undefined }, "Reconnect attached player undefined, expected 0"],
      ["another token", { player_token: "other-token" }, "Reconnect changed the player token"],
    ])("rejects the join before any bootstrap, cache or session when the reply carries %s", async (_name, overrides, message) => {
      const { join, fallback } = await fallbackWithReply(() => plainStarted(2, overrides));
      const { adapter, events } = join;
      try {
        await expect(settledAs(join)).resolves.toMatchObject({ code: "WS_ERROR", message, recoverable: false });
        // Reach guard: the reply reached the real handler on the fallback socket.
        expect(sentFrames(fallback, "Reconnect")).toHaveLength(1);
        expect(typesOf(events)).toEqual(["serverHello", "serverHello"]);
        await expect(adapter.getSnapshot()).rejects.toBeDefined();
        expect(adapter.nativeSession).toBeNull();
        expect(fallback.close).toHaveBeenCalled();
        expect(fallbackOf(adapter)).toBe("finished");
        expect(latchesOf(adapter)).toEqual(CLEAN_LATCHES);
      } finally {
        adapter.dispose();
      }
    });

    it("reports the Full key before the seat or token when a reply is wrong on all three (the shared comparator's order)", async () => {
      const { join } = await fallbackWithReply(() => plainStarted(2, {
        full_key: { game_code: "GAME01", generation: 8 }, your_player: 1, player_token: "other-token",
      }));
      try {
        await expect(settledAs(join)).resolves.toMatchObject({
          code: "WS_ERROR", message: "Server changed the Full session identity", recoverable: false,
        });
      } finally {
        join.adapter.dispose();
      }
    });

    it.each([
      ["omits the token", { player_token: undefined }],
      ["repeats the retained token", { player_token: "codec-token" }],
    ])("accepts a reply that %s and publishes the retained session once", async (_name, overrides) => {
      const { join } = await fallbackWithReply(() => plainStarted(2, overrides));
      const { adapter, events } = join;
      try {
        await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
        expect(typesOf(events).filter((type) => type === "sessionChanged")).toHaveLength(1);
        expect(adapter.nativeSession).toEqual({
          gameCode: "GAME01", playerId: 0, playerToken: "codec-token", fullKey: REPLY_OWNERSHIP_FULL_KEY,
        });
      } finally {
        adapter.dispose();
      }
    });

    it.each([
      ["GameCreated", { type: "GameCreated", data: { game_code: "GAME01", player_token: "codec-token", full_key: REPLY_OWNERSHIP_FULL_KEY } }],
      ["SessionAttached", { type: "SessionAttached", data: { game_code: "GAME01", player_id: 0, player_token: "codec-token", full_key: REPLY_OWNERSHIP_FULL_KEY } }],
    ])("rejects a pregame %s reply as an impossible premise instead of treating it as a host", async (_name, frame) => {
      const { join, fallback } = await fallbackWithReply(() => JSON.stringify(frame));
      const { adapter, events } = join;
      try {
        await expect(settledAs(join)).resolves.toMatchObject({ code: "WS_ERROR", recoverable: false });
        expect(sentFrames(fallback, "Reconnect")).toHaveLength(1);
        expect(typesOf(events)).toEqual(["serverHello", "serverHello"]);
        expect(adapter.nativeSession).toBeNull();
      } finally {
        adapter.dispose();
      }
    });

    describe("a salvaged Full key that fails validation", () => {
      const withHeldFullKey = async (generation: number) => {
        const join = await startJoin();
        // An identity this adapter already holds makes the salvaged key a change of identity
        // when its generation differs. Seeded directly: no production route holds a Full key
        // without the rest of a session before the salvaged admission.
        (join.adapter as unknown as { fullSessionKey: unknown }).fullSessionKey = {
          game_code: "GAME01", generation,
        };
        await failFirstFull(join, "decodeFailure");
        return join;
      };

      it("writes no provisional credential and starts no episode", async () => {
        const join = await withHeldFullKey(3);
        const { adapter, raw, events } = join;
        try {
          await expect(settledAs(join)).resolves.toMatchObject({
            code: "WS_ERROR", message: "Server changed the Full session identity", recoverable: false,
          });
          // Reach guard: the failed Full carried intact credentials of this join's game, so
          // only the key check stood between them and the adapter.
          expect(policyOf(adapter)).toEqual({ kind: "snapshotOnly", reason: "decodeFailure" });
          expect(fallbackOf(adapter)).toBe("unused");
          await vi.advanceTimersByTimeAsync(10_000);
          expect(MockWebSocket.last).toBe(raw);
          expect(typesOf(events)).toEqual(["serverHello", "error"]);
          expect(adapter.gameCode).toBeNull();
          expect(adapter.playerId).toBeNull();
          expect((adapter as unknown as { playerToken: unknown }).playerToken).toBeNull();
          expect(adapter.nativeSession).toBeNull();
          // The key already held is not replaced by the rejected one.
          expect((adapter as unknown as { fullSessionKey: unknown }).fullSessionKey)
            .toEqual({ game_code: "GAME01", generation: 3 });
        } finally {
          adapter.dispose();
        }
      });

      it("admits the same credentials when the held Full identity agrees", async () => {
        const join = await withHeldFullKey(7);
        const { adapter, events } = join;
        try {
          const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
          expect(fallbackOf(adapter)).toBe("reconnecting");
          expect(adapter.gameCode).toBe("GAME01");
          expect(adapter.playerId).toBe(0);
          expect((adapter as unknown as { playerToken: unknown }).playerToken).toBe("codec-token");
          expect(sentFrames(fallback, "Reconnect")).toHaveLength(1);
          expect(episodeEventsOf(events)).toEqual([]);
        } finally {
          adapter.dispose();
        }
      });
    });

    it("still accepts a legacy plain GameStarted without a Full key outside the fallback", async () => {
      MockWebSocket.last = null;
      const adapter = new WebSocketAdapter(
        "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
        undefined, undefined, undefined, { stateCodecSessionProvider: null },
      );
      try {
        const init = adapter.initialize();
        const raw = await snapshotOnlyHandshake("JoinGameWithPassword", null);
        raw.dispatchSynthetic("message", JSON.stringify({
          type: "GameStarted", data: { state: createMockState(), your_player: 0 },
        }));
        await init;
        expect(fallbackOf(adapter)).toBe("unused");
      } finally {
        adapter.dispose();
      }
    });
  });

  describe("deadline and retry budget", () => {
    it("fails the join once at the episode's deadline when the replacement socket never answers", async () => {
      const join = await startJoin();
      const { adapter, events } = join;
      try {
        const failedAt = Date.now();
        await failFirstFull(join, "decodeFailure");
        const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
        // A transport that delivers its close synchronously must find the
        // episode already detached: the failure it reports stays the episode's.
        fallback.close.mockImplementation(() => fallback.dispatchSynthetic("close"));
        // The bound runs from the failure: just inside it the join still waits.
        await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS - 1000 - (Date.now() - failedAt));
        // Reach guard: the socket is open, the Reconnect went out, and the join still waits.
        expect(sentFrames(fallback, "Reconnect")).toHaveLength(1);
        expect(join.outcome.value).toBe("never settled");
        await vi.advanceTimersByTimeAsync(2000);
        await expect(settledAs(join)).resolves.toMatchObject({
          code: "WS_CLOSED", message: FAILURE_MESSAGE, recoverable: true,
        });
        expect(fallback.close).toHaveBeenCalled();
        expect(episodeEventsOf(events)).toEqual([]);
        expect(vi.getTimerCount()).toBe(0);
      } finally {
        adapter.dispose();
      }
    });

    it.each(["a natural close", "a handshake that closes before the hello"] as const)("retries after %s inside the same budget, without a second join or episode", async (loss) => {
      const join = await startJoin();
      const { adapter, events, provider } = join;
      try {
        await failFirstFull(join, "bootstrapRejection");
        const first = loss === "a natural close"
          ? await snapshotOnlyHandshake("Reconnect", join.raw)
          : await vi.waitFor(() => { expect(MockWebSocket.last).not.toBe(join.raw); }).then(latestSocket);
        first.dispatchSynthetic("close");
        // The loss does not reject the initialization (retry precedes the generic rejection).
        await vi.advanceTimersByTimeAsync(2000);
        expect(join.outcome.value).toBe("never settled");
        expect(fallbackOf(adapter)).toBe("reconnecting");

        const second = await snapshotOnlyHandshake("Reconnect", first);
        expect(second).not.toBe(first);
        expect(offeredCodec(second)).toBe(false);
        second.dispatchSynthetic("message", plainStarted(2));
        await vi.waitFor(() => expect(join.outcome.value).toEqual({ resolvedWith: undefined }));
        expect(provider).toHaveBeenCalledOnce();
        expect(sentFrames(join.raw, "JoinGameWithPassword")).toHaveLength(1);
        expect(sentFrames(second, "Reconnect")).toHaveLength(1);
        expect(episodeEventsOf(events)).toEqual([]);
      } finally {
        adapter.dispose();
      }
    });

    it("exhausts the budget through repeated lost attempts with the single failure and no re-admission", async () => {
      const join = await startJoin();
      const { adapter, events } = join;
      try {
        await failFirstFull(join, "decodeFailure");
        const dialled = new Set<MockWebSocket>();
        let last: MockWebSocket = join.raw;
        for (let step = 0; step < 100 && join.outcome.value === "never settled"; step += 1) {
          await vi.advanceTimersByTimeAsync(2000);
          const socket = latestSocket();
          if (socket !== last) {
            last = socket;
            dialled.add(socket);
            socket.dispatchSynthetic("close");
          }
        }
        await expect(settledAs(join)).resolves.toMatchObject({
          code: "WS_CLOSED", message: FAILURE_MESSAGE, recoverable: true,
        });
        // Reach guard: several attempts were really dialled before the budget ended.
        expect(dialled.size).toBeGreaterThan(2);
        expect(episodeEventsOf(events)).toEqual([]);
        expect(fallbackOf(adapter)).toBe("finished");
        expect(vi.getTimerCount()).toBe(0);
        expect([...dialled].flatMap((socket) => sentFrames(socket, "JoinGameWithPassword"))).toEqual([]);
      } finally {
        adapter.dispose();
      }
    });

    it("succeeds on a reply just inside the bound and leaves no live timer behind", async () => {
      const join = await startJoin();
      const { adapter, events } = join;
      try {
        const failedAt = Date.now();
        await failFirstFull(join, "decodeFailure");
        const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
        await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS - 1000 - (Date.now() - failedAt));
        expect(join.outcome.value).toBe("never settled");
        fallback.dispatchSynthetic("message", plainStarted(2));
        await vi.waitFor(() => expect(join.outcome.value).toEqual({ resolvedWith: undefined }));
        await vi.advanceTimersByTimeAsync(10_000);
        expect(join.outcome.value).toEqual({ resolvedWith: undefined });
        expect(episodeEventsOf(events)).toEqual([]);
        expect((await adapter.getSnapshot()).state.turn_number).toBe(2);
      } finally {
        adapter.dispose();
      }
    });
  });

  describe("transport and setup failures", () => {
    it("fails the join once, without episode events, when the replacement socket errors", async () => {
      const join = await startJoin();
      const { adapter, events } = join;
      try {
        await failFirstFull(join, "decodeFailure");
        const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
        expect(join.outcome.value).toBe("never settled");
        fallback.onerror?.();
        await expect(settledAs(join)).resolves.toMatchObject({
          code: "WS_CLOSED", message: FAILURE_MESSAGE, recoverable: true,
        });
        expect(episodeEventsOf(events)).toEqual([]);
        expect(latchesOf(adapter)).toEqual(CLEAN_LATCHES);
      } finally {
        adapter.dispose();
      }
    });

    it("fails the join once, without an error event, when the replacement's setup send fails", async () => {
      const join = await startJoin();
      const { adapter, events } = join;
      try {
        await failFirstFull(join, "decodeFailure");
        const fallback = await nextHandshake(join.raw, (socket) => {
          socket.send.mockImplementation((text: unknown) => {
            if (typeof text === "string" && text.includes('"type":"Reconnect"')) throw new Error("socket is gone");
          });
        });
        await vi.waitFor(() => expect(fallback.send).toHaveBeenCalledWith(expect.stringContaining('"type":"Reconnect"')));
        await expect(settledAs(join)).resolves.toMatchObject({
          code: "WS_CLOSED", message: FAILURE_MESSAGE, recoverable: true,
        });
        expect(fallback.close).toHaveBeenCalled();
        expect(episodeEventsOf(events)).toEqual([]);
        expect(latchesOf(adapter)).toEqual(CLEAN_LATCHES);
      } finally {
        adapter.dispose();
      }
    });
  });

  describe("refusal, handshake, password and withheld replies", () => {
    const replyWith = async (frame: unknown, kind: FirstFullFailure = "decodeFailure") => {
      const join = await startJoin();
      await failFirstFull(join, kind);
      const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
      fallback.dispatchSynthetic("message", JSON.stringify(frame));
      return { join, fallback };
    };

    it.each([
      ["a refusal", { message: "Game not found" }, "ACTION_REJECTED", true],
      ["a stale refusal", { message: "Engine error: ReorderHand: expected 3 ids, got 2" }, "STALE_ACTION", false],
      ["a rejected deck", { message: "Deck is not legal", code: "deck_rejected" }, "DECK_REJECTED", false],
    ])("keeps the existing initialization error for %s and announces nothing else", async (_name, data, code, recoverable) => {
      const { join } = await replyWith({ type: "Error", data });
      try {
        await expect(settledAs(join)).resolves.toMatchObject({ code, message: data.message, recoverable });
        expect(typesOf(join.events)).toEqual(["serverHello", "serverHello"]);
        expect(latchesOf(join.adapter)).toEqual(CLEAN_LATCHES);
      } finally {
        join.adapter.dispose();
      }
    });

    it("maps a protocol mismatch to the handshake rejection, then adds the incompatible hello", async () => {
      const join = await startJoin();
      try {
        await failFirstFull(join, "decodeFailure");
        const initPendingAtHello: boolean[] = [];
        join.adapter.onEvent((event) => {
          if (event.type === "serverHello" && !event.compatible) {
            initPendingAtHello.push((join.adapter as unknown as { initReject: unknown }).initReject !== null);
          }
        });
        await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(join.raw));
        latestSocket().dispatchSynthetic("message", JSON.stringify({
          type: "ServerHello",
          data: {
            server_version: "9.9.9", build_commit: "other", protocol_version: PROTOCOL_VERSION + 1000,
            mode: "Full",
          },
        }));
        await expect(settledAs(join)).resolves.toMatchObject({ code: "WS_ERROR", recoverable: false });
        // The initialization was already rejected when the incompatible hello was announced.
        expect(initPendingAtHello).toEqual([false]);
        expect(join.events.filter((event) => event.type === "serverHello" && event.compatible === false)).toHaveLength(1);
        expect(episodeEventsOf(join.events)).toEqual([]);
        expect(latchesOf(join.adapter)).toEqual(CLEAN_LATCHES);
      } finally {
        join.adapter.dispose();
      }
    });

    it("rejects with PASSWORD_REQUIRED and announces neither the password prompt nor a reconnect failure", async () => {
      const { join } = await replyWith({ type: "PasswordRequired", data: { game_code: "GAME01" } });
      try {
        await expect(settledAs(join)).resolves.toMatchObject({
          code: "PASSWORD_REQUIRED", message: "Room requires a password", recoverable: true,
        });
        expect(typesOf(join.events)).toEqual(["serverHello", "serverHello"]);
      } finally {
        join.adapter.dispose();
      }
    });

    it("fails a withheld GameStarted bootstrap with the server's reason and no synthetic bootstrap", async () => {
      const { join } = await replyWith({
        type: "StateWithheld",
        data: {
          full_key: REPLY_OWNERSHIP_FULL_KEY, state_revision: 3,
          withheld: { type: "GameStarted", data: { your_player: 0 } },
          reason: "The game state is too large to send.",
        },
      });
      try {
        await expect(settledAs(join)).resolves.toMatchObject({
          code: "REQUEST_FAILED", message: "The game state is too large to send.", recoverable: true,
        });
        expect(typesOf(join.events)).toEqual(["serverHello", "serverHello"]);
        expect(latchesOf(join.adapter)).toEqual(CLEAN_LATCHES);
      } finally {
        join.adapter.dispose();
      }
    });

    it("leaves a withheld StateUpdate pending until the real GameStarted completes the join", async () => {
      const { join, fallback } = await replyWith({
        type: "StateWithheld",
        data: {
          full_key: REPLY_OWNERSHIP_FULL_KEY, state_revision: 3,
          withheld: { type: "StateUpdate" }, reason: "The game state is too large to send.",
        },
      });
      try {
        await vi.waitFor(() => expect(typesOf(join.events)).toContain("stateDelivery"));
        expect(join.outcome.value).toBe("never settled");
        expect(fallbackOf(join.adapter)).toBe("reconnecting");
        fallback.dispatchSynthetic("message", plainStarted(4));
        await vi.waitFor(() => expect(join.outcome.value).toEqual({ resolvedWith: undefined }));
        expect(episodeEventsOf(join.events)).toEqual([]);
      } finally {
        join.adapter.dispose();
      }
    });
  });

  describe("the episode router (salvaged source)", () => {
    beforeEach(() => { terminalStore.commit.mockClear(); });

    it.each(ROUTED_TERMINALS)("fails the join once on %s, publishes nothing and lets a later explicit reconnect start fresh", async (_name, frame, failure) => {
      const join = await startJoin();
      const { adapter, events } = join;
      try {
        await failFirstFull(join, "decodeFailure");
        const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
        // Reach guards: the episode is active on its replacement socket with provisional credentials.
        expect(fallbackOf(adapter)).toBe("reconnecting");
        expect(join.outcome.value).toBe("never settled");
        expect(adapter.nativeSession).not.toBeNull();

        fallback.dispatchSynthetic("message", JSON.stringify(frame));
        await expect(settledAs(join)).resolves.toMatchObject(failure);
        // Let any asynchronous terminal handling that must not have started run.
        await vi.advanceTimersByTimeAsync(0);
        expect(typesOf(events)).toEqual(["serverHello", "serverHello"]);
        expect(fallbackOf(adapter)).toBe("finished");
        expect(latchesOf(adapter)).toEqual(CLEAN_LATCHES);
        expect(adapter.nativeSession).toBeNull();
        expect(adapter.gameCode).toBeNull();
        expect(terminalStore.commit).not.toHaveBeenCalled();
        expect(vi.getTimerCount()).toBe(0);

        expect(adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
        const fresh = await snapshotOnlyHandshake("Reconnect", fallback);
        expect(sentFrames(fresh, "Reconnect")).toHaveLength(1);
        expect(offeredCodec(fresh)).toBe(false);
      } finally {
        adapter.dispose();
      }
    });

    it.each(ROUTED_TERMINALS)("still delivers %s through its ordinary handler on a healthy join (positive control)", async (_name, frame, _failure, ordinaryEvent) => {
      const join = await startJoin();
      try {
        deliverCodec(join.raw, "Full", codecGameStarted(1, 1));
        await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
        expect(fallbackOf(join.adapter)).toBe("unused");
        join.raw.dispatchSynthetic("message", JSON.stringify(frame));
        await vi.waitFor(() => expect(typesOf(join.events)).toContain(ordinaryEvent));
      } finally {
        join.adapter.dispose();
      }
    });

    it("leaves the episode pending on an AiDriverFault, a fault of the game's driver, and still completes on the real GameStarted", async () => {
      const join = await startJoin();
      try {
        await failFirstFull(join, "decodeFailure");
        const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
        expect(fallbackOf(join.adapter)).toBe("reconnecting");

        fallback.dispatchSynthetic("message", JSON.stringify(AI_DRIVER_FAULT));
        // Reach guard: the ordinary handler ran, so only the episode's fate is under test.
        expect(typesOf(join.events)).toContain("aiDriverFault");
        expect(join.events.filter((event) => event.type === "error")).toEqual([
          { type: "error", message: AI_DRIVER_FAULT_MESSAGE },
        ]);
        expect(fallbackOf(join.adapter)).toBe("reconnecting");
        expect(join.outcome.value).toBe("never settled");

        fallback.dispatchSynthetic("message", plainStarted(3));
        await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
        expect((await join.adapter.getSnapshot()).state.turn_number).toBe(3);
        expect(fallbackOf(join.adapter)).toBe("finished");
      } finally {
        join.adapter.dispose();
      }
    });
  });

  describe("cleanup and later reuse", () => {
    const endings = {
      "the deadline": async (join: Join, fallback: MockWebSocket) => {
        void fallback;
        await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS);
        return join;
      },
      "a server refusal": async (join: Join, fallback: MockWebSocket) => {
        fallback.dispatchSynthetic("message", JSON.stringify({ type: "Error", data: { message: "Game not found" } }));
        return join;
      },
      "a withheld bootstrap": async (join: Join, fallback: MockWebSocket) => {
        fallback.dispatchSynthetic("message", JSON.stringify({
          type: "StateWithheld",
          data: {
            full_key: REPLY_OWNERSHIP_FULL_KEY, state_revision: 3,
            withheld: { type: "GameStarted", data: { your_player: 0 } }, reason: "too large",
          },
        }));
        return join;
      },
    } as const;

    it.each(Object.keys(endings) as (keyof typeof endings)[])("clears every latch and the provisional session after %s, and an explicit reconnect starts fresh", async (ending) => {
      const join = await startJoin();
      const { adapter, events } = join;
      try {
        await failFirstFull(join, "decodeFailure");
        const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
        expect(adapter.nativeSession).not.toBeNull();
        await endings[ending](join, fallback);
        await expect(settledAs(join)).resolves.toBeInstanceOf(AdapterError);
        expect(latchesOf(adapter)).toEqual(CLEAN_LATCHES);
        expect(adapter.nativeSession).toBeNull();
        expect(adapter.gameCode).toBeNull();
        expect(episodeEventsOf(events)).toEqual([]);
        expect(events.filter((event) => event.type === "sessionChanged")).toEqual([]);
        expect(vi.getTimerCount()).toBe(0);

        // A later explicit valid reconnect is not blocked by the consumed episode.
        expect(adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
        const fresh = await snapshotOnlyHandshake("Reconnect", fallback);
        expect(sentFrames(fresh, "Reconnect")).toHaveLength(1);
        expect(offeredCodec(fresh)).toBe(false);
        expect(fallbackOf(adapter)).toBe("finished");
      } finally {
        adapter.dispose();
      }
    });
  });

  describe("stale opening and disposal", () => {
    /**
     * Steps time until a replacement handshake that was created late in the
     * budget is in flight, so the deadline lands strictly inside it (the 5 s
     * handshake timeout cannot end it first). Backoff jitter is pinned to its
     * lower bound, so the dial schedule (about 9 s per unanswered handshake)
     * puts a dial at about 122.8 s of the 125 s budget.
     */
    async function lateHandshake(join: Join, failedAt: number): Promise<MockWebSocket> {
      let seen: MockWebSocket | null = MockWebSocket.last;
      for (let step = 0; step < 800; step += 1) {
        await vi.advanceTimersByTimeAsync(250);
        const socket = latestSocket();
        const fresh = socket !== seen;
        seen = socket;
        const opening = (join.adapter as unknown as { socketOpening: boolean }).socketOpening;
        if (fresh && opening && Date.now() - failedAt >= 121_000) return socket;
      }
      throw new Error("no late replacement handshake was observed");
    }

    it("closes a handshake that lands after the deadline without a Reconnect or any event, and leaves a successor's opening alone", async () => {
      const join = await startJoin();
      const { adapter, events } = join;
      const jitter = vi.spyOn(Math, "random").mockReturnValue(0);
      try {
        const failedAt = Date.now();
        await failFirstFull(join, "decodeFailure");
        const stale = await lateHandshake(join, failedAt);
        expect(Date.now() - failedAt).toBeLessThan(REMOTE_BUDGET_MS);
        expect(join.outcome.value).toBe("never settled");
        await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS - (Date.now() - failedAt) + 100);
        await expect(settledAs(join)).resolves.toMatchObject({ code: "WS_CLOSED", message: FAILURE_MESSAGE });
        expect(latchesOf(adapter)).toEqual(CLEAN_LATCHES);

        // A successor opens (explicit reconnect) while the stale handshake is still in flight.
        expect(adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
        expect((adapter as unknown as { socketOpening: boolean }).socketOpening).toBe(true);
        const eventsBefore = events.length;
        stale.dispatchSynthetic("message", CODEC_SESSION_HELLO);
        await vi.waitFor(() => expect(stale.close).toHaveBeenCalled());
        expect(sentFrames(stale, "Reconnect")).toHaveLength(0);
        expect(events).toHaveLength(eventsBefore);
        // The stale continuation did not clear the successor's opening latch.
        expect((adapter as unknown as { socketOpening: boolean }).socketOpening).toBe(true);
        const successor = await snapshotOnlyHandshake("Reconnect", stale);
        expect(sentFrames(successor, "Reconnect")).toHaveLength(1);
      } finally {
        jitter.mockRestore();
        adapter.dispose();
      }
    });

    it.each(["backoff", "handshake", "waiting for the reply"] as const)("disposal during %s keeps the exact disposal result and dials nothing afterwards", async (phase) => {
      const join = await startJoin();
      const { adapter, events } = join;
      await failFirstFull(join, "decodeFailure");
      if (phase === "waiting for the reply") {
        await snapshotOnlyHandshake("Reconnect", join.raw);
      } else if (phase === "handshake") {
        await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(join.raw));
        expect((adapter as unknown as { socketOpening: boolean }).socketOpening).toBe(true);
      } else {
        const first = await snapshotOnlyHandshake("Reconnect", join.raw);
        first.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(0);
        expect((adapter as unknown as { reconnectTimer: unknown }).reconnectTimer).not.toBeNull();
      }
      expect(join.outcome.value).toBe("never settled");
      adapter.dispose();
      await expect(settledAs(join)).resolves.toMatchObject({
        code: "WS_CLOSED", message: DISPOSED_MESSAGE, recoverable: true,
      });
      expect(events.slice(-2)).toEqual([
        { type: "actionPendingChanged", pending: false },
        { type: "latencyChanged", latencyMs: null },
      ]);
      expect(episodeEventsOf(events)).toEqual([]);
      expect(vi.getTimerCount()).toBe(0);
      const dialled = MockWebSocket.last;
      await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS + 10_000);
      expect(MockWebSocket.last).toBe(dialled);
    });
  });

  describe("success reentrancy and regression", () => {
    it("consumes the episode before any subscriber can run, so a disposing subscriber leaves no timer and settles the join once", async () => {
      const join = await startJoin();
      const { adapter } = join;
      const stateAtBootstrap: string[] = [];
      adapter.onEvent((event) => {
        if (event.type === "bootstrapDelivered") {
          stateAtBootstrap.push(fallbackOf(adapter));
          adapter.dispose();
        }
      });
      await failFirstFull(join, "decodeFailure");
      const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
      fallback.dispatchSynthetic("message", plainStarted(2));
      await vi.waitFor(() => expect(stateAtBootstrap).toEqual(["finished"]));
      await expect(settledAs(join)).resolves.toMatchObject({ code: "WS_CLOSED", message: DISPOSED_MESSAGE });
      expect(vi.getTimerCount()).toBe(0);
    });

    it("treats a loss after the fallback succeeded as an ordinary established recovery, announced normally", async () => {
      const join = await startJoin();
      const { adapter, events } = join;
      try {
        await failFirstFull(join, "decodeFailure");
        const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
        fallback.dispatchSynthetic("message", plainStarted(2));
        await vi.waitFor(() => expect(join.outcome.value).toEqual({ resolvedWith: undefined }));
        expect(episodeEventsOf(events)).toEqual([]);

        fallback.dispatchSynthetic("close");
        await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({ type: "reconnecting" })));
        await vi.advanceTimersByTimeAsync(1000);
        const next = await snapshotOnlyHandshake("Reconnect", fallback);
        next.dispatchSynthetic("message", plainStarted(3));
        await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
        expect(fallbackOf(adapter)).toBe("finished");
      } finally {
        adapter.dispose();
      }
    });
  });

  describe("joins that never enter the fallback", () => {
    it("treats a failure after a healthy bootstrap as an established recovery with its normal feedback", async () => {
      const codec = scriptedCodec();
      const { adapter, raw, events } = await establishCodecAdapter(() => Promise.resolve(codec.session));
      try {
        codec.decode.mockRejectedValueOnce(new Error("decoder trapped"));
        deliverCodec(raw, "Delta", codecStateUpdate(2, 2));
        await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({ type: "reconnecting" })));
        expect(fallbackOf(adapter)).toBe("unused");
        expect(sentFrames(raw, "JoinGameWithPassword")).toHaveLength(1);
      } finally {
        adapter.dispose();
      }
    });

    it.each([
      ["a failing frame without credentials", () => codecStateUpdate(1, 1)],
      ["credentials for another game", () => codecGameStarted(1, 1, { full_key: { game_code: "OTHER1", generation: 7 } })],
    ])("rejects the join as before for %s, with no second socket", async (_name, envelope) => {
      const join = await startJoin();
      try {
        await failFirstFull(join, "bootstrapRejection", envelope());
        await expect(settledAs(join)).resolves.toMatchObject({
          code: "WS_CLOSED", message: "Connection closed before game started", recoverable: true,
        });
        await vi.advanceTimersByTimeAsync(10_000);
        expect(MockWebSocket.last).toBe(join.raw);
        expect(fallbackOf(join.adapter)).toBe("unused");
      } finally {
        join.adapter.dispose();
      }
    });

    it("keeps a spectator join out of the fallback", async () => {
      const join = await startJoin("spectate");
      try {
        await failFirstFull(join, "bootstrapRejection");
        await expect(settledAs(join)).resolves.toMatchObject({
          code: "WS_CLOSED", message: "Connection closed before game started", recoverable: true,
        });
        await vi.advanceTimersByTimeAsync(10_000);
        expect(MockWebSocket.last).toBe(join.raw);
        expect(fallbackOf(join.adapter)).toBe("unused");
      } finally {
        join.adapter.dispose();
      }
    });

    it("keeps an established reconnect's failed Full out of the initial fallback", async () => {
      const first = scriptedCodec();
      const second = scriptedCodec();
      const sessions = [first.session, second.session];
      const { adapter, raw, events } = await establishCodecAdapter(() => Promise.resolve(sessions.shift() ?? null));
      try {
        raw.dispatchSynthetic("close");
        await vi.advanceTimersByTimeAsync(1000);
        const attempt = await codecHandshake("Reconnect", raw);
        second.rejectNext(1);
        deliverCodec(attempt, "Full", codecGameStarted(2, 2));
        await vi.waitFor(() => expect(second.close).toHaveBeenCalledOnce());
        await vi.waitFor(() => expect(
          events.filter((event) => (event as { type: string }).type === "reconnecting"),
        ).toHaveLength(2));
        expect(fallbackOf(adapter)).toBe("unused");
        expect(events).not.toContainEqual(expect.objectContaining({ type: "reconnectFailed" }));
      } finally {
        adapter.dispose();
      }
    });
  });

  // ── Held source: a waiting-room joiner whose attachment already accepted
  // SessionAttached. The session is published and the token, seat and Full key
  // are known; only the bootstrap codec failed. Credential-only recovery again:
  // one snapshot-only `Reconnect` carrying the HELD tuple, never a second join.

  describe("held credentials (a waiting-room joiner's own SessionAttached)", () => {
    const HELD_SESSION = {
      gameCode: "GAME01", playerId: 0, playerToken: "codec-token", fullKey: REPLY_OWNERSHIP_FULL_KEY,
    };
    const sessionAttached = (overrides: Record<string, unknown> = {}) => JSON.stringify({
      type: "SessionAttached",
      data: {
        game_code: "GAME01", player_id: 0, player_token: "codec-token",
        full_key: REPLY_OWNERSHIP_FULL_KEY, ...overrides,
      },
    });
    const gameCreated = (overrides: Record<string, unknown> = {}) => JSON.stringify({
      type: "GameCreated",
      data: { game_code: "GAME01", player_token: "codec-token", full_key: REPLY_OWNERSHIP_FULL_KEY, ...overrides },
    });
    const playerSlots = JSON.stringify({ type: "PlayerSlotsUpdate", data: { slots: [] } });
    /** Events that precede any failure: the joiner's whole published prefix. */
    const prefixOf = (waitingSnapshot: boolean) => [
      "serverHello", "sessionChanged", "sessionAttached", ...(waitingSnapshot ? ["stateChanged"] : []),
    ];
    const returnedFromWaiting = () => codecStateUpdate(1, 1, {}, { legal_actions: [{ type: "WaitingRoom" }] });
    /** The failed no-token GameStarted Full a started game's first state frame can be. */
    const tokenlessStarted = () => codecGameStarted(1, 1, { player_token: undefined });
    const pregameReturnOf = (adapter: WebSocketAdapter) => (adapter as unknown as {
      initialFallback: { pregameReturn?: string };
    }).initialFallback.pregameReturn;

    /** A join that has accepted its SessionAttached (and, optionally, a delivered waiting snapshot). */
    async function startHeld(waitingSnapshot: boolean) {
      const join = await startJoin();
      join.raw.dispatchSynthetic("message", sessionAttached());
      await vi.waitFor(() => expect(typesOf(join.events)).toContain("sessionAttached"));
      if (waitingSnapshot) {
        deliverCodec(join.raw, "Full", returnedFromWaiting());
        await vi.waitFor(() => expect(typesOf(join.events)).toContain("stateChanged"));
        expect((await join.adapter.getLegalActions()).actions).toEqual([{ type: "WaitingRoom" }]);
      }
      // Reach guards: the held session is published and the join is still pending.
      expect(typesOf(join.events)).toEqual(prefixOf(waitingSnapshot));
      expect(join.adapter.nativeSession).toEqual(HELD_SESSION);
      expect(join.outcome.value).toBe("never settled");
      return join;
    }

    /** A held join whose bootstrap failed, with the replacement attachment's socket open. */
    async function heldFallback(options: {
      waitingSnapshot?: boolean; reason?: FirstFullFailure; envelope?: unknown; kind?: "Full" | "Delta";
    } = {}) {
      const join = await startHeld(options.waitingSnapshot ?? false);
      await failFirstFull(
        join, options.reason ?? "decodeFailure", options.envelope ?? tokenlessStarted(), options.kind ?? "Full",
      );
      const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
      return { join, fallback };
    }

    const heldReconnect = {
      type: "Reconnect",
      data: { game_code: "GAME01", player_token: "codec-token", full_key: REPLY_OWNERSHIP_FULL_KEY },
    };

    describe("admission by reason and frame kind", () => {
      const failedFrames: [string, "Full" | "Delta", () => unknown][] = [
        ["a failed Delta", "Delta", () => codecStateUpdate(2, 2)],
        ["a failed waiting StateUpdate Full", "Full", () => codecStateUpdate(1, 1)],
        ["a failed GameStarted Full without a token", "Full", tokenlessStarted],
      ];
      const reasons: FirstFullFailure[] = ["decodeFailure", "bootstrapRejection"];

      it.each(reasons.flatMap((reason) => failedFrames.map(([name, kind, envelope]) => [reason, name, kind, envelope] as const)))(
        "admits the held tuple on %s of %s although the failed notice carries no credentials",
        async (reason, _name, kind, envelope) => {
          const { join, fallback } = await heldFallback({ reason, kind, envelope: envelope() });
          try {
            // Reach guards: the failed attachment's notice had no credentials to salvage,
            // the codec was offered once and the sticky provider was not consulted again.
            expect(offeredCodec(join.raw)).toBe(true);
            expect(offeredCodec(fallback)).toBe(false);
            expect(join.provider).toHaveBeenCalledOnce();
            expect(sentFrames(fallback, "Reconnect")).toEqual([heldReconnect]);
            expect(sentFrames(join.raw, "JoinGameWithPassword")).toHaveLength(1);
            expect(sentFrames(fallback, "JoinGameWithPassword")).toHaveLength(0);
            expect(fallbackOf(join.adapter)).toBe("reconnecting");
            expect(join.outcome.value).toBe("never settled");
            // Admission publishes nothing: the held session was published once, by SessionAttached.
            expect(typesOf(join.events)).toEqual([...prefixOf(false), "serverHello"]);

            fallback.dispatchSynthetic("message", plainStarted(2));
            await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
            expect(episodeEventsOf(join.events)).toEqual([]);
          } finally {
            join.adapter.dispose();
          }
        },
      );

      it.each([false, true])("recovers a held join after receive overload with waiting snapshot=%s", async (waitingSnapshot) => {
        const join = await startHeld(waitingSnapshot);
        let release: (() => void) | undefined;
        const decodedBefore = join.codec.decode.mock.calls.length;
        join.codec.decode.mockImplementationOnce(() => new Promise((resolve) => {
          release = () => resolve({ kind: "decoded", json: JSON.stringify(tokenlessStarted()) });
        }));
        try {
          deliverCodec(join.raw, "Full", tokenlessStarted());
          await vi.waitFor(() => expect(join.codec.decode).toHaveBeenCalledTimes(decodedBefore + 1));
          for (let revision = 2; revision <= 65; revision++) {
            deliverCodec(join.raw, "Delta", codecStateUpdate(revision, revision));
          }
          await vi.waitFor(() => expect(join.codec.close).toHaveBeenCalledOnce());
          expect(policyOf(join.adapter)).toEqual({ kind: "snapshotOnly", reason: "receiveOverload" });
          expect(join.outcome.value).toBe("never settled");

          const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
          expect(sentFrames(fallback, "Reconnect")).toEqual([heldReconnect]);
          expect(offeredCodec(fallback)).toBe(false);
          expect(join.provider).toHaveBeenCalledOnce();
          fallback.dispatchSynthetic("message", plainStarted(2));
          await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
          expect(episodeEventsOf(join.events)).toEqual([]);

          release?.();
          await vi.advanceTimersByTimeAsync(0);
          expect((await join.adapter.getSnapshot()).state.turn_number).toBe(2);
          expect(join.codec.decode).toHaveBeenCalledTimes(decodedBefore + 1);
          expect(join.codec.close).toHaveBeenCalledOnce();
        } finally {
          release?.();
          join.adapter.dispose();
        }
      });

      it("agrees with a token-bearing failed Full that repeats the held tuple, and admits", async () => {
        const { join, fallback } = await heldFallback({ envelope: codecGameStarted(1, 1) });
        try {
          expect(sentFrames(fallback, "Reconnect")).toEqual([heldReconnect]);
          fallback.dispatchSynthetic("message", plainStarted(2));
          await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
        } finally {
          join.adapter.dispose();
        }
      });

      it.each([
        ["another token", { player_token: "other-token" }],
        ["another seat", { your_player: 1 }],
        ["another generation", { full_key: { game_code: "GAME01", generation: 8 } }],
        ["another game", { full_key: { game_code: "OTHER1", generation: 7 } }],
      ])("rejects the join as an identity failure, keeping the held session, when the failed Full carries %s", async (_name, overrides) => {
        const join = await startHeld(false);
        try {
          await failFirstFull(join, "decodeFailure", codecGameStarted(1, 1, overrides));
          await expect(settledAs(join)).resolves.toMatchObject({
            code: "WS_ERROR",
            message: "Failed state frame carried credentials that differ from this join's session",
            recoverable: false,
          });
          // Reach guard: the failure reached the adapter's admission as an eligible held join.
          expect(policyOf(join.adapter)).toEqual({ kind: "snapshotOnly", reason: "decodeFailure" });
          expect(fallbackOf(join.adapter)).toBe("unused");
          expect(join.adapter.nativeSession).toEqual(HELD_SESSION);
          await vi.advanceTimersByTimeAsync(10_000);
          expect(MockWebSocket.last).toBe(join.raw);
        } finally {
          join.adapter.dispose();
        }
      });

      it("does not admit the held source for a codec frame that precedes the confirmation", async () => {
        MockWebSocket.last = null;
        const codec = scriptedCodec();
        const adapter = new WebSocketAdapter(
          "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
          undefined, undefined, undefined, { stateCodecSessionProvider: () => Promise.resolve(codec.session) },
        );
        const events: Seen[] = [];
        adapter.onEvent((event) => events.push(event as Seen));
        const outcome: { value: unknown } = { value: "never settled" };
        adapter.initialize().then(
          (value) => { outcome.value = { resolvedWith: value }; },
          (error) => { outcome.value = error; },
        );
        try {
          const raw = await snapshotOnlyHandshake("JoinGameWithPassword", null);
          raw.dispatchSynthetic("message", sessionAttached());
          await vi.waitFor(() => expect(typesOf(events)).toContain("sessionAttached"));
          // The confirmation never arrives: the Full is a protocol violation, not a bootstrap failure.
          deliverCodec(raw, "Full", codecGameStarted(1, 1));
          await vi.waitFor(() => expect(codec.close).toHaveBeenCalledOnce());
          expect(policyOf(adapter)).toEqual({ kind: "snapshotOnly", reason: "confirmationFailure" });
          expect(codec.decode).not.toHaveBeenCalled();
          await expect(vi.waitFor(() => {
            if (outcome.value === "never settled") throw new Error("pending");
          }).then(() => outcome.value)).resolves.toMatchObject({
            code: "WS_CLOSED", message: "Connection closed before game started", recoverable: true,
          });
          expect(fallbackOf(adapter)).toBe("unused");
          await vi.advanceTimersByTimeAsync(10_000);
          expect(MockWebSocket.last).toBe(raw);
        } finally {
          adapter.dispose();
        }
      });

      it("never uses the held tuple of another attachment for a later attachment's failure", async () => {
        // The first attachment accepts SessionAttached; an explicit reconnect then
        // replaces it while the join is pending, and the replacement's codec fails.
        const join = await startHeld(false);
        try {
          const secondCodec = scriptedCodec();
          join.provider.mockResolvedValueOnce(secondCodec.session);
          expect(join.adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
          const second = await codecHandshake("Reconnect", join.raw);
          secondCodec.decode.mockRejectedValueOnce(new Error("decoder trapped"));
          deliverCodec(second, "Delta", codecStateUpdate(2, 2));
          await vi.waitFor(() => expect(secondCodec.close).toHaveBeenCalled());
          // Reach guard: the replacement's failure was reported as the sticky policy.
          expect(policyOf(join.adapter)).toEqual({ kind: "snapshotOnly", reason: "decodeFailure" });
          await expect(settledAs(join)).resolves.toMatchObject({
            code: "WS_CLOSED", message: "Connection closed before game started",
          });
          expect(fallbackOf(join.adapter)).toBe("unused");
          await vi.advanceTimersByTimeAsync(10_000);
          expect(MockWebSocket.last).toBe(second);
        } finally {
          join.adapter.dispose();
        }
      });
    });

    describe("pregame return", () => {
      it.each<FirstFullFailure>(["decodeFailure", "bootstrapRejection"])(
        "waits for the game to start after GameCreated, SessionAttached and PlayerSlotsUpdate, then resolves the join once (%s)",
        async (reason) => {
          const { join, fallback } = await heldFallback({
            waitingSnapshot: true, reason, envelope: codecStateUpdate(2, 2),
          });
          const { adapter, events } = join;
          try {
            // The failed waiting snapshot is discarded without a synthetic state event.
            expect(typesOf(events)).toEqual([...prefixOf(true), "serverHello"]);
            await expect(adapter.getSnapshot()).rejects.toBeDefined();
            expect(sentFrames(fallback, "Reconnect")).toEqual([heldReconnect]);
            expect(adapter.nativeSession).toEqual(HELD_SESSION);

            fallback.dispatchSynthetic("message", gameCreated());
            await vi.waitFor(() => expect(pregameReturnOf(adapter)).toBe("gameCreated"));
            expect(typesOf(events)).toEqual([...prefixOf(true), "serverHello"]);
            expect(join.outcome.value).toBe("never settled");

            const timersWithDeadline = vi.getTimerCount();
            fallback.dispatchSynthetic("message", sessionAttached());
            await vi.waitFor(() => expect(fallbackOf(adapter)).toBe("awaitingGameStarted"));
            // The matching SessionAttached ended the episode's deadline (and only it).
            expect(vi.getTimerCount()).toBe(timersWithDeadline - 1);
            fallback.dispatchSynthetic("message", playerSlots);
            await vi.advanceTimersByTimeAsync(0);
            // No host, session, reconnect or state event, and the join is still pending.
            expect(typesOf(events)).toEqual([...prefixOf(true), "serverHello"]);
            expect(join.outcome.value).toBe("never settled");
            await expect(adapter.getSnapshot()).rejects.toBeDefined();
            expect(sentFrames(fallback, "JoinGameWithPassword")).toHaveLength(0);

            // Waiting for the game to start owns no deadline: far past the episode's budget.
            await vi.advanceTimersByTimeAsync(3 * REMOTE_BUDGET_MS);
            expect(join.outcome.value).toBe("never settled");
            expect(MockWebSocket.last).toBe(fallback);

            fallback.dispatchSynthetic("message", plainStarted(5));
            await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
            expect(typesOf(events)).toEqual([
              ...prefixOf(true), "serverHello", "bootstrapDelivered", "playerIdentity",
            ]);
            expect(typesOf(events).filter((type) => type === "sessionChanged")).toHaveLength(1);
            expect(episodeEventsOf(events)).toEqual([]);
            expect(fallbackOf(adapter)).toBe("finished");
            expect((await adapter.getSnapshot()).state.turn_number).toBe(5);
          } finally {
            adapter.dispose();
          }
          expect(vi.getTimerCount()).toBe(0);
        },
      );

      it("takes the same route from a healthy waiting snapshot to the started game with no fallback", async () => {
        const join = await startHeld(true);
        try {
          deliverCodec(join.raw, "Full", tokenlessStarted());
          await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
          expect(typesOf(join.events)).toEqual([...prefixOf(true), "bootstrapDelivered", "playerIdentity"]);
          expect(fallbackOf(join.adapter)).toBe("unused");
          expect(MockWebSocket.last).toBe(join.raw);
        } finally {
          join.adapter.dispose();
        }
      });

      it.each(["close", "error"] as const)("uses base pending-initialization settlement for a transport %s after the pregame return", async (loss) => {
        const { join, fallback } = await heldFallback({ waitingSnapshot: false });
        try {
          fallback.dispatchSynthetic("message", gameCreated());
          fallback.dispatchSynthetic("message", sessionAttached());
          await vi.waitFor(() => expect(fallbackOf(join.adapter)).toBe("awaitingGameStarted"));
          if (loss === "close") fallback.dispatchSynthetic("close");
          else fallback.onerror?.();
          await expect(settledAs(join)).resolves.toMatchObject(loss === "close"
            ? { code: "WS_CLOSED", message: "Connection closed before game started", recoverable: true }
            : { code: "WS_ERROR", message: "WebSocket connection failed", recoverable: true });
          // Not a second episode: nothing is dialled again and no reconnect is announced.
          await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS);
          expect(MockWebSocket.last).toBe(fallback);
          expect(fallbackOf(join.adapter)).toBe("awaitingGameStarted");
          expect(join.events.filter((event) => event.type === "reconnecting")).toEqual([]);
        } finally {
          join.adapter.dispose();
        }
      });
    });

    describe("in-game return", () => {
      it.each([
        [false, "decodeFailure"], [false, "bootstrapRejection"],
        [true, "decodeFailure"], [true, "bootstrapRejection"],
      ] as const)("answers a failed GameStarted with a plain GameStarted that publishes no second session (waiting snapshot: %s, %s)", async (waitingSnapshot, reason) => {
        const { join, fallback } = await heldFallback({ waitingSnapshot, reason });
        const { adapter, events } = join;
        try {
          // Admission discarded any waiting snapshot without a synthetic state event.
          expect(typesOf(events)).toEqual([...prefixOf(waitingSnapshot), "serverHello"]);
          await expect(adapter.getSnapshot()).rejects.toBeDefined();

          fallback.dispatchSynthetic("message", plainStarted(5, { legal_actions: [{ type: "PassPriority" }] }));
          await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
          expect(typesOf(events)).toEqual([
            ...prefixOf(waitingSnapshot), "serverHello", "bootstrapDelivered", "playerIdentity",
          ]);
          expect(typesOf(events).filter((type) => type === "sessionChanged")).toHaveLength(1);
          expect(episodeEventsOf(events)).toEqual([]);
          // The explicit accepted state, not the discarded wait cache, is what is playable.
          expect((await adapter.getSnapshot()).state.turn_number).toBe(5);
          expect((await adapter.getLegalActions()).actions).toEqual([{ type: "PassPriority" }]);
          expect(adapter.nativeSession).toEqual(HELD_SESSION);
          expect(fallbackOf(adapter)).toBe("finished");

          // The recovered seat is usable on the replacement socket.
          const completion = adapter.submitAction({ type: "PassPriority" } as GameAction, 0);
          await vi.waitFor(() => expect(submissionTypes(fallback)).toHaveLength(1));
          fallback.dispatchSynthetic("message", JSON.stringify(
            codecStateUpdate(6, 6, { command_id: lastSubmissionCommandId(fallback) }),
          ));
          await expect(completion).resolves.toMatchObject({ events: [] });
        } finally {
          adapter.dispose();
        }
      });

      it("takes the same route to the started game on a healthy decode, with no second socket", async () => {
        const join = await startHeld(false);
        try {
          deliverCodec(join.raw, "Full", tokenlessStarted());
          await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
          expect(typesOf(join.events)).toEqual([...prefixOf(false), "bootstrapDelivered", "playerIdentity"]);
          expect((await join.adapter.getSnapshot()).state.turn_number).toBe(1);
          expect(MockWebSocket.last).toBe(join.raw);
        } finally {
          join.adapter.dispose();
        }
      });
    });

    describe("identity preflight of the held reply", () => {
      it.each([
        ["a missing Full key", { full_key: undefined }, "Server omitted a valid Full session identity"],
        ["another game", { full_key: { game_code: "OTHER1", generation: 7 } }, "Server changed the Full session identity"],
        ["another generation", { full_key: { game_code: "GAME01", generation: 8 } }, "Server changed the Full session identity"],
        ["another seat", { your_player: 1 }, "Reconnect attached player 1, expected 0"],
        ["a missing seat", { your_player: undefined }, "Reconnect attached player undefined, expected 0"],
        ["another token", { player_token: "other-token" }, "Reconnect changed the player token"],
      ])("rejects the join before any bootstrap, cache or second session when the in-game reply carries %s", async (_name, overrides, message) => {
        const { join, fallback } = await heldFallback({ waitingSnapshot: true });
        fallback.dispatchSynthetic("message", plainStarted(2, overrides));
        try {
          await expect(settledAs(join)).resolves.toMatchObject({ code: "WS_ERROR", message, recoverable: false });
          // Reach guard: the reply reached the real handler on the replacement socket.
          expect(sentFrames(fallback, "Reconnect")).toHaveLength(1);
          expect(typesOf(join.events)).toEqual([...prefixOf(true), "serverHello"]);
          await expect(join.adapter.getSnapshot()).rejects.toBeDefined();
          expect(join.adapter.nativeSession).toEqual(HELD_SESSION);
          expect(fallback.close).toHaveBeenCalled();
          expect(fallbackOf(join.adapter)).toBe("finished");
          expect(latchesOf(join.adapter)).toEqual(CLEAN_LATCHES);
        } finally {
          join.adapter.dispose();
        }
      });

      it.each([
        ["omits the token", { player_token: undefined }],
        ["repeats the held token", { player_token: "codec-token" }],
      ])("accepts an in-game reply that %s and publishes no further session", async (_name, overrides) => {
        const { join, fallback } = await heldFallback();
        try {
          fallback.dispatchSynthetic("message", plainStarted(2, overrides));
          await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
          expect(typesOf(join.events).filter((type) => type === "sessionChanged")).toHaveLength(1);
          expect(join.adapter.nativeSession).toEqual(HELD_SESSION);
        } finally {
          join.adapter.dispose();
        }
      });

      it.each([
        ["another seat", { your_player: 1 }, "Reconnect attached player 1, expected 0"],
        ["a missing seat", { your_player: undefined }, "Reconnect attached player undefined, expected 0"],
        ["another token", { player_token: "other-token" }, "Reconnect changed the player token"],
        ["another generation", { full_key: { game_code: "GAME01", generation: 8 } }, "Server changed the Full session identity"],
      ])("applies the strict preflight to the GameStarted that follows a pregame return (%s)", async (_name, overrides, message) => {
        const { join, fallback } = await heldFallback();
        try {
          fallback.dispatchSynthetic("message", gameCreated());
          fallback.dispatchSynthetic("message", sessionAttached());
          await vi.waitFor(() => expect(fallbackOf(join.adapter)).toBe("awaitingGameStarted"));
          fallback.dispatchSynthetic("message", plainStarted(2, overrides));
          await expect(settledAs(join)).resolves.toMatchObject({ code: "WS_ERROR", message, recoverable: false });
          // Reach guard: the return completed, and nothing of the rejected reply was published.
          expect(typesOf(join.events)).toEqual([...prefixOf(false), "serverHello", "error"]);
          expect(join.events[join.events.length - 1]).toMatchObject({ type: "error", message });
          await expect(join.adapter.getSnapshot()).rejects.toBeDefined();
          expect(join.adapter.nativeSession).toEqual(HELD_SESSION);
        } finally {
          join.adapter.dispose();
        }
      });

      describe("pregame prefix", () => {
        const afterPrefix = async (frames: string[]) => {
          const held = await heldFallback();
          for (const frame of frames) held.fallback.dispatchSynthetic("message", frame);
          return held;
        };
        const rejectedAs = async (join: Join, fallback: MockWebSocket, message: string) => {
          await expect(settledAs(join)).resolves.toMatchObject({ code: "WS_ERROR", message, recoverable: false });
          // Reach guard: the frames reached the real handler on the replacement socket.
          expect(sentFrames(fallback, "Reconnect")).toHaveLength(1);
          expect(typesOf(join.events)).toEqual([...prefixOf(false), "serverHello"]);
          expect(join.adapter.nativeSession).toEqual(HELD_SESSION);
          expect(fallbackOf(join.adapter)).toBe("finished");
          expect(latchesOf(join.adapter)).toEqual(CLEAN_LATCHES);
        };

        it.each([
          ["another game", { game_code: "OTHER1" }, "Reconnect attached game OTHER1, expected GAME01"],
          ["another token", { player_token: "other-token" }, "Reconnect changed the player token"],
          ["no token", { player_token: undefined }, "Reconnect changed the player token"],
          ["another generation", { full_key: { game_code: "GAME01", generation: 8 } }, "Server changed the Full session identity"],
          ["no Full key", { full_key: undefined }, "Server omitted a valid Full session identity"],
        ])("rejects a GameCreated that carries %s, before anything is published", async (_name, overrides, message) => {
          const { join, fallback } = await afterPrefix([gameCreated(overrides)]);
          try {
            await rejectedAs(join, fallback, message);
          } finally {
            join.adapter.dispose();
          }
        });

        it.each([
          ["another game", { game_code: "OTHER1" }, "Reconnect attached game OTHER1, expected GAME01"],
          ["another seat", { player_id: 1 }, "Reconnect attached player 1, expected 0"],
          ["a missing seat", { player_id: undefined }, "Reconnect attached player undefined, expected 0"],
          ["another token", { player_token: "other-token" }, "Reconnect changed the player token"],
          ["another generation", { full_key: { game_code: "GAME01", generation: 8 } }, "Server changed the Full session identity"],
          ["no Full key", { full_key: undefined }, "Server omitted a valid Full session identity"],
        ])("rejects a SessionAttached that carries %s after a matching GameCreated", async (_name, overrides, message) => {
          const { join, fallback } = await heldFallback();
          try {
            fallback.dispatchSynthetic("message", gameCreated());
            await vi.waitFor(() => expect(pregameReturnOf(join.adapter)).toBe("gameCreated"));
            fallback.dispatchSynthetic("message", sessionAttached(overrides));
            await rejectedAs(join, fallback, message);
          } finally {
            join.adapter.dispose();
          }
        });

        it("accepts a GameCreated that carries no seat, since the protocol gives it none", async () => {
          const { join, fallback } = await heldFallback();
          try {
            const created = gameCreated();
            // Reach guard: the frame really is seatless, and it reaches the real handler.
            expect(JSON.parse(created).data).not.toHaveProperty("player_id");
            fallback.dispatchSynthetic("message", created);
            await vi.waitFor(() => expect(pregameReturnOf(join.adapter)).toBe("gameCreated"));
            expect(join.outcome.value).toBe("never settled");
            expect(fallbackOf(join.adapter)).toBe("reconnecting");
            expect(typesOf(join.events)).toEqual([...prefixOf(false), "serverHello"]);
          } finally {
            join.adapter.dispose();
          }
        });

        it("rejects a SessionAttached that is not preceded by the matching GameCreated", async () => {
          const { join, fallback } = await afterPrefix([sessionAttached()]);
          try {
            await rejectedAs(join, fallback, "Server answered the join's reconnect with an unexpected pregame session reply");
          } finally {
            join.adapter.dispose();
          }
        });

        it("keeps the deadline when a GameCreated is never followed by its SessionAttached, and fails the join at the bound", async () => {
          const failedAt = Date.now();
          const { join, fallback } = await afterPrefix([gameCreated()]);
          try {
            await vi.waitFor(() => expect(pregameReturnOf(join.adapter)).toBe("gameCreated"));
            await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS - 1000 - (Date.now() - failedAt));
            // Reach guard: the matching GameCreated was processed and the join still waits.
            expect(pregameReturnOf(join.adapter)).toBe("gameCreated");
            expect(join.outcome.value).toBe("never settled");
            await vi.advanceTimersByTimeAsync(2000);
            await expect(settledAs(join)).resolves.toMatchObject({
              code: "WS_CLOSED", message: FAILURE_MESSAGE, recoverable: true,
            });
            expect(fallback.close).toHaveBeenCalled();
            expect(typesOf(join.events)).toEqual([...prefixOf(false), "serverHello"]);
            expect(join.adapter.nativeSession).toEqual(HELD_SESSION);
            expect(vi.getTimerCount()).toBe(0);
          } finally {
            join.adapter.dispose();
          }
        });
      });
    });

    describe("deadline and retry budget", () => {
      it("fails the join once at the episode's deadline when the replacement socket never answers", async () => {
        const failedAt = Date.now();
        const { join, fallback } = await heldFallback({ waitingSnapshot: true });
        try {
          fallback.close.mockImplementation(() => fallback.dispatchSynthetic("close"));
          await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS - 1000 - (Date.now() - failedAt));
          expect(sentFrames(fallback, "Reconnect")).toHaveLength(1);
          expect(join.outcome.value).toBe("never settled");
          await vi.advanceTimersByTimeAsync(2000);
          await expect(settledAs(join)).resolves.toMatchObject({
            code: "WS_CLOSED", message: FAILURE_MESSAGE, recoverable: true,
          });
          expect(episodeEventsOf(join.events)).toEqual([]);
          expect(vi.getTimerCount()).toBe(0);
        } finally {
          join.adapter.dispose();
        }
      });

      it.each(["a natural close", "a handshake that closes before the hello"] as const)("retries after %s inside the same budget, without a second join or episode", async (loss) => {
        const join = await startHeld(false);
        try {
          await failFirstFull(join, "bootstrapRejection", tokenlessStarted());
          const first = loss === "a natural close"
            ? await snapshotOnlyHandshake("Reconnect", join.raw)
            : await vi.waitFor(() => { expect(MockWebSocket.last).not.toBe(join.raw); }).then(latestSocket);
          first.dispatchSynthetic("close");
          await vi.advanceTimersByTimeAsync(2000);
          expect(join.outcome.value).toBe("never settled");
          expect(fallbackOf(join.adapter)).toBe("reconnecting");

          const second = await snapshotOnlyHandshake("Reconnect", first);
          expect(second).not.toBe(first);
          expect(offeredCodec(second)).toBe(false);
          second.dispatchSynthetic("message", plainStarted(2));
          await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
          expect(join.provider).toHaveBeenCalledOnce();
          expect(sentFrames(join.raw, "JoinGameWithPassword")).toHaveLength(1);
          expect(sentFrames(second, "Reconnect")).toEqual([heldReconnect]);
          expect(episodeEventsOf(join.events)).toEqual([]);
        } finally {
          join.adapter.dispose();
        }
      });

      it("exhausts the budget through repeated lost attempts with the single failure and no re-admission", async () => {
        const join = await startHeld(false);
        try {
          await failFirstFull(join, "decodeFailure", tokenlessStarted());
          const dialled = new Set<MockWebSocket>();
          let last: MockWebSocket = join.raw;
          for (let step = 0; step < 100 && join.outcome.value === "never settled"; step += 1) {
            await vi.advanceTimersByTimeAsync(2000);
            const socket = latestSocket();
            if (socket !== last) {
              last = socket;
              dialled.add(socket);
              socket.dispatchSynthetic("close");
            }
          }
          await expect(settledAs(join)).resolves.toMatchObject({
            code: "WS_CLOSED", message: FAILURE_MESSAGE, recoverable: true,
          });
          expect(dialled.size).toBeGreaterThan(2);
          expect(episodeEventsOf(join.events)).toEqual([]);
          expect(fallbackOf(join.adapter)).toBe("finished");
          expect(vi.getTimerCount()).toBe(0);
          expect(join.adapter.nativeSession).toEqual(HELD_SESSION);
          expect([...dialled].flatMap((socket) => sentFrames(socket, "JoinGameWithPassword"))).toEqual([]);
        } finally {
          join.adapter.dispose();
        }
      });

      it("succeeds on a reply just inside the bound and leaves the timer inert", async () => {
        const failedAt = Date.now();
        const join = await startHeld(false);
        try {
          await failFirstFull(join, "decodeFailure", tokenlessStarted());
          const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
          await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS - 1000 - (Date.now() - failedAt));
          expect(join.outcome.value).toBe("never settled");
          fallback.dispatchSynthetic("message", plainStarted(2));
          await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
          await vi.advanceTimersByTimeAsync(10_000);
          expect(join.outcome.value).toEqual({ resolvedWith: undefined });
          expect(episodeEventsOf(join.events)).toEqual([]);
          expect((await join.adapter.getSnapshot()).state.turn_number).toBe(2);
        } finally {
          join.adapter.dispose();
        }
      });
    });

    describe("terminal failures keep the published session", () => {
      const terminated = async (join: Join, waitingSnapshot: boolean) => {
        // Reach guard shared by every terminal row: one prefix, one replacement hello, then nothing.
        expect(typesOf(join.events)).toEqual([...prefixOf(waitingSnapshot), "serverHello"]);
        expect(join.adapter.nativeSession).toEqual(HELD_SESSION);
        expect(latchesOf(join.adapter)).toEqual(CLEAN_LATCHES);
        expect(episodeEventsOf(join.events)).toEqual([]);
        await expect(join.adapter.getSnapshot()).rejects.toBeDefined();
      };

      it.each([false, true])("fails the join once, without episode events, when the replacement socket errors (waiting snapshot: %s)", async (waitingSnapshot) => {
        const { join, fallback } = await heldFallback({ waitingSnapshot });
        try {
          expect(join.outcome.value).toBe("never settled");
          fallback.onerror?.();
          await expect(settledAs(join)).resolves.toMatchObject({
            code: "WS_CLOSED", message: FAILURE_MESSAGE, recoverable: true,
          });
          await terminated(join, waitingSnapshot);
        } finally {
          join.adapter.dispose();
        }
      });

      it("fails the join once, without an error event, when the replacement's setup send fails", async () => {
        const join = await startHeld(false);
        try {
          await failFirstFull(join, "decodeFailure", tokenlessStarted());
          const fallback = await nextHandshake(join.raw, (socket) => {
            socket.send.mockImplementation((text: unknown) => {
              if (typeof text === "string" && text.includes('"type":"Reconnect"')) throw new Error("socket is gone");
            });
          });
          await vi.waitFor(() => expect(fallback.send).toHaveBeenCalledWith(expect.stringContaining('"type":"Reconnect"')));
          await expect(settledAs(join)).resolves.toMatchObject({
            code: "WS_CLOSED", message: FAILURE_MESSAGE, recoverable: true,
          });
          expect(fallback.close).toHaveBeenCalled();
          await terminated(join, false);
        } finally {
          join.adapter.dispose();
        }
      });

      it.each([
        ["a refusal", { message: "Game not found" }, "ACTION_REJECTED", true],
        ["a stale refusal", { message: "Engine error: ReorderHand: expected 3 ids, got 2" }, "STALE_ACTION", false],
        ["a rejected deck", { message: "Deck is not legal", code: "deck_rejected" }, "DECK_REJECTED", false],
      ])("keeps the existing initialization error for %s and announces nothing else", async (_name, data, code, recoverable) => {
        const { join, fallback } = await heldFallback({ waitingSnapshot: true });
        try {
          fallback.dispatchSynthetic("message", JSON.stringify({ type: "Error", data }));
          await expect(settledAs(join)).resolves.toMatchObject({ code, message: data.message, recoverable });
          await terminated(join, true);
        } finally {
          join.adapter.dispose();
        }
      });

      it("maps a protocol mismatch to the handshake rejection, then adds the incompatible hello", async () => {
        const join = await startHeld(false);
        try {
          await failFirstFull(join, "decodeFailure", tokenlessStarted());
          const initPendingAtHello: boolean[] = [];
          join.adapter.onEvent((event) => {
            if (event.type === "serverHello" && !event.compatible) {
              initPendingAtHello.push((join.adapter as unknown as { initReject: unknown }).initReject !== null);
            }
          });
          await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(join.raw));
          latestSocket().dispatchSynthetic("message", JSON.stringify({
            type: "ServerHello",
            data: {
              server_version: "9.9.9", build_commit: "other", protocol_version: PROTOCOL_VERSION + 1000,
              mode: "Full",
            },
          }));
          await expect(settledAs(join)).resolves.toMatchObject({ code: "WS_ERROR", recoverable: false });
          expect(initPendingAtHello).toEqual([false]);
          expect(join.events.filter((event) => event.type === "serverHello" && event.compatible === false)).toHaveLength(1);
          expect(episodeEventsOf(join.events)).toEqual([]);
          expect(join.adapter.nativeSession).toEqual(HELD_SESSION);
          expect(latchesOf(join.adapter)).toEqual(CLEAN_LATCHES);
        } finally {
          join.adapter.dispose();
        }
      });

      it("rejects with PASSWORD_REQUIRED and announces neither the password prompt nor a reconnect failure", async () => {
        const { join, fallback } = await heldFallback();
        try {
          fallback.dispatchSynthetic("message", JSON.stringify({ type: "PasswordRequired", data: { game_code: "GAME01" } }));
          await expect(settledAs(join)).resolves.toMatchObject({
            code: "PASSWORD_REQUIRED", message: "Room requires a password", recoverable: true,
          });
          await terminated(join, false);
        } finally {
          join.adapter.dispose();
        }
      });

      it.each([false, true])("fails a withheld GameStarted bootstrap with the server's reason and no synthetic bootstrap (waiting snapshot: %s)", async (waitingSnapshot) => {
        const { join, fallback } = await heldFallback({ waitingSnapshot });
        try {
          fallback.dispatchSynthetic("message", JSON.stringify({
            type: "StateWithheld",
            data: {
              full_key: REPLY_OWNERSHIP_FULL_KEY, state_revision: 3,
              withheld: { type: "GameStarted", data: { your_player: 0 } },
              reason: "The game state is too large to send.",
            },
          }));
          await expect(settledAs(join)).resolves.toMatchObject({
            code: "REQUEST_FAILED", message: "The game state is too large to send.", recoverable: true,
          });
          await terminated(join, waitingSnapshot);
        } finally {
          join.adapter.dispose();
        }
      });

      it("leaves a withheld StateUpdate pending until the real GameStarted completes the join", async () => {
        const { join, fallback } = await heldFallback();
        try {
          fallback.dispatchSynthetic("message", JSON.stringify({
            type: "StateWithheld",
            data: {
              full_key: REPLY_OWNERSHIP_FULL_KEY, state_revision: 3,
              withheld: { type: "StateUpdate" }, reason: "The game state is too large to send.",
            },
          }));
          await vi.waitFor(() => expect(typesOf(join.events)).toContain("stateDelivery"));
          expect(join.outcome.value).toBe("never settled");
          expect(fallbackOf(join.adapter)).toBe("reconnecting");
          fallback.dispatchSynthetic("message", plainStarted(4));
          await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
          expect(episodeEventsOf(join.events)).toEqual([]);
        } finally {
          join.adapter.dispose();
        }
      });
    });

    describe("the episode router (held source)", () => {
      beforeEach(() => { terminalStore.commit.mockClear(); });

      it.each(ROUTED_TERMINALS)("fails the join once on %s, keeps the published session and lets a later explicit reconnect start fresh", async (_name, frame, failure) => {
        const { join, fallback } = await heldFallback({ waitingSnapshot: true });
        const { adapter, events } = join;
        try {
          // Reach guards: the episode is active on its replacement socket and the join is pending.
          expect(fallbackOf(adapter)).toBe("reconnecting");
          expect(join.outcome.value).toBe("never settled");

          fallback.dispatchSynthetic("message", JSON.stringify(frame));
          await expect(settledAs(join)).resolves.toMatchObject(failure);
          await vi.advanceTimersByTimeAsync(0);
          // One prefix, one replacement hello, then nothing from the failure itself.
          expect(typesOf(events)).toEqual([...prefixOf(true), "serverHello"]);
          expect(fallbackOf(adapter)).toBe("finished");
          expect(latchesOf(adapter)).toEqual(CLEAN_LATCHES);
          expect(adapter.nativeSession).toEqual(HELD_SESSION);
          expect(terminalStore.commit).not.toHaveBeenCalled();
          expect(vi.getTimerCount()).toBe(0);

          expect(adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
          const fresh = await snapshotOnlyHandshake("Reconnect", fallback);
          expect(sentFrames(fresh, "Reconnect")).toHaveLength(1);
          expect(offeredCodec(fresh)).toBe(false);
        } finally {
          adapter.dispose();
        }
      });

      it("leaves the episode pending on an AiDriverFault, a fault of the game's driver, and still completes on the real GameStarted", async () => {
        const { join, fallback } = await heldFallback();
        try {
          expect(fallbackOf(join.adapter)).toBe("reconnecting");
          fallback.dispatchSynthetic("message", JSON.stringify(AI_DRIVER_FAULT));
          // Reach guard: the ordinary handler ran, so only the episode's fate is under test.
          expect(typesOf(join.events)).toContain("aiDriverFault");
          expect(fallbackOf(join.adapter)).toBe("reconnecting");
          expect(join.outcome.value).toBe("never settled");

          fallback.dispatchSynthetic("message", plainStarted(3));
          await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
          expect((await join.adapter.getSnapshot()).state.turn_number).toBe(3);
          expect(fallbackOf(join.adapter)).toBe("finished");
        } finally {
          join.adapter.dispose();
        }
      });

      // After the pregame return the episode is parked in awaitingGameStarted: its deadline is over and
      // the join waits for the game to start as a joiner that never failed does, so the router is not
      // active and a game that ends meanwhile takes its ordinary handlers (base behavior).
      it("keeps base behavior for a TerminalResult and a GameOver that arrive while waiting for the game to start", async () => {
        terminalStore.commit.mockClear();
        const { join, fallback } = await heldFallback();
        try {
          fallback.dispatchSynthetic("message", gameCreated());
          fallback.dispatchSynthetic("message", sessionAttached());
          await vi.waitFor(() => expect(fallbackOf(join.adapter)).toBe("awaitingGameStarted"));

          fallback.dispatchSynthetic("message", JSON.stringify(ROUTED_TERMINALS[1][1]));
          await vi.waitFor(() => expect(typesOf(join.events)).toContain("terminalDelivery"));
          expect(terminalStore.commit).toHaveBeenCalledOnce();
          expect(join.events.filter((event) => event.type === "sessionChanged").slice(-1)[0])
            .toMatchObject({ session: { terminalOnly: true } });

          fallback.dispatchSynthetic("message", JSON.stringify(ROUTED_TERMINALS[2][1]));
          expect(typesOf(join.events)).toContain("gameOver");
          // Not a join outcome there: the original initialization is untouched.
          expect(fallbackOf(join.adapter)).toBe("awaitingGameStarted");
          expect(join.outcome.value).toBe("never settled");
        } finally {
          join.adapter.dispose();
        }
      });
    });

    describe("cleanup and later reuse", () => {
      const endings = {
        "the deadline": async (_fallback: MockWebSocket) => { await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS); },
        "a server refusal": async (fallback: MockWebSocket) => {
          fallback.dispatchSynthetic("message", JSON.stringify({ type: "Error", data: { message: "Game not found" } }));
        },
        "a withheld bootstrap": async (fallback: MockWebSocket) => {
          fallback.dispatchSynthetic("message", JSON.stringify({
            type: "StateWithheld",
            data: {
              full_key: REPLY_OWNERSHIP_FULL_KEY, state_revision: 3,
              withheld: { type: "GameStarted", data: { your_player: 0 } }, reason: "too large",
            },
          }));
        },
      } as const;

      it.each(Object.keys(endings) as (keyof typeof endings)[])("clears every latch and the waiting cache but keeps the held session after %s, and an explicit reconnect starts fresh", async (ending) => {
        const { join, fallback } = await heldFallback({ waitingSnapshot: true });
        const { adapter, events } = join;
        try {
          // An ordinary StateUpdate may be delivered during the episode (it cannot end it);
          // the terminal cleanup must discard what it cached.
          fallback.dispatchSynthetic("message", JSON.stringify(codecStateUpdate(2, 2)));
          await vi.waitFor(() => expect(latchesOf(adapter).snapshot).not.toBeNull());
          expect(latchesOf(adapter).cachedStateRevision).toBe(2);
          expect(fallbackOf(adapter)).toBe("reconnecting");
          await endings[ending](fallback);
          await expect(settledAs(join)).resolves.toBeInstanceOf(AdapterError);
          expect(latchesOf(adapter)).toEqual(CLEAN_LATCHES);
          expect(adapter.nativeSession).toEqual(HELD_SESSION);
          expect(episodeEventsOf(events)).toEqual([]);
          // The held session was published once, by SessionAttached, and not retracted.
          expect(typesOf(events).filter((type) => type === "sessionChanged")).toHaveLength(1);
          expect(vi.getTimerCount()).toBe(0);

          expect(adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
          const fresh = await snapshotOnlyHandshake("Reconnect", fallback);
          expect(sentFrames(fresh, "Reconnect")).toHaveLength(1);
          expect(offeredCodec(fresh)).toBe(false);
          expect(fallbackOf(adapter)).toBe("finished");
        } finally {
          adapter.dispose();
        }
      });
    });

    describe("stale opening and disposal", () => {
      it.each(["backoff", "handshake", "waiting for the reply"] as const)("disposal during %s keeps the exact disposal result and dials nothing afterwards", async (phase) => {
        const join = await startHeld(true);
        const { adapter, events } = join;
        await failFirstFull(join, "decodeFailure", tokenlessStarted());
        if (phase === "waiting for the reply") {
          await snapshotOnlyHandshake("Reconnect", join.raw);
        } else if (phase === "handshake") {
          await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(join.raw));
          expect((adapter as unknown as { socketOpening: boolean }).socketOpening).toBe(true);
        } else {
          const first = await snapshotOnlyHandshake("Reconnect", join.raw);
          first.dispatchSynthetic("close");
          await vi.advanceTimersByTimeAsync(0);
          expect((adapter as unknown as { reconnectTimer: unknown }).reconnectTimer).not.toBeNull();
        }
        expect(join.outcome.value).toBe("never settled");
        adapter.dispose();
        await expect(settledAs(join)).resolves.toMatchObject({
          code: "WS_CLOSED", message: DISPOSED_MESSAGE, recoverable: true,
        });
        expect(events.slice(-2)).toEqual([
          { type: "actionPendingChanged", pending: false },
          { type: "latencyChanged", latencyMs: null },
        ]);
        expect(episodeEventsOf(events)).toEqual([]);
        // Disposal keeps its own contract and clears every credential.
        expect(adapter.nativeSession).toBeNull();
        expect(vi.getTimerCount()).toBe(0);
        const dialled = MockWebSocket.last;
        await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS + 10_000);
        expect(MockWebSocket.last).toBe(dialled);
      });

      it("closes a handshake that lands after the deadline without a Reconnect or any event, and leaves a successor's opening alone", async () => {
        const jitter = vi.spyOn(Math, "random").mockReturnValue(0);
        const failedAt = Date.now();
        const join = await startHeld(false);
        const { adapter, events } = join;
        try {
          await failFirstFull(join, "decodeFailure", tokenlessStarted());
          let stale: MockWebSocket | null = null;
          let seen: MockWebSocket | null = MockWebSocket.last;
          for (let step = 0; step < 800 && stale === null; step += 1) {
            await vi.advanceTimersByTimeAsync(250);
            const socket = latestSocket();
            const fresh = socket !== seen;
            seen = socket;
            const opening = (adapter as unknown as { socketOpening: boolean }).socketOpening;
            if (fresh && opening && Date.now() - failedAt >= 121_000) stale = socket;
          }
          if (stale === null) throw new Error("no late replacement handshake was observed");
          expect(join.outcome.value).toBe("never settled");
          await vi.advanceTimersByTimeAsync(REMOTE_BUDGET_MS - (Date.now() - failedAt) + 100);
          await expect(settledAs(join)).resolves.toMatchObject({ code: "WS_CLOSED", message: FAILURE_MESSAGE });
          expect(latchesOf(adapter)).toEqual(CLEAN_LATCHES);

          expect(adapter.tryReconnect(RECONNECT_SESSION as never)).toBe(true);
          expect((adapter as unknown as { socketOpening: boolean }).socketOpening).toBe(true);
          const eventsBefore = events.length;
          stale.dispatchSynthetic("message", CODEC_SESSION_HELLO);
          await vi.waitFor(() => expect(stale!.close).toHaveBeenCalled());
          expect(sentFrames(stale, "Reconnect")).toHaveLength(0);
          expect(events).toHaveLength(eventsBefore);
          expect((adapter as unknown as { socketOpening: boolean }).socketOpening).toBe(true);
          const successor = await snapshotOnlyHandshake("Reconnect", stale);
          expect(sentFrames(successor, "Reconnect")).toHaveLength(1);
        } finally {
          jitter.mockRestore();
          adapter.dispose();
        }
      });
    });

    describe("success reentrancy and regression", () => {
      it("consumes the episode before any subscriber can run, so a disposing subscriber leaves no timer and settles the join once", async () => {
        const join = await startHeld(true);
        const { adapter } = join;
        const stateAtBootstrap: string[] = [];
        adapter.onEvent((event) => {
          if (event.type === "bootstrapDelivered") {
            stateAtBootstrap.push(fallbackOf(adapter));
            adapter.dispose();
          }
        });
        await failFirstFull(join, "decodeFailure", tokenlessStarted());
        const fallback = await snapshotOnlyHandshake("Reconnect", join.raw);
        fallback.dispatchSynthetic("message", plainStarted(2));
        await vi.waitFor(() => expect(stateAtBootstrap).toEqual(["finished"]));
        await expect(settledAs(join)).resolves.toMatchObject({ code: "WS_CLOSED", message: DISPOSED_MESSAGE });
        expect(vi.getTimerCount()).toBe(0);
      });

      it("treats a loss after the held fallback succeeded as an ordinary established recovery, announced normally", async () => {
        const { join, fallback } = await heldFallback();
        try {
          fallback.dispatchSynthetic("message", plainStarted(2));
          await expect(settledAs(join)).resolves.toEqual({ resolvedWith: undefined });
          expect(episodeEventsOf(join.events)).toEqual([]);

          fallback.dispatchSynthetic("close");
          await vi.waitFor(() => expect(join.events).toContainEqual(expect.objectContaining({ type: "reconnecting" })));
          await vi.advanceTimersByTimeAsync(1000);
          const next = await snapshotOnlyHandshake("Reconnect", fallback);
          next.dispatchSynthetic("message", plainStarted(3));
          await vi.waitFor(() => expect(join.events).toContainEqual({ type: "reconnected", inGame: true }));
          expect(fallbackOf(join.adapter)).toBe("finished");
        } finally {
          join.adapter.dispose();
        }
      });

      it("announces no host or reconnect event for a pregame return's GameCreated", async () => {
        const { join, fallback } = await heldFallback();
        try {
          fallback.dispatchSynthetic("message", gameCreated());
          fallback.dispatchSynthetic("message", sessionAttached());
          await vi.waitFor(() => expect(fallbackOf(join.adapter)).toBe("awaitingGameStarted"));
          expect(join.events.filter((event) => ["reconnected", "gameCreated", "waitingForOpponent"].includes(event.type))).toEqual([]);
        } finally {
          join.adapter.dispose();
        }
      });
    });
  });
});
