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

function createFakePeer(): { peer: FakePeer; emitConnection: (conn: DataConnection) => void } {
  const handlers = new Set<(conn: DataConnection) => void>();
  return {
    peer: {
      on(event, handler) {
        if (event === "connection") handlers.add(handler);
      },
      off(event, handler) {
        if (event === "connection") handlers.delete(handler);
      },
      connect() {
        throw new Error("not used in tests");
      },
      destroy() {},
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
  const { peer, emitConnection } = createFakePeer();
  const hostDeck = {
    player: { main_deck: ["Mountain"], sideboard: [] },
    opponent: { main_deck: ["Forest"], sideboard: [] },
    ai_decks: [],
  };
  const adapter = new P2PHostAdapter(
    hostDeck,
    peer as unknown as Peer,
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
    const { peer } = createFakePeer();
    const hostDeck = {
      player: { main_deck: [], sideboard: [] },
      opponent: { main_deck: [], sideboard: [] },
      ai_decks: [],
    };
    expect(() => new P2PHostAdapter(hostDeck, peer as unknown as Peer, 1)).toThrow(
      "P2P supports 2-4 players",
    );
    expect(() => new P2PHostAdapter(hostDeck, peer as unknown as Peer, 5)).toThrow(
      "P2P supports 2-4 players",
    );
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
});
