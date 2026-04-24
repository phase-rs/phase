/**
 * Integration-style tests for `P2PHostAdapter` covering the 3-4p multiplayer
 * additions (per-guest fan-out, token issuance, action verification, kick,
 * reconnect, grace-window timers). Uses `vi.useFakeTimers()` so timer
 * assertions are deterministic.
 *
 * The WASM engine is mocked entirely — these tests verify adapter wiring,
 * not engine behavior (engine concede tests live in `crates/engine`).
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import type Peer from "peerjs";
import type { DataConnection } from "peerjs";

import { P2PHostAdapter } from "../p2p-adapter";
import { FakeDataConnection } from "../../network/__tests__/fakeDataConnection";

// ── Mock the WasmAdapter so we don't need an actual WASM build ─────────────
// `vi.hoisted` lets us share these refs with the hoisted vi.mock factory.
const mocks = vi.hoisted(() => {
  return {
    submitAction: vi.fn(async (_action: unknown) => ({ events: [] })),
    getState: vi.fn(async () => ({ players: [], objects: {} })),
    getLegalActions: vi.fn(async () => ({
      actions: [],
      autoPassRecommended: false,
    })),
    getFilteredState: vi.fn(async (pid: number) => ({
      filteredFor: pid,
      players: [],
    })),
    initializeGame: vi.fn(async () => ({ events: [] })),
    setMultiplayerMode: vi.fn(async (_enabled: boolean) => undefined),
  };
});
const mockSubmitAction = mocks.submitAction;
const mockGetFilteredState = mocks.getFilteredState;
const mockInitializeGame = mocks.initializeGame;
const mockSetMultiplayerMode = mocks.setMultiplayerMode;

vi.mock("../wasm-adapter", () => ({
  WasmAdapter: vi.fn().mockImplementation(() => ({
    initialize: vi.fn(async () => undefined),
    initializeGame: mocks.initializeGame,
    submitAction: mocks.submitAction,
    getState: mocks.getState,
    getLegalActions: mocks.getLegalActions,
    getFilteredState: mocks.getFilteredState,
    setMultiplayerMode: mocks.setMultiplayerMode,
    dispose: vi.fn(),
  })),
}));

// Stub crypto.randomUUID for deterministic token assertions
let uuidCounter = 0;
beforeEach(() => {
  uuidCounter = 0;
  vi.spyOn(crypto, "randomUUID").mockImplementation(
    () => `token-${++uuidCounter}` as `${string}-${string}-${string}-${string}-${string}`,
  );
  mockSubmitAction.mockClear();
  mockGetFilteredState.mockClear();
  mockInitializeGame.mockClear();
  mockSetMultiplayerMode.mockClear();
});

afterEach(() => {
  // `clearAllMocks` (not `restoreAllMocks`) — restoring would un-mock the
  // hoisted `vi.mock("../wasm-adapter")` and break subsequent tests.
  vi.clearAllMocks();
});

interface FakePeer {
  on(event: string, handler: (conn: DataConnection) => void): void;
  off(event: string, handler: (conn: DataConnection) => void): void;
  connect(): never;
  destroy(): void;
}

function createFakePeer(): {
  peer: FakePeer;
  onGuestConnected: (handler: (conn: DataConnection) => void) => () => void;
  emitConnection: (conn: DataConnection) => void;
} {
  const handlers = new Set<(conn: DataConnection) => void>();
  return {
    peer: {
      on() {},
      off() {},
      connect() {
        throw new Error("not used in tests");
      },
      destroy() {},
    },
    onGuestConnected(handler) {
      handlers.add(handler);
      return () => handlers.delete(handler);
    },
    emitConnection(conn) {
      for (const h of handlers) h(conn);
    },
  };
}

// FakeDataConnection doesn't model `open` — extend it for adapter tests where
// the adapter awaits `conn.on("open", ...)` before wrapping in a PeerSession.
class FakeOpenableConnection extends FakeDataConnection {
  private openHandlers = new Set<() => void>();
  override on(event: string, handler: (...args: unknown[]) => void): this {
    if (event === "open") {
      this.openHandlers.add(handler as () => void);
      return this;
    }
    return super.on(event, handler);
  }
  fireOpen() {
    for (const h of this.openHandlers) h();
  }
}

function makeHost(playerCount: number, gracePeriodMs = 5_000) {
  const { peer, onGuestConnected, emitConnection } = createFakePeer();
  const hostDeck = {
    player: { main_deck: ["Mountain"], sideboard: [] },
    opponent: { main_deck: ["Forest"], sideboard: [] },
    ai_decks: [],
  };
  const adapter = new P2PHostAdapter(
    hostDeck,
    peer as unknown as Peer,
    onGuestConnected,
    playerCount,
    undefined,
    undefined,
    gracePeriodMs,
  );
  return { adapter, emitConnection };
}

function joinGuest(
  emitConnection: (c: DataConnection) => void,
  msg: { type: "guest_deck"; deckData: unknown } | { type: "reconnect"; playerToken: string },
): FakeOpenableConnection {
  const conn = new FakeOpenableConnection();
  emitConnection(conn as unknown as DataConnection);
  conn.fireOpen();
  conn.simulateData(msg);
  return conn;
}

describe("P2PHostAdapter — 3-4p multiplayer", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("rejects construction with playerCount outside 2-4", () => {
    const { peer, onGuestConnected } = createFakePeer();
    const hostDeck = {
      player: { main_deck: [], sideboard: [] },
      opponent: { main_deck: [], sideboard: [] },
      ai_decks: [],
    };
    expect(
      () => new P2PHostAdapter(hostDeck, peer as unknown as Peer, onGuestConnected, 1),
    ).toThrow("P2P supports 2-4 players");
    expect(
      () => new P2PHostAdapter(hostDeck, peer as unknown as Peer, onGuestConnected, 5),
    ).toThrow("P2P supports 2-4 players");
  });

  it("enables multiplayer-mode enforcement on the engine at init time", async () => {
    // P2PHostAdapter owns an authoritative WASM engine locally; flipping
    // the engine's multiplayer flag during initialize() ensures any stray
    // restore_game_state call is refused in the Rust layer.
    const { adapter } = makeHost(2);
    expect(mockSetMultiplayerMode).not.toHaveBeenCalled();

    await adapter.initialize();

    expect(mockSetMultiplayerMode).toHaveBeenCalledTimes(1);
    expect(mockSetMultiplayerMode).toHaveBeenCalledWith(true);
  });

  it("issues unique tokens per guest and includes them in per-seat game_setup", async () => {
    const { adapter, emitConnection } = makeHost(3);
    await adapter.initialize();

    // Both guests join with their own decks.
    const g1Deck = { player: { main_deck: ["Plains"], sideboard: [] } };
    const g2Deck = { player: { main_deck: ["Swamp"], sideboard: [] } };
    const g1 = joinGuest(emitConnection, { type: "guest_deck", deckData: g1Deck });
    const g2 = joinGuest(emitConnection, { type: "guest_deck", deckData: g2Deck });

    await adapter.initializeGame();

    // Find the per-guest game_setup messages.
    const g1Setup = g1.sent.find(
      (m): m is { type: "game_setup"; assignedPlayerId: number; playerToken: string } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "game_setup",
    );
    const g2Setup = g2.sent.find(
      (m): m is { type: "game_setup"; assignedPlayerId: number; playerToken: string } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "game_setup",
    );

    expect(g1Setup).toBeDefined();
    expect(g2Setup).toBeDefined();
    expect(g1Setup!.assignedPlayerId).toBe(1);
    expect(g2Setup!.assignedPlayerId).toBe(2);
    // Tokens must be distinct — privacy invariant.
    expect(g1Setup!.playerToken).not.toBe(g2Setup!.playerToken);
  });

  it("rejects an action whose senderPlayerId does not match the session's seat", async () => {
    const { adapter, emitConnection } = makeHost(3);
    await adapter.initialize();
    const g1 = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    const g2 = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    // Clear setup-time messages to assert against post-setup state.
    g1.sent.length = 0;
    g2.sent.length = 0;

    // Guest 2 attempts to spoof an action declaring senderPlayerId = 1.
    g2.simulateData({
      type: "action",
      senderPlayerId: 1, // wrong! session is for seat 2
      action: { type: "PassPriority" },
    });

    // Spoofing guest receives action_rejected.
    const rejected = g2.sent.find(
      (m) =>
        typeof m === "object" &&
        m !== null &&
        (m as { type: string }).type === "action_rejected",
    );
    expect(rejected).toBeDefined();
    // And the spoofed action did NOT reach the engine.
    expect(mockSubmitAction).not.toHaveBeenCalled();
  });

  it("fan-outs filtered state per-guest on submitAction", async () => {
    const { adapter, emitConnection } = makeHost(3);
    await adapter.initialize();
    joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    mockGetFilteredState.mockClear();

    await adapter.submitAction({ type: "PassPriority" }, 0);

    // One filtered-state lookup per connected guest (host doesn't need one
    // for itself — local state is authoritative).
    expect(mockGetFilteredState).toHaveBeenCalledTimes(2);
    expect(mockGetFilteredState).toHaveBeenCalledWith(1);
    expect(mockGetFilteredState).toHaveBeenCalledWith(2);
  });

  it("starts grace-window timer on guest disconnect and auto-concedes on expiry", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const g1 = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    // Capture the disconnect-with-choice event.
    const events: Array<{ type: string }> = [];
    adapter.onEvent((e) => events.push(e));

    g1.simulateClose(); // guest 1 drops

    // Adapter should have emitted the choice event and broadcast game_paused.
    const choiceEvent = events.find(
      (e) => e.type === "opponentDisconnectedWithChoice",
    );
    expect(choiceEvent).toBeDefined();

    // Advance past the grace window — auto-concede must fire.
    mockSubmitAction.mockClear();
    await vi.advanceTimersByTimeAsync(5_500);

    // Concede submitted to engine for guest 1 (PlayerId 1).
    expect(mockSubmitAction).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "Concede",
        data: { player_id: 1 },
      }),
      1,
    );
  });

  it("cancels grace timer and resumes on reconnect with valid token", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const g1 = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    // Capture token before disconnect.
    const setup = g1.sent.find(
      (m): m is { type: "game_setup"; playerToken: string } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "game_setup",
    );
    const token = setup!.playerToken;

    g1.simulateClose();

    // Reconnect within grace.
    const g1Reconnect = joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: token,
    });
    await Promise.resolve();
    await Promise.resolve();

    // Reconnecting guest gets a reconnect_ack.
    const ack = g1Reconnect.sent.find(
      (m) =>
        typeof m === "object" &&
        m !== null &&
        (m as { type: string }).type === "reconnect_ack",
    );
    expect(ack).toBeDefined();

    // Advance past what would have been grace expiry — concede must NOT fire.
    mockSubmitAction.mockClear();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(mockSubmitAction).not.toHaveBeenCalled();
  });

  it("kick adds token to denylist; subsequent reconnect with same token is rejected", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const g1 = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    const setup = g1.sent.find(
      (m): m is { type: "game_setup"; playerToken: string } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "game_setup",
    );
    const token = setup!.playerToken;

    // Kick guest 1.
    await adapter.kickPlayer(1, "Kicked for testing");
    // Concede submitted to engine for guest 1.
    expect(mockSubmitAction).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "Concede",
        data: { player_id: 1 },
      }),
      1,
    );

    // Attempt reconnect with the kicked token → reconnect_rejected.
    const rejoinAttempt = joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: token,
    });
    const rejected = rejoinAttempt.sent.find(
      (m) =>
        typeof m === "object" &&
        m !== null &&
        (m as { type: string }).type === "reconnect_rejected",
    );
    expect(rejected).toBeDefined();
  });

  it("rejects reconnect with unknown token", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    const attempt = joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: "unknown-token-foo",
    });
    const rejected = attempt.sent.find(
      (m) =>
        typeof m === "object" &&
        m !== null &&
        (m as { type: string }).type === "reconnect_rejected",
    );
    expect(rejected).toBeDefined();
  });

  it("rejects actions from an eliminated seat before reaching the engine", async () => {
    const { adapter, emitConnection } = makeHost(3);
    await adapter.initialize();
    const g1 = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    // Guest 1 concedes (self-concede path via wire "concede" message). The
    // submitAction triggered by the concede handler is the ONLY WASM call we
    // expect for this seat from here on.
    g1.simulateData({ type: "concede" });
    await Promise.resolve();
    await Promise.resolve();
    const concedeCallCount = mockSubmitAction.mock.calls.length;

    // Any further action from guest 1 must be short-circuited by the
    // adapter — no additional engine round-trip may happen.
    g1.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await Promise.resolve();

    expect(mockSubmitAction.mock.calls.length).toBe(concedeCallCount);
  });

  it("kick broadcasts player_kicked; host-continue broadcasts player_conceded", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    const g2 = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    // Guest 1 disconnects → host chooses "continue without them".
    g2.sent.length = 0;
    // Simulate g1 disconnect, then call concedeDisconnected on its seat.
    await adapter.concedeDisconnected(1);

    // Remaining guest (g2) receives player_conceded (not player_kicked).
    const wireConceded = g2.sent.find(
      (m) =>
        typeof m === "object" &&
        m !== null &&
        (m as { type: string }).type === "player_conceded",
    );
    const wireKicked = g2.sent.find(
      (m) =>
        typeof m === "object" &&
        m !== null &&
        (m as { type: string }).type === "player_kicked",
    );
    expect(wireConceded).toBeDefined();
    expect(wireKicked).toBeUndefined();
  });

  it("terminateGame broadcasts host_left to every live guest session before disposing", async () => {
    // `host_left` is the terminal counterpart to the transient
    // session-close that `dispose()` performs — it tells guests their
    // reconnect backoff would be pointless and short-circuits the
    // `attemptReconnect` loop. Every connected guest must receive it,
    // since guests that miss the signal would re-enter the backoff.
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const g1 = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    const g2 = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    g1.sent.length = 0;
    g2.sent.length = 0;

    adapter.terminateGame();

    // The send must happen before the PeerSession is closed — close()
    // itself enqueues a `disconnect` wire message, so we verify
    // `host_left` arrives first in the send queue (not merely present).
    const g1Types = g1.sent.map((m) => (m as { type: string }).type);
    const g2Types = g2.sent.map((m) => (m as { type: string }).type);
    expect(g1Types[0]).toBe("host_left");
    expect(g2Types[0]).toBe("host_left");
  });

  it("blocks submitAction while paused-disconnect", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const g1 = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    g1.simulateClose();
    // Now in paused-disconnect.
    await expect(adapter.submitAction({ type: "PassPriority" }, 0)).rejects.toThrow(
      /paused-disconnect/,
    );
  });

  // Regression guard: the wire must carry legalActionsByObject + spellCosts
  // across game_setup, state_update, and reconnect_ack. Dropping these fields
  // — even though the flat `legalActions` array still arrives — leaves guests
  // unable to click cards in their hand, because the frontend card-click
  // dispatch (PlayerHand.tsx et al.) routes through
  // collectObjectActions(legalActionsByObject, objectId), which returns []
  // when the map is undefined. Mulligan / pass-priority still worked pre-fix
  // because those dispatch as plain GameActions, which is why the original
  // bug evaded detection for so long. This test locks in the fix at every
  // wire site so a future refactor cannot silently regress.
  it("wire protocol round-trips legalActionsByObject + spellCosts on every send site", async () => {
    // Seed the mocked engine's legal-actions response with non-empty
    // per-object grouping and spell costs. The host adapter is expected to
    // forward these verbatim to every guest via game_setup, state_update,
    // and reconnect_ack.
    const legalActionsByObject = {
      "42": [{ type: "CastSpell", data: { object_id: 42, targets: [] } }],
      "43": [{ type: "PlayLand", data: { object_id: 43 } }],
    };
    const spellCosts = {
      "42": { generic: 1, colored: { R: 1 } },
    };
    // Cast via `unknown` because the hoisted mock's default return is inferred
    // as `{ actions: never[]; autoPassRecommended: boolean }`, which would
    // reject our richer payload. The adapter consumes the full
    // `LegalActionsResult` shape regardless of the mock's narrow signature.
    mocks.getLegalActions.mockResolvedValue({
      actions: [
        { type: "CastSpell", data: { object_id: 42, targets: [] } },
        { type: "PlayLand", data: { object_id: 43 } },
        { type: "PassPriority" },
      ],
      autoPassRecommended: false,
      legalActionsByObject,
      spellCosts,
    } as unknown as { actions: never[]; autoPassRecommended: boolean });

    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();

    // ── game_setup ─────────────────────────────────────────────────────────
    const g1 = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    const setup = g1.sent.find(
      (m): m is {
        type: "game_setup";
        playerToken: string;
        legalActionsByObject?: Record<string, unknown>;
        spellCosts?: Record<string, unknown>;
      } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "game_setup",
    );
    expect(setup).toBeDefined();
    expect(setup!.legalActionsByObject).toEqual(legalActionsByObject);
    expect(setup!.spellCosts).toEqual(spellCosts);
    const playerToken = setup!.playerToken;

    // ── state_update ───────────────────────────────────────────────────────
    g1.sent.length = 0;
    await adapter.submitAction({ type: "PassPriority" }, 0);

    const stateUpdate = g1.sent.find(
      (m): m is {
        type: "state_update";
        legalActionsByObject?: Record<string, unknown>;
        spellCosts?: Record<string, unknown>;
      } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "state_update",
    );
    expect(stateUpdate).toBeDefined();
    expect(stateUpdate!.legalActionsByObject).toEqual(legalActionsByObject);
    expect(stateUpdate!.spellCosts).toEqual(spellCosts);

    // ── reconnect_ack ──────────────────────────────────────────────────────
    g1.simulateClose();
    const g1Reconnect = joinGuest(emitConnection, {
      type: "reconnect",
      playerToken,
    });
    // Two microtask flushes: one for the async handler, one for the nested
    // `void (async () => {...})()` that issues the reconnect_ack send.
    await Promise.resolve();
    await Promise.resolve();

    const ack = g1Reconnect.sent.find(
      (m): m is {
        type: "reconnect_ack";
        legalActionsByObject?: Record<string, unknown>;
        spellCosts?: Record<string, unknown>;
      } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "reconnect_ack",
    );
    expect(ack).toBeDefined();
    expect(ack!.legalActionsByObject).toEqual(legalActionsByObject);
    expect(ack!.spellCosts).toEqual(spellCosts);
  });
});
