import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { diagnosticIdFor, getDiagnosticHistory } from "../../services/troubleshooting";
import { loadDiagnosticHistory } from "../../services/diagnosticHistory";

const lanGate = vi.hoisted(() => ({ supported: false, probe: vi.fn(), authorize: vi.fn() }));
const terminalStore = vi.hoisted(() => ({ commit: vi.fn(async () => true) }));
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
  const manual = new WebSocketAdapter("ws://192.168.1.2:9374/ws", "join", { main_deck: [], sideboard: [] }, "ABC123");
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
  const manual = new WebSocketAdapter("ws://192.168.1.2:9374/ws", "join", { main_deck: [], sideboard: [] }, "ABC123");
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
      const adapter = new WebSocketAdapter("wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01");
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
