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

import { P2PGuestAdapter, P2PHostAdapter, playerSlotsFromSeatView } from "../p2p-adapter";
import type { FormatConfig, GameEvent, GameLogEntry, GameState } from "../types";
import { FakeDataConnection } from "../../network/__tests__/fakeDataConnection";
import { WIRE_PROTOCOL_VERSION } from "../../network/protocol";

// `vi.mock` is hoisted above imports, so the factory can't reference module
// scope. Inline the wire-format stub. See `./protocolTestStub.ts` for the
// rationale: `CompressionStream` doesn't drain under fake timers in happy-dom,
// so adapter tests bypass the gzip path. The dedicated `protocol.test.ts`
// exercises the real wire format under real timers.
vi.mock("../../network/protocol", async (orig) => {
  const real = await orig<typeof import("../../network/protocol")>();
  const SENTINEL = 0xff;
  return {
    ...real,
    encodeWireMessage: async (msg: unknown) => {
      const bytes = new TextEncoder().encode(JSON.stringify(msg));
      const out = new Uint8Array(1 + bytes.length);
      out[0] = SENTINEL;
      out.set(bytes, 1);
      return out;
    },
    decodeWireMessage: async (bytes: Uint8Array) => {
      if (bytes[0] !== SENTINEL) throw new Error(`unexpected wire format: 0x${bytes[0].toString(16)}`);
      return real.validateMessage(JSON.parse(new TextDecoder().decode(bytes.subarray(1))));
    },
  };
});

// ── Mock the WasmAdapter so we don't need an actual WASM build ─────────────
// `vi.hoisted` lets us share these refs with the hoisted vi.mock factory.
const mocks = vi.hoisted(() => {
  return {
    initialize: vi.fn(async () => undefined),
    submitAction: vi.fn(async (_action: unknown) => ({ events: [] })),
    getState: vi.fn(async () => ({ players: [], objects: {} })),
    getLegalActions: vi.fn(async () => ({
      actions: [],
      autoPassRecommended: false,
    })),
    getLegalActionsForViewer: vi.fn(async (_pid: number) => ({
      actions: [],
      autoPassRecommended: false,
    })),
    getFilteredState: vi.fn(async (pid: number) => ({
      filteredFor: pid,
      players: [],
    })),
    getViewerSnapshot: vi.fn(async (pid: number) => ({
      state: { filteredFor: pid, players: [] },
      actions: [],
      autoPassRecommended: false,
    })),
    getAiAction: vi.fn(async (_difficulty: string, _playerId: number) => null),
    projectSeatView: vi.fn(async (stateJson: string) => {
      const state = JSON.parse(stateJson) as {
        seats: Array<{ type: string }>;
        format: FormatConfig;
        gameStarted: boolean;
      };
      return {
        seats: state.seats,
        format: state.format,
        teamInfo: state.format.team_based
          ? state.seats.map((_seat, seatIndex) => ({
            teamIndex: Math.floor(seatIndex / 2),
            positionInTeam: seatIndex % 2,
          }))
          : undefined,
        isFull: state.seats.every((seat) => seat.type !== "WaitingHuman"),
        gameStarted: state.gameStarted,
      };
    }),
    applySeatMutation: vi.fn(async (_stateJson: string, _mutationJson: string) => ({
      state: {
        seats: [{ type: "HostHuman" }, { type: "Ai", data: { difficulty: "Medium", deck: { type: "Random" } } }],
        tokens: ["host", ""],
        format: {
          format: "Standard",
          starting_life: 20,
          min_players: 2,
          max_players: 2,
          deck_size: 60,
          singleton: false,
          command_zone: false,
          commander_damage_threshold: null,
          range_of_influence: null,
          team_based: false,
          uses_commander: false,
          allow_debug_actions: false,
        },
        gameStarted: false,
      },
      delta: {
        mutatedSeats: [1],
        invalidatedTokens: [],
        removedAi: [],
        newAi: [[1, "Medium", { main_deck: [], sideboard: [], commander: [] }]],
        renumbering: null,
        nowStarted: false,
      },
    })),
    initializeGame: vi.fn(async () => ({ events: [] })),
    setMultiplayerMode: vi.fn(async (_enabled: boolean) => undefined),
  };
});
const mockSubmitAction = mocks.submitAction;
const mockGetViewerSnapshot = mocks.getViewerSnapshot;
const mockInitializeGame = mocks.initializeGame;
const mockSetMultiplayerMode = mocks.setMultiplayerMode;
const mockProjectSeatView = mocks.projectSeatView;
interface AsyncMockWithResolvedValueOnce {
  mockClear: () => void;
  mockResolvedValueOnce: (value: unknown) => AsyncMockWithResolvedValueOnce;
}
const mockGetState = mocks.getState as unknown as AsyncMockWithResolvedValueOnce;
const mockGetAiAction = mocks.getAiAction as unknown as AsyncMockWithResolvedValueOnce;

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function debugLogEntry(value: string): GameLogEntry {
  return {
    seq: 0,
    turn: 1,
    phase: "PreCombatMain",
    category: "Debug",
    segments: [{ type: "Text", value }],
  };
}

function remoteState(label: string): GameState {
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

function projectSeatViewFromState(stateJson: string) {
  const state = JSON.parse(stateJson) as {
    seats: Array<{ type: string }>;
    format: FormatConfig;
    gameStarted: boolean;
  };
  return {
    seats: state.seats,
    format: state.format,
    teamInfo: state.format.team_based
      ? state.seats.map((_seat, seatIndex) => ({
        teamIndex: Math.floor(seatIndex / 2),
        positionInTeam: seatIndex % 2,
      }))
      : undefined,
    isFull: state.seats.every((seat) => seat.type !== "WaitingHuman"),
    gameStarted: state.gameStarted,
  };
}

async function flushPromises(iterations = 5): Promise<void> {
  for (let i = 0; i < iterations; i++) {
    await Promise.resolve();
  }
}

vi.mock("../wasm-adapter", () => ({
  WasmAdapter: vi.fn().mockImplementation(function () {
    return {
      initialize: mocks.initialize,
      initializeGame: mocks.initializeGame,
      submitAction: mocks.submitAction,
      getState: mocks.getState,
      getLegalActions: mocks.getLegalActions,
      getLegalActionsForViewer: mocks.getLegalActionsForViewer,
      getFilteredState: mocks.getFilteredState,
      getViewerSnapshot: mocks.getViewerSnapshot,
      getAiAction: mocks.getAiAction,
      applySeatMutation: mocks.applySeatMutation,
      projectSeatView: mocks.projectSeatView,
      setMultiplayerMode: mocks.setMultiplayerMode,
      dispose: vi.fn(),
    };
  }),
}));

// Stub crypto.randomUUID for deterministic token assertions
const mockInitialize = mocks.initialize;
let uuidCounter = 0;
beforeEach(() => {
  uuidCounter = 0;
  vi.spyOn(crypto, "randomUUID").mockImplementation(
    () => `token-${++uuidCounter}` as `${string}-${string}-${string}-${string}-${string}`,
  );
  mockInitialize.mockClear();
  mockSubmitAction.mockClear();
  mockGetViewerSnapshot.mockClear();
  mockInitializeGame.mockClear();
  mockSetMultiplayerMode.mockClear();
  mockProjectSeatView.mockClear();
  mockGetState.mockClear();
  mockGetAiAction.mockClear();
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

function twoHeadedGiantConfig(): FormatConfig {
  return {
    format: "TwoHeadedGiant",
    starting_life: 30,
    min_players: 4,
    max_players: 4,
    deck_size: 60,
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: true,
    uses_commander: false,
    allow_debug_actions: false,
  };
}

function makeHost(playerCount: number, gracePeriodMs = 5_000, formatConfig?: FormatConfig) {
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
    formatConfig,
    undefined,
    gracePeriodMs,
  );
  return { adapter, emitConnection };
}

async function joinGuest(
  emitConnection: (c: DataConnection) => void,
  msg: { type: "guest_deck"; deckData: unknown } | { type: "reconnect"; playerToken: string },
): Promise<FakeOpenableConnection> {
  const conn = new FakeOpenableConnection();
  emitConnection(conn as unknown as DataConnection);
  conn.fireOpen();
  await conn.simulateData(msg);
  return conn;
}

describe("P2PHostAdapter — 3-4p multiplayer", () => {
  beforeEach(() => {
    // `toFake` opt-in: keep `queueMicrotask` real so the binary wire-format
    // encode/decode chain (CompressionStream, Response.text) drives stream
    // backpressure callbacks correctly. Faking those would deadlock the
    // gzip path.
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("rejects construction with playerCount outside 2-6", () => {
    const { peer, onGuestConnected } = createFakePeer();
    const hostDeck = {
      player: { main_deck: [], sideboard: [] },
      opponent: { main_deck: [], sideboard: [] },
      ai_decks: [],
    };
    expect(
      () => new P2PHostAdapter(hostDeck, peer as unknown as Peer, onGuestConnected, 1),
    ).toThrow("P2P supports 2-6 players");
    expect(
      () => new P2PHostAdapter(hostDeck, peer as unknown as Peer, onGuestConnected, 7),
    ).toThrow("P2P supports 2-6 players");
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

  it("projects team metadata from wire SeatView into player slots", () => {
    const slots = playerSlotsFromSeatView({
      seats: [
        { type: "HostHuman" },
        { type: "JoinedHuman" },
        { type: "WaitingHuman" },
        { type: "Ai", data: { difficulty: "Medium", deck: { type: "Random" } } },
      ],
      format: twoHeadedGiantConfig(),
      teamInfo: [
        { teamIndex: 0, positionInTeam: 0 },
        { teamIndex: 0, positionInTeam: 1 },
        { teamIndex: 1, positionInTeam: 0 },
        { teamIndex: 1, positionInTeam: 1 },
      ],
      isFull: false,
      gameStarted: false,
    });

    expect(slots.map((slot) => slot.teamInfo?.teamIndex)).toEqual([0, 0, 1, 1]);
    expect(slots.map((slot) => slot.teamInfo?.positionInTeam)).toEqual([0, 1, 0, 1]);
  });

  it("uses the Rust-projected host-local SeatView for team metadata", async () => {
    const { adapter } = makeHost(4, 5_000, twoHeadedGiantConfig());
    await adapter.initialize();

    const slots = adapter.getPlayerSlots();

    expect(mockProjectSeatView).toHaveBeenCalled();
    expect(slots.map((slot) => slot.teamInfo?.teamIndex)).toEqual([0, 0, 1, 1]);
    expect(slots.map((slot) => slot.teamInfo?.positionInTeam)).toEqual([0, 1, 0, 1]);
  });

  it("serializes host-local SeatView projections for overlapping guest joins", async () => {
    const { adapter, emitConnection } = makeHost(3);
    await adapter.initialize();
    const baselineCalls = mockProjectSeatView.mock.calls.length;
    const firstProjection = deferred<ReturnType<typeof projectSeatViewFromState>>();
    const secondProjection = deferred<ReturnType<typeof projectSeatViewFromState>>();
    let firstStateJson = "";
    let secondStateJson = "";
    mockProjectSeatView
      .mockImplementationOnce(async (stateJson: string) => {
        firstStateJson = stateJson;
        return firstProjection.promise;
      })
      .mockImplementationOnce(async (stateJson: string) => {
        secondStateJson = stateJson;
        return secondProjection.promise;
      });

    const firstJoin = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: ["Plains"], sideboard: [] } },
    });
    const secondJoin = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: ["Swamp"], sideboard: [] } },
    });
    await Promise.all([firstJoin, secondJoin]);
    await flushPromises();

    expect(mockProjectSeatView).toHaveBeenCalledTimes(baselineCalls + 1);

    firstProjection.resolve(projectSeatViewFromState(firstStateJson));
    await flushPromises(20);

    expect(mockProjectSeatView).toHaveBeenCalledTimes(baselineCalls + 2);

    secondProjection.resolve(projectSeatViewFromState(secondStateJson));
    await flushPromises();

    expect(adapter.getPlayerSlots().map((slot) => slot.kind.type)).toEqual([
      "HostHuman",
      "JoinedHuman",
      "JoinedHuman",
    ]);
  });

  it("ignores a queued guest join if that session disconnected before registration", async () => {
    const { adapter, emitConnection } = makeHost(3);
    await adapter.initialize();
    const baselineCalls = mockProjectSeatView.mock.calls.length;
    const firstProjection = deferred<ReturnType<typeof projectSeatViewFromState>>();
    let firstStateJson = "";
    mockProjectSeatView.mockImplementationOnce(async (stateJson: string) => {
      firstStateJson = stateJson;
      return firstProjection.promise;
    });

    const firstJoin = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: ["Plains"], sideboard: [] } },
    });
    const secondConn = new FakeOpenableConnection();
    emitConnection(secondConn as unknown as DataConnection);
    secondConn.fireOpen();
    const secondJoin = secondConn.simulateData({
      type: "guest_deck",
      deckData: { player: { main_deck: ["Swamp"], sideboard: [] } },
    });
    await flushPromises();

    secondConn.simulateClose();
    firstProjection.resolve(projectSeatViewFromState(firstStateJson));
    await firstJoin;
    await secondJoin;
    await flushPromises();

    expect(mockProjectSeatView).toHaveBeenCalledTimes(baselineCalls + 1);
    expect(adapter.getPlayerSlots().map((slot) => slot.kind.type)).toEqual([
      "HostHuman",
      "JoinedHuman",
      "WaitingHuman",
    ]);
  });

  it("queues buffered guest joins until WASM initialization can project SeatView", async () => {
    const initialize = deferred<undefined>();
    mockInitialize.mockImplementationOnce(() => initialize.promise);
    const { adapter, emitConnection } = makeHost(2);
    const initializeHost = adapter.initialize();
    const guestJoin = joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: ["Plains"], sideboard: [] } },
    });
    await flushPromises();

    expect(mockProjectSeatView).not.toHaveBeenCalled();

    initialize.resolve(undefined);
    await initializeHost;
    await guestJoin;
    await flushPromises();

    expect(mockProjectSeatView).toHaveBeenCalled();
    expect(adapter.getPlayerSlots().map((slot) => slot.kind.type)).toEqual([
      "HostHuman",
      "JoinedHuman",
    ]);
  });

  it("drives AI seats through simultaneous mulligan prompts", async () => {
    const { adapter } = makeHost(2);
    await adapter.initialize();
    await adapter.applySeatMutation({
      type: "SetKind",
      data: {
        seatIndex: 1,
        kind: {
          type: "Ai",
          data: { difficulty: "Medium", deck: { type: "Random" } },
        },
      },
    });

    mockGetState
      .mockResolvedValueOnce({
        waiting_for: {
          type: "MulliganDecision",
          data: {
            pending: [
              { player: 0, mulligan_count: 0, phase: { type: "Declare" } },
              { player: 1, mulligan_count: 0, phase: { type: "Declare" } },
            ],
            free_first_mulligan: false,
          },
        },
      })
      .mockResolvedValueOnce({
        waiting_for: { type: "Priority", data: { player: 0 } },
      })
      .mockResolvedValueOnce({
        waiting_for: { type: "Priority", data: { player: 0 } },
      });
    mockGetAiAction.mockResolvedValueOnce({
      type: "MulliganDecision",
      data: { choice: { type: "Keep" } },
    });

    await adapter.initializeGame();

    expect(mockGetAiAction).toHaveBeenCalledWith("Medium", 1);
    expect(mockSubmitAction).toHaveBeenCalledWith(
      { type: "MulliganDecision", data: { choice: { type: "Keep" } } },
      1,
    );
  });

  it("keeps the host AI loop silent when the host controls an AI seat's turn", async () => {
    const { adapter } = makeHost(2);
    await adapter.initialize();
    await adapter.applySeatMutation({
      type: "SetKind",
      data: {
        seatIndex: 1,
        kind: {
          type: "Ai",
          data: { difficulty: "Medium", deck: { type: "Random" } },
        },
      },
    });

    mockGetState.mockResolvedValueOnce({
      waiting_for: { type: "Priority", data: { player: 1 } },
      priority_player: 0,
    });

    await adapter.initializeGame();

    expect(mockGetAiAction).not.toHaveBeenCalled();
    expect(mockSubmitAction).not.toHaveBeenCalled();
  });

  it("drives the AI submitter when an AI controls the host's turn", async () => {
    const { adapter } = makeHost(2);
    await adapter.initialize();
    await adapter.applySeatMutation({
      type: "SetKind",
      data: {
        seatIndex: 1,
        kind: {
          type: "Ai",
          data: { difficulty: "Medium", deck: { type: "Random" } },
        },
      },
    });

    mockGetState
      .mockResolvedValueOnce({
        waiting_for: { type: "Priority", data: { player: 0 } },
        priority_player: 1,
      })
      .mockResolvedValueOnce({
        waiting_for: { type: "Priority", data: { player: 0 } },
        priority_player: 0,
      })
      .mockResolvedValueOnce({
        waiting_for: { type: "Priority", data: { player: 0 } },
        priority_player: 0,
      });
    mockGetAiAction.mockResolvedValueOnce({ type: "PassPriority" });

    await adapter.initializeGame();

    expect(mockGetAiAction).toHaveBeenCalledWith("Medium", 1);
    expect(mockSubmitAction).toHaveBeenCalledWith({ type: "PassPriority" }, 1);
  });

  it("issues unique tokens per guest and includes them in per-seat game_setup", async () => {
    const { adapter, emitConnection } = makeHost(3);
    await adapter.initialize();

    // Both guests join with their own decks.
    const g1Deck = { player: { main_deck: ["Plains"], sideboard: [] } };
    const g2Deck = { player: { main_deck: ["Swamp"], sideboard: [] } };
    const g1 = await joinGuest(emitConnection, { type: "guest_deck", deckData: g1Deck });
    const g2 = await joinGuest(emitConnection, { type: "guest_deck", deckData: g2Deck });

    await adapter.initializeGame();

    // Find the per-guest game_setup messages.
    const g1Setup = (await g1.getSentMessages()).find(
      (m): m is { type: "game_setup"; assignedPlayerId: number; playerToken: string } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "game_setup",
    );
    const g2Setup = (await g2.getSentMessages()).find(
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
    const g1 = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    const g2 = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    // Clear setup-time messages to assert against post-setup state.
    g1.sent.length = 0;
    g2.sent.length = 0;

    // Guest 2 attempts to spoof an action declaring senderPlayerId = 1.
    await g2.simulateData({
      type: "action",
      senderPlayerId: 1, // wrong! session is for seat 2
      action: { type: "PassPriority" },
    });

    // Spoofing guest receives action_rejected.
    const rejected = (await g2.getSentMessages()).find(
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
    await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    mockGetViewerSnapshot.mockClear();

    await adapter.submitAction({ type: "PassPriority" }, 0);

    // One filtered-state lookup per connected guest (host doesn't need one
    // for itself — local state is authoritative).
    expect(mockGetViewerSnapshot).toHaveBeenCalledTimes(2);
    expect(mockGetViewerSnapshot).toHaveBeenCalledWith(1);
    expect(mockGetViewerSnapshot).toHaveBeenCalledWith(2);
  });

  it("holds the seat on guest disconnect and NEVER auto-concedes on grace expiry", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const g1 = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    // Capture g1's token before it drops, to prove the seat stays reclaimable.
    const setup = (await g1.getSentMessages()).find(
      (m): m is { type: "game_setup"; playerToken: string } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "game_setup",
    );
    const token = setup!.playerToken;

    // Capture the disconnect-with-choice event.
    const events: Array<{ type: string }> = [];
    adapter.onEvent((e) => events.push(e));

    g1.simulateClose(); // guest 1 drops

    // Adapter emits the choice event so the host can decide — but takes no
    // automatic action against the dropped player.
    expect(
      events.find((e) => e.type === "opponentDisconnectedWithChoice"),
    ).toBeDefined();

    // Advance well past the old grace window — a dropped player must NOT be
    // auto-conceded. The seat is held indefinitely, waiting for them.
    mockSubmitAction.mockClear();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(mockSubmitAction).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "Concede" }),
      expect.anything(),
    );

    // The seat is still reclaimable long after the old grace window: a
    // reconnect with the original token still yields a reconnect_ack — proving
    // the seat was held, not conceded or freed.
    const g1Reconnect = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: token,
    });
    await Promise.resolve();
    await Promise.resolve();
    const ack = (await g1Reconnect.getSentMessages()).find(
      (m) =>
        typeof m === "object" &&
        m !== null &&
        (m as { type: string }).type === "reconnect_ack",
    );
    expect(ack).toBeDefined();
  });

  it("cancels grace timer and resumes on reconnect with valid token", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const g1 = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    // Capture token before disconnect.
    const setup = (await g1.getSentMessages()).find(
      (m): m is { type: "game_setup"; playerToken: string } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "game_setup",
    );
    const token = setup!.playerToken;

    g1.simulateClose();

    // Reconnect within grace.
    const g1Reconnect = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: token,
    });
    await Promise.resolve();
    await Promise.resolve();

    // Reconnecting guest gets a reconnect_ack.
    const ack = (await g1Reconnect.getSentMessages()).find(
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
    const g1 = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    const setup = (await g1.getSentMessages()).find(
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
    const rejoinAttempt = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: token,
    });
    const rejected = (await rejoinAttempt.getSentMessages()).find(
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
    await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    const attempt = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: "unknown-token-foo",
    });
    const rejected = (await attempt.getSentMessages()).find(
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
    const g1 = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    // Guest 1 concedes (self-concede path via wire "concede" message). The
    // submitAction triggered by the concede handler is the ONLY WASM call we
    // expect for this seat from here on.
    await g1.simulateData({ type: "concede" });
    await Promise.resolve();
    await Promise.resolve();
    const concedeCallCount = mockSubmitAction.mock.calls.length;

    // Any further action from guest 1 must be short-circuited by the
    // adapter — no additional engine round-trip may happen.
    await g1.simulateData({
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
    await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    const g2 = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    // Guest 1 disconnects → host chooses "continue without them".
    g2.sent.length = 0;
    // Simulate g1 disconnect, then call concedeDisconnected on its seat.
    await adapter.concedeDisconnected(1);

    // Remaining guest (g2) receives player_conceded (not player_kicked).
    const wireConceded = (await g2.getSentMessages()).find(
      (m) =>
        typeof m === "object" &&
        m !== null &&
        (m as { type: string }).type === "player_conceded",
    );
    const wireKicked = (await g2.getSentMessages()).find(
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
    const g1 = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    const g2 = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    g1.sent.length = 0;
    g2.sent.length = 0;

    await adapter.terminateGame();

    // The send must happen before the PeerSession is closed — close()
    // itself enqueues a `disconnect` wire message, so we verify
    // `host_left` arrives first in the send queue (not merely present).
    const g1Sent = await g1.getSentMessages();
    const g2Sent = await g2.getSentMessages();
    const g1Types = g1Sent.map((m) => (m as { type: string }).type);
    const g2Types = g2Sent.map((m) => (m as { type: string }).type);
    expect(g1Types[0]).toBe("host_left");
    expect(g2Types[0]).toBe("host_left");
  });

  it("blocks submitAction while paused-disconnect", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const g1 = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await joinGuest(emitConnection, {
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
    // `LegalActionsResult` / `ViewerSnapshot` shape regardless of the mock's
    // narrow signature. Populate `getViewerSnapshot` because `broadcastStateUpdate`
    // and `game_setup` now use the combined viewer-snapshot call.
    // Same unknown-cast pattern as the original `mocks.getLegalActions.mockResolvedValue`
    // — the hoisted mock's default return type is narrower than a full
    // `ViewerSnapshot`, so we widen through `unknown` to inject a richer payload.
    (mocks.getViewerSnapshot as unknown as {
      mockImplementation: (fn: (pid: number) => Promise<unknown>) => void;
    }).mockImplementation(async (pid: number) => ({
      state: { filteredFor: pid, players: [] },
      actions: [
        { type: "CastSpell", data: { object_id: 42, targets: [] } },
        { type: "PlayLand", data: { object_id: 43 } },
        { type: "PassPriority" },
      ],
      autoPassRecommended: false,
      legalActionsByObject,
      spellCosts,
    }));

    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();

    // ── game_setup ─────────────────────────────────────────────────────────
    const g1 = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    const setup = (await g1.getSentMessages()).find(
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

    const stateUpdate = (await g1.getSentMessages()).find(
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
    const g1Reconnect = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken,
    });
    // Two microtask flushes: one for the async handler, one for the nested
    // `void (async () => {...})()` that issues the reconnect_ack send.
    await Promise.resolve();
    await Promise.resolve();

    const ack = (await g1Reconnect.getSentMessages()).find(
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

  it("state_update broadcasts engine log entries to guests", async () => {
    const logEntries = [debugLogEntry("AI guesses Nonland")];
    const events: GameEvent[] = [{ type: "ChoiceMade", data: { player: 1 } } as unknown as GameEvent];
    (mocks.submitAction as unknown as {
      mockResolvedValueOnce: (value: unknown) => void;
    }).mockResolvedValueOnce({ events, log_entries: logEntries });

    const { adapter, emitConnection } = makeHost(2);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    guest.sent.length = 0;
    await adapter.submitAction({ type: "PassPriority" }, 0);

    const stateUpdate = (await guest.getSentMessages()).find(
      (m): m is { type: "state_update"; logEntries?: GameLogEntry[] } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "state_update",
    );
    expect(stateUpdate).toBeDefined();
    expect(stateUpdate!.logEntries).toEqual(logEntries);
  });

  it("guest receive path exposes state_update log entries for pending and unsolicited updates", async () => {
    const { peer } = createFakePeer();
    const conn = new FakeDataConnection();
    const adapter = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      peer as unknown as Peer,
      "host-peer",
      conn as unknown as DataConnection,
    );
    const emitted = vi.fn();
    adapter.onEvent(emitted);
    await adapter.initialize();

    await conn.simulateData({
      type: "game_setup",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 1,
      playerToken: "seat-token",
      state: remoteState("setup"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
    });
    await adapter.initializeGame();

    const pendingLogs = [debugLogEntry("AI guesses Land")];
    const pendingEvents: GameEvent[] = [
      { type: "ChoiceMade", data: { player: 1 } } as unknown as GameEvent,
    ];
    const pendingSubmit = adapter.submitAction({ type: "PassPriority" }, 1);
    await conn.simulateData({
      type: "state_update",
      state: remoteState("pending"),
      events: pendingEvents,
      logEntries: pendingLogs,
      legalActions: [],
      autoPassRecommended: false,
    });
    await expect(pendingSubmit).resolves.toEqual({
      events: pendingEvents,
      log_entries: pendingLogs,
    });

    const unsolicitedLogs = [debugLogEntry("Player guesses Nonland")];
    const unsolicitedEvents: GameEvent[] = [
      { type: "CardPredicateGuessMade", data: { player: 1 } } as unknown as GameEvent,
    ];
    const unsolicitedState = remoteState("unsolicited");
    await conn.simulateData({
      type: "state_update",
      state: unsolicitedState,
      events: unsolicitedEvents,
      logEntries: unsolicitedLogs,
      legalActions: [],
      autoPassRecommended: false,
    });

    expect(emitted).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "stateChanged",
        state: unsolicitedState,
        events: unsolicitedEvents,
        logEntries: unsolicitedLogs,
      }),
    );
  });
});
