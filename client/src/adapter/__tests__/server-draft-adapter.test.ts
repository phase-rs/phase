import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { EMPTY_DRAFT_POOL_GROUPS } from "../draft-adapter";
import { ServerDraftAdapter } from "../server-draft-adapter";
import { PROTOCOL_VERSION } from "../ws-adapter";
import { AdapterError, AdapterErrorCode } from "../types";
import type { DraftPlayerView } from "../draft-adapter";
import type { GameAction, GameLogEntry, GameState, LegalActionsResult, ObjectAction, SubmitResult } from "../types";
import { StateCodecRuntime, type StateCodecSession } from "../state-codec-runtime";
import { STATE_CODEC_SOCKET_TIMEOUT_MS } from "../../services/stateCodecSocket";
import type { EngineModule } from "../engine-queries";
import type * as OpenPhaseSocketModule from "../../services/openPhaseSocket";
import type {
  InteractionChoiceId,
  InteractionId,
  InteractionPreviewRequest,
  InteractionSubmission,
  PreviewRequestId,
} from "../generated/interaction";
import { usePreferencesStore } from "../../stores/preferencesStore";
import type { PrintingEntry } from "../../services/scryfall";
import type * as Scryfall from "../../services/scryfall";

const artIO = vi.hoisted(() => ({
  face: vi.fn(),
  catalog: vi.fn(),
  bounds: vi.fn(),
  // Present on the fake shared engine only while the default-wiring test runs.
  openCodec: null as null | (() => Promise<unknown>),
}));
vi.mock("../wasm-adapter", () => ({
  getSharedAdapter: () => ({
    getCardFaceData: artIO.face,
    checkDeckPrintingsBounds: artIO.bounds,
    ...(artIO.openCodec ? { openStateCodecSession: artIO.openCodec } : {}),
  }),
}));
vi.mock("../../services/scryfall", async (original) => ({
  ...await original<typeof Scryfall>(), getCardPrintings: artIO.catalog,
}));

const ART_A: PrintingEntry = {
  id: "a", set: "lea", set_name: "Alpha", collector_number: "001", released_at: "1993-01-01",
  border_color: "black", frame_effects: [], full_art: false, faces: [],
};
const ART_B: PrintingEntry = { ...ART_A, id: "b", set: "leb", collector_number: "002", released_at: "1994-01-01" };
const MAIN_ART = [
  { section: "main_deck", name: "Plains", count: 1, printing: { set_code: "lea", collector_number: "001" } },
  { section: "main_deck", name: "Island", count: 1, printing: { set_code: "leb", collector_number: "002" } },
];

function heldArt<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}

async function drainArt(): Promise<void> {
  for (let i = 0; i < 30; i++) await Promise.resolve();
}

beforeEach(() => {
  usePreferencesStore.setState({ artChain: [], artOverrides: {} });
  artIO.face.mockReset().mockImplementation(async (name: string) => ({ scryfall_oracle_id: name }));
  artIO.catalog.mockReset().mockResolvedValue([ART_B, ART_A]);
  artIO.bounds.mockReset().mockResolvedValue(undefined);
});

// ── openPhaseSocket injection ─────────────────────────────────────────
//
// The adapter opens its draft socket through `openPhaseSocket`. Most tests
// below drive the real helper over the stubbed global WebSocket; the open-
// failure suite injects one pre-socket rejection (e.g. a LAN authorization
// denial, which the helper forwards unwrapped) and otherwise passes through.
const { injectedOpenFailure, openedUrls } = vi.hoisted(() => ({
  injectedOpenFailure: { current: null as null | { error: unknown } },
  openedUrls: [] as string[],
}));

vi.mock("../../services/openPhaseSocket", async (importOriginal) => {
  const actual = await importOriginal<typeof OpenPhaseSocketModule>();
  return {
    ...actual,
    openPhaseSocket: (...args: Parameters<typeof actual.openPhaseSocket>) => {
      openedUrls.push(args[0]);
      const injection = injectedOpenFailure.current;
      if (injection) return Promise.reject(injection.error);
      return actual.openPhaseSocket(...args);
    },
  };
});

// ── MockWebSocket (copied from ws-adapter.test.ts) ─────────────────────

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
  dispatchSynthetic(type: "message" | "close", data?: string) {
    if (type === "message" && data !== undefined) {
      // Production handles messages through the onmessage IDL property only.
      this.onmessage?.({ data });
    } else if (type === "close") {
      this.onclose?.();
      this.dispatchEvent(new Event("close"));
    }
  }
}

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

const CORRELATED_HELLO = JSON.stringify({
  ...JSON.parse(SERVER_HELLO),
  data: { ...JSON.parse(SERVER_HELLO).data, command_correlation: "CommandIdV1" },
});

/**
 * Drives an adapter through the openPhaseSocket handshake.
 * Returns the mock ws after the handshake settles.
 */
async function completeHandshake(hello = SERVER_HELLO): Promise<MockWebSocket> {
  await Promise.resolve();
  const ws = MockWebSocket.last!;
  ws.dispatchSynthetic("message", hello);
  await Promise.resolve();
  await Promise.resolve();
  return ws;
}

function receive(ws: MockWebSocket, type: string, data: unknown): void {
  ws.dispatchSynthetic("message", JSON.stringify({ type, data }));
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

function createMockDraftView(overrides: Partial<DraftPlayerView> = {}): DraftPlayerView {
  return {
    status: "Drafting",
    kind: "Premier",
    launch_capability: "None",
    distribution: "PickAndPass",
    commanders_required: 0,
    current_pack_number: 0,
    pick_number: 0,
    pass_direction: "Left",
    current_pack: null,
    required_pick_count: 0,
    pick_selection_mode: "Direct",
    pool: [],
    draft_effects: [],
    pool_groups: EMPTY_DRAFT_POOL_GROUPS,
    seats: [],
    cards_per_pack: 14,
    pack_sizes: [14, 14, 14],
    pack_set_codes: ["TST", "TST", "TST"],
    pack_pick_steps: [14, 14, 14],
    pick_steps_per_pack: 14,
    pack_count: 3,
    min_deck_size: 40,
    addable_cards: ["Plains", "Island", "Swamp", "Mountain", "Forest"],
    timer_remaining_ms: null,
    standings: [],
    current_round: 0,
    next_pairing_round: 1,
    tournament_format: "Swiss",
    pod_policy: "Competitive",
    pairings: [],
    match_config: { match_type: "Bo1" },
    ...overrides,
  };
}

function debugLogEntry(value: string): GameLogEntry {
  return {
    seq: 0,
    turn: 1,
    phase: "PreCombatMain",
    category: "Debug",
    segments: [{ type: "Text", value }],
    presentation: { importance: "Diagnostic", tone: "Diagnostic", boundary: "None", visibility: "Public" },
  };
}

function matchState(label: string): GameState {
  return {
    label,
    turn_number: 1,
    active_player: 0,
    priority_player: 0,
    phase: "PreCombatMain",
    players: [],
    objects: {},
  } as unknown as GameState;
}

const viewerInteraction = {
  waitingForKind: { simultaneous: null, terminal: false, code: "choose" },
  authorizedSubmitters: [0],
  canSubmit: true,
  autoPassRecommended: false,
  opportunities: [],
  attachmentFans: {},
  attachmentViews: {},
  availability: { type: "inputRequired" },
} as LegalActionsResult["viewerInteraction"];

const objectActions: Record<string, ObjectAction[]> = {
  "42": [{ type: "PassPriority" }],
};

const FULL_KEY = { game_code: "GAME01", generation: 7 };

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

function enterAcceptedFullMatch(adapter: ServerDraftAdapter, ws: MockWebSocket): void {
  const events = vi.fn();
  adapter.onEvent(events);
  receive(ws, "DraftMatchStart", {
    match_id: "r1-t0",
    round: 1,
    game_code: FULL_KEY.game_code,
    full_key: FULL_KEY,
    player_token: "match-draft-token",
    your_player: 0,
    opponent_name: "Bob",
  });
  const startedState = matchState("accepted-full-start");
  receive(ws, "GameStarted", {
    state: startedState,
    your_player: 0,
    full_key: FULL_KEY,
  });
  expect(events).toHaveBeenCalledWith(expect.objectContaining({
    type: "gameStateUpdated",
    state: startedState,
  }));
  expect(adapter.currentPhase).toBe("match");
  ws.send.mockClear();
}

function deliverOwnershipStateUpdate(ws: MockWebSocket, marker: string): void {
  receive(ws, "StateUpdate", {
    state: matchState(`reply-${marker}`),
    events: [{ marker }],
    full_key: FULL_KEY,
  });
}

async function exerciseOverlappingSubmissionOwnership(
  ws: MockWebSocket,
  submitFirst: () => Promise<SubmitResult>,
  submitSecond: () => Promise<SubmitResult>,
): Promise<void> {
  const readFirst = trackRejection(submitFirst());
  const readSecond = trackRejection(submitSecond());

  deliverOwnershipStateUpdate(ws, "A");
  const [firstAfterA, secondAfterA] = await Promise.all([readFirst(), readSecond()]);
  const secondWasSent = submissionTypes(ws).length > 1;

  if (secondWasSent) deliverOwnershipStateUpdate(ws, "B");
  const firstAfterB = await readFirst();
  await readSecond();

  const secondErrorCode = typeof secondAfterA === "object" && secondAfterA !== null && "code" in secondAfterA
    ? secondAfterA.code
    : null;
  expect({
    secondWasSent,
    secondErrorCode,
    firstOwnsReplyA: deliveredEventMarker(firstAfterA) === "A",
    firstSettlesWithoutAnOrphan: deliveredEventMarker(firstAfterB) === "A",
  }).toEqual({
    secondWasSent: false,
    secondErrorCode: AdapterErrorCode.ACTION_NOT_SENT,
    firstOwnsReplyA: true,
    firstSettlesWithoutAnOrphan: true,
  });
}

describe("ServerDraftAdapter", () => {
  let adapter: ServerDraftAdapter;
  let ws: MockWebSocket;

  beforeEach(async () => {
    MockWebSocket.last = null;
    adapter = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: null });
    // Start a createDraft flow — this triggers attachSocket.
    const createPromise = adapter.createDraft({
      displayName: "Alice",
      setCodes: ["MKM"],
      kind: "Premier",
      public: true,
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      podSize: 8,
    });
    ws = await completeHandshake();
    // Simulate DraftCreated to resolve the create promise.
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftCreated",
        data: { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 },
      }),
    );
    await createPromise;
  });

  afterEach(() => {
    adapter.dispose();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("sends a tagged Uniform source instead of legacy set codes", () => {
    const create = ws.send.mock.calls
      .map(([frame]) => JSON.parse(frame as string))
      .find((frame) => frame.type === "CreateDraftWithSettings");

    expect(create).toMatchObject({
      data: {
        source: { type: "Uniform", data: { set_codes: ["MKM"] } },
      },
    });
    expect(create.data).not.toHaveProperty("set_codes");
  });

  it("sends Chaos candidates without a client assignment schedule", async () => {
    MockWebSocket.last = null;
    const chaos = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: null });
    const createPromise = chaos.createDraft({
      displayName: "Alice",
      source: { type: "Chaos", data: { candidate_codes: ["AAA", "BBB"] } },
      kind: "Premier",
      public: true,
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      podSize: 8,
    });
    const chaosWs = await completeHandshake();
    const create = chaosWs.send.mock.calls
      .map(([frame]) => JSON.parse(frame as string))
      .find((frame) => frame.type === "CreateDraftWithSettings");

    expect(create).toMatchObject({
      data: {
        source: { type: "Chaos", data: { candidate_codes: ["AAA", "BBB"] } },
      },
    });
    expect(JSON.stringify(create)).not.toContain("assignments");
    chaosWs.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftCreated",
        data: { draft_code: "CHAOS1", player_token: "tok", seat_index: 0 },
      }),
    );
    await createPromise;
  });

  it("rejects a stale Full server before setup or draft updates can mutate state", async () => {
    MockWebSocket.last = null;
    const staleAdapter = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: null });
    const listener = vi.fn();
    staleAdapter.onEvent(listener);
    const createPromise = staleAdapter.createDraft({
      displayName: "Alice",
      setCodes: ["MKM"],
      kind: "CommanderDraft",
      public: true,
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      podSize: 8,
    });

    await Promise.resolve();
    const staleWs = MockWebSocket.last!;
    staleWs.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "ServerHello",
        data: {
          server_version: "0.0.0-stale",
          build_commit: "stalehash",
          protocol_version: PROTOCOL_VERSION - 1,
          mode: "Full",
        },
      }),
    );

    await expect(createPromise).rejects.toThrow("older than supported");
    expect(staleWs.close).toHaveBeenCalledOnce();
    expect(staleWs.send).not.toHaveBeenCalled();
    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "serverHello",
        compatible: false,
        info: expect.objectContaining({ protocolVersion: PROTOCOL_VERSION - 1, mode: "Full" }),
      }),
    );

    staleWs.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftStateUpdate",
        data: {
          view: {
            ...createMockDraftView({ kind: "CommanderDraft", pick_selection_mode: "Ordered" }),
            pick_selection_mode: undefined,
          },
        },
      }),
    );

    expect(staleAdapter.currentDraftView).toBeNull();
    expect(listener).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "draftViewUpdated" }),
    );
  });

  it("transitions phase to match on DraftMatchStart", () => {
    expect(adapter.currentPhase).toBe("lobby");

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftMatchStart",
        data: {
          match_id: "r1-t0",
          round: 1,
          game_code: "GAME01",
          full_key: FULL_KEY,
          player_token: "gametok",
          your_player: 0,
          opponent_name: "Bob",
        },
      }),
    );

    expect(adapter.currentPhase).toBe("match");
    expect(adapter.playerId).toBe(0);
    expect(adapter.currentMatchId).toBe("r1-t0");
  });

  it("reattaches once with the draft credential carried by DraftMatchStart", () => {
    const listener = vi.fn();
    adapter.onEvent(listener);
    const matchStart = {
      match_id: "r1-t0",
      round: 1,
      game_code: "GAME01",
      full_key: FULL_KEY,
      player_token: "match-draft-token",
      your_player: 0,
      opponent_name: "Bob",
    };
    ws.send.mockClear();

    receive(ws, "DraftMatchStart", matchStart);

    expect(ws.send).toHaveBeenCalledExactlyOnceWith(
      JSON.stringify({
        type: "ReconnectDraft",
        data: {
          draft_code: "ABCD12",
          player_token: "match-draft-token",
        },
      }),
    );

    ws.send.mockClear();
    receive(ws, "DraftMatchStart", matchStart);

    expect(ws.send).not.toHaveBeenCalled();
    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({ type: "matchStarting", matchId: "r1-t0" }),
    );
  });

  it("routes submitAction only during match phase", async () => {
    // Not in match phase yet — should throw.
    await expect(adapter.submitAction({ type: "PassPriority" }, 0)).rejects.toThrow(
      "Not in a match phase",
    );
  });

  describe("Issue #9319 pending submission reply ownership", () => {
    beforeEach(() => {
      enterAcceptedFullMatch(adapter, ws);
    });

    it("settles accepted ActionNoOp and releases the next Action and Interaction", async () => {
      const pendingStates: boolean[] = [];
      adapter.onEvent((event) => {
        if (event.type === "actionPendingChanged") pendingStates.push(event.pending);
      });

      const zeroCountCreate: GameAction = {
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
      };
      const noOpPending = trackRejection(adapter.submitAction(zeroCountCreate, 0));
      expect(JSON.parse(ws.send.mock.calls[0][0] as string)).toEqual({
        type: "Action",
        data: { action: zeroCountCreate },
      });

      receive(ws, "ActionNoOp", undefined);
      const noOpOutcome = await noOpPending();

      const actionPending = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));
      deliverOwnershipStateUpdate(ws, "reply-after-noop-action");
      const actionOutcome = await actionPending();

      const interactionPending = trackRejection(
        adapter.submitInteraction(interactionSubmission("after-noop"), 0),
      );
      deliverOwnershipStateUpdate(ws, "reply-after-noop-interaction");
      const interactionOutcome = await interactionPending();

      expect.soft(noOpOutcome).toMatchObject({
        resolvedWith: { events: [], log_entries: [] },
      });
      expect.soft(actionOutcome).toMatchObject({
        resolvedWith: { events: [{ marker: "reply-after-noop-action" }] },
      });
      expect.soft(interactionOutcome).toMatchObject({
        resolvedWith: { events: [{ marker: "reply-after-noop-interaction" }] },
      });
      expect.soft(submissionTypes(ws)).toEqual(["Action", "Action", "Interaction"]);
      expect.soft(pendingStates).toEqual([true, false, true, false, true, false]);
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
      deliverOwnershipStateUpdate(ws, "A");
      const [firstResult, secondResult] = await Promise.all([first(), second()]);

      expect(submissionTypes(ws)).toEqual(["Action"]);
      expect(deliveredEventMarker(firstResult)).toBe("A");
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
      deliverOwnershipStateUpdate(ws, "reply-after-rejection");
      await expect(next()).resolves.toMatchObject({
        resolvedWith: { events: [{ marker: "reply-after-rejection" }] },
      });
    });

    it("issue #9319 sequential Action and Interaction submissions remain usable", async () => {
      const action = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));
      deliverOwnershipStateUpdate(ws, "A");
      await expect(action()).resolves.toMatchObject({
        resolvedWith: { events: [{ marker: "A" }] },
      });

      const interaction = trackRejection(adapter.submitInteraction(interactionSubmission("sequential-one"), 0));
      deliverOwnershipStateUpdate(ws, "B");
      await expect(interaction()).resolves.toMatchObject({
        resolvedWith: { events: [{ marker: "B" }] },
      });

      const nextInteraction = trackRejection(adapter.submitInteraction(interactionSubmission("sequential-two"), 0));
      deliverOwnershipStateUpdate(ws, "C");
      await expect(nextInteraction()).resolves.toMatchObject({
        resolvedWith: { events: [{ marker: "C" }] },
      });
    });
  });

  it("rejects createDraft when the post-handshake setup frame cannot be sent", async () => {
    MockWebSocket.last = null;
    const setupFailingAdapter = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: null });
    const createPromise = setupFailingAdapter.createDraft({
      displayName: "Alice",
      setCodes: ["MKM"],
      kind: "Premier",
      public: true,
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      podSize: 8,
    });
    await Promise.resolve();
    const setupWs = MockWebSocket.last!;
    setupWs.send
      .mockImplementationOnce(() => undefined)
      .mockImplementationOnce(() => {
        throw new Error("InvalidStateError");
      });

    setupWs.dispatchSynthetic("message", SERVER_HELLO);

    await expect(createPromise).rejects.toThrow("Failed to send setup frame");
  });

  it("rejects submitAction and clears pending state when the socket throws on send", async () => {
    const listener = vi.fn();
    adapter.onEvent(listener);
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftMatchStart",
        data: {
          match_id: "r1-t0",
          round: 1,
          game_code: "GAME01",
          full_key: FULL_KEY,
          player_token: "gametok",
          your_player: 0,
          opponent_name: "Bob",
        },
      }),
    );
    ws.send.mockImplementationOnce(() => {
      throw new Error("InvalidStateError");
    });

    await expect(
      adapter.submitAction({ type: "PassPriority" }, 0),
    ).rejects.toThrow("Failed to send action");

    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({ type: "actionPendingChanged", pending: false }),
    );
    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({ type: "error" }),
    );

    const next = adapter.submitInteraction(interactionSubmission("after-send-failure"), 0);
    receive(ws, "ActionRejected", {
      rejection: {
        code: "invalid_action",
        disposition: "invalid",
        message: "Action rejected",
        related_object_ids: [],
      },
    });
    await expect(next).rejects.toMatchObject({ code: "ACTION_REJECTED" });
  });

  it("returns to between_rounds after GameOver", () => {
    // Enter match phase first.
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftMatchStart",
        data: {
          match_id: "r1-t0",
          round: 1,
          game_code: "GAME01",
          full_key: FULL_KEY,
          player_token: "gametok",
          your_player: 0,
          opponent_name: "Bob",
        },
      }),
    );
    expect(adapter.currentPhase).toBe("match");

    // Simulate GameOver.
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "GameOver",
        data: { winner: 0, reason: "opponent conceded" },
      }),
    );

    expect(adapter.currentPhase).toBe("between_rounds");
  });

  it("emits log entries for unsolicited match StateUpdate messages", () => {
    const listener = vi.fn();
    adapter.onEvent(listener);
    const state = matchState("server-draft-unsolicited");
    const logEntries = [debugLogEntry("AI guesses Land")];

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftMatchStart",
        data: {
          match_id: "r1-t0",
          round: 1,
          game_code: "GAME01",
          full_key: FULL_KEY,
          player_token: "gametok",
          your_player: 0,
          opponent_name: "Bob",
        },
      }),
    );

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "GameStarted",
        data: {
          state: matchState("server-draft-started"),
          your_player: 0,
          full_key: FULL_KEY,
        },
      }),
    );
    listener.mockClear();

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "StateUpdate",
        data: {
          state,
          events: [],
          log_entries: logEntries,
          legal_actions: [],
          auto_pass_recommended: false,
          full_key: FULL_KEY,
        },
      }),
    );

    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "gameStateUpdated",
        state,
        events: [],
        logEntries,
      }),
    );
  });

  it("caches GameStarted interaction and per-object action data", async () => {
    const state = matchState("server-draft-started");

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftMatchStart",
        data: {
          match_id: "r1-t0",
          round: 1,
          game_code: "GAME01",
          full_key: FULL_KEY,
          player_token: "gametok",
          your_player: 0,
          opponent_name: "Bob",
        },
      }),
    );
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "GameStarted",
        data: {
          state,
          your_player: 0,
          full_key: FULL_KEY,
          legal_actions_by_object: objectActions,
          viewer_interaction: viewerInteraction,
        },
      }),
    );

    await expect(adapter.getLegalActions()).resolves.toMatchObject({
      legalActionsByObject: objectActions,
      viewerInteraction,
    });
  });

  it("caches StateUpdate interaction and per-object action data", async () => {
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftMatchStart",
        data: {
          match_id: "r1-t0",
          round: 1,
          game_code: "GAME01",
          full_key: FULL_KEY,
          player_token: "gametok",
          your_player: 0,
          opponent_name: "Bob",
        },
      }),
    );
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "GameStarted",
        data: {
          state: matchState("server-draft-started"),
          your_player: 0,
          full_key: FULL_KEY,
        },
      }),
    );
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "StateUpdate",
        data: {
          state: matchState("server-draft-update"),
          events: [],
          full_key: FULL_KEY,
          legal_actions_by_object: objectActions,
          viewer_interaction: viewerInteraction,
        },
      }),
    );

    await expect(adapter.getLegalActions()).resolves.toMatchObject({
      legalActionsByObject: objectActions,
      viewerInteraction,
    });
  });

  it("accepts game frames only for the Full generation announced by the draft", async () => {
    const listener = vi.fn();
    adapter.onEvent(listener);
    const staleKey = { game_code: "GAME01", generation: 6 };
    receive(ws, "DraftMatchStart", {
      match_id: "r1-t0",
      round: 1,
      game_code: "GAME01",
      full_key: FULL_KEY,
      player_token: "gametok",
      your_player: 0,
      opponent_name: "Bob",
    });
    receive(ws, "GameStarted", {
      state: matchState("stale-start"),
      your_player: 0,
      full_key: staleKey,
    });
    await expect(adapter.getState()).rejects.toThrow("No game state available");

    receive(ws, "GameStarted", {
      state: matchState("current-start"),
      your_player: 0,
      full_key: FULL_KEY,
    });
    listener.mockClear();
    receive(ws, "StateUpdate", {
      state: matchState("stale-update"),
      events: [],
      full_key: staleKey,
    });
    receive(ws, "OpponentDisconnected", { grace_seconds: 30, full_key: staleKey });

    await expect(adapter.getState()).resolves.toMatchObject({ label: "current-start" });
    expect(listener).not.toHaveBeenCalled();

    receive(ws, "StateUpdate", {
      state: matchState("current-update"),
      events: [],
      full_key: FULL_KEY,
    });
    receive(ws, "OpponentDisconnected", { grace_seconds: 30, full_key: FULL_KEY });
    receive(ws, "OpponentReconnected", { full_key: FULL_KEY });

    await expect(adapter.getState()).resolves.toMatchObject({ label: "current-update" });
    expect(listener).toHaveBeenCalledWith({
      type: "opponentDisconnected",
      graceSeconds: 30,
    });
    expect(listener).toHaveBeenCalledWith({ type: "opponentReconnected" });
  });

  it("does not send ReportMatchResult on GameOver", () => {
    // Enter match phase.
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftMatchStart",
        data: {
          match_id: "r1-t0",
          round: 1,
          game_code: "GAME01",
          full_key: FULL_KEY,
          player_token: "gametok",
          your_player: 0,
          opponent_name: "Bob",
        },
      }),
    );
    ws.send.mockClear();

    // Simulate GameOver.
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "GameOver",
        data: { winner: 0, reason: "life total" },
      }),
    );

    // Verify no ReportMatchResult was sent.
    for (const call of ws.send.mock.calls) {
      const msg = JSON.parse(call[0] as string);
      expect(msg.type).not.toBe("DraftAction");
      expect(msg.data?.action?.type).not.toBe("ReportMatchResult");
    }
  });

  it("submitPick sends DraftAction with correct seat", async () => {
    ws.send.mockClear();
    const pickPromise = adapter.submitPick("card-001");

    // Verify the sent message.
    expect(ws.send).toHaveBeenCalledWith(
      JSON.stringify({
        type: "DraftAction",
        data: {
          draft_code: "ABCD12",
          action: { type: "Pick", data: { seat: 0, card_instance_ids: ["card-001"] } },
        },
      }),
    );

    // Resolve the pending pick with a DraftStateUpdate.
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftStateUpdate",
        data: { view: createMockDraftView({ pick_number: 1 }) },
      }),
    );

    const result = await pickPromise;
    expect(result.pick_number).toBe(1);
  });

  it("submitDeck sends DraftAction carrying the commander designation", async () => {
    // This file is the run's one TYPECHECK-BLIND payload: `private send(msg:
    // unknown)` means neither `tsc` nor the adapter's own type can see a skew.
    // `JSON.stringify` equality is what makes this discriminate — it reds if
    // `commanders` is OMITTED, MISNAMED (`commander`, `commanderNames`), or
    // MISORDERED relative to `main_deck`. The key order deliberately mirrors
    // the Rust struct's field order (`seat`, `main_deck`, `commanders`); serde
    // does not care, but pinning it makes a future reorder visible.
    //
    // Reach limitation, stated rather than softened: this row asserts the
    // payload is EMITTED. It does not, and cannot, prove that any production
    // path calls the emitter — measured, none does (D5). The row exists so the
    // seam cannot rot silently.
    ws.send.mockClear();
    const deckPromise = adapter.submitDeck(
      ["Plains", "Island"],
      ["Kenrith, the Returned King"],
    );
    await drainArt();

    expect(ws.send).toHaveBeenCalledWith(
      JSON.stringify({
        type: "DraftAction",
        data: {
          draft_code: "ABCD12",
          action: {
            type: "SubmitDeck",
            data: {
              seat: 0,
              main_deck: ["Plains", "Island"],
              commanders: ["Kenrith, the Returned King"],
            },
          },
        },
      }),
    );

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftStateUpdate",
        data: { view: createMockDraftView({ pick_number: 2 }) },
      }),
    );

    const result = await deckPromise;
    expect(result.pick_number).toBe(2);
  });

  describe("owner printing preparation (V9–V11)", () => {
    beforeEach(() => {
      ws.send.mockClear();
      usePreferencesStore.setState({
        artChain: [{ type: "newest" }],
        artOverrides: { Plains: { scryfallId: "a", setCode: "ignored", collectorNumber: "ignored" } },
      });
    });

    it("emits exact main-only owner bytes and clears art on an empty-preference resubmission", async () => {
      const pending = adapter.submitDeck(["Plains", "Island"], ["Plains"]);
      await drainArt();
      expect(ws.send.mock.calls).toEqual([['{"type":"DraftAction","data":{"draft_code":"ABCD12","action":{"type":"SubmitDeck","data":{"seat":0,"main_deck":["Plains","Island"],"commanders":["Plains"],"printings":[{"section":"main_deck","name":"Plains","count":1,"printing":{"set_code":"lea","collector_number":"001"}},{"section":"main_deck","name":"Island","count":1,"printing":{"set_code":"leb","collector_number":"002"}}]}}}}']]);
      expect(artIO.bounds).toHaveBeenCalledWith(MAIN_ART);
      receive(ws, "DraftStateUpdate", { view: createMockDraftView({ status: "Deckbuilding" }) });
      await pending;
      usePreferencesStore.setState({ artChain: [], artOverrides: {} });
      artIO.face.mockClear();
      artIO.bounds.mockClear();
      ws.send.mockClear();
      const replacement = adapter.submitDeck(["Plains"], []);
      await drainArt();
      expect(ws.send.mock.calls).toEqual([['{"type":"DraftAction","data":{"draft_code":"ABCD12","action":{"type":"SubmitDeck","data":{"seat":0,"main_deck":["Plains"],"commanders":[]}}}}']]);
      expect(artIO.face).not.toHaveBeenCalled();
      expect(artIO.bounds).not.toHaveBeenCalled();
      receive(ws, "DraftStateUpdate", { view: createMockDraftView({ pick_number: 8 }) });
      await expect(replacement).resolves.toMatchObject({ pick_number: 8 });
    });

    it("binds one deep preference snapshot and both name arrays before metadata awaits", async () => {
      const metadata = heldArt<PrintingEntry[]>();
      artIO.catalog.mockReturnValue(metadata.promise);
      const readPreferences = vi.spyOn(usePreferencesStore, "getState");
      const main = ["Plains", "Island"];
      const commanders = ["Plains"];
      const pending = adapter.submitDeck(main, commanders);
      await drainArt();
      expect(readPreferences).toHaveBeenCalledTimes(1);
      const preferences = usePreferencesStore.getState();
      preferences.artOverrides.Plains.scryfallId = "b";
      preferences.artChain[0].type = "oldest";
      main[0] = "Mountain";
      commanders[0] = "Island";
      metadata.resolve([ART_B, ART_A]);
      await drainArt();
      expect(readPreferences).toHaveBeenCalledTimes(2); // one explicit read above, no continuation read
      expect(JSON.parse(ws.send.mock.calls[0][0]).data.action.data).toEqual({
        seat: 0, main_deck: ["Plains", "Island"], commanders: ["Plains"], printings: MAIN_ART,
      });
      receive(ws, "DraftStateUpdate", { view: createMockDraftView() });
      await pending;
      const next = adapter.submitDeck(["Plains", "Island"], commanders);
      await drainArt();
      expect(JSON.parse(ws.send.mock.calls[1][0]).data.action.data).toEqual({
        seat: 0, main_deck: ["Plains", "Island"], commanders: ["Island"],
        printings: [
          { ...MAIN_ART[0], printing: MAIN_ART[1].printing },
          { ...MAIN_ART[1], printing: MAIN_ART[0].printing },
        ],
      });
      receive(ws, "DraftStateUpdate", { view: createMockDraftView() });
      await next;
    });

    it.each(["lookup", "bounds"])("omits the WHOLE sidecar at the shared deadline with held %s and never sends late art", async (stage) => {
      vi.useFakeTimers();
      const metadata = heldArt<PrintingEntry[]>();
      const bounds = heldArt<void>();
      artIO.catalog.mockImplementation((name: string) => name === "Island" ? metadata.promise : Promise.resolve([ART_B, ART_A]));
      if (stage === "bounds") artIO.bounds.mockReturnValue(bounds.promise);
      const pending = adapter.submitDeck(["Plains", "Island"], []);
      await drainArt();
      expect(artIO.catalog).toHaveBeenCalledWith("Plains");
      expect(artIO.catalog).toHaveBeenCalledWith("Island");
      await vi.advanceTimersByTimeAsync(3000);
      if (stage === "bounds") {
        metadata.resolve([ART_B, ART_A]);
        await drainArt();
        expect(artIO.bounds).toHaveBeenCalledWith(MAIN_ART);
      }
      await vi.advanceTimersByTimeAsync(1999);
      expect(ws.send).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1);
      const frames = ws.send.mock.calls.filter(([raw]) => JSON.parse(raw).type === "DraftAction");
      expect(frames).toEqual([['{"type":"DraftAction","data":{"draft_code":"ABCD12","action":{"type":"SubmitDeck","data":{"seat":0,"main_deck":["Plains","Island"],"commanders":[]}}}}']]);
      receive(ws, "DraftStateUpdate", { view: createMockDraftView({ pick_number: 9 }) });
      await expect(pending).resolves.toMatchObject({ pick_number: 9 });
      metadata.resolve([ART_B, ART_A]);
      bounds.resolve(undefined);
      await drainArt();
      expect(ws.send.mock.calls.filter(([raw]) => JSON.parse(raw).type === "DraftAction")).toEqual(frames);
    });

    it("retains annotated art when the complete bounds check finishes at 4999 ms", async () => {
      vi.useFakeTimers();
      const bounds = heldArt<void>();
      artIO.bounds.mockReturnValue(bounds.promise);
      const pending = adapter.submitDeck(["Plains", "Island"], []);
      await drainArt();
      expect(artIO.bounds).toHaveBeenCalledWith(MAIN_ART);
      await vi.advanceTimersByTimeAsync(4999);
      expect(ws.send).not.toHaveBeenCalled();
      bounds.resolve(undefined);
      await drainArt();
      expect(JSON.parse(ws.send.mock.calls[0][0]).data.action.data.printings).toEqual(MAIN_ART);
      receive(ws, "DraftStateUpdate", { view: createMockDraftView() });
      await pending;
    });

    it("sends legal names without art on bounds rejection, then accepts a live annotated retry", async () => {
      artIO.bounds.mockRejectedValueOnce(new Error("engine bound"));
      const pending = adapter.submitDeck(["Plains", "Island"], []);
      await drainArt();
      expect(JSON.parse(ws.send.mock.calls[0][0]).data.action.data).toEqual({
        seat: 0, main_deck: ["Plains", "Island"], commanders: [],
      });
      expect(artIO.bounds).toHaveBeenCalledWith(MAIN_ART);
      receive(ws, "DraftStateUpdate", { view: createMockDraftView() });
      await pending;
      const next = adapter.submitDeck(["Plains", "Island"], []);
      await drainArt();
      expect(JSON.parse(ws.send.mock.calls[1][0]).data.action.data.printings).toEqual(MAIN_ART);
      receive(ws, "DraftStateUpdate", { view: createMockDraftView() });
      await next;
    });

    it.each(["DraftStateUpdate", "DraftJoined"])("keeps preparation ownership through unsolicited %s and refuses every competing action", async (type) => {
      const metadata = heldArt<PrintingEntry[]>();
      artIO.catalog.mockReturnValue(metadata.promise);
      const settled = vi.fn();
      const pending = adapter.submitDeck(["Plains", "Island"], []).then(settled, settled);
      await drainArt();
      const view = createMockDraftView({ status: "Deckbuilding", pick_number: 17 });
      receive(ws, type, { draft_code: "ABCD12", player_token: "tok123", seat_index: 0, view });
      await drainArt();
      expect(adapter.currentDraftView).toEqual(view);
      expect(adapter.currentPhase).toBe("deckbuilding");
      expect(settled).not.toHaveBeenCalled();
      const competing = vi.fn();
      void adapter.submitDeck(["Island"], []).then(competing, competing);
      await drainArt();
      expect(competing).toHaveBeenCalledWith(expect.objectContaining({ message: expect.stringContaining("still in flight") }));
      await expect(adapter.submitPick("card")).rejects.toThrow("still in flight");
      await expect(adapter.submitSharedStackDecision(0, "Take")).rejects.toThrow("still in flight");
      await expect(adapter.joinDraft("OTHER1", "Bob")).rejects.toThrow("still in flight");
      expect(ws.send).not.toHaveBeenCalled();
      metadata.resolve([ART_B, ART_A]);
      await drainArt();
      expect(JSON.parse(ws.send.mock.calls[0][0]).data.action.data.printings).toEqual(MAIN_ART);
      receive(ws, "DraftStateUpdate", { view: createMockDraftView({ pick_number: 18 }) });
      await pending;
      expect(settled).toHaveBeenCalledWith(expect.objectContaining({ pick_number: 18 }));
    });

    it("ignores an earlier action's rejection while preparation has sent nothing, then submits on the same slot", async () => {
      const metadata = heldArt<PrintingEntry[]>();
      artIO.catalog.mockReturnValue(metadata.promise);
      const rejectedEvents: string[] = [];
      adapter.onEvent((event) => {
        if (event.type === "draftActionRejected") rejectedEvents.push(event.reason);
      });
      const pending = adapter.submitDeck(["Plains", "Island"], []);
      const readOutcome = trackRejection(pending);
      await drainArt();
      expect(ws.send).not.toHaveBeenCalled();
      // Nothing has been sent, so this rejection can only answer an EARLIER
      // action; it must not abort a submission the server never saw.
      receive(ws, "DraftActionRejected", { reason: "PileNotActive" });
      expect(await readOutcome()).toBe("never settled");
      expect(rejectedEvents).toEqual(["PileNotActive"]);
      expect(ws.send).not.toHaveBeenCalled();
      metadata.resolve([ART_B, ART_A]);
      await drainArt();
      expect(JSON.parse(ws.send.mock.calls[0][0]).data.action.data.printings).toEqual(MAIN_ART);
      receive(ws, "DraftStateUpdate", { view: createMockDraftView({ pick_number: 20 }) });
      await expect(pending).resolves.toMatchObject({ pick_number: 20 });
    });

    it("still rejects a submission the server refuses AFTER it was sent", async () => {
      const pending = adapter.submitDeck(["Plains", "Island"], []);
      await drainArt();
      expect(ws.send.mock.calls).toHaveLength(1);
      receive(ws, "DraftActionRejected", { reason: "DeckInvalid" });
      await expect(pending).rejects.toThrow("DeckInvalid");
    });

    it.each(["draft_code", "player_token", "seat_index", "socket", "closed"])("revalidates %s after preparation rather than sending stale names", async (changed) => {
      const metadata = heldArt<PrintingEntry[]>();
      artIO.catalog.mockReturnValue(metadata.promise);
      const pending = adapter.submitDeck(["Plains"], []);
      const rejected = vi.fn();
      void pending.then(rejected, rejected);
      await drainArt();
      let live = ws;
      if (changed === "socket") {
        const reconnecting = adapter.reconnectDraft();
        live = await completeHandshake();
        // A reconnect settles on the authoritative draft update (upstream #10457).
        receive(live, "DraftStateUpdate", { view: createMockDraftView() });
        await reconnecting;
        live.send.mockClear();
      } else if (changed === "closed") {
        ws.readyState = 3;
      } else {
        receive(ws, "DraftJoined", {
          draft_code: "ABCD12", player_token: "tok123", seat_index: 0,
          [changed]: changed === "seat_index" ? 1 : "REPLACED",
          view: createMockDraftView(),
        });
      }
      metadata.resolve([ART_B, ART_A]);
      await drainArt();
      expect(rejected).toHaveBeenCalledWith(expect.objectContaining({ message: "Draft session changed during deck preparation" }));
      expect(ws.send).not.toHaveBeenCalled();
      expect(live.send).not.toHaveBeenCalled();
      ws.readyState = MockWebSocket.OPEN;
      const next = adapter.submitDeck(["Plains"], []);
      await drainArt();
      expect(JSON.parse(live.send.mock.calls[0][0]).data.action.data.printings).toEqual([MAIN_ART[0]]);
      receive(live, "DraftStateUpdate", { view: createMockDraftView() });
      await next;
    });

    it.each(["close", "error", "dispose"])("cancels held work on %s without sending or clearing a successor's slot", async (cancel) => {
      const metadata = heldArt<PrintingEntry[]>();
      artIO.catalog.mockReturnValueOnce(metadata.promise);
      const pending = adapter.submitDeck(["Plains"], []);
      const rejected = expect(pending).rejects.toThrow();
      await drainArt();
      if (cancel === "close") ws.dispatchSynthetic("close");
      else if (cancel === "error") ws.onerror?.();
      else adapter.dispose();
      await rejected;
      let live = ws;
      let successor: Promise<DraftPlayerView>;
      if (cancel === "dispose") {
        successor = adapter.joinDraft("NEXT12", "Next");
        live = await completeHandshake();
      } else {
        successor = adapter.submitPick("successor");
      }
      const successorSettled = vi.fn();
      void successor.then(successorSettled, successorSettled);
      const before = live.send.mock.calls.slice();
      metadata.resolve([ART_B, ART_A]);
      await drainArt();
      expect(live.send.mock.calls).toEqual(before);
      expect(successorSettled).not.toHaveBeenCalled();
      receive(live, cancel === "dispose" ? "DraftJoined" : "DraftStateUpdate", {
        draft_code: "NEXT12", player_token: "next-token", seat_index: 1,
        view: createMockDraftView({ pick_number: 19 }),
      });
      await drainArt();
      expect(successorSettled).toHaveBeenCalledWith(expect.objectContaining({ pick_number: 19 }));
      await expect(successor).resolves.toMatchObject({ pick_number: 19 });
      expect(ws.send.mock.calls.some(([raw]) => JSON.parse(raw).data?.action?.type === "SubmitDeck")).toBe(false);
    });

    it("releases owned callbacks after send or preference-capture failure", async () => {
      ws.send.mockImplementationOnce(() => { throw new Error("send failed"); });
      await expect(adapter.submitDeck(["Plains"], [])).rejects.toThrow("Failed to send draft action");
      const preferences = vi.spyOn(usePreferencesStore, "getState").mockImplementationOnce(() => { throw new Error("capture failed"); });
      await expect(adapter.submitDeck(["Plains"], [])).rejects.toThrow("capture failed");
      preferences.mockRestore();
      const next = adapter.submitDeck(["Plains"], []);
      await drainArt();
      expect(JSON.parse(ws.send.mock.lastCall![0]).data.action.data.printings).toEqual([MAIN_ART[0]]);
      receive(ws, "DraftStateUpdate", { view: createMockDraftView() });
      await next;
    });
  });

  /**
   * ONE SLOT, ONE ACTION. `draftResolve`/`draftReject` is a single pair, and
   * every submit path used to assign straight into it. A second action
   * overwrote the first's callbacks; the next `DraftStateUpdate` then resolved
   * only the survivor and nulled both, so the first caller's promise never
   * settled at all -- a permanently pending `await`, not a lost result.
   *
   * REVERT-FAILING: drop the `claimDraftAction` guard back to bare assignment
   * and the first leg reds (the second action is accepted) and the last leg
   * hangs until the test times out (the pick never settles).
   */
  it("refuses a second draft action while one is still in flight", async () => {
    const pickPromise = adapter.submitPick("card-inflight");

    // The intruder is refused IMMEDIATELY and by name, rather than silently
    // taking the slot.
    await expect(adapter.submitDeck(["deck-card"], [])).rejects.toThrow(
      /Another draft action is still in flight; SubmitDeck was not sent/,
    );

    // Reach guard: exactly one action reached the wire. Without this the
    // rejection above could be satisfied by an adapter that sends nothing.
    const draftActions = ws.send.mock.calls.filter(([raw]) =>
      typeof raw === "string" && raw.includes("\"DraftAction\""));
    expect(draftActions).toHaveLength(1);

    // THE POINT. The first action is untouched by the refusal and still settles.
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftStateUpdate",
        data: { view: createMockDraftView({ pick_number: 7 }) },
      }),
    );
    await expect(pickPromise).resolves.toMatchObject({ pick_number: 7 });
  });

  /**
   * The paired positive: the guard is a single-flight gate, not a one-shot
   * latch. Every settle site nulls both callbacks together, which is what
   * releases it — so the action after a completed one must be accepted. Without
   * this, "refuse everything after the first action" would pass the test above
   * and break the draft entirely.
   */
  it("accepts the next draft action once the previous one has settled", async () => {
    const first = adapter.submitPick("card-first");
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftStateUpdate",
        data: { view: createMockDraftView({ pick_number: 1 }) },
      }),
    );
    await first;

    const second = adapter.submitPick("card-second");
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftStateUpdate",
        data: { view: createMockDraftView({ pick_number: 2 }) },
      }),
    );
    await expect(second).resolves.toMatchObject({ pick_number: 2 });
  });

  /**
   * THE RELEASE PATH, ON THE ROUTE THAT REJECTS.
   *
   * This is the leg whose failure is WORSE than the bug the guard fixes. The
   * gate reads "in flight" off the callback pair itself, so a settle site that
   * rejects without nulling BOTH callbacks leaves the slot claimed forever and
   * every later action for the rest of the session is refused with "Another
   * draft action is still in flight" -- a permanent wedge, where the unguarded
   * behaviour merely stranded one promise.
   *
   * `DraftActionRejected` is the live rejection route (a refused pick, a refused
   * shared-stack decision), so it is the one that has to release.
   */
  it("releases the action slot when the server rejects the action", async () => {
    const rejected = adapter.submitPick("card-refused");
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({ type: "DraftActionRejected", data: { reason: "PileNotActive" } }),
    );
    await expect(rejected).rejects.toThrow("PileNotActive");

    // THE CLAIM: the next action is accepted, not refused as "still in flight".
    const next = adapter.submitPick("card-after-refusal");
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftStateUpdate",
        data: { view: createMockDraftView({ pick_number: 9 }) },
      }),
    );
    await expect(next).resolves.toMatchObject({ pick_number: 9 });
  });

  it("DraftStateUpdate resolves pending pick promise", async () => {
    const pickPromise = adapter.submitPick("card-002");

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftStateUpdate",
        data: { view: createMockDraftView({ pick_number: 2 }) },
      }),
    );

    const result = await pickPromise;
    expect(result.pick_number).toBe(2);
  });

  it("DraftStateUpdate preserves workspace presentation metadata", async () => {
    const pickPromise = adapter.submitPick("card-003");
    const workspaceMetadata: Pick<
      DraftPlayerView["pool_groups"],
      "workspace_capabilities" | "workspace_row_classification"
    > = {
      workspace_capabilities: {
        rarity_group_order: ["mythic", "rare", "uncommon", "common", "rarity_other"],
      },
      workspace_row_classification: {
        creature_instance_ids: ["creature-1", "creature-2"],
        noncreature_instance_ids: ["instant-1"],
      },
    };
    const view = createMockDraftView({
      pick_number: 3,
      pool_groups: {
        ...EMPTY_DRAFT_POOL_GROUPS,
        ...workspaceMetadata,
      },
    });

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftStateUpdate",
        data: { view },
      }),
    );

    const result = await pickPromise;
    expect(result.pool_groups).toMatchObject(workspaceMetadata);
    expect(adapter.currentDraftView?.pool_groups).toMatchObject(workspaceMetadata);
  });

  it("DraftTimerSync emits timerSync event", () => {
    const listener = vi.fn();
    adapter.onEvent(listener);

    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftTimerSync",
        data: { remaining_ms: 5000 },
      }),
    );

    expect(listener).toHaveBeenCalledWith({
      type: "timerSync",
      remainingMs: 5000,
    });
  });

  it("dispose closes WebSocket and clears state", () => {
    adapter.dispose();

    expect(ws.close).toHaveBeenCalled();
    expect(adapter.currentPhase).toBe("lobby");
    expect(adapter.playerId).toBeNull();
    expect(adapter.currentDraftView).toBeNull();
    expect(adapter.currentMatchId).toBeNull();
  });

  // `dispose()` used to null all three handle pairs instead of rejecting them,
  // so every caller awaiting a server reply was orphaned — and a gameplay
  // caller holds the module-level dispatch mutex while it waits.
  it("dispose rejects the in-flight submit, draft and init promises", async () => {
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftMatchStart",
        data: {
          match_id: "r1-t0",
          round: 1,
          game_code: "GAME01",
          full_key: FULL_KEY,
          player_token: "gametok",
          your_player: 0,
          opponent_name: "Bob",
        },
      }),
    );

    const submit = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));
    const pick = trackRejection(adapter.submitPick("card-1"));
    const reconnect = trackRejection(adapter.reconnectDraft());

    adapter.dispose();

    // Asserted as one tuple rather than three statements: a per-leg assertion
    // would abort on the first orphan and leave the other two unprobeable.
    expect([await submit(), await pick(), await reconnect()]).toMatchObject([
      { code: "WS_CLOSED", message: "Adapter disposed during action", recoverable: true },
      { code: "WS_CLOSED", message: "Adapter disposed during draft operation", recoverable: true },
      { code: "WS_CLOSED", message: "Adapter disposed before draft started", recoverable: true },
    ]);
    await expect(adapter.submitInteraction(interactionSubmission("after-dispose"), 0)).rejects.toMatchObject({
      code: "WS_ERROR",
    });
  });

  it("releases the submission slot after a socket close", async () => {
    enterAcceptedFullMatch(adapter, ws);
    const pending = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));

    ws.readyState = 3; // CLOSED
    ws.dispatchSynthetic("close");

    expect(await pending()).toMatchObject({ code: "WS_CLOSED" });
    await expect(adapter.submitInteraction(interactionSubmission("after-close"), 0)).rejects.toMatchObject({
      code: "WS_ERROR",
    });
  });

  it("DraftOver sets phase to complete", () => {
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftOver",
        data: {
          standings: [
            { seat_index: 0, display_name: "Alice", match_wins: 3, match_losses: 0, game_wins: 6, game_losses: 1 },
          ],
        },
      }),
    );

    expect(adapter.currentPhase).toBe("complete");
  });

  it("updates phase from DraftStateUpdate view status", () => {
    ws.dispatchSynthetic(
      "message",
      JSON.stringify({
        type: "DraftStateUpdate",
        data: { view: createMockDraftView({ status: "Deckbuilding" }) },
      }),
    );

    expect(adapter.currentPhase).toBe("deckbuilding");
  });

  // Issue #5913, fourth transport. `ServerDraftAdapter` is a full
  // `EngineAdapter` once the pod's game starts, so the engine's stale verdict
  // must classify here exactly as it does for WASM, WebSocket and P2P — a
  // generic ACTION_REJECTED would leave a server-hosted draft player seeing the
  // red error every other seat no longer sees (`dispatchAction` suppresses only
  // STALE_ACTION).
  describe("game-phase action rejections", () => {
    beforeEach(() => {
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "DraftMatchStart",
          data: {
            match_id: "r1-t0",
            round: 1,
            game_code: "GAME01",
            full_key: FULL_KEY,
            player_token: "gametok",
            your_player: 0,
            opponent_name: "Bob",
          },
        }),
      );
    });

    it("classifies a stale ReorderHand rejection as STALE_ACTION", async () => {
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

    it("still surfaces a non-stale rejection as a recoverable ACTION_REJECTED", async () => {
      const pending = adapter.submitAction({ type: "PassPriority" }, 0);
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ActionRejected",
          data: { rejection: { code: "invalid_action", disposition: "invalid", message: "That action is not valid in the current game state.", related_object_ids: [] } },
        }),
      );

      await expect(pending).rejects.toMatchObject({
        code: "ACTION_REJECTED",
        recoverable: true,
      });
    });

    // The preview path answers against the same engine state an action would,
    // so it can carry the same stale verdict and must classify identically.
    it("classifies a stale mana-payment preview rejection as STALE_ACTION", async () => {
      const pending = adapter.previewManaPayment(
        { type: "ReorderHand", data: { order: [1, 2, 3] } },
        0,
      );
      const calls = ws.send.mock.calls;
      const sent = JSON.parse(calls[calls.length - 1][0] as string);
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ManaPaymentPreviewRejected",
          data: {
            request_id: sent.data.request_id,
            rejection: { code: "stale_action", disposition: "stale", message: "That action is based on outdated game state.", related_object_ids: [] },
          },
        }),
      );

      await expect(pending).rejects.toMatchObject({
        code: "STALE_ACTION",
        recoverable: false,
      });
    });

    it("still surfaces a non-stale preview rejection as a recoverable ACTION_REJECTED", async () => {
      const pending = adapter.previewManaPayment({ type: "PassPriority" }, 0);
      const calls = ws.send.mock.calls;
      const sent = JSON.parse(calls[calls.length - 1][0] as string);
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "ManaPaymentPreviewRejected",
          data: { request_id: sent.data.request_id, rejection: { code: "invalid_action", disposition: "invalid", message: "That action is not valid in the current game state.", related_object_ids: [] } },
        }),
      );

      await expect(pending).rejects.toMatchObject({
        code: "ACTION_REJECTED",
        recoverable: true,
      });
    });

    it("settles operational action and matching preview failures", async () => {
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
      const calls = ws.send.mock.calls;
      const sent = JSON.parse(calls[calls.length - 1][0] as string);
      const settled = vi.fn();
      void preview.then(settled, settled);
      ws.dispatchSynthetic(
        "message",
        JSON.stringify({ type: "ManaPaymentPreviewFailed", data: { request_id: sent.data.request_id + 1, message: "other preview failed" } }),
      );
      await Promise.resolve();
      expect(settled).not.toHaveBeenCalled();
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

    /**
     * An allocation whose segments are UNEQUAL and whose `choiceId` order is
     * NOT the candidate publication order, so a sort or a canonicalisation in
     * the adapter layer is caught rather than coinciding.
     */
    const cid = (id: string) => id as InteractionChoiceId;
    const previewRequest = {
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

    const previewAnswer = (requestId: string) => ({
      requestId,
      interactionId: "interaction-1",
      status: { type: "confirmable" },
      progress: { selected: 3, minimum: 1, maximum: 3, aggregate: 6, confirmable: true },
      outcome: "advanced",
      summaries: ["confirmAvailable", "progress"],
    });

    function sentPreviewFrame() {
      const calls = ws.send.mock.calls;
      for (let i = calls.length - 1; i >= 0; i--) {
        const parsed = JSON.parse(calls[i][0] as string);
        if (parsed.type === "PreviewInteraction") return parsed;
      }
      throw new Error("no PreviewInteraction frame was sent");
    }

    // Row 8, draft-adapter leg.
    it("sends the authored request verbatim and resolves its own answer", async () => {
      ws.send.mockClear();
      const pending = adapter.previewInteraction(previewRequest, 0);

      const frame = sentPreviewFrame();
      expect(frame.type).toBe("PreviewInteraction");
      expect(frame.data.request).toEqual(previewRequest);
      const pin = frame.data.request.response.data.pins[0];
      // Reach guard: more than one segment, so dropping all but the first fails.
      expect(pin.amounts.length).toBeGreaterThan(1);
      expect(pin.amounts).toEqual([
        { choiceId: "choice-c", amount: 3 },
        { choiceId: "choice-a", amount: 1 },
        { choiceId: "choice-b", amount: 2 },
      ]);
      expect(pin.amounts.map((a: { choiceId: string }) => a.choiceId)).toEqual(pin.choiceIds);

      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "InteractionPreview",
          data: { preview: previewAnswer("preview-req-1") },
        }),
      );
      await expect(pending).resolves.toMatchObject({ requestId: "preview-req-1" });
    });

    // Row 12, close-site leg on this adapter.
    it("rejects in-flight previews on socket close, keeping answered ones", async () => {
      const answered = adapter.previewInteraction(previewRequest, 0);
      const unanswered = adapter.previewInteraction(
        { ...previewRequest, requestId: "preview-req-2" as PreviewRequestId },
        0,
      );

      ws.dispatchSynthetic(
        "message",
        JSON.stringify({
          type: "InteractionPreview",
          data: { preview: previewAnswer("preview-req-1") },
        }),
      );
      await expect(answered).resolves.toMatchObject({ requestId: "preview-req-1" });

      ws.dispatchSynthetic("close");
      await expect(answered).resolves.toMatchObject({ requestId: "preview-req-1" });
      await expect(unanswered).rejects.toMatchObject({
        code: "WS_CLOSED",
        message: "Connection closed during interaction preview",
      });
    });

    // Row 12, dispose-site leg. THIS is the site a single-site wiring drops —
    // measured on the mana twin, deleting only the `dispose` call leaves this
    // promise never settling while the close-site leg above still passes.
    it("rejects in-flight previews when the adapter is disposed", async () => {
      const pending = adapter.previewInteraction(previewRequest, 0);
      adapter.dispose();
      await expect(pending).rejects.toMatchObject({
        code: "WS_CLOSED",
        message: "Adapter disposed during interaction preview",
      });
    });

    // The draft adapter's own precondition, which its WS twin does not carry.
    it("refuses a preview outside the match phase", async () => {
      const lobbyAdapter = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: null });
      await expect(lobbyAdapter.previewInteraction(previewRequest, 0)).rejects.toMatchObject({
        code: "PHASE_ERROR",
      });
      // Reach guard: the adapter under test IS in the match phase, so the
      // refusal above is the precondition and not a universally-throwing method.
      expect(adapter.currentPhase).toBe("match");
    });
  });
});

describe("ServerDraftAdapter game-command correlation", () => {
  const keep: GameAction = { type: "MulliganDecision", data: { choice: { type: "Keep" } } };

  /** One draft seat that has reached an accepted Full match on its own socket. */
  async function seatInMatch(hello: string): Promise<{ seat: ServerDraftAdapter; socket: MockWebSocket }> {
    MockWebSocket.last = null;
    const seat = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: null });
    const created = seat.createDraft({
      displayName: "Seat",
      setCodes: ["MKM"],
      kind: "Premier",
      public: true,
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      podSize: 2,
    });
    const socket = await completeHandshake(hello);
    receive(socket, "DraftCreated", { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 });
    await created;
    enterAcceptedFullMatch(seat, socket);
    return { seat, socket };
  }

  function sentCommandId(socket: MockWebSocket): unknown {
    const frame = socket.send.mock.calls
      .map(([raw]) => JSON.parse(raw as string) as { type?: string; command_id?: unknown })
      .filter((sent) => sent.type === "Action" || sent.type === "Interaction")
      .pop();
    if (!frame) throw new Error("No game submission was sent");
    return frame.command_id;
  }

  function deliverUpdate(socket: MockWebSocket, marker: string, commandId?: unknown): void {
    socket.dispatchSynthetic("message", JSON.stringify({
      type: "StateUpdate",
      command_id: commandId,
      data: { state: matchState(`reply-${marker}`), events: [{ marker }], full_key: FULL_KEY },
    }));
  }

  describe("with CommandIdV1 negotiated", () => {
    it("keeps both seats' simultaneous Keep submissions owned when the peer's StateUpdate arrives", async () => {
      const seats = [await seatInMatch(CORRELATED_HELLO), await seatInMatch(CORRELATED_HELLO)];
      try {
        const outcomes = seats.map(({ seat }, player) => trackRejection(seat.submitAction(keep, player)));
        const ids = seats.map(({ socket }) => sentCommandId(socket));
        expect(ids).toEqual([expect.any(Number), expect.any(Number)]);

        // Seat 0's Keep completes and is broadcast to seat 1 without seat 1's id.
        deliverUpdate(seats[0].socket, "keep-0", ids[0]);
        deliverUpdate(seats[1].socket, "keep-0-as-peer-update");
        const first = await Promise.all(outcomes.map((read) => read()));
        expect(deliveredEventMarker(first[0])).toBe("keep-0");
        expect(first[1]).toBe("never settled");
        await expect(seats[1].seat.submitAction(keep, 1)).rejects.toMatchObject({
          code: AdapterErrorCode.ACTION_NOT_SENT,
        });

        deliverUpdate(seats[1].socket, "keep-1", ids[1]);
        expect(deliveredEventMarker(await outcomes[1]())).toBe("keep-1");
      } finally {
        seats.forEach(({ seat }) => seat.dispose());
      }
    });

    it("renders a peer StateUpdate as ambient state instead of settling the command", async () => {
      const { seat, socket } = await seatInMatch(CORRELATED_HELLO);
      try {
        const events = vi.fn();
        seat.onEvent(events);
        const outcome = trackRejection(seat.submitAction(keep, 0));
        deliverUpdate(socket, "peer-update");
        expect(await outcome()).toBe("never settled");
        expect(events).toHaveBeenCalledWith(expect.objectContaining({ type: "gameStateUpdated" }));
        expect(events).not.toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
      } finally {
        seat.dispose();
      }
    });

    it("settles only on its own id for ActionNoOp, ActionRejected and ActionFailed", async () => {
      const { seat, socket } = await seatInMatch(CORRELATED_HELLO);
      try {
        const events = vi.fn();
        seat.onEvent(events);
        const outcome = trackRejection(seat.submitAction(keep, 0));
        const id = sentCommandId(socket);
        if (typeof id !== "number") throw new Error("Expected a numeric command ID");
        const data = {
          message: "Another request failed",
          rejection: { code: "invalid_action", disposition: "invalid", message: "Other rejection", related_object_ids: [] },
        };
        const errors = () => events.mock.calls.filter(([event]) => event.type === "error").length;
        for (const type of ["ActionNoOp", "ActionRejected", "ActionFailed"]) {
          receive(socket, type, data);
        }
        // An id-less ActionFailed still surfaces once; none of them settles or releases the slot.
        expect(errors()).toBe(1);
        for (const type of ["ActionNoOp", "ActionRejected", "ActionFailed"]) {
          socket.dispatchSynthetic("message", JSON.stringify({ type, data, command_id: id + 1 }));
        }
        expect(errors()).toBe(1);
        expect(await outcome()).toBe("never settled");
        expect(events).not.toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });

        socket.dispatchSynthetic("message", JSON.stringify({ type: "ActionNoOp", command_id: id }));
        await expect(outcome()).resolves.toEqual({ resolvedWith: { events: [], log_entries: [] } });
        expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
      } finally {
        seat.dispose();
      }
    });

    it("settles a withheld command as applied only on its own id and surfaces the reason", async () => {
      const { seat, socket } = await seatInMatch(CORRELATED_HELLO);
      try {
        const events = vi.fn();
        seat.onEvent(events);
        const outcome = trackRejection(seat.submitAction(keep, 0));
        const id = sentCommandId(socket);
        if (typeof id !== "number") throw new Error("Expected a numeric command ID");
        const withheld = (commandId: number) => socket.dispatchSynthetic("message", JSON.stringify({
          type: "StateWithheld",
          command_id: commandId,
          data: { full_key: FULL_KEY, state_revision: 6, withheld: { type: "StateUpdate" }, reason: "too large to deliver" },
        }));
        withheld(id + 1);
        expect(await outcome()).toBe("never settled");
        withheld(id);
        await expect(outcome()).resolves.toEqual({ resolvedWith: { events: [], log_entries: [] } });
        expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
        expect(events).toHaveBeenCalledWith({ type: "error", message: "too large to deliver" });
      } finally {
        seat.dispose();
      }
    });

    it("rejects with a matching ActionRejected and ignores that command's late echo afterwards", async () => {
      const { seat, socket } = await seatInMatch(CORRELATED_HELLO);
      try {
        const first = seat.submitAction(keep, 0);
        const id = sentCommandId(socket);
        socket.dispatchSynthetic("message", JSON.stringify({
          type: "ActionRejected",
          command_id: id,
          data: { rejection: { code: "invalid_action", disposition: "invalid", message: "No", related_object_ids: [] } },
        }));
        await expect(first).rejects.toMatchObject({ code: "ACTION_REJECTED" });

        const next = trackRejection(seat.submitAction({ type: "PassPriority" }, 0));
        const nextId = sentCommandId(socket);
        expect(nextId).not.toEqual(id);
        deliverUpdate(socket, "late-echo", id);
        expect(await next()).toBe("never settled");
        deliverUpdate(socket, "own", nextId);
        expect(deliveredEventMarker(await next())).toBe("own");
      } finally {
        seat.dispose();
      }
    });

    it("never reuses a command id after the slot is dropped by a socket close", async () => {
      const { seat, socket } = await seatInMatch(CORRELATED_HELLO);
      try {
        const dropped = seat.submitAction(keep, 0);
        const oldId = sentCommandId(socket);
        const closed = expect(dropped).rejects.toMatchObject({ code: "WS_CLOSED" });
        socket.dispatchSynthetic("close");
        await closed;
        socket.readyState = MockWebSocket.OPEN;
        const next = trackRejection(seat.submitAction(keep, 0));
        expect(sentCommandId(socket)).not.toEqual(oldId);
        expect(await next()).toBe("never settled");
      } finally {
        seat.dispose();
      }
    });
  });

  describe("without the capability", () => {
    it("sends no command_id and keeps settling on the first completion frame", async () => {
      const { seat, socket } = await seatInMatch(SERVER_HELLO);
      try {
        const events = vi.fn();
        seat.onEvent(events);
        const outcome = trackRejection(seat.submitAction(keep, 0));
        expect(sentCommandId(socket)).toBeUndefined();
        // Any StateUpdate is the legacy acknowledgement, even one carrying an unrelated id.
        deliverUpdate(socket, "legacy-first", 99);
        expect(deliveredEventMarker(await outcome())).toBe("legacy-first");
        expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });

        // With nothing pending, legacy still releases the indicator and surfaces a failure.
        events.mockClear();
        receive(socket, "ActionNoOp", undefined);
        expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
        events.mockClear();
        receive(socket, "ActionRejected", {
          rejection: { code: "invalid_action", disposition: "invalid", message: "Nothing pending", related_object_ids: [] },
        });
        expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
        events.mockClear();
        receive(socket, "ActionFailed", { message: "Nothing pending" });
        expect(events).toHaveBeenCalledWith({ type: "error", message: "Nothing pending" });
      } finally {
        seat.dispose();
      }
    });
  });

  describe("pending-indicator bookkeeping", () => {
    it("frees the slot when a listener throws while the submission is announced", async () => {
      const { seat, socket } = await seatInMatch(CORRELATED_HELLO);
      try {
        let armed = true;
        seat.onEvent((event) => {
          if (event.type === "actionPendingChanged" && event.pending && armed) {
            armed = false;
            throw new Error("listener failed");
          }
        });
        await expect(seat.submitAction(keep, 0)).rejects.toThrow("listener failed");
        expect(submissionTypes(socket)).toEqual([]);

        const next = trackRejection(seat.submitAction(keep, 0));
        expect(submissionTypes(socket)).toEqual(["Action"]);
        deliverUpdate(socket, "after-throw", sentCommandId(socket));
        expect(deliveredEventMarker(await next())).toBe("after-throw");
      } finally {
        seat.dispose();
      }
    });

    it.each(["close", "dispose"] as const)(
      "sends nothing when a listener ends the connection with %s while the submission is announced",
      async (ending) => {
        const { seat, socket } = await seatInMatch(CORRELATED_HELLO);
        try {
          const events = vi.fn();
          seat.onEvent(events);
          seat.onEvent((event) => {
            if (event.type !== "actionPendingChanged" || !event.pending) return;
            if (ending === "close") {
              socket.readyState = 3; // CLOSED
              socket.dispatchSynthetic("close");
            } else {
              seat.dispose();
            }
          });
          const outcome = trackRejection(seat.submitAction(keep, 0));
          expect(await outcome()).toMatchObject({ code: "WS_CLOSED" });
          expect(submissionTypes(socket)).toEqual([]);
          // A send attempted against the ended connection would also report a transport error.
          expect(events).not.toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
        } finally {
          seat.dispose();
        }
      },
    );

    it("announces the released indicator when the socket closes under a pending submission", async () => {
      const { seat, socket } = await seatInMatch(CORRELATED_HELLO);
      try {
        const events = vi.fn();
        seat.onEvent(events);
        const pending = trackRejection(seat.submitAction(keep, 0));
        expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: true });
        events.mockClear();
        socket.readyState = 3; // CLOSED
        socket.dispatchSynthetic("close");
        expect(await pending()).toMatchObject({ code: "WS_CLOSED" });
        expect(events).toHaveBeenCalledWith({ type: "actionPendingChanged", pending: false });
      } finally {
        seat.dispose();
      }
    });
  });
});

describe("draft attachment settlement", () => {
  const settings = {
    displayName: "Alice",
    setCodes: ["MKM"],
    kind: "Premier" as const,
    public: true,
    tournamentFormat: "Swiss" as const,
    podPolicy: "Competitive" as const,
    podSize: 8,
  };
  const credentials = { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 };
  const lobbyHello = JSON.stringify({
    ...JSON.parse(SERVER_HELLO),
    data: { ...JSON.parse(SERVER_HELLO).data, mode: "LobbyOnly" },
  });
  let adapter: ServerDraftAdapter;
  let established: MockWebSocket;

  function frames(ws: MockWebSocket): string[] {
    return ws.send.mock.calls.map(([raw]) => {
      // Outbound frames are authored by the adapter, not external input.
      const frame: { type: string } = JSON.parse(raw as string);
      return frame.type;
    });
  }

  function start(kind: "create" | "reconnect"): () => Promise<unknown> {
    return trackRejection(kind === "create" ? adapter.createDraft(settings) : adapter.reconnectDraft());
  }

  function reply(ws: MockWebSocket, kind: "create" | "reconnect"): void {
    expect(ws.onmessage).toBeTypeOf("function");
    if (kind === "create") receive(ws, "DraftCreated", credentials);
    else receive(ws, "DraftStateUpdate", { view: createMockDraftView() });
  }

  beforeEach(async () => {
    adapter = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: null });
    const created = adapter.createDraft(settings);
    established = await completeHandshake();
    receive(established, "DraftCreated", credentials);
    await created;
  });

  afterEach(() => {
    adapter.dispose();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("1 resolves draft-only reconnect after installing its view and phase", async () => {
    const read = start("reconnect");
    const ws = await completeHandshake();
    expect(frames(ws)).toContain("ReconnectDraft");
    const view = createMockDraftView({ status: "Deckbuilding" });
    receive(ws, "DraftStateUpdate", { view });
    expect(await read()).toEqual({ resolvedWith: undefined });
    expect(adapter.currentDraftView).toEqual(view);
    expect(adapter.currentPhase).toBe("deckbuilding");
  });

  it("2 waits through match progress until the reconnect draft update", async () => {
    const read = start("reconnect");
    const ws = await completeHandshake();
    enterAcceptedFullMatch(adapter, ws);
    expect(await read()).toBe("never settled");
    receive(ws, "DraftStateUpdate", { view: createMockDraftView({ status: "MatchInProgress" }) });
    expect(await read()).toEqual({ resolvedWith: undefined });
    expect(adapter.currentPhase).toBe("match");
  });

  it("4 rejects once then accepts authoritative state without closing transport", async () => {
    const events = vi.fn();
    adapter.onEvent(events);
    const read = start("reconnect");
    const ws = await completeHandshake();
    receive(ws, "DraftActionRejected", { reason: "seat already reattached" });
    const rejected = await read();
    expect(rejected).toMatchObject({
      code: "ACTION_REJECTED", message: "seat already reattached", recoverable: true,
    });
    const view = createMockDraftView({ status: "Deckbuilding" });
    receive(ws, "DraftStateUpdate", { view });
    expect(await read()).toBe(rejected);
    expect(adapter.currentDraftView).toEqual(view);
    expect(adapter.currentPhase).toBe("deckbuilding");
    expect(events).toHaveBeenCalledWith({ type: "draftViewUpdated", view });
    expect(ws.close).not.toHaveBeenCalled();
  });

  it.each(["create", "reconnect"] as const)("%s C2/5 rejects LobbyOnly Error and retains the error event", async (kind) => {
    const events = vi.fn();
    adapter.onEvent(events);
    const read = start(kind);
    const ws = await completeHandshake(lobbyHello);
    expect(frames(ws)).toContain(kind === "create" ? "CreateDraftWithSettings" : "ReconnectDraft");
    receive(ws, "Error", { message: "Drafts unavailable in LobbyOnly mode" });
    expect(await read()).toMatchObject({
      code: "WS_ERROR", message: "Drafts unavailable in LobbyOnly mode",
    });
    expect(events).toHaveBeenCalledWith({ type: "error", message: "Drafts unavailable in LobbyOnly mode" });
  });

  it("C1 rejects create on DraftActionRejected", async () => {
    const read = start("create");
    const ws = await completeHandshake();
    receive(ws, "DraftActionRejected", { reason: "invalid pod" });
    expect(await read()).toMatchObject({ code: "ACTION_REJECTED", message: "invalid pod", recoverable: true });
  });

  it.each([
    ["create", "create"], ["create", "reconnect"],
    ["reconnect", "create"], ["reconnect", "reconnect"],
  ] as const)("6 supersedes %s with %s without retiring a sent socket", async (firstKind, nextKind) => {
    const first = start(firstKind);
    const a = await completeHandshake();
    const next = start(nextKind);
    expect(await first()).toMatchObject({ code: "WS_CLOSED", message: expect.stringMatching(/supersed/i), recoverable: true });
    expect(a.close).not.toHaveBeenCalled();
    const b = await completeHandshake();
    receive(b, nextKind === "create" ? "DraftStateUpdate" : "DraftCreated",
      nextKind === "create" ? { view: createMockDraftView() } : credentials);
    expect(await next()).toBe("never settled");
    reply(b, nextKind);
    expect(await next()).toEqual({ resolvedWith: undefined });
  });

  it("7a closes a queued successful handshake without late hello or setup", async () => {
    const events = vi.fn();
    adapter.onEvent(events);
    const first = start("reconnect");
    const a = MockWebSocket.last!;
    expect(a.onmessage).toBeTypeOf("function");
    a.dispatchSynthetic("message", SERVER_HELLO);
    const next = start("create");
    const b = await completeHandshake();
    expect(frames(a)).toEqual(["ClientHello"]);
    expect(a.close).toHaveBeenCalled();
    expect(events.mock.calls.filter(([event]) => event.type === "serverHello")).toHaveLength(1);
    expect(await first()).toMatchObject({ message: expect.stringMatching(/supersed/i) });
    reply(b, "create");
    expect(await next()).toEqual({ resolvedWith: undefined });
  });

  it("7b queued handshake failure cannot reject its replacement", async () => {
    const first = start("reconnect");
    const a = MockWebSocket.last!;
    expect(a.onerror).toBeTypeOf("function");
    a.onerror!();
    const next = start("create");
    const b = await completeHandshake();
    expect(await next()).toBe("never settled");
    expect(await first()).toMatchObject({ message: expect.stringMatching(/supersed/i) });
    reply(b, "create");
    expect(await next()).toEqual({ resolvedWith: undefined });
  });

  it.each(["DraftStateUpdate", "DraftActionRejected"] as const)(
    "8a stale %s does not settle the current reconnect",
    async (type) => {
      const read = start("reconnect");
      const b = await completeHandshake();
      expect(established.onmessage).toBeTypeOf("function");
      const staleView = createMockDraftView({ pick_number: 9 });
      receive(established, type, type === "DraftStateUpdate" ? { view: staleView } : { reason: "old failure" });
      expect(await read()).toBe("never settled");
      if (type === "DraftStateUpdate") expect(adapter.currentDraftView).toEqual(staleView);
      reply(b, "reconnect");
      expect(await read()).toEqual({ resolvedWith: undefined });
    },
  );

  it.each(["error", "close"] as const)("8b/8c stale %s preserves current init", async (type) => {
    const events = vi.fn();
    adapter.onEvent(events);
    const read = start("reconnect");
    const b = await completeHandshake();
    const callback = type === "error" ? established.onerror : established.onclose;
    expect(callback).toBeTypeOf("function");
    callback!();
    expect(await read()).toBe("never settled");
    expect(events).not.toHaveBeenCalledWith({ type: "disconnected" });
    reply(b, "reconnect");
    expect(await read()).toEqual({ resolvedWith: undefined });
  });

  it("10 listener exception rejects its owner and closes the incomplete installed socket", async () => {
    const sentinel = new Error("hello listener sentinel");
    adapter.onEvent((event) => { if (event.type === "serverHello") throw sentinel; });
    const read = start("reconnect");
    const ws = await completeHandshake();
    expect(await read()).toBe(sentinel);
    expect(ws.close).toHaveBeenCalled();
    expect(frames(ws)).toEqual(["ClientHello"]);
    expect(await trackRejection(adapter.submitPick("after-listener-failure"))()).toMatchObject({ code: "WS_CLOSED" });
  });

  it("11 reentrant hello replacement closes A without sending its setup", async () => {
    let next: (() => Promise<unknown>) | undefined;
    const off = adapter.onEvent((event) => {
      if (event.type === "serverHello") {
        off();
        next = start("create");
      }
    });
    const first = start("reconnect");
    const a = MockWebSocket.last!;
    await completeHandshake();
    expect(frames(a)).toEqual(["ClientHello"]);
    expect(a.close).toHaveBeenCalled();
    expect(await first()).toMatchObject({ message: expect.stringMatching(/supersed/i) });
    expect(next).toBeTypeOf("function");
    const b = await completeHandshake();
    reply(b, "create");
    expect(await next!()).toEqual({ resolvedWith: undefined });
  });

  it("J1 opening reconnect is superseded by join", async () => {
    const first = start("reconnect");
    const a = MockWebSocket.last!;
    const join = trackRejection(adapter.joinDraft("NEXT12", "Alice"));
    const b = await completeHandshake();
    expect(await first()).toMatchObject({ message: expect.stringMatching(/supersed/i) });
    expect(a.close).toHaveBeenCalled();
    const view = createMockDraftView();
    receive(b, "DraftJoined", { ...credentials, view });
    expect(await join()).toEqual({ resolvedWith: view });
  });

  it.each(["success", "failure"] as const)("J2 sent join survives replacement reconnect %s", async (outcome) => {
    const join = trackRejection(adapter.joinDraft("NEXT12", "Alice"));
    const j = await completeHandshake();
    expect(frames(j)).toContain("JoinDraftWithPassword");
    const read = start("reconnect");
    const b = await completeHandshake();
    if (outcome === "failure") {
      receive(b, "Error", { message: "reconnect failed" });
      expect(await read()).toMatchObject({ code: "WS_ERROR", message: "reconnect failed" });
      expect(await join()).toBe("never settled");
    }
    const view = createMockDraftView({ pick_number: 3 });
    receive(j, "DraftJoined", { ...credentials, view });
    expect(await join()).toEqual({ resolvedWith: view });
    expect(j.close).not.toHaveBeenCalled();
    if (outcome === "success") {
      expect(await read()).toBe("never settled");
      reply(b, "reconnect");
      expect(await read()).toEqual({ resolvedWith: undefined });
    }
  });

  it("J3 opening join is cancelled by reconnect without rejecting the replacement", async () => {
    const join = trackRejection(adapter.joinDraft("NEXT12", "Alice"));
    const j = MockWebSocket.last!;
    const read = start("reconnect");
    const b = await completeHandshake();
    expect(await join()).toMatchObject({ message: expect.stringMatching(/supersed/i) });
    expect(j.close).toHaveBeenCalled();
    reply(b, "reconnect");
    expect(await read()).toEqual({ resolvedWith: undefined });
  });

  it("J4 join stays cancellable during its hello notification", async () => {
    let created: (() => Promise<unknown>) | undefined;
    const off = adapter.onEvent((event) => {
      if (event.type === "serverHello") {
        off();
        created = start("create");
      }
    });
    const join = trackRejection(adapter.joinDraft("NEXT12", "Alice"));
    const j = MockWebSocket.last!;
    await completeHandshake();
    expect(await join()).toMatchObject({ message: expect.stringMatching(/supersed/i) });
    expect(frames(j)).toEqual(["ClientHello"]);
    expect(j.close).toHaveBeenCalled();
    expect(created).toBeTypeOf("function");
    const b = await completeHandshake();
    reply(b, "create");
    expect(await created!()).toEqual({ resolvedWith: undefined });
  });

  it("J6 preserves a join whose pair was settled by another socket before its handshake resumes", async () => {
    const events = vi.fn();
    adapter.onEvent(events);
    const join = trackRejection(adapter.joinDraft("NEXT12", "Alice"));
    const j = MockWebSocket.last!;
    // The join holds the draft pair while its handshake is still open, so no
    // hello has been accepted and no setup frame can have gone out yet.
    expect(await join()).toBe("never settled");
    expect(frames(j)).toEqual([]);
    expect(events.mock.calls.filter(([event]) => event.type === "serverHello")).toHaveLength(0);

    // Draft replies are uncorrelated: the established socket's authoritative
    // update settles the pending join promise before J resumes its handshake.
    const view = createMockDraftView({ pick_number: 7 });
    receive(established, "DraftStateUpdate", { view });
    expect(await join()).toEqual({ resolvedWith: view });

    // The pair is released, but the join attachment still owes its setup
    // frame: continuation rests on attachment identity + signal, not on the
    // pair still being installed.
    const resumed = await completeHandshake();
    expect(resumed).toBe(j);
    expect(frames(j)).toContain("JoinDraftWithPassword");
  });

  it.each(["create", "reconnect", "join"] as const)("L1 dispose fences queued %s continuation", async (kind) => {
    const read = kind === "join"
      ? trackRejection(adapter.joinDraft("NEXT12", "Alice"))
      : start(kind);
    const a = MockWebSocket.last!;
    expect(a.onmessage).toBeTypeOf("function");
    a.dispatchSynthetic("message", SERVER_HELLO);
    adapter.dispose();
    await Promise.resolve();
    await Promise.resolve();
    expect(frames(a)).toEqual(["ClientHello"]);
    expect(a.close).toHaveBeenCalled();
    expect(await read()).toMatchObject({ code: "WS_CLOSED", message: expect.stringMatching(/disposed/) });
  });

  it("H1 preserves reconnect handshake timeout", async () => {
    vi.useFakeTimers();
    try {
      let outcome: unknown = "never settled";
      void adapter.reconnectDraft().then(
        (value) => { outcome = { resolvedWith: value }; },
        (error) => { outcome = error; },
      );
      const ws = MockWebSocket.last!;
      expect(ws.onmessage).toBeTypeOf("function");
      await vi.advanceTimersByTimeAsync(5000);
      expect(outcome).toMatchObject({ code: "WS_ERROR", message: expect.stringMatching(/5000ms/), recoverable: true });
      expect(ws.close).toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("H3 preserves reconnect websocket error before hello", async () => {
    const read = start("reconnect");
    const ws = MockWebSocket.last!;
    expect(ws.onerror).toBeTypeOf("function");
    ws.onerror!();
    expect(await read()).toMatchObject({ code: "WS_ERROR", message: "WebSocket error during handshake", recoverable: true });
    expect(ws.close).toHaveBeenCalled();
  });

  it.each(["error", "close"] as const)("H4 preserves current installed reconnect %s failure", async (type) => {
    const events = vi.fn();
    adapter.onEvent(events);
    const read = start("reconnect");
    const ws = await completeHandshake();
    expect(frames(ws)).toContain("ReconnectDraft");
    const callback = type === "error" ? ws.onerror : ws.onclose;
    expect(callback).toBeTypeOf("function");
    callback!();
    expect(await read()).toMatchObject({ code: type === "error" ? "WS_ERROR" : "WS_CLOSED", recoverable: true });
    // The close settled this socket's own reconnect init: that rejection is the
    // operation's failure signal, not a lost established connection.
    if (type === "close") expect(events).not.toHaveBeenCalledWith({ type: "disconnected" });
  });

  it("D1 settles the owning init before close notification listeners run (failing-before regression)", async () => {
    enterAcceptedFullMatch(adapter, established);
    const submission = trackRejection(adapter.submitAction({ type: "PassPriority" }, 0));
    const reconnect = start("reconnect");
    const b = await completeHandshake();
    expect(frames(b)).toContain("ReconnectDraft");
    expect(b.onclose).toBeTypeOf("function");

    const sentinel = new Error("close notification listener sentinel");
    let listenerThrew = false;
    adapter.onEvent((event) => {
      // One-shot: `dispose()` re-emits `pending: false` in teardown and must
      // not turn this listener into a hook failure.
      if (event.type === "actionPendingChanged" && !event.pending && !listenerThrew) {
        listenerThrew = true;
        throw sentinel;
      }
    });
    // The listener runs and its exception escapes `onclose`. The owning init
    // must already be settled by then, so this rejection is observable.
    expect(() => b.onclose!()).toThrow(sentinel);
    expect(listenerThrew).toBe(true);
    expect(await reconnect()).toMatchObject({
      code: "WS_CLOSED",
      message: "Connection closed before draft started",
      recoverable: true,
    });
    expect(await submission()).toMatchObject({
      code: "WS_CLOSED",
      message: "Connection closed during action",
      recoverable: true,
    });
  });

  it("D2 a throwing waitingForPlayers listener does not strand a successful create (failing-before regression)", async () => {
    const create = start("create");
    const b = await completeHandshake();
    expect(frames(b)).toContain("CreateDraftWithSettings");
    expect(b.onmessage).toBeTypeOf("function");

    const sentinel = new Error("waitingForPlayers listener sentinel");
    let listenerThrew = false;
    adapter.onEvent((event) => {
      // One-shot: teardown emits other events, and a listener that fires twice
      // would blur which dispatch this row pins.
      if (event.type === "waitingForPlayers" && !listenerThrew) {
        listenerThrew = true;
        throw sentinel;
      }
    });
    // The listener runs and its exception escapes the message handler. The
    // create init must already be settled by then: the base adapter emitted
    // `waitingForPlayers` BEFORE resolving the create, so this row is
    // base-failing (the promise never settles there), not parent-failing.
    expect(() => receive(b, "DraftCreated", credentials)).toThrow(sentinel);
    expect(listenerThrew).toBe(true);
    expect(await create()).toEqual({ resolvedWith: undefined });
  });

  it("H5 established socket close without a pending init emits disconnected once", () => {
    const events = vi.fn();
    adapter.onEvent(events);
    // beforeEach awaited the real DraftCreated create, so no init is pending
    // and `established` is the installed socket holding the draft token.
    // Dispatch the close event only: production listens through the `onclose`
    // property, and this test environment (happy-dom) routes a dispatched
    // "close" event to that property. `dispatchSynthetic("close")` also invokes
    // `onclose` directly, which delivers this one close twice.
    established.dispatchEvent(new Event("close"));
    expect(events.mock.calls.filter(([event]) => event.type === "disconnected")).toHaveLength(1);
  });

  it("P1 reconnect handshake failure does not reject a sent pick", async () => {
    const pick = trackRejection(adapter.submitPick("card-1"));
    expect(frames(established)).toContain("DraftAction");
    const read = start("reconnect");
    const failed = MockWebSocket.last!;
    expect(failed.onerror).toBeTypeOf("function");
    failed.onerror!();
    expect(await read()).toMatchObject({ code: "WS_ERROR" });
    expect(await pick()).toBe("never settled");
    const view = createMockDraftView({ pick_number: 4 });
    receive(established, "DraftStateUpdate", { view });
    expect(await pick()).toEqual({ resolvedWith: view });
  });

  it("P2 reconnect setup failure detaches before close and preserves a sent pick", async () => {
    const pick = trackRejection(adapter.submitPick("card-1"));
    const read = start("reconnect");
    const failed = MockWebSocket.last!;
    let installedHandlers = false;
    failed.close.mockImplementation(() => failed.dispatchSynthetic("close"));
    failed.send.mockImplementation((raw: string) => {
      const frame: { type: string } = JSON.parse(raw);
      if (frame.type === "ReconnectDraft") {
        installedHandlers = typeof failed.onclose === "function" && typeof failed.onerror === "function";
        throw new Error("setup send sentinel");
      }
    });
    await completeHandshake();
    expect(installedHandlers).toBe(true);
    expect(await pick()).toBe("never settled");
    expect(await read()).toMatchObject({ code: "WS_CLOSED", message: "Failed to send setup frame" });
    expect(failed.close).toHaveBeenCalled();
    const view = createMockDraftView({ pick_number: 5 });
    receive(established, "DraftStateUpdate", { view });
    expect(await pick()).toEqual({ resolvedWith: view });
  });

  it("S1 a throwing error listener during a failed setup send still rejects the reconnect (failing-before regression)", async () => {
    const pick = trackRejection(adapter.submitPick("card-1"));
    expect(frames(established)).toContain("DraftAction");
    const reconnect = start("reconnect");
    const failed = MockWebSocket.last!;
    let setupSendAttempted = false;
    let listenerEntered = false;
    failed.send.mockImplementation((raw: string) => {
      const frame: { type: string } = JSON.parse(raw);
      if (frame.type === "ReconnectDraft") {
        setupSendAttempted = true;
        throw new Error("setup send sentinel");
      }
    });
    const listenerSentinel = new Error("setup-send error listener sentinel");
    adapter.onEvent((event) => {
      if (event.type === "error") {
        listenerEntered = true;
        throw listenerSentinel;
      }
    });
    await completeHandshake();
    expect(setupSendAttempted).toBe(true);
    expect(listenerEntered).toBe(true);
    // `send()` emits `error` before the attachment catch runs, so the listener's
    // exception escapes through that catch; the catch must still reject the
    // owning reconnect and close the incomplete socket.
    expect(await reconnect()).toBe(listenerSentinel);
    expect(failed.close).toHaveBeenCalled();
    const view = createMockDraftView({ pick_number: 6 });
    receive(established, "DraftStateUpdate", { view });
    expect(await pick()).toEqual({ resolvedWith: view });
  });

  it("P3 reconnect without credentials rejects without opening a socket", async () => {
    adapter.dispose();
    const last = MockWebSocket.last;
    expect(await trackRejection(adapter.reconnectDraft())()).toMatchObject({
      code: "WS_ERROR", message: "No draft session to reconnect to", recoverable: false,
    });
    expect(MockWebSocket.last).toBe(last);
  });
});

describe("draft attachment open failures", () => {
  const settings = {
    displayName: "Alice",
    setCodes: ["MKM"],
    kind: "Premier" as const,
    public: true,
    tournamentFormat: "Swiss" as const,
    podPolicy: "Competitive" as const,
    podSize: 8,
  };
  const credentials = { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 };
  let adapter: ServerDraftAdapter;

  beforeEach(async () => {
    injectedOpenFailure.current = null;
    openedUrls.length = 0;
    MockWebSocket.last = null;
    adapter = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: null });
    const created = adapter.createDraft(settings);
    const established = await completeHandshake();
    receive(established, "DraftCreated", credentials);
    await created;
    // Forget the setup connection: only the injected failure may open now.
    openedUrls.length = 0;
    MockWebSocket.last = null;
  });

  afterEach(() => {
    injectedOpenFailure.current = null;
    adapter.dispose();
    vi.restoreAllMocks();
  });

  function start(kind: "create" | "reconnect" | "join"): Promise<unknown> {
    switch (kind) {
      case "create":
        return adapter.createDraft(settings);
      case "reconnect":
        return adapter.reconnectDraft();
      case "join":
        return adapter.joinDraft("NEXT12", "Alice");
    }
  }

  // `openPhaseSocket` forwards ordinary rejections from LAN authorization
  // unwrapped, so both shapes reach `attachSocket` before a socket exists.
  const failures = [
    { label: "an Error", error: new Error("LAN approval denied"), message: "Error: LAN approval denied" },
    { label: "a non-Error value", error: "denied", message: "denied" },
  ];

  it.each(
    (["create", "reconnect", "join"] as const).flatMap((kind) =>
      failures.map((failure) => ({ kind, ...failure })),
    ),
  )("$kind through $label rejects as a recoverable WS_ERROR without opening a socket", async ({ kind, error, message }) => {
    injectedOpenFailure.current = { error };
    const read = trackRejection(start(kind));

    const outcome = await read();
    expect(outcome).toBeInstanceOf(AdapterError);
    expect(outcome).toMatchObject({ code: "WS_ERROR", message, recoverable: true });
    // The failure came from the open itself: it was attempted exactly once and
    // no transport was ever created, so no setup frame could have been sent.
    expect(openedUrls).toEqual(["ws://localhost:9374/ws"]);
    expect(MockWebSocket.last).toBeNull();
  });
});

// ── Negotiated StateCodecV1 delivery ────────────────────────────────────
//
// The decoder is substituted: these rows prove the draft transport routes,
// gates and recovers. Exact reconstruction is the real-WASM integration row.

const CODEC_SERVER_HELLO = JSON.stringify({
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
const CODEC_SERVER_CONFIRMATION = JSON.stringify({
  type: "ServerHello",
  data: { server_version: "0.0.0-test", selected_state_codec: "StateCodecV1" },
});

type DraftDecodeResult =
  | { kind: "decoded"; json: string }
  | { kind: "resyncRequired" };

function draftCodecSession(handler: (frame: Uint8Array) => DraftDecodeResult) {
  const decode = vi.fn(async (frame: Uint8Array) => handler(frame));
  const close = vi.fn();
  return { session: { decode, close }, decode, close };
}

function draftFrame(type: "Full" | "Delta", envelope: unknown): string {
  return JSON.stringify({ type, data: { state: envelope, revision: 1 } });
}

/** Decode handler that unwraps the frame's embedded envelope verbatim. */
function decodeEnvelope(frame: Uint8Array): DraftDecodeResult {
  const parsed = JSON.parse(new TextDecoder().decode(frame)) as { data: { state: unknown } };
  return { kind: "decoded", json: JSON.stringify(parsed.data.state) };
}

async function startCodecDraft(session: StateCodecSession, correlated = true) {
  MockWebSocket.last = null;
  const provider = vi.fn(async () => session);
  const adapter = new ServerDraftAdapter("ws://localhost:9374/ws", {
    stateCodecSessionProvider: provider,
  });
  const createPromise = adapter.createDraft({
    displayName: "Alice",
    setCodes: ["MKM"],
    kind: "Premier",
    public: true,
    tournamentFormat: "Swiss",
    podPolicy: "Competitive",
    podSize: 8,
  });
  await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
  const ws = MockWebSocket.last!;
  const hello = JSON.parse(CODEC_SERVER_HELLO);
  if (!correlated) delete hello.data.command_correlation;
  ws.dispatchSynthetic("message", JSON.stringify(hello));
  await vi.waitFor(() => expect(ws.send).toHaveBeenCalledWith(
    expect.stringContaining('"type":"CreateDraftWithSettings"'),
  ));
  ws.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
  receive(ws, "DraftCreated", { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 });
  await createPromise;
  // Private transport identity is observed only to discriminate retired-wrapper races.
  const attached = adapter as unknown as { ws: unknown };
  return { adapter, ws, session, provider, transport: attached.ws };
}

/**
 * Drives a codec socket to an accepted match: DraftMatchStart, then the Full
 * frame whose decoded text is the genuine GameStarted envelope.
 */
async function enterCodecMatch(
  ws: MockWebSocket,
  events: unknown[],
  matchId = "r1-t0",
): Promise<void> {
  receive(ws, "DraftMatchStart", {
    match_id: matchId,
    round: 1,
    game_code: FULL_KEY.game_code,
    full_key: FULL_KEY,
    player_token: "match-draft-token",
    your_player: 0,
    opponent_name: "Bob",
  });
  ws.dispatchSynthetic("message", draftFrame("Full", {
    type: "GameStarted",
    data: { state: matchState("accepted-full-start"), your_player: 0, full_key: FULL_KEY },
  }));
  await vi.waitFor(() =>
    expect(events).toContainEqual(expect.objectContaining({ type: "gameStateUpdated" })),
  );
  // GameStarted cannot be a Delta baseline. Establish StateUpdate shape first.
  ws.dispatchSynthetic("message", draftFrame("Full", {
    type: "StateUpdate",
    data: { state: matchState("accepted-update"), events: [], full_key: FULL_KEY },
  }));
  await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({
    type: "gameStateUpdated", state: expect.objectContaining({ label: "accepted-update" }),
  })));
  ws.send.mockClear();
}

/** The snapshot-transport counterpart of {@link enterCodecMatch}: plain frames, no decoder. */
async function enterPlainMatch(
  ws: MockWebSocket,
  events: unknown[],
  matchId = "r1-t0",
): Promise<void> {
  receive(ws, "DraftMatchStart", {
    match_id: matchId,
    round: 1,
    game_code: FULL_KEY.game_code,
    full_key: FULL_KEY,
    player_token: "match-draft-token",
    your_player: 0,
    opponent_name: "Bob",
  });
  receive(ws, "GameStarted", {
    state: matchState("plain-start"), your_player: 0, full_key: FULL_KEY,
  });
  await vi.waitFor(() =>
    expect(events).toContainEqual(expect.objectContaining({ type: "gameStateUpdated" })),
  );
  receive(ws, "StateUpdate", {
    state: matchState("plain-update"), events: [], full_key: FULL_KEY,
  });
  await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({
    type: "gameStateUpdated", state: expect.objectContaining({ label: "plain-update" }),
  })));
  ws.send.mockClear();
}

/** Whether this socket's ClientHello offered StateCodecV1. */
function offeredCodec(ws: MockWebSocket): boolean {
  const hello = ws.send.mock.calls
    .map(([text]) => JSON.parse(text as string) as { type?: string; data?: { state_codecs?: unknown } })
    .find((frame) => frame.type === "ClientHello");
  if (!hello) throw new Error("no ClientHello was sent");
  return hello.data?.state_codecs !== undefined;
}

/** The adapter's delivery policy, observed only to discriminate when it was decided. */
function policyOf(adapter: ServerDraftAdapter): unknown {
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
    free: () => undefined,
    clone_handle: () => handle(json),
  });
  const engine = {
    state_codec_decode: vi.fn((frame: Uint8Array) => {
      if (script.trapNext) {
        script.trapNext = false;
        throw new WebAssembly.RuntimeError("unreachable");
      }
      const parsed = JSON.parse(new TextDecoder().decode(frame)) as { data: { state: unknown } };
      return handle(JSON.stringify(parsed.data.state));
    }),
  };
  const runtime = new StateCodecRuntime(engine as unknown as EngineModule);
  const provider = vi.fn(async (): Promise<StateCodecSession | null> => {
    const sessionId = runtime.open();
    return {
      decode: async (frame) => runtime.decode(sessionId, frame),
      close: () => runtime.close(sessionId),
    };
  });
  return { engine, runtime, script, provider };
}

/**
 * Submits a command on `ws` and completes it with its correlated StateUpdate,
 * as a codec Delta frame or as a plain snapshot frame.
 */
async function completeDraftCommand(
  adapter: ServerDraftAdapter,
  ws: MockWebSocket,
  label: string,
  transport: "codec" | "plain",
): Promise<void> {
  const before = submissionTypes(ws).length;
  const settled = trackRejection(adapter.submitAction({ type: "PassPriority" } as GameAction, 0));
  await vi.waitFor(() => expect(submissionTypes(ws)).toHaveLength(before + 1));
  const completion = {
    type: "StateUpdate",
    command_id: lastSubmissionCommandId(ws),
    data: { state: matchState(label), events: [], full_key: FULL_KEY },
  };
  ws.dispatchSynthetic("message", transport === "codec"
    ? draftFrame("Delta", completion)
    : JSON.stringify(completion));
  await expect(settled()).resolves.toMatchObject({ resolvedWith: { events: [] } });
}

describe("ServerDraftAdapter — negotiated StateCodecV1", () => {
  it("emits the same match state and events as the snapshot stream", async () => {
    const matchStart = {
      match_id: "r1-t0",
      round: 1,
      game_code: FULL_KEY.game_code,
      full_key: FULL_KEY,
      player_token: "match-draft-token",
      your_player: 0,
      opponent_name: "Bob",
    };
    const startedState = matchState("codec-start");
    const gameStarted = { type: "GameStarted", data: {
      state: startedState, your_player: 0, full_key: FULL_KEY,
      legal_actions: [{ type: "PassPriority" }], auto_pass_recommended: true,
      spell_costs: { "12": { generic: 1 } }, derived: { marker: "雪 🧙" },
    } };
    const updateState = matchState("codec-update");
    const stateUpdate = {
      type: "StateUpdate",
      data: { state: updateState, events: [{ marker: "codec-update" }], full_key: FULL_KEY,
        log_entries: [debugLogEntry("Éowyn")] },
    };

    const codec = await startCodecDraft(draftCodecSession(decodeEnvelope).session);
    const codecEvents: unknown[] = [];
    codec.adapter.onEvent((event) => codecEvents.push(event));
    receive(codec.ws, "DraftMatchStart", matchStart);
    codec.ws.dispatchSynthetic("message", draftFrame("Full", gameStarted));
    codec.ws.dispatchSynthetic("message", draftFrame("Full", stateUpdate));

    const snapshot = new ServerDraftAdapter("ws://localhost:9374/ws", {
      stateCodecSessionProvider: null,
    });
    const snapshotEvents: unknown[] = [];
    snapshot.onEvent((event) => snapshotEvents.push(event));
    MockWebSocket.last = null;
    const createPromise = snapshot.createDraft({
      displayName: "Alice",
      setCodes: ["MKM"],
      kind: "Premier",
      public: true,
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      podSize: 8,
    });
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
    const ws = MockWebSocket.last!;
    ws.dispatchSynthetic("message", SERVER_HELLO);
    await vi.waitFor(() =>
      expect((snapshot as unknown as { ws: unknown }).ws).not.toBeNull(),
    );
    receive(ws, "DraftCreated", { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 });
    await createPromise;
    receive(ws, "DraftMatchStart", matchStart);
    ws.dispatchSynthetic("message", JSON.stringify(gameStarted));
    ws.dispatchSynthetic("message", JSON.stringify(stateUpdate));

    const shape = (events: unknown[]) => events
      .filter((event): event is { type: string } =>
        typeof event === "object" && event !== null && "type" in event)
      .filter((event) => event.type === "gameStateUpdated")
      .map((event) => {
        const typed = event as {
          state?: unknown;
          events?: unknown;
          legalResult?: unknown;
          logEntries?: unknown;
        };
        return {
          state: typed.state,
          events: typed.events,
          legalResult: typed.legalResult,
          logEntries: typed.logEntries,
        };
      });

    expect(shape(codecEvents)).toHaveLength(2);
    const expected = [{
      state: { ...startedState, derived: gameStarted.data.derived }, events: [],
      legalResult: { actions: gameStarted.data.legal_actions, autoPassRecommended: true,
        endContinuousEffectOffers: [], manaPaymentShortcutActions: [],
        spellCosts: gameStarted.data.spell_costs, legalActionsByObject: undefined,
        activationBlockReasons: undefined, viewerInteraction: undefined },
      logEntries: undefined,
    }, {
      state: { ...updateState, derived: undefined }, events: stateUpdate.data.events,
      legalResult: { actions: [], autoPassRecommended: false, endContinuousEffectOffers: [],
        manaPaymentShortcutActions: [], spellCosts: undefined, legalActionsByObject: undefined,
        activationBlockReasons: undefined, viewerInteraction: undefined },
      logEntries: stateUpdate.data.log_entries,
    }];
    expect(shape(snapshotEvents)).toEqual(expected);
    expect(shape(codecEvents)).toEqual(expected);
    expect(shape(codecEvents)).toEqual(shape(snapshotEvents));
    codec.adapter.dispose();
    snapshot.dispose();
  });

  it("treats a rejection in a new match as bootstrap-fatal, not recoverable", async () => {
    let calls = 0;
    const session = draftCodecSession((frame) => {
      calls++;
      // Match A's GameStarted and first update establish a live stream.
      return calls <= 2 ? decodeEnvelope(frame) : { kind: "resyncRequired" };
    });
    const { adapter, ws } = await startCodecDraft(session.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));

    await enterCodecMatch(ws, events);
    expect(events).toContainEqual(expect.objectContaining({ type: "gameStateUpdated" }));

    // Match B: a different match id resets bootstrap to pending.
    receive(ws, "DraftMatchStart", {
      match_id: "r2-t0",
      round: 2,
      game_code: FULL_KEY.game_code,
      full_key: FULL_KEY,
      player_token: "match-draft-token",
      your_player: 0,
      opponent_name: "Carol",
    });
    const startedB = { type: "GameStarted", data: { state: matchState("b-start"), your_player: 0, full_key: FULL_KEY } };
    ws.dispatchSynthetic("message", draftFrame("Full", startedB));

    await vi.waitFor(() => expect(events).toContainEqual({ type: "disconnected" }));
    expect(ws.send).toHaveBeenCalledWith('{"type":"ResyncState"}');
    expect(
      ws.send.mock.calls.filter(([value]) => value === '{"type":"ResyncState"}'),
    ).toHaveLength(1);
    expect(
      events.some((event) =>
        (event as { type?: string; state?: { label?: string } }).type === "gameStateUpdated"
        && (event as { state?: { label?: string } }).state?.label === "b-start"),
    ).toBe(false);
  });

  it.each([
    ["Action", true], ["Interaction", true], ["Action", false], ["Interaction", false],
  ] as const)("recovers an established %s (correlated=%s), gating new submissions until Full", async (kind, correlated) => {
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws } = await startCodecDraft(session.session, correlated);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);

    // Reject an established StateUpdate Delta, then recover on a new Full.
    let calls = 0;
    session.decode.mockImplementation(async (frame: Uint8Array) => {
      calls++;
      return calls === 1 ? { kind: "resyncRequired" } : decodeEnvelope(frame);
    });

    const pending = kind === "Action" ? adapter.submitAction({ type: "PassPriority" }, 0)
      : adapter.submitInteraction(interactionSubmission("codec-initial"), 0);
    const observed = trackRejection(pending);
    const oldId = correlated ? lastSubmissionCommandId(ws) : undefined;
    const reentrant: Promise<unknown>[] = [];
    const stop = adapter.onEvent((event) => {
      if ((event.type === "actionPendingChanged" && !event.pending) || event.type === "gameStateUpdated") {
        reentrant.push(adapter.submitInteraction(interactionSubmission("codec-reentrant"), 0).catch((error: unknown) => error));
      }
    });
    ws.dispatchSynthetic("message", draftFrame("Delta", {
      type: "StateUpdate",
      data: { state: matchState("peer"), events: [], full_key: FULL_KEY },
    }));
    await expect(observed()).resolves.toMatchObject({
      code: "WS_CLOSED",
      recoverable: true,
    });

    const sentBefore = submissionTypes(ws).length;
    await expect(adapter.submitAction({ type: "PassPriority" } as GameAction, 0)).rejects
      .toMatchObject({ code: "ACTION_NOT_SENT" });
    expect(submissionTypes(ws)).toHaveLength(sentBefore);

    ws.dispatchSynthetic("message", draftFrame("Full", {
      type: "StateUpdate",
      data: { state: matchState("resync"), events: [], full_key: FULL_KEY },
    }));
    await vi.waitFor(() => expect(() => (adapter as unknown as { stateCodec: { assertNotRecovering(): void } }).stateCodec.assertNotRecovering()).not.toThrow());
    expect(await Promise.all(reentrant)).toEqual([
      expect.objectContaining({ code: "ACTION_NOT_SENT" }),
      expect.objectContaining({ code: "ACTION_NOT_SENT" }),
    ]);
    stop();

    const resumed = adapter.submitAction({ type: "PassPriority" } as GameAction, 0);
    await vi.waitFor(() => expect(submissionTypes(ws)).toHaveLength(sentBefore + 1));
    const settled = vi.fn();
    void resumed.then(settled);
    if (correlated) {
      ws.dispatchSynthetic("message", draftFrame("Delta", {
        type: "StateUpdate", command_id: oldId,
        data: { state: matchState("old-echo"), events: [], full_key: FULL_KEY },
      }));
      await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({
        type: "gameStateUpdated", state: expect.objectContaining({ label: "old-echo" }),
      })));
      expect(settled).not.toHaveBeenCalled();
    }
    ws.dispatchSynthetic("message", draftFrame("Delta", {
      type: "StateUpdate", command_id: correlated ? lastSubmissionCommandId(ws) : undefined,
      data: { state: matchState("own"), events: [], full_key: FULL_KEY },
    }));
    await expect(resumed).resolves.toMatchObject({ events: [] });
    adapter.dispose();
  });
});

describe("ServerDraftAdapter — retired codec transport", () => {
  it("does not let a retired socket's recovery drop the replacement's pending command", async () => {
    // Two attachments live during the overlap; only the current one owns the
    // command slot. Full suite semantics allow a superseded socket to keep
    // delivering its own draft replies (see J2), so the isolation must come
    // from the attachment identity check, not from dropping frames.
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider, transport } = await startCodecDraft(session.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);

    const replacementSession = draftCodecSession(decodeEnvelope);
    provider.mockResolvedValueOnce(replacementSession.session);
    // A replacement attachment takes over the adapter.
    const reconnecting = adapter.reconnectDraft();
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(ws));
    const replacement = MockWebSocket.last!;
    replacement.dispatchSynthetic("message", CODEC_SERVER_HELLO);
    await vi.waitFor(() => {
      const attached = adapter as unknown as { ws: unknown };
      expect(attached.ws).not.toBe(transport);
    });
    replacement.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
    receive(replacement, "DraftStateUpdate", { view: createMockDraftView({ pick_number: 1 }) });
    await reconnecting;
    events.length = 0;
    await enterCodecMatch(replacement, events);
    events.length = 0;
    // The command is owned and sent by the replacement, not its predecessor.
    const pending = adapter.submitAction({ type: "PassPriority" } as GameAction, 0);
    const observed = trackRejection(pending);
    const commandId = lastSubmissionCommandId(replacement);

    // A rejection on the RETIRED transport must not touch the live slot.
    session.decode.mockImplementationOnce(async () => ({ kind: "resyncRequired" }));
    ws.dispatchSynthetic("message", draftFrame("Delta", {
      type: "StateUpdate",
      data: { state: matchState("retired"), events: [], full_key: FULL_KEY },
    }));
    await vi.waitFor(() => expect(ws.send).toHaveBeenCalledWith('{"type":"ResyncState"}'));
    expect(events).not.toContainEqual({ type: "actionPendingChanged", pending: false });
    expect(() => (adapter as unknown as { stateCodec: { assertNotRecovering(): void } }).stateCodec.assertNotRecovering()).not.toThrow();

    // The live command still settles on its own correlated completion,
    // delivered on the transport that now owns the slot.
    replacement.dispatchSynthetic("message", draftFrame("Full", {
      type: "StateUpdate",
      data: { state: matchState("own"), events: [], full_key: FULL_KEY },
      command_id: commandId,
    }));
    await expect(observed()).resolves.toMatchObject({ resolvedWith: { events: [] } });
    adapter.dispose();
  });

  it("fences a retired delayed decode while the replacement accepts a live update", async () => {
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider } = await startCodecDraft(session.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);
    let finish!: (value: DraftDecodeResult) => void;
    session.decode.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    ws.dispatchSynthetic("message", draftFrame("Delta", {}));
    await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
    const replacementSession = draftCodecSession(decodeEnvelope);
    provider.mockResolvedValueOnce(replacementSession.session);
    const reattached = adapter.reconnectDraft();
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(ws));
    const replacement = MockWebSocket.last!;
    replacement.dispatchSynthetic("message", CODEC_SERVER_HELLO);
    await vi.waitFor(() => expect(replacement.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"ReconnectDraft"')));
    replacement.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
    receive(replacement, "DraftStateUpdate", { view: createMockDraftView() });
    await reattached;
    events.length = 0;
    await enterCodecMatch(replacement, events);
    events.length = 0;
    finish({ kind: "decoded", json: JSON.stringify({ type: "StateUpdate",
      data: { state: matchState("retired-late"), events: [], full_key: FULL_KEY } }) });
    replacement.dispatchSynthetic("message", draftFrame("Delta", { type: "StateUpdate",
      data: { state: matchState("live"), events: [], full_key: FULL_KEY } }));
    await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({
      type: "gameStateUpdated", state: expect.objectContaining({ label: "live" }),
    })));
    expect(events).not.toContainEqual(expect.objectContaining({
      type: "gameStateUpdated", state: expect.objectContaining({ label: "retired-late" }),
    }));
    adapter.dispose();
    ws.dispatchSynthetic("close");
  });
});

describe("ServerDraftAdapter — codec identity and bootstrap boundaries", () => {
  it("keeps ignored identity frames on the wire baseline and accepts the matching stream", async () => {
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws } = await startCodecDraft(session.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);
    events.length = 0;
    const wrongKey = { ...FULL_KEY, generation: FULL_KEY.generation + 1 };
    const rejected = { type: "StateUpdate", data: { state: matchState("ignored-key"),
      events: [], full_key: wrongKey } };
    ws.dispatchSynthetic("message", draftFrame("Full", rejected));
    ws.dispatchSynthetic("message", draftFrame("Delta", rejected));
    ws.dispatchSynthetic("message", draftFrame("Full", { type: "GameStarted",
      data: { state: matchState("wrong-player"), your_player: 1, full_key: FULL_KEY } }));
    ws.dispatchSynthetic("message", draftFrame("Full", { type: "StateUpdate",
      data: { state: matchState("missing-key"), events: [] } }));
    ws.dispatchSynthetic("message", draftFrame("Full", { type: "StateUpdate",
      data: { state: matchState("matching-full"), events: [], full_key: FULL_KEY } }));
    ws.dispatchSynthetic("message", draftFrame("Delta", { type: "StateUpdate",
      data: { state: matchState("matching-delta"), events: [], full_key: FULL_KEY } }));
    await vi.waitFor(() => expect(events).toHaveLength(2));
    expect(events).toEqual([
      expect.objectContaining({ type: "gameStateUpdated", state: expect.objectContaining({ label: "matching-full" }) }),
      expect.objectContaining({ type: "gameStateUpdated", state: expect.objectContaining({ label: "matching-delta" }) }),
    ]);
    expect(session.decode).toHaveBeenCalledTimes(8);
    expect(session.close).not.toHaveBeenCalled();
    expect(ws.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');
    expect(ws.close).not.toHaveBeenCalled();
    adapter.dispose();
  });

  it.each(["Full", "Delta"] as const)("closes on an initial rejected %s and reattaches with genuine bootstrap", async (type) => {
    const first = draftCodecSession(() => ({ kind: "resyncRequired" }));
    const { adapter, ws, provider } = await startCodecDraft(first.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    receive(ws, "DraftMatchStart", { match_id: "r1-t0", round: 1,
      game_code: FULL_KEY.game_code, full_key: FULL_KEY,
      player_token: "match-token", your_player: 0, opponent_name: "Opponent" });
    ws.dispatchSynthetic("message", draftFrame(type, { type: "GameStarted",
      data: { state: matchState("bad-start"), your_player: 0, full_key: FULL_KEY } }));
    await vi.waitFor(() => expect(events).toContainEqual({ type: "disconnected" }));
    expect(first.close).toHaveBeenCalledOnce();
    expect(ws.send.mock.calls.filter(([text]) => text === '{"type":"ResyncState"}')).toHaveLength(1);
    ws.dispatchSynthetic("message", draftFrame("Full", { type: "StateUpdate",
      data: { state: matchState("late"), full_key: FULL_KEY, events: [] } }));
    ws.dispatchSynthetic("message", draftFrame("Delta", {}));
    await expect(adapter.getSnapshot()).rejects.toThrow("No game state");
    // A rejection before bootstrap is a terminal codec reason: this adapter's
    // next attachment is snapshot-only, so the provider is not consulted again
    // and play is restored from plain frames.
    expect(provider).toHaveBeenCalledOnce();
    const reattached = adapter.reconnectDraft();
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(ws));
    const replacement = MockWebSocket.last!;
    replacement.dispatchSynthetic("message", CODEC_SERVER_HELLO);
    await vi.waitFor(() => expect(replacement.send).toHaveBeenCalledWith(expect.stringContaining('"type":"ReconnectDraft"')));
    expect(offeredCodec(replacement)).toBe(false);
    receive(replacement, "DraftStateUpdate", { view: createMockDraftView() });
    await reattached;
    events.length = 0;
    await enterPlainMatch(replacement, events);
    receive(replacement, "StateUpdate", { state: matchState("reattached-update"), events: [], full_key: FULL_KEY });
    await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({
      type: "gameStateUpdated", state: expect.objectContaining({ label: "reattached-update" }),
    })));
    expect(provider).toHaveBeenCalledOnce();
    expect(first.decode).toHaveBeenCalledOnce();
    expect(replacement.send).not.toHaveBeenCalledWith('{"type":"ResyncState"}');
    adapter.dispose();
  });
});

describe("ServerDraftAdapter — codec degradation policy", () => {
  const pass = { type: "PassPriority" } as GameAction;
  const stateUpdate = (label: string) => ({
    type: "StateUpdate", data: { state: matchState(label), events: [], full_key: FULL_KEY },
  });

  async function reattachSnapshotOnly(adapter: ServerDraftAdapter, previous: MockWebSocket) {
    const reattached = adapter.reconnectDraft();
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(previous));
    const replacement = MockWebSocket.last!;
    replacement.dispatchSynthetic("message", CODEC_SERVER_HELLO);
    await vi.waitFor(() => expect(replacement.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"ReconnectDraft"')));
    receive(replacement, "DraftStateUpdate", { view: createMockDraftView() });
    await reattached;
    return replacement;
  }

  // Confirmation has no deadline and heartbeat does not bound the wait,
  // so an initial attachment whose second hello is slow still completes and
  // is not degraded: a deadline would fail it after the create/join request
  // was sent, orphaning the seat whose credentials arrive in the reply.
  it.each(["create", "join"] as const)(
    "completes an initial %s whose confirmation hello arrives after the former deadline, leaving the policy negotiating",
    async (kind) => {
      vi.useFakeTimers();
      const session = draftCodecSession(decodeEnvelope);
      const provider = vi.fn(async () => session.session);
      MockWebSocket.last = null;
      const adapter = new ServerDraftAdapter("ws://localhost:9374/ws", {
        stateCodecSessionProvider: provider,
      });
      const events: unknown[] = [];
      adapter.onEvent((event) => events.push(event));
      try {
        const attachment = kind === "create"
          ? adapter.createDraft({
            displayName: "Alice", setCodes: ["MKM"], kind: "Premier", public: true,
            tournamentFormat: "Swiss", podPolicy: "Competitive", podSize: 8,
          })
          : adapter.joinDraft("ABCD12", "Alice");
        const outcome = attachment.then(
          () => "resolved" as const,
          (error: unknown) => error,
        );
        await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
        const ws = MockWebSocket.last!;
        ws.dispatchSynthetic("message", CODEC_SERVER_HELLO);
        const setupType = kind === "create" ? "CreateDraftWithSettings" : "JoinDraftWithPassword";
        await vi.waitFor(() => expect(ws.send).toHaveBeenCalledWith(
          expect.stringContaining(`"type":"${setupType}"`)));
        // Reach guard: the codec was offered and the setup request is on the wire.
        expect(offeredCodec(ws)).toBe(true);

        await vi.advanceTimersByTimeAsync(STATE_CODEC_SOCKET_TIMEOUT_MS + 1000);
        expect(ws.close).not.toHaveBeenCalled();
        expect(session.close).not.toHaveBeenCalled();
        expect(policyOf(adapter)).toEqual({ kind: "negotiate" });
        // The settlement helpers below wait on real macrotasks.
        vi.useRealTimers();

        ws.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
        if (kind === "create") {
          receive(ws, "DraftCreated", { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 });
        } else {
          receive(ws, "DraftJoined", {
            draft_code: "ABCD12", player_token: "tok123", seat_index: 0, view: createMockDraftView(),
          });
        }
        // The owner is settled with the reply, not rejected.
        expect(await outcome).toBe("resolved");
        expect(ws.close).not.toHaveBeenCalled();
        expect(session.close).not.toHaveBeenCalled();

        // The match start (a GameStarted Full) bootstraps through the codec.
        await enterCodecMatch(ws, events);
        expect(session.decode).toHaveBeenCalledTimes(2);
        await completeDraftCommand(adapter, ws, "slow-confirmation-command", "codec");
        expect(session.decode).toHaveBeenCalledTimes(3);
        expect(policyOf(adapter)).toEqual({ kind: "negotiate" });
        expect(provider).toHaveBeenCalledOnce();
      } finally {
        adapter.dispose();
        vi.useRealTimers();
      }
    },
  );

  it("tolerates one completed recovery, closes after the second and reattaches snapshot-only; a fresh adapter negotiates", async () => {
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider } = await startCodecDraft(session.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);
    const seen = (label: string) => expect(events).toContainEqual(expect.objectContaining({
      type: "gameStateUpdated", state: expect.objectContaining({ label }),
    }));
    try {
      // Recovery one completes; the command after it is admitted.
      session.decode.mockImplementationOnce(async () => ({ kind: "resyncRequired" }));
      ws.dispatchSynthetic("message", draftFrame("Delta", stateUpdate("corrupt-1")));
      await vi.waitFor(() => expect(ws.send.mock.calls.filter(([t]) => t === '{"type":"ResyncState"}')).toHaveLength(1));
      ws.dispatchSynthetic("message", draftFrame("Full", stateUpdate("resync-1")));
      await vi.waitFor(() => seen("resync-1"));
      void adapter.submitAction(pass, 0).catch(() => undefined);
      await vi.waitFor(() => expect(submissionTypes(ws)).toHaveLength(1));
      expect(ws.close).not.toHaveBeenCalled();

      // Recovery two: the Full is delivered, then the stream closes.
      ws.dispatchSynthetic("message", draftFrame("Delta", stateUpdate("healthy")));
      await vi.waitFor(() => seen("healthy"));
      session.decode.mockImplementationOnce(async () => ({ kind: "resyncRequired" }));
      ws.dispatchSynthetic("message", draftFrame("Delta", stateUpdate("corrupt-2")));
      await vi.waitFor(() => expect(ws.send.mock.calls.filter(([t]) => t === '{"type":"ResyncState"}')).toHaveLength(2));
      ws.dispatchSynthetic("message", draftFrame("Full", stateUpdate("resync-2")));
      await vi.waitFor(() => seen("resync-2"));
      await vi.waitFor(() => expect(events).toContainEqual({ type: "disconnected" }));
      expect(session.close).toHaveBeenCalledOnce();

      const replacement = await reattachSnapshotOnly(adapter, ws);
      expect(provider).toHaveBeenCalledOnce();
      expect(offeredCodec(replacement)).toBe(false);
      events.length = 0;
      await enterPlainMatch(replacement, events);
      expect(provider).toHaveBeenCalledOnce();

      // The degraded adapter stays usable: a plain command completes ...
      await completeDraftCommand(adapter, replacement, "plain-command-1", "plain");
      // ... a new match on the snapshot socket stays snapshot ...
      events.length = 0;
      await enterPlainMatch(replacement, events, "r2-t0");
      await completeDraftCommand(adapter, replacement, "plain-command-2", "plain");
      expect(provider).toHaveBeenCalledOnce();
      // ... and the new match did not re-enable negotiation for the next attachment.
      replacement.dispatchSynthetic("close");
      await vi.waitFor(() => expect(events).toContainEqual({ type: "disconnected" }));
      const again = await reattachSnapshotOnly(adapter, replacement);
      expect(offeredCodec(again)).toBe(false);
      expect(provider).toHaveBeenCalledOnce();
    } finally {
      adapter.dispose();
    }

    const next = draftCodecSession(decodeEnvelope);
    const fresh = await startCodecDraft(next.session);
    const freshEvents: unknown[] = [];
    fresh.adapter.onEvent((event) => freshEvents.push(event));
    try {
      expect(offeredCodec(fresh.ws)).toBe(true);
      // The fresh adapter decodes, bootstraps and completes a command, not only advertises.
      await enterCodecMatch(fresh.ws, freshEvents);
      expect(next.decode).toHaveBeenCalledTimes(2);
      await completeDraftCommand(fresh.adapter, fresh.ws, "fresh-command", "codec");
      expect(next.decode).toHaveBeenCalledTimes(3);
    } finally {
      fresh.adapter.dispose();
    }
  });

  it("closes a lease that arrives after the adapter degraded, opens the pending attachment snapshot-only and settles it", async () => {
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider } = await startCodecDraft(session.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);
    let release!: (lease: StateCodecSession | null) => void;
    provider.mockImplementationOnce(() => new Promise<StateCodecSession>((resolve) => {
      release = resolve as (lease: StateCodecSession | null) => void;
    }));
    const late = draftCodecSession(decodeEnvelope);
    try {
      const reattached = adapter.reconnectDraft();
      const settled = trackRejection(reattached);
      await vi.waitFor(() => expect(release).toBeTypeOf("function"));

      session.decode.mockImplementationOnce(async () => { throw new Error("decoder trapped"); });
      ws.dispatchSynthetic("message", draftFrame("Delta", stateUpdate("never-applied")));
      await vi.waitFor(() => expect(session.close).toHaveBeenCalledOnce());
      // Reach guard: A is gone and B's preparation is still outstanding.
      expect(late.close).not.toHaveBeenCalled();

      release(late.session);
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(ws));
      const replacement = MockWebSocket.last!;
      replacement.dispatchSynthetic("message", CODEC_SERVER_HELLO);
      await vi.waitFor(() => expect(replacement.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"ReconnectDraft"')));
      expect(late.close).toHaveBeenCalledOnce();
      expect(offeredCodec(replacement)).toBe(false);
      receive(replacement, "DraftStateUpdate", { view: createMockDraftView() });
      // The attachment owner is settled, not stranded.
      expect(await settled()).not.toBe("never settled");
      await enterPlainMatch(replacement, events);
      expect(late.decode).not.toHaveBeenCalled();
    } finally {
      adapter.dispose();
    }
  });

  afterEach(() => { artIO.openCodec = null; });

  it("captures a confirmation failure that lands before adoption, rejects the reattachment once and reattaches snapshot-only", async () => {
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider, transport } = await startCodecDraft(session.session);
    const early = draftCodecSession(decodeEnvelope);
    provider.mockResolvedValueOnce(early.session);
    const events: unknown[] = [];
    const policyAtServerHello: unknown[] = [];
    adapter.onEvent((event) => {
      events.push(event);
      // The reason must already be consumed when a serverHello listener runs.
      if ((event as { type?: string }).type === "serverHello") policyAtServerHello.push(policyOf(adapter));
    });
    try {
      const reattached = trackRejection(adapter.reconnectDraft());
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(ws));
      const candidate = MockWebSocket.last!;
      const adoptedAtFailure: boolean[] = [];
      // The contradictory confirmation is delivered from inside the ClientHello
      // send, i.e. after the wrapper is installed and before openPhaseSocket
      // resolves: the failure is published before the adapter adopts the socket.
      candidate.send.mockImplementation((text: unknown) => {
        if (typeof text !== "string" || !text.includes('"type":"ClientHello"')) return;
        adoptedAtFailure.push((adapter as unknown as { ws: unknown }).ws !== transport);
        candidate.dispatchSynthetic("message", JSON.stringify({
          type: "ServerHello",
          data: { server_version: "0.0.0-test", selected_state_codec: "SomeOtherCodec" },
        }));
      });
      candidate.dispatchSynthetic("message", CODEC_SERVER_HELLO);

      expect(await reattached()).toMatchObject({
        code: "WS_CLOSED", message: "Connection closed before draft started", recoverable: true,
      });
      // Reach guard: the codec really was offered, and no socket was adopted yet.
      expect(offeredCodec(candidate)).toBe(true);
      expect(adoptedAtFailure).toEqual([false]);
      expect(provider).toHaveBeenCalledTimes(2);
      // One wrapper failure: lease released once, no resync, one serverHello for the attachment.
      expect(early.close).toHaveBeenCalledOnce();
      expect(candidate.send.mock.calls.filter(([text]) => text === '{"type":"ResyncState"}')).toHaveLength(0);
      expect(policyAtServerHello).toEqual([{ kind: "snapshotOnly", reason: "confirmationFailure" }]);
      expect(events.filter((event) => (event as { type?: string }).type === "serverHello")).toHaveLength(1);

      // The next explicit attachment of the same adapter is snapshot-only.
      const next = await reattachSnapshotOnly(adapter, candidate);
      expect(offeredCodec(next)).toBe(false);
      expect(provider).toHaveBeenCalledTimes(2);
    } finally {
      adapter.dispose();
    }
  });

  it("adopts an already-negotiated candidate when the current attachment degrades, then reattaches snapshot-only", async () => {
    const first = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider } = await startCodecDraft(first.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);
    const second = draftCodecSession(decodeEnvelope);
    provider.mockResolvedValueOnce(second.session);
    try {
      // B holds its lease and awaits its hello while A is still current.
      const settled = trackRejection(adapter.reconnectDraft());
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(ws));
      const candidate = MockWebSocket.last!;
      expect(provider).toHaveBeenCalledTimes(2);

      first.decode.mockImplementationOnce(async () => { throw new Error("decoder trapped"); });
      ws.dispatchSynthetic("message", draftFrame("Delta", stateUpdate("never-applied")));
      await vi.waitFor(() => expect(first.close).toHaveBeenCalledOnce());
      expect(policyOf(adapter)).toEqual({ kind: "snapshotOnly", reason: "decodeFailure" });
      // Reach guard: A is gone and B's lease and handshake are untouched.
      expect(second.close).not.toHaveBeenCalled();
      expect(candidate.close).not.toHaveBeenCalled();

      // B is not retired for A's failure: it negotiates, answers the reattachment and bootstraps.
      candidate.dispatchSynthetic("message", CODEC_SERVER_HELLO);
      await vi.waitFor(() => expect(candidate.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"ReconnectDraft"')));
      expect(offeredCodec(candidate)).toBe(true);
      candidate.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
      receive(candidate, "DraftStateUpdate", { view: createMockDraftView() });
      // The attachment owner is settled by the authoritative reply, not stranded.
      expect(await settled()).toEqual({ resolvedWith: undefined });
      events.length = 0;
      await enterCodecMatch(candidate, events);
      await completeDraftCommand(adapter, candidate, "overlap-command", "codec");
      expect(second.decode).toHaveBeenCalledTimes(3);
      expect(second.close).not.toHaveBeenCalled();

      // The next attachment (C) is plain.
      candidate.dispatchSynthetic("close");
      const plain = await reattachSnapshotOnly(adapter, candidate);
      expect(offeredCodec(plain)).toBe(false);
      expect(provider).toHaveBeenCalledTimes(2);
    } finally {
      adapter.dispose();
    }
  });

  it("keeps negotiating across a recovery, a network reconnect and a second recovery on the replacement", async () => {
    const first = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider } = await startCodecDraft(first.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);
    const seen = (label: string) => expect(events).toContainEqual(expect.objectContaining({
      type: "gameStateUpdated", state: expect.objectContaining({ label }),
    }));
    const resyncs = (socket: MockWebSocket) =>
      socket.send.mock.calls.filter(([text]) => text === '{"type":"ResyncState"}').length;
    try {
      // Recovery one completes without closing and admits a command.
      first.decode.mockImplementationOnce(async () => ({ kind: "resyncRequired" }));
      ws.dispatchSynthetic("message", draftFrame("Delta", stateUpdate("corrupt-1")));
      await vi.waitFor(() => expect(resyncs(ws)).toBe(1));
      ws.dispatchSynthetic("message", draftFrame("Full", stateUpdate("resync-1")));
      await vi.waitFor(() => seen("resync-1"));
      await completeDraftCommand(adapter, ws, "command-1", "codec");
      expect(ws.close).not.toHaveBeenCalled();

      // An explicit reattachment after a network loss still advertises the codec.
      ws.dispatchSynthetic("close");
      await vi.waitFor(() => expect(events).toContainEqual({ type: "disconnected" }));
      const second = draftCodecSession(decodeEnvelope);
      provider.mockResolvedValueOnce(second.session);
      const reattached = adapter.reconnectDraft();
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(ws));
      const replacement = MockWebSocket.last!;
      replacement.dispatchSynthetic("message", CODEC_SERVER_HELLO);
      await vi.waitFor(() => expect(replacement.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"ReconnectDraft"')));
      // Reach guard: the first recovery did not degrade the adapter.
      expect(offeredCodec(replacement)).toBe(true);
      replacement.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
      receive(replacement, "DraftStateUpdate", { view: createMockDraftView() });
      await reattached;
      events.length = 0;
      await enterCodecMatch(replacement, events);

      // One recovery on the replacement wrapper is the first for that wrapper.
      second.decode.mockImplementationOnce(async () => ({ kind: "resyncRequired" }));
      replacement.dispatchSynthetic("message", draftFrame("Delta", stateUpdate("corrupt-2")));
      await vi.waitFor(() => expect(resyncs(replacement)).toBe(1));
      replacement.dispatchSynthetic("message", draftFrame("Full", stateUpdate("resync-2")));
      await vi.waitFor(() => seen("resync-2"));
      await completeDraftCommand(adapter, replacement, "command-2", "codec");
      expect(replacement.close).not.toHaveBeenCalled();
      expect(second.close).not.toHaveBeenCalled();
      expect(policyOf(adapter)).toEqual({ kind: "negotiate" });
      expect(provider).toHaveBeenCalledTimes(2);
    } finally {
      adapter.dispose();
    }
  });

  it("creates the degraded attachment's socket in the same turn, while a negotiating attachment still awaits its lease", async () => {
    const first = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider } = await startCodecDraft(first.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    const second = draftCodecSession(decodeEnvelope);
    provider.mockResolvedValueOnce(second.session);
    try {
      ws.dispatchSynthetic("close");
      await vi.waitFor(() => expect(events).toContainEqual({ type: "disconnected" }));

      // Positive provider case: a negotiating adapter creates no socket until
      // its lease has been prepared.
      const reattached = adapter.reconnectDraft();
      expect(provider).toHaveBeenCalledTimes(2);
      expect(MockWebSocket.last).toBe(ws);
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(ws));
      const candidate = MockWebSocket.last!;
      candidate.dispatchSynthetic("message", CODEC_SERVER_HELLO);
      await vi.waitFor(() => expect(candidate.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"ReconnectDraft"')));
      expect(offeredCodec(candidate)).toBe(true);
      candidate.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
      receive(candidate, "DraftStateUpdate", { view: createMockDraftView() });
      await reattached;

      // A real qualifying failure degrades the adapter ...
      second.decode.mockImplementationOnce(async () => { throw new Error("decoder trapped"); });
      candidate.dispatchSynthetic("message", draftFrame("Delta", stateUpdate("never-applied")));
      await vi.waitFor(() => expect(second.close).toHaveBeenCalledOnce());
      expect(policyOf(adapter)).toEqual({ kind: "snapshotOnly", reason: "decodeFailure" });

      // ... so the next attachment opens its socket without any await in between.
      const again = adapter.reconnectDraft();
      expect(MockWebSocket.last).not.toBe(candidate);
      expect(provider).toHaveBeenCalledTimes(2);
      const plain = MockWebSocket.last!;
      plain.dispatchSynthetic("message", CODEC_SERVER_HELLO);
      await vi.waitFor(() => expect(plain.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"ReconnectDraft"')));
      expect(offeredCodec(plain)).toBe(false);
      receive(plain, "DraftStateUpdate", { view: createMockDraftView() });
      await again;
    } finally {
      adapter.dispose();
    }
  });

  it("settles an adopted candidate normally when the shared runtime traps on the retired attachment before the candidate's next decode", async () => {
    const { runtime, script, provider: runtimeProvider } = realRuntimeCodec();
    const lease = await runtimeProvider();
    const { adapter, ws, provider } = await startCodecDraft(lease!);
    provider.mockImplementation(async () => (await runtimeProvider())!);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);
    try {
      expect(runtime.state).toEqual({ kind: "ready" });
      // B bootstraps on its own session of the same runtime while A is retired but still live.
      const reattached = adapter.reconnectDraft();
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(ws));
      const candidate = MockWebSocket.last!;
      candidate.dispatchSynthetic("message", CODEC_SERVER_HELLO);
      await vi.waitFor(() => expect(candidate.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"ReconnectDraft"')));
      expect(offeredCodec(candidate)).toBe(true);
      candidate.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
      receive(candidate, "DraftStateUpdate", { view: createMockDraftView() });
      await reattached;
      events.length = 0;
      await enterCodecMatch(candidate, events);
      expect(provider).toHaveBeenCalledTimes(2);
      const pending = trackRejection(adapter.submitAction(pass, 0));
      await vi.waitFor(() => expect(submissionTypes(candidate)).toHaveLength(1));

      // A genuine wasm trap on the retired attachment poisons the shared runtime.
      script.trapNext = true;
      ws.dispatchSynthetic("message", draftFrame("Delta", stateUpdate("trap")));
      await vi.waitFor(() => expect(runtime.state).toEqual({ kind: "poisoned", reason: "wasmTrap" }));
      await vi.waitFor(() => expect(ws.close).toHaveBeenCalled());
      // Reach guard: B already held a session and was neither closed nor degraded by A's trap.
      expect(candidate.close).not.toHaveBeenCalled();
      expect(policyOf(adapter)).toEqual({ kind: "negotiate" });

      // B's next decode meets the poison: its owner settles, it is not abandoned.
      events.length = 0;
      candidate.dispatchSynthetic("message", draftFrame("Delta", stateUpdate("poisoned")));
      expect(await pending()).toMatchObject({
        code: "WS_CLOSED", message: "Connection closed during action", recoverable: true,
      });
      await vi.waitFor(() => expect(events).toContainEqual({ type: "disconnected" }));
      expect(candidate.close).toHaveBeenCalled();
      expect(policyOf(adapter)).toEqual({ kind: "snapshotOnly", reason: "decodeFailure" });

      // The explicit reattachment continues on plain frames without the provider.
      const plain = await reattachSnapshotOnly(adapter, candidate);
      expect(offeredCodec(plain)).toBe(false);
      expect(provider).toHaveBeenCalledTimes(2);
      await enterPlainMatch(plain, events);
      await completeDraftCommand(adapter, plain, "after-trap", "plain");
    } finally {
      adapter.dispose();
    }
  });

  it("completes a command on a default-constructed fresh draft adapter after the trap, through the shared facade", async () => {
    const { engine, runtime, script, provider } = realRuntimeCodec();
    artIO.openCodec = provider;
    const settings = { displayName: "Alice", setCodes: ["MKM"], kind: "Premier" as const,
      public: true, tournamentFormat: "Swiss" as const, podPolicy: "Competitive" as const, podSize: 8 };
    MockWebSocket.last = null;
    const first = new ServerDraftAdapter("ws://localhost:9374/ws");
    const firstEvents: unknown[] = [];
    first.onEvent((event) => firstEvents.push(event));
    const creating = first.createDraft(settings);
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
    const trapped = MockWebSocket.last!;
    trapped.dispatchSynthetic("message", CODEC_SERVER_HELLO);
    await vi.waitFor(() => expect(trapped.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"CreateDraftWithSettings"')));
    expect(offeredCodec(trapped)).toBe(true);
    trapped.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
    receive(trapped, "DraftCreated", { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 });
    await creating;
    receive(trapped, "DraftMatchStart", { match_id: "r1-t0", round: 1, game_code: FULL_KEY.game_code,
      full_key: FULL_KEY, player_token: "match-token", your_player: 0, opponent_name: "Bob" });
    script.trapNext = true;
    trapped.dispatchSynthetic("message", draftFrame("Full", { type: "GameStarted",
      data: { state: matchState("trap"), your_player: 0, full_key: FULL_KEY } }));
    await vi.waitFor(() => expect(firstEvents).toContainEqual({ type: "disconnected" }));
    expect(runtime.state).toEqual({ kind: "poisoned", reason: "wasmTrap" });
    first.dispose();

    const second = new ServerDraftAdapter("ws://localhost:9374/ws");
    const events: unknown[] = [];
    second.onEvent((event) => events.push(event));
    try {
      const creatingNext = second.createDraft(settings);
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(trapped));
      const plain = MockWebSocket.last!;
      plain.dispatchSynthetic("message", CODEC_SERVER_HELLO);
      await vi.waitFor(() => expect(plain.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"CreateDraftWithSettings"')));
      // The default route consulted the shared facade; the runtime itself refused the lease.
      expect(provider).toHaveBeenCalledTimes(2);
      expect(offeredCodec(plain)).toBe(false);
      receive(plain, "DraftCreated", { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 });
      await creatingNext;

      // A genuine plain match, then a correlated command completion.
      await enterPlainMatch(plain, events);
      await completeDraftCommand(second, plain, "plain-after-trap", "plain");
      expect(engine.state_codec_decode).toHaveBeenCalledOnce();
    } finally {
      second.dispose();
    }
  });

  it("offers no codec from a fresh draft adapter after a decoder trap on the shared runtime", async () => {
    const engine = {
      state_codec_decode: vi.fn(() => { throw new WebAssembly.RuntimeError("unreachable"); }),
    };
    const runtime = new StateCodecRuntime(engine as unknown as EngineModule);
    const provider = vi.fn(async (): Promise<StateCodecSession | null> => {
      const sessionId = runtime.open();
      return {
        decode: async (frame) => runtime.decode(sessionId, frame),
        close: () => runtime.close(sessionId),
      };
    });
    const create = (adapter: ServerDraftAdapter) => adapter.createDraft({
      displayName: "Alice", setCodes: ["MKM"], kind: "Premier", public: true,
      tournamentFormat: "Swiss", podPolicy: "Competitive", podSize: 8,
    });

    MockWebSocket.last = null;
    const first = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: provider });
    const firstEvents: unknown[] = [];
    first.onEvent((event) => firstEvents.push(event));
    const creating = create(first);
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
    const trapped = MockWebSocket.last!;
    trapped.dispatchSynthetic("message", CODEC_SERVER_HELLO);
    await vi.waitFor(() => expect(trapped.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"CreateDraftWithSettings"')));
    expect(offeredCodec(trapped)).toBe(true);
    trapped.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
    receive(trapped, "DraftCreated", { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 });
    await creating;
    receive(trapped, "DraftMatchStart", { match_id: "r1-t0", round: 1, game_code: FULL_KEY.game_code,
      full_key: FULL_KEY, player_token: "match-token", your_player: 0, opponent_name: "Bob" });
    trapped.dispatchSynthetic("message", draftFrame("Full", { type: "GameStarted",
      data: { state: matchState("trap"), your_player: 0, full_key: FULL_KEY } }));
    await vi.waitFor(() => expect(firstEvents).toContainEqual({ type: "disconnected" }));
    expect(engine.state_codec_decode).toHaveBeenCalledOnce();
    expect(runtime.state).toEqual({ kind: "poisoned", reason: "wasmTrap" });
    first.dispose();

    const second = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: provider });
    try {
      const creatingNext = create(second);
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(trapped));
      const plain = MockWebSocket.last!;
      plain.dispatchSynthetic("message", CODEC_SERVER_HELLO);
      await vi.waitFor(() => expect(plain.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"CreateDraftWithSettings"')));
      // The provider was consulted and refused by the runtime itself.
      expect(provider).toHaveBeenCalledTimes(2);
      expect(offeredCodec(plain)).toBe(false);
      receive(plain, "DraftCreated", { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 });
      await creatingNext;
      expect(engine.state_codec_decode).toHaveBeenCalledOnce();
    } finally {
      second.dispose();
    }
  });
});

describe("ServerDraftAdapter — bounded codec preparation", () => {
  const settings = { displayName: "Alice", setCodes: ["MKM"], kind: "Premier" as const,
    public: true, tournamentFormat: "Swiss" as const, podPolicy: "Competitive" as const, podSize: 8 };

  it.each(["throw", "reject", "timeout", "before deadline"] as const)(
    "connects through preparation %s with the selected transport", async (mode) => {
      vi.useFakeTimers();
      const session = draftCodecSession(decodeEnvelope);
      let finish!: (value: StateCodecSession) => void;
      const provider = () => {
        if (mode === "throw") throw new Error("unavailable");
        if (mode === "reject") return Promise.reject(new Error("unavailable"));
        return new Promise<StateCodecSession>((resolve) => { finish = resolve; });
      };
      MockWebSocket.last = null;
      const adapter = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: provider });
      try {
        const created = adapter.createDraft(settings);
        if (mode === "before deadline") {
          await vi.advanceTimersByTimeAsync(4999);
          expect(MockWebSocket.last).toBeNull();
          finish(session.session);
        }
        await vi.advanceTimersByTimeAsync(mode === "timeout" ? 5000 : 0);
        const socket = MockWebSocket.last as MockWebSocket | null;
        expect(socket).not.toBeNull();
        socket!.dispatchSynthetic("message", CODEC_SERVER_HELLO);
        await vi.advanceTimersByTimeAsync(0);
        const negotiated = mode === "before deadline";
        expect(socket!.send.mock.calls.some(([text]) => text.includes("state_codecs"))).toBe(negotiated);
        if (negotiated) socket!.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
        receive(socket!, "DraftCreated", { draft_code: "ABCD12", player_token: "tok", seat_index: 0 });
        await vi.advanceTimersByTimeAsync(0);
        await created;
        if (mode === "timeout") {
          finish(session.session);
          await vi.advanceTimersByTimeAsync(0);
          expect(session.close).toHaveBeenCalledOnce();
          expect(MockWebSocket.last).toBe(socket);
        }
      } finally {
        adapter.dispose();
        vi.useRealTimers();
      }
    },
  );

  it.each(["dispose", "supersede", "resolved then dispose"] as const)(
    "releases the late lease after %s without opening an abandoned socket", async (mode) => {
      MockWebSocket.last = null;
      const session = draftCodecSession(decodeEnvelope);
      let finish!: (value: StateCodecSession) => void;
      const provider = vi.fn(() => new Promise<StateCodecSession | null>((resolve) => { finish = resolve; }));
      const adapter = new ServerDraftAdapter("ws://localhost:9374/ws", { stateCodecSessionProvider: provider });
      const abandoned = adapter.createDraft(settings).catch((error: unknown) => error);
      if (mode === "resolved then dispose") finish(session.session);
      let replacement: Promise<unknown> | null = null;
      if (mode === "supersede") {
        provider.mockImplementationOnce(async () => null);
        replacement = adapter.createDraft(settings).catch((error: unknown) => error);
      } else adapter.dispose();
      expect(await abandoned).toMatchObject({ code: "WS_CLOSED" });
      if (mode !== "resolved then dispose") finish(session.session);
      await vi.waitFor(() => expect(session.close).toHaveBeenCalledOnce());
      if (mode === "supersede") {
        const raw = MockWebSocket.last!;
        expect(raw).not.toBeNull();
        raw.dispatchSynthetic("message", CODEC_SERVER_HELLO);
        await vi.waitFor(() => expect(raw.send).toHaveBeenCalledWith(expect.stringContaining('"type":"CreateDraftWithSettings"')));
        receive(raw, "DraftCreated", { draft_code: "ABCD12", player_token: "tok", seat_index: 0 });
        await replacement;
      } else expect(MockWebSocket.last).toBeNull();
      adapter.dispose();
    },
  );
});

describe("ServerDraftAdapter — commands across socket replacement", () => {
  async function replaceSocket(
    adapter: ServerDraftAdapter,
    provider: { mockResolvedValueOnce: (session: StateCodecSession) => unknown },
    previous: MockWebSocket,
  ) {
    const replacementSession = draftCodecSession(decodeEnvelope);
    provider.mockResolvedValueOnce(replacementSession.session);
    const reattached = adapter.reconnectDraft();
    await vi.waitFor(() => expect(MockWebSocket.last).not.toBe(previous));
    const replacement = MockWebSocket.last!;
    replacement.dispatchSynthetic("message", CODEC_SERVER_HELLO);
    await vi.waitFor(() => expect(replacement.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"ReconnectDraft"')));
    replacement.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
    receive(replacement, "DraftStateUpdate", { view: createMockDraftView() });
    await reattached;
    return { replacement, replacementSession };
  }

  it.each(["is answered", "is closed"] as const)(
    "settles a command claimed before reconnectDraft replaced the socket when the retired socket %s",
    async (how) => {
      const session = draftCodecSession(decodeEnvelope);
      const { adapter, ws, provider } = await startCodecDraft(session.session);
      const events: unknown[] = [];
      adapter.onEvent((event) => events.push(event));
      await enterCodecMatch(ws, events);

      const claimed = trackRejection(adapter.submitAction({ type: "PassPriority" } as GameAction, 0));
      const claimedId = lastSubmissionCommandId(ws);
      const { replacement } = await replaceSocket(adapter, provider, ws);
      // Reach guard: the replacement is current, yet the claimed command is
      // still in flight, so a second submission is refused.
      await expect(adapter.submitAction({ type: "PassPriority" } as GameAction, 0)).rejects
        .toMatchObject({ code: "ACTION_NOT_SENT" });
      events.length = 0;

      if (how === "is answered") {
        ws.dispatchSynthetic("message", draftFrame("Delta", {
          type: "StateUpdate", command_id: claimedId,
          data: { state: matchState("retired-answer"), events: [{ marker: "retired-answer" }], full_key: FULL_KEY },
        }));
        await expect(claimed()).resolves.toMatchObject({
          resolvedWith: { events: [{ marker: "retired-answer" }] },
        });
        // The retired socket never replaces the replacement's state.
        expect(events).not.toContainEqual(expect.objectContaining({ type: "gameStateUpdated" }));
      } else {
        ws.dispatchSynthetic("close");
        await expect(claimed()).resolves.toMatchObject({
          code: "WS_CLOSED", message: "Connection closed during action", recoverable: true,
        });
      }
      expect(events).toContainEqual({ type: "actionPendingChanged", pending: false });

      // The slot is free again: the replacement can carry the next command.
      await enterCodecMatch(replacement, events);
      void adapter.submitAction({ type: "PassPriority" } as GameAction, 0).catch(() => undefined);
      await vi.waitFor(() => expect(submissionTypes(replacement)).toHaveLength(1));
      adapter.dispose();
    },
  );

  describe("snapshot paired with a retired socket's answer", () => {
    const passPriority = { type: "PassPriority" } as GameAction;
    const labelOf = async (adapter: ServerDraftAdapter) =>
      (await adapter.getSnapshot()).state as unknown as { label: string };

    it("caches the retired reply's state before the command resolves while the replacement has delivered none", async () => {
      const session = draftCodecSession(decodeEnvelope);
      const { adapter, ws, provider } = await startCodecDraft(session.session);
      const events: unknown[] = [];
      adapter.onEvent((event) => events.push(event));
      await enterCodecMatch(ws, events);

      const claimed = adapter.submitAction(passPriority, 0);
      const observed = trackRejection(claimed);
      const claimedId = lastSubmissionCommandId(ws);
      await replaceSocket(adapter, provider, ws);
      // Reach guard: the replacement delivered no game state, so the only
      // snapshot is still the retired socket's earlier one.
      expect(await labelOf(adapter)).toMatchObject({ label: "accepted-update" });
      events.length = 0;

      ws.dispatchSynthetic("message", draftFrame("Delta", {
        type: "StateUpdate", command_id: claimedId,
        data: {
          state: matchState("retired-answer"), events: [{ marker: "retired-answer" }],
          legal_actions: [passPriority], full_key: FULL_KEY,
        },
      }));
      // The caller reads the snapshot in the same turn the command resolves.
      const result = await claimed;
      expect(result.events).toEqual([{ marker: "retired-answer" }]);
      expect(await labelOf(adapter)).toMatchObject({ label: "retired-answer" });
      expect((await adapter.getLegalActions()).actions).toEqual([passPriority]);
      expect(await observed()).toMatchObject({ resolvedWith: { events: [{ marker: "retired-answer" }] } });
      // The retired socket still never delivers game state.
      expect(events).toContainEqual({ type: "actionPendingChanged", pending: false });
      expect(events).not.toContainEqual(expect.objectContaining({ type: "gameStateUpdated" }));
      adapter.dispose();
    });

    it("keeps the replacement's own state when the retired reply arrives after it", async () => {
      const session = draftCodecSession(decodeEnvelope);
      const { adapter, ws, provider } = await startCodecDraft(session.session);
      const events: unknown[] = [];
      adapter.onEvent((event) => events.push(event));
      await enterCodecMatch(ws, events);

      const claimed = adapter.submitAction(passPriority, 0);
      const claimedId = lastSubmissionCommandId(ws);
      const { replacement } = await replaceSocket(adapter, provider, ws);
      await enterCodecMatch(replacement, events);
      replacement.dispatchSynthetic("message", draftFrame("Delta", {
        type: "StateUpdate",
        data: { state: matchState("replacement-own"), events: [], full_key: FULL_KEY },
      }));
      await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({
        type: "gameStateUpdated", state: expect.objectContaining({ label: "replacement-own" }),
      })));
      // Reach guard: the replacement owns the snapshot before the retired reply.
      expect(await labelOf(adapter)).toMatchObject({ label: "replacement-own" });
      events.length = 0;

      ws.dispatchSynthetic("message", draftFrame("Delta", {
        type: "StateUpdate", command_id: claimedId,
        data: { state: matchState("retired-answer"), events: [{ marker: "retired-answer" }], full_key: FULL_KEY },
      }));
      const result = await claimed;
      // The command still resolves with the retired reply's events, positively
      // proving the frame was processed, yet the snapshot does not roll back.
      expect(result.events).toEqual([{ marker: "retired-answer" }]);
      expect(await labelOf(adapter)).toMatchObject({ label: "replacement-own" });
      expect(events).toContainEqual({ type: "actionPendingChanged", pending: false });
      expect(events).not.toContainEqual(expect.objectContaining({ type: "gameStateUpdated" }));
      adapter.dispose();
    });

    it("lets the replacement's later state supersede the retired reply's cached state", async () => {
      const session = draftCodecSession(decodeEnvelope);
      const { adapter, ws, provider } = await startCodecDraft(session.session);
      const events: unknown[] = [];
      adapter.onEvent((event) => events.push(event));
      await enterCodecMatch(ws, events);

      const claimed = adapter.submitAction(passPriority, 0);
      const claimedId = lastSubmissionCommandId(ws);
      const { replacement } = await replaceSocket(adapter, provider, ws);
      ws.dispatchSynthetic("message", draftFrame("Delta", {
        type: "StateUpdate", command_id: claimedId,
        data: { state: matchState("retired-answer"), events: [{ marker: "retired-answer" }], full_key: FULL_KEY },
      }));
      await claimed;
      // Reach guard: the retired reply's state was cached.
      expect(await labelOf(adapter)).toMatchObject({ label: "retired-answer" });
      events.length = 0;

      await enterCodecMatch(replacement, events);
      expect(await labelOf(adapter)).toMatchObject({ label: "accepted-update" });
      replacement.dispatchSynthetic("message", draftFrame("Delta", {
        type: "StateUpdate",
        data: { state: matchState("replacement-newer"), events: [{ marker: "newer" }], full_key: FULL_KEY },
      }));
      await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({
        type: "gameStateUpdated",
        state: expect.objectContaining({ label: "replacement-newer" }),
        events: [{ marker: "newer" }],
      })));
      expect(await labelOf(adapter)).toMatchObject({ label: "replacement-newer" });
      adapter.dispose();
    });
  });

  it.each([
    ["omits the Full-session key", undefined],
    ["carries another generation's key", { ...FULL_KEY, generation: FULL_KEY.generation + 1 }],
  ] as const)(
    "leaves a command claimed on the retired socket pending when its completion %s",
    async (_label, fullKey) => {
      const session = draftCodecSession(decodeEnvelope);
      const { adapter, ws, provider } = await startCodecDraft(session.session);
      const events: unknown[] = [];
      adapter.onEvent((event) => events.push(event));
      await enterCodecMatch(ws, events);

      const claimed = trackRejection(adapter.submitAction({ type: "PassPriority" } as GameAction, 0));
      const claimedId = lastSubmissionCommandId(ws);
      await replaceSocket(adapter, provider, ws);
      events.length = 0;

      // Reach guard source: the frame is decoded and routed by the retired
      // wrapper (the decoder call count advances) before the identity fence.
      const decodedBefore = session.decode.mock.calls.length;
      ws.dispatchSynthetic("message", draftFrame("Delta", {
        type: "StateUpdate", command_id: claimedId,
        data: { state: matchState("wrong-match"), events: [{ marker: "wrong-match" }], full_key: fullKey },
      }));
      await vi.waitFor(() => expect(session.decode.mock.calls.length).toBeGreaterThan(decodedBefore));
      // Drain the receive queue, then nothing may have settled or been emitted.
      expect(await claimed()).toBe("never settled");
      expect(events).not.toContainEqual({ type: "actionPendingChanged", pending: false });
      expect(events).not.toContainEqual(expect.objectContaining({ type: "gameStateUpdated" }));

      // Positive control: a completion carrying the accepted key still settles
      // the same command, with its own payload and not the rejected frame's.
      ws.dispatchSynthetic("message", draftFrame("Delta", {
        type: "StateUpdate", command_id: claimedId,
        data: { state: matchState("right-match"), events: [{ marker: "right-match" }], full_key: FULL_KEY },
      }));
      await expect(claimed()).resolves.toMatchObject({
        resolvedWith: { events: [{ marker: "right-match" }] },
      });
      expect(events).toContainEqual({ type: "actionPendingChanged", pending: false });
      adapter.dispose();
    },
  );

  it("leaves a command claimed on the replacement pending when the retired socket closes", async () => {
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider } = await startCodecDraft(session.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);
    const { replacement } = await replaceSocket(adapter, provider, ws);
    await enterCodecMatch(replacement, events);
    events.length = 0;

    const pending = adapter.submitAction({ type: "PassPriority" } as GameAction, 0);
    const observed = trackRejection(pending);
    const commandId = lastSubmissionCommandId(replacement);
    ws.dispatchSynthetic("close");
    // Reach guard: the retired transport's close was processed.
    await vi.waitFor(() => expect(session.close).toHaveBeenCalledOnce());
    expect(events).not.toContainEqual({ type: "actionPendingChanged", pending: false });

    replacement.dispatchSynthetic("message", draftFrame("Delta", {
      type: "StateUpdate", command_id: commandId,
      data: { state: matchState("own"), events: [], full_key: FULL_KEY },
    }));
    await expect(observed()).resolves.toMatchObject({ resolvedWith: { events: [] } });
    adapter.dispose();
  });

  it("leaves a command claimed on the retired socket pending when the current socket's codec recovers", async () => {
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider } = await startCodecDraft(session.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);

    const claimed = trackRejection(adapter.submitAction({ type: "PassPriority" } as GameAction, 0));
    const claimedId = lastSubmissionCommandId(ws);
    const { replacement, replacementSession } = await replaceSocket(adapter, provider, ws);
    // Drop the retired socket's state events so the wait below observes the replacement's own GameStarted.
    events.length = 0;
    await enterCodecMatch(replacement, events);
    events.length = 0;

    // The replacement's codec stream rejects a frame: recovery of the CURRENT socket.
    replacementSession.decode.mockImplementationOnce(async () => ({ kind: "resyncRequired" }));
    replacement.dispatchSynthetic("message", draftFrame("Delta", {
      type: "StateUpdate",
      data: { state: matchState("corrupt"), events: [], full_key: FULL_KEY },
    }));
    await vi.waitFor(() => expect(replacement.send).toHaveBeenCalledWith('{"type":"ResyncState"}'));
    // Reach guard: the recovery gate is set, so the interruption ran.
    await vi.waitFor(() => expect(() => (adapter as unknown as { stateCodec: { assertNotRecovering(): void } }).stateCodec.assertNotRecovering())
      .toThrow());
    // The command belongs to the still-open retired socket: not rejected, not released.
    expect(await claimed()).toBe("never settled");
    expect(events).not.toContainEqual({ type: "actionPendingChanged", pending: false });

    // The retired socket's valid-key completion still settles it.
    ws.dispatchSynthetic("message", draftFrame("Delta", {
      type: "StateUpdate", command_id: claimedId,
      data: { state: matchState("retired-answer"), events: [{ marker: "retired-answer" }], full_key: FULL_KEY },
    }));
    await expect(claimed()).resolves.toMatchObject({
      resolvedWith: { events: [{ marker: "retired-answer" }] },
    });
    expect(events).toContainEqual({ type: "actionPendingChanged", pending: false });
    adapter.dispose();
  });

  it("still drops a command claimed on the current socket when that socket's codec recovers after a replacement", async () => {
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider } = await startCodecDraft(session.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);
    const { replacement, replacementSession } = await replaceSocket(adapter, provider, ws);
    // Drop the retired socket's state events so the wait below observes the replacement's own GameStarted.
    events.length = 0;
    await enterCodecMatch(replacement, events);
    events.length = 0;

    const claimed = trackRejection(adapter.submitAction({ type: "PassPriority" } as GameAction, 0));
    lastSubmissionCommandId(replacement);
    replacementSession.decode.mockImplementationOnce(async () => ({ kind: "resyncRequired" }));
    replacement.dispatchSynthetic("message", draftFrame("Delta", {
      type: "StateUpdate",
      data: { state: matchState("corrupt"), events: [], full_key: FULL_KEY },
    }));
    await vi.waitFor(() => expect(replacement.send).toHaveBeenCalledWith('{"type":"ResyncState"}'));
    await expect(claimed()).resolves.toMatchObject({
      code: "WS_CLOSED", recoverable: true,
    });
    expect(events).toContainEqual({ type: "actionPendingChanged", pending: false });
    adapter.dispose();
  });

  // A pending preview is owned by the socket it was sent on: a retired socket's
  // close can no longer be answered for its own previews, and must not touch the
  // replacement's.
  const PREVIEW_KINDS = {
    "mana-payment": {
      frameType: "PreviewManaPayment",
      closedMessage: "Connection closed during mana-payment preview",
      start: (adapter: ServerDraftAdapter, _tag: string) =>
        adapter.previewManaPayment({ type: "PassPriority" } as GameAction, 0),
      answer: (target: MockWebSocket, _tag: string) => {
        const calls = target.send.mock.calls
          .map(([raw]) => JSON.parse(raw as string) as { type?: string; data?: { request_id: number } })
          .filter((frame) => frame.type === "PreviewManaPayment");
        receive(target, "ManaPaymentPreview", {
          request_id: calls[calls.length - 1].data!.request_id,
          source_ids: [7],
        });
        return [7];
      },
    },
    interaction: {
      frameType: "PreviewInteraction",
      closedMessage: "Connection closed during interaction preview",
      start: (adapter: ServerDraftAdapter, tag: string) =>
        adapter.previewInteraction({
          requestId: tag as PreviewRequestId,
          interactionId: "interaction-1" as InteractionId,
          response: {
            type: "shortcut",
            data: { decision: { type: "fixed", data: { iterations: 6 } }, pins: [] },
          },
        } satisfies InteractionPreviewRequest, 0),
      answer: (target: MockWebSocket, tag: string) => {
        receive(target, "InteractionPreview", { preview: { requestId: tag } });
        return { requestId: tag };
      },
    },
  } as const;

  it.each(Object.keys(PREVIEW_KINDS) as (keyof typeof PREVIEW_KINDS)[])(
    "rejects the %s preview sent on the retired socket when that socket closes after reconnectDraft",
    async (kind) => {
      const spec = PREVIEW_KINDS[kind];
      const session = draftCodecSession(decodeEnvelope);
      const { adapter, ws, provider } = await startCodecDraft(session.session);
      const events: unknown[] = [];
      adapter.onEvent((event) => events.push(event));
      await enterCodecMatch(ws, events);
      const retiredPreview = trackRejection(spec.start(adapter, "preview-retired"));
      // Reach guard: the preview really went out on the soon-to-be-retired socket.
      expect(ws.send.mock.calls.map(([raw]) => (JSON.parse(raw as string) as { type?: string }).type))
        .toContain(spec.frameType);

      await replaceSocket(adapter, provider, ws);
      // Adopting the replacement does not settle it; only the retired close does.
      await expect(retiredPreview()).resolves.toBe("never settled");

      ws.dispatchSynthetic("close");
      await expect(retiredPreview()).resolves.toMatchObject({
        code: "WS_CLOSED", message: spec.closedMessage, recoverable: true,
      });
      adapter.dispose();
    },
  );

  it.each(Object.keys(PREVIEW_KINDS) as (keyof typeof PREVIEW_KINDS)[])(
    "leaves the %s preview sent on the replacement pending when the retired socket closes",
    async (kind) => {
      const spec = PREVIEW_KINDS[kind];
      const session = draftCodecSession(decodeEnvelope);
      const { adapter, ws, provider } = await startCodecDraft(session.session);
      const events: unknown[] = [];
      adapter.onEvent((event) => events.push(event));
      await enterCodecMatch(ws, events);
      const retiredPreview = trackRejection(spec.start(adapter, "preview-retired"));
      const { replacement } = await replaceSocket(adapter, provider, ws);
      await enterCodecMatch(replacement, events);
      const ownPreview = trackRejection(spec.start(adapter, "preview-own"));
      expect(replacement.send.mock.calls.map(([raw]) => (JSON.parse(raw as string) as { type?: string }).type))
        .toContain(spec.frameType);

      ws.dispatchSynthetic("close");
      // Reach guard: the retired close was processed and rejected its own preview.
      await expect(retiredPreview()).resolves.toMatchObject({
        code: "WS_CLOSED", message: spec.closedMessage,
      });
      // The replacement's own preview is untouched, and still resolves.
      await expect(ownPreview()).resolves.toBe("never settled");
      const answer = spec.answer(replacement, "preview-own");
      await expect(ownPreview()).resolves.toMatchObject({ resolvedWith: answer });
      adapter.dispose();
    },
  );

  it("keeps the still-current socket on the recovery path while a replacement attachment starts", async () => {
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws, provider } = await startCodecDraft(session.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);

    // The replacement attachment is still preparing; the old socket is current.
    let release!: (session: StateCodecSession | null) => void;
    provider.mockImplementationOnce(() => new Promise<StateCodecSession>((resolve) => {
      release = resolve as (session: StateCodecSession | null) => void;
    }));
    const reattached = adapter.reconnectDraft();
    void reattached.catch(() => undefined);
    await vi.waitFor(() => expect(release).toBeTypeOf("function"));

    const claimed = trackRejection(adapter.submitAction({ type: "PassPriority" } as GameAction, 0));
    session.decode.mockImplementationOnce(async () => ({ kind: "resyncRequired" }));
    ws.dispatchSynthetic("message", draftFrame("Delta", {
      type: "StateUpdate",
      data: { state: matchState("corrupt"), events: [], full_key: FULL_KEY },
    }));
    await vi.waitFor(() => expect(ws.send).toHaveBeenCalledWith('{"type":"ResyncState"}'));
    // Recovery path, not the pre-bootstrap fatal path.
    await expect(claimed()).resolves.toMatchObject({
      code: "WS_CLOSED", recoverable: true,
    });
    expect(ws.close).not.toHaveBeenCalled();
    expect(session.close).not.toHaveBeenCalled();
    await expect(adapter.submitAction({ type: "PassPriority" } as GameAction, 0)).rejects
      .toMatchObject({ code: "ACTION_NOT_SENT" });

    ws.dispatchSynthetic("message", draftFrame("Full", {
      type: "StateUpdate",
      data: { state: matchState("resync"), events: [], full_key: FULL_KEY },
    }));
    await vi.waitFor(() => expect(events).toContainEqual(expect.objectContaining({
      type: "gameStateUpdated", state: expect.objectContaining({ label: "resync" }),
    })));
    void adapter.submitAction({ type: "PassPriority" } as GameAction, 0).catch(() => undefined);
    await vi.waitFor(() => expect(submissionTypes(ws)).toHaveLength(2));
    adapter.dispose();
    release(null);
  });

  it("reports an adapter dispose, not a lost connection, to a pending action in codec mode", async () => {
    const session = draftCodecSession(decodeEnvelope);
    const { adapter, ws } = await startCodecDraft(session.session);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await enterCodecMatch(ws, events);
    const pending = trackRejection(adapter.submitAction({ type: "PassPriority" } as GameAction, 0));
    expect(submissionTypes(ws)).toHaveLength(1);

    adapter.dispose();
    await expect(pending()).resolves.toMatchObject({
      code: "WS_CLOSED", message: "Adapter disposed during action", recoverable: true,
    });
    expect(session.close).toHaveBeenCalledOnce();
    expect(ws.close).toHaveBeenCalled();
  });
});

describe("ServerDraftAdapter — production codec provider default", () => {
  afterEach(() => { artIO.openCodec = null; });

  it("advertises StateCodecV1 and decodes through the shared engine's session when no provider is injected", async () => {
    const codec = draftCodecSession(decodeEnvelope);
    const open = vi.fn(() => Promise.resolve(codec.session));
    artIO.openCodec = open;
    MockWebSocket.last = null;
    // No options at all: the adapter's default provider applies.
    const adapter = new ServerDraftAdapter("ws://localhost:9374/ws");
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    try {
      const createPromise = adapter.createDraft({
        displayName: "Alice",
        setCodes: ["MKM"],
        kind: "Premier",
        public: true,
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
        podSize: 8,
      });
      await vi.waitFor(() => expect(MockWebSocket.last).not.toBeNull());
      const ws = MockWebSocket.last!;
      ws.dispatchSynthetic("message", CODEC_SERVER_HELLO);
      await vi.waitFor(() => expect(ws.send).toHaveBeenCalledWith(
        expect.stringContaining('"type":"CreateDraftWithSettings"'),
      ));
      const hello = ws.send.mock.calls
        .map(([text]) => JSON.parse(text as string) as { type?: string; data?: { state_codecs?: unknown } })
        .find((frame) => frame.type === "ClientHello");
      expect(hello?.data?.state_codecs).toEqual(["StateCodecV1"]);
      expect(open).toHaveBeenCalledOnce();

      ws.dispatchSynthetic("message", CODEC_SERVER_CONFIRMATION);
      receive(ws, "DraftCreated", { draft_code: "ABCD12", player_token: "tok123", seat_index: 0 });
      await createPromise;
      await enterCodecMatch(ws, events);
      // The Full frames reached the adapter only through the shared engine's session.
      expect(codec.decode).toHaveBeenCalledTimes(2);
      expect(events).toContainEqual(expect.objectContaining({
        type: "gameStateUpdated", state: expect.objectContaining({ label: "accepted-update" }),
      }));
    } finally {
      adapter.dispose();
    }
  });
});
