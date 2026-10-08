import { beforeEach, describe, expect, it, vi } from "vitest";

import { EMPTY_DRAFT_POOL_GROUPS } from "../draft-adapter";
import { ServerDraftAdapter } from "../server-draft-adapter";
import { PROTOCOL_VERSION } from "../ws-adapter";
import { AdapterErrorCode } from "../types";
import type { DraftPlayerView } from "../draft-adapter";
import type { GameAction, GameLogEntry, GameState, LegalActionsResult, ObjectAction, SubmitResult } from "../types";
import type {
  InteractionChoiceId,
  InteractionId,
  InteractionPreviewRequest,
  InteractionSubmission,
  PreviewRequestId,
} from "../generated/interaction";

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

/**
 * Drives an adapter through the openPhaseSocket handshake.
 * Returns the mock ws after the handshake settles.
 */
async function completeHandshake(): Promise<MockWebSocket> {
  await Promise.resolve();
  const ws = MockWebSocket.last!;
  ws.dispatchSynthetic("message", SERVER_HELLO);
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
    adapter = new ServerDraftAdapter("ws://localhost:9374/ws");
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
    const chaos = new ServerDraftAdapter("ws://localhost:9374/ws");
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
    const staleAdapter = new ServerDraftAdapter("ws://localhost:9374/ws");
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
    const setupFailingAdapter = new ServerDraftAdapter("ws://localhost:9374/ws");
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
        code: "WS_ERROR",
        message: "action persistence failed",
        recoverable: false,
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
        code: "WS_ERROR",
        message: "preview lookup failed",
        recoverable: false,
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
      const lobbyAdapter = new ServerDraftAdapter("ws://localhost:9374/ws");
      await expect(lobbyAdapter.previewInteraction(previewRequest, 0)).rejects.toMatchObject({
        code: "PHASE_ERROR",
      });
      // Reach guard: the adapter under test IS in the match phase, so the
      // refusal above is the precondition and not a universally-throwing method.
      expect(adapter.currentPhase).toBe("match");
    });
  });
});
