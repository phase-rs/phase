import { beforeEach, describe, expect, it, vi } from "vitest";

import { EMPTY_DRAFT_POOL_GROUPS } from "../draft-adapter";
import { ServerDraftAdapter } from "../server-draft-adapter";
import { PROTOCOL_VERSION } from "../ws-adapter";
import type { DraftPlayerView } from "../draft-adapter";
import type { GameLogEntry, GameState, LegalActionsResult, ObjectAction } from "../types";

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
      this.onmessage?.({ data });
      this.dispatchEvent(new MessageEvent("message", { data }));
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

function createMockDraftView(overrides: Partial<DraftPlayerView> = {}): DraftPlayerView {
  return {
    status: "Drafting",
    kind: "Premier",
    launch_capability: "None",
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

  it("routes submitAction only during match phase", async () => {
    // Not in match phase yet — should throw.
    await expect(adapter.submitAction({ type: "PassPriority" }, 0)).rejects.toThrow(
      "Not in a match phase",
    );
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
          player_token: "gametok",
          your_player: 0,
          opponent_name: "Bob",
        },
      }),
    );

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
        type: "GameStarted",
        data: {
          state,
          your_player: 0,
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
        type: "StateUpdate",
        data: {
          state: matchState("server-draft-update"),
          events: [],
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
  });
});
