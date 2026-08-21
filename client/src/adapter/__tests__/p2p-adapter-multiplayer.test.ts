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
import { AdapterError, AdapterErrorCode, supportsAiDecisionDiagnostics, supportsMatchConcede, type FormatConfig, type GameAction, type GameEvent, type GameLogEntry, type GameState } from "../types";
import { FakeDataConnection } from "../../network/__tests__/fakeDataConnection";
import { WIRE_PROTOCOL_VERSION } from "../../network/protocol";
import { p2pFinalStateCommitment } from "../../services/p2pTerminalResult";

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

vi.mock("../../services/p2pTerminalResult", async (orig) => {
  const actual = await orig<typeof import("../../services/p2pTerminalResult")>();
  return {
    ...actual,
    clearP2PTerminalResult: vi.fn(async () => undefined),
    commitP2PTerminalResult: vi.fn(async () => true),
  };
});

// ── Mock the WasmAdapter so we don't need an actual WASM build ─────────────
// `vi.hoisted` lets us share these refs with the hoisted vi.mock factory.
const mocks = vi.hoisted(() => {
  const getState = vi.fn(async () => ({
    players: [],
    objects: {},
    waiting_for: { type: "Priority", data: { player: 0 } },
  }));
  const getLegalActions = vi.fn(async () => ({
    actions: [],
    autoPassRecommended: false,
  }));
  const checkDeckCompatibility = vi.fn(async () => ({
    selected_format_compatible: true,
    selected_format_reasons: [] as string[],
  }));
  // Local monotonic stamp — the hoisted factory runs before imports, so it
  // can't call the adapter module's `nextSnapshotSeq`. Only ordering matters
  // to these assertions, and `seq` is never compared across clients.
  let seq = 0;
  return {
    initialize: vi.fn(async () => undefined),
    submitAction: vi.fn(async (_action: unknown) => ({ events: [] })),
    checkDeckCompatibility,
    getState,
    getLegalActions,
    /**
     * Reads through the SAME `getState`/`getLegalActions` mocks the tests
     * script with `mockResolvedValueOnce`, so a host AI-loop iteration consumes
     * exactly the two `getState` values it always did (loop-top read + the
     * post-submit pair read) and every scripted sequence still lines up.
     */
    getSnapshot: vi.fn(async () => ({
      state: await getState(),
      legalResult: await getLegalActions(),
      seq: ++seq,
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
    getAiActionProposal: vi.fn(async (_difficulty: string, _playerId: number) => null),
    submitAiActionProposal: vi.fn(async () => ({
      status: "applied",
      result: { events: [], log_entries: [] },
    })),
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
    /**
     * The host's atomic claim: the engine refuses an occupied engine and takes
     * the multiplayer flag in this one call. Default "the engine accepted" — a
     * real engine with nothing installed answers the same way.
     */
    initializeMultiplayerHostGame: vi.fn(async () => ({ events: [] })),
    setMultiplayerMode: vi.fn(async (_enabled: boolean) => undefined),
    /**
     * Replaces the bare `dispose()` the host used to call on its engine.
     * Shared by every mock instance: assertions read the `claimed` argument.
     */
    releaseHostSession: vi.fn(async (_claimed: boolean) => undefined),
    setAiDecisionDiagnosticsEnabled: vi.fn(),
    subscribeAiDecisionDiagnostics: vi.fn(() => () => {}),
  };
});

const nativeWebSocketMocks = vi.hoisted(() => ({
  initializePregame: vi.fn(),
  waitForPlayerSlots: vi.fn(),
  onEvent: vi.fn(),
  sendAbandonGame: vi.fn(),
  sendSeatMutation: vi.fn(),
  dispose: vi.fn(),
}));

vi.mock("../ws-adapter", () => ({
  WebSocketAdapter: vi.fn().mockImplementation(function () {
    return {
      initializePregame: nativeWebSocketMocks.initializePregame,
      waitForPlayerSlots: nativeWebSocketMocks.waitForPlayerSlots,
      onEvent: nativeWebSocketMocks.onEvent,
      sendAbandonGame: nativeWebSocketMocks.sendAbandonGame,
      sendSeatMutation: nativeWebSocketMocks.sendSeatMutation,
      dispose: nativeWebSocketMocks.dispose,
    };
  }),
}));
const mockSubmitAction = mocks.submitAction;
const mockCheckDeckCompatibility = mocks.checkDeckCompatibility;
const mockGetViewerSnapshot = mocks.getViewerSnapshot;
const mockInitializeHostGame = mocks.initializeMultiplayerHostGame;
const mockSetMultiplayerMode = mocks.setMultiplayerMode;
const mockProjectSeatView = mocks.projectSeatView;
interface AsyncMockWithResolvedValueOnce {
  mockClear: () => void;
  mockResolvedValueOnce: (value: unknown) => AsyncMockWithResolvedValueOnce;
  mockResolvedValue: (value: unknown) => AsyncMockWithResolvedValueOnce;
}
const mockGetState = mocks.getState as unknown as AsyncMockWithResolvedValueOnce;
const mockGetAiActionProposal = mocks.getAiActionProposal as unknown as AsyncMockWithResolvedValueOnce;
const mockSubmitAiActionProposal = mocks.submitAiActionProposal as unknown as AsyncMockWithResolvedValueOnce;

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
    presentation: { importance: "Diagnostic", tone: "Diagnostic", boundary: "None", visibility: "Public" },
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

// `getHostAdapter` is how the host acquires its engine (shared worker on
// memory-constrained devices, a private one everywhere else). Both exports
// hand back the same instance shape here — the branch itself is exercised
// against the real module in `wasm-adapter.test.ts`.
vi.mock("../wasm-adapter", () => {
  const createEngine = () => ({
    initialize: mocks.initialize,
    initializeMultiplayerHostGame: mocks.initializeMultiplayerHostGame,
    submitAction: mocks.submitAction,
    checkDeckCompatibility: mocks.checkDeckCompatibility,
    getState: mocks.getState,
    getLegalActions: mocks.getLegalActions,
    getSnapshot: mocks.getSnapshot,
    getLegalActionsForViewer: mocks.getLegalActionsForViewer,
    getFilteredState: mocks.getFilteredState,
    getViewerSnapshot: mocks.getViewerSnapshot,
    getAiActionProposal: mocks.getAiActionProposal,
    submitAiActionProposal: mocks.submitAiActionProposal,
    applySeatMutation: mocks.applySeatMutation,
    projectSeatView: mocks.projectSeatView,
    setMultiplayerMode: mocks.setMultiplayerMode,
    releaseHostSession: mocks.releaseHostSession,
    setAiDecisionDiagnosticsEnabled: mocks.setAiDecisionDiagnosticsEnabled,
    subscribeAiDecisionDiagnostics: mocks.subscribeAiDecisionDiagnostics,
    dispose: vi.fn(),
  });
  return {
    WasmAdapter: vi.fn().mockImplementation(createEngine),
    getHostAdapter: vi.fn(createEngine),
  };
});

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
  mockCheckDeckCompatibility.mockClear();
  mockGetViewerSnapshot.mockClear();
  mockSetMultiplayerMode.mockClear();
  mockProjectSeatView.mockClear();
  mockGetState.mockClear();
  mockGetAiActionProposal.mockClear();
  mockSubmitAiActionProposal.mockClear();
  mocks.setAiDecisionDiagnosticsEnabled.mockClear();
  mocks.subscribeAiDecisionDiagnostics.mockClear();
  // `mockReset`, not `mockClear`: these two carry per-test
  // `mockResolvedValueOnce`/`mockRejectedValueOnce` overrides, and only
  // `mockReset` drops an unconsumed one (a test that throws before consuming it
  // would otherwise leak a rejecting host-start into the next test). Both are
  // `vi.fn(impl)`, so the reset restores their default implementations.
  mocks.initializeMultiplayerHostGame.mockReset();
  mocks.releaseHostSession.mockReset();
  nativeWebSocketMocks.initializePregame.mockReset();
  nativeWebSocketMocks.waitForPlayerSlots.mockReset();
  nativeWebSocketMocks.onEvent.mockClear();
  nativeWebSocketMocks.sendAbandonGame.mockReset();
  nativeWebSocketMocks.sendSeatMutation.mockReset();
  nativeWebSocketMocks.dispose.mockClear();
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

function commanderConfig(): FormatConfig {
  return {
    format: "Commander",
    starting_life: 40,
    min_players: 2,
    max_players: 6,
    deck_size: 100,
    singleton: true,
    command_zone: true,
    commander_damage_threshold: 21,
    range_of_influence: null,
    team_based: false,
    uses_commander: true,
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

function makeNativeHost() {
  const { peer, onGuestConnected, emitConnection } = createFakePeer();
  const adapter = new P2PHostAdapter(
    {
      player: { main_deck: ["Mountain"], sideboard: [] },
      opponent: { main_deck: ["Forest"], sideboard: [] },
      ai_decks: [],
    },
    peer as unknown as Peer,
    onGuestConnected,
    2,
    commanderConfig(),
    undefined,
    5_000,
    undefined,
    true,
    undefined,
    undefined,
    {},
  );
  return { adapter, emitConnection };
}

const NATIVE_HOST_ATTACHMENT = {
  playerId: 0,
  playerToken: "native-host-token",
  gameCode: "native-game",
  fullKey: "native-full-key",
};

const NATIVE_GUEST_ATTACHMENT = {
  playerId: 1,
  playerToken: "native-guest-token",
  gameCode: "native-game",
  fullKey: "native-full-key",
};

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

  it("exposes decision diagnostics only on the browser WASM host", () => {
    const { adapter } = makeHost(2, 5_000, { ...commanderConfig(), allow_debug_actions: false });
    const guest = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      createFakePeer().peer as unknown as Peer,
      "host-peer",
      new FakeDataConnection() as unknown as DataConnection,
    );

    expect(supportsAiDecisionDiagnostics(adapter)).toBe(true);
    expect(supportsAiDecisionDiagnostics(guest)).toBe(false);
    expect("setAiDecisionDiagnosticsEnabled" in P2PHostAdapter.prototype).toBe(false);
    if (supportsAiDecisionDiagnostics(adapter)) {
      adapter.setAiDecisionDiagnosticsEnabled(true);
    }
    expect(mocks.setAiDecisionDiagnosticsEnabled).toHaveBeenCalledWith(true);
  });

  it("exposes local diagnostics after native initialization falls back to WASM", async () => {
    const { adapter: nativeHost } = makeNativeHost();
    expect(supportsAiDecisionDiagnostics(nativeHost)).toBe(false);
    nativeWebSocketMocks.waitForPlayerSlots.mockResolvedValue([]);
    nativeWebSocketMocks.initializePregame.mockRejectedValue(new Error("native unavailable"));

    await nativeHost.initialize();

    expect(nativeWebSocketMocks.initializePregame).toHaveBeenCalledOnce();
    expect(supportsAiDecisionDiagnostics(nativeHost)).toBe(true);
    if (supportsAiDecisionDiagnostics(nativeHost)) {
      nativeHost.setAiDecisionDiagnosticsEnabled(true);
      const listener = vi.fn();
      const unsubscribe = vi.fn();
      mocks.subscribeAiDecisionDiagnostics.mockReturnValueOnce(unsubscribe);

      const returnedUnsubscribe = nativeHost.subscribeAiDecisionDiagnostics(listener);

      expect(mocks.subscribeAiDecisionDiagnostics).toHaveBeenCalledWith(listener);
      expect(returnedUnsubscribe).toBe(unsubscribe);
      returnedUnsubscribe();
      expect(unsubscribe).toHaveBeenCalledOnce();
    }
    expect(mocks.setAiDecisionDiagnosticsEnabled).toHaveBeenCalledWith(true);
  });

  it("exposes local diagnostics after native guest attachment falls back to WASM", async () => {
    const { adapter, emitConnection } = makeNativeHost();
    nativeWebSocketMocks.waitForPlayerSlots.mockResolvedValue([]);
    nativeWebSocketMocks.initializePregame
      .mockResolvedValueOnce(NATIVE_HOST_ATTACHMENT)
      .mockRejectedValueOnce(new Error("native guest unavailable"));

    await adapter.initialize();
    expect(supportsAiDecisionDiagnostics(adapter)).toBe(false);
    await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: ["Plains"], sideboard: [] } },
    });
    await flushPromises();

    expect(nativeWebSocketMocks.initializePregame).toHaveBeenCalledTimes(2);
    expect(supportsAiDecisionDiagnostics(adapter)).toBe(true);
    if (supportsAiDecisionDiagnostics(adapter)) {
      adapter.setAiDecisionDiagnosticsEnabled(true);
    }
    expect(mocks.setAiDecisionDiagnosticsEnabled).toHaveBeenCalledWith(true);
  });

  it("exposes local diagnostics after native pregame seat release falls back to WASM", async () => {
    const { adapter, emitConnection } = makeNativeHost();
    nativeWebSocketMocks.waitForPlayerSlots.mockResolvedValue([]);
    nativeWebSocketMocks.initializePregame
      .mockResolvedValueOnce(NATIVE_HOST_ATTACHMENT)
      .mockResolvedValueOnce(NATIVE_GUEST_ATTACHMENT);
    nativeWebSocketMocks.sendSeatMutation.mockRejectedValue(new Error("native seat sync unavailable"));

    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: ["Plains"], sideboard: [] } },
    });
    await flushPromises();
    expect(supportsAiDecisionDiagnostics(adapter)).toBe(false);
    guest.simulateClose();
    await vi.waitFor(() => expect(supportsAiDecisionDiagnostics(adapter)).toBe(true));

    expect(nativeWebSocketMocks.sendSeatMutation).toHaveBeenCalledOnce();
    expect(supportsAiDecisionDiagnostics(adapter)).toBe(true);
    if (supportsAiDecisionDiagnostics(adapter)) {
      adapter.setAiDecisionDiagnosticsEnabled(true);
    }
    expect(mocks.setAiDecisionDiagnosticsEnabled).toHaveBeenCalledWith(true);
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

  it("claims the engine through the atomic host-start call, never a client flag flip", async () => {
    // The engine's multiplayer flag is process-wide and nothing ever clears it,
    // so an open host lobby must leave zero engine footprint. The claim belongs
    // to the engine, made inside the same call that installs the game: a client
    // flag flip followed by a separate install is two round-trips, and a local
    // `initializeGame` sharing this worker can land between them.
    const { adapter } = makeHost(2);
    expect(mockSetMultiplayerMode).not.toHaveBeenCalled();

    await adapter.initialize();

    expect(mockSetMultiplayerMode).not.toHaveBeenCalled();

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
    await adapter.initializeGame();

    expect(mockInitializeHostGame).toHaveBeenCalledTimes(1);
    expect(mockSetMultiplayerMode).not.toHaveBeenCalled();
  });

  it("does not reinitialize the host during the lobby-to-game handoff", async () => {
    const { adapter } = makeHost(2);

    await Promise.all([adapter.initialize(), adapter.initialize()]);
    await adapter.initialize();

    expect(mockInitialize).toHaveBeenCalledTimes(1);
    expect(mockSetMultiplayerMode).not.toHaveBeenCalled();
  });

  it("fences a stale host when a same-session resume claims a new incarnation", async () => {
    const persistedSession = {
      gameId: "lease-game",
      roomCode: "ABCDE",
      sessionKey: "stable-p2p-session",
      useBroker: false,
      playerTokens: {},
      guestDecks: {},
      kickedTokens: [],
      eliminatedSeats: [],
      playerCount: 2,
      hostDeckData: {
        player: { main_deck: ["Mountain"], sideboard: [] },
        opponent: { main_deck: ["Forest"], sideboard: [] },
        ai_decks: [],
      },
      gameStarted: false,
    };
    const stalePeer = createFakePeer();
    const currentPeer = createFakePeer();
    const stale = new P2PHostAdapter(
      persistedSession.hostDeckData,
      stalePeer.peer as unknown as Peer,
      stalePeer.onGuestConnected,
      2,
      undefined,
      undefined,
      5_000,
      undefined,
      true,
      undefined,
      { gameId: "lease-game", roomCode: "ABCDE", resumeData: { session: persistedSession } },
    );
    await stale.initialize();

    const current = new P2PHostAdapter(
      persistedSession.hostDeckData,
      currentPeer.peer as unknown as Peer,
      currentPeer.onGuestConnected,
      2,
      undefined,
      undefined,
      5_000,
      undefined,
      true,
      undefined,
      { gameId: "lease-game", roomCode: "ABCDE", resumeData: { session: persistedSession } },
    );
    await current.initialize();

    const staleGuest = new FakeOpenableConnection();
    stalePeer.emitConnection(staleGuest as unknown as DataConnection);
    staleGuest.fireOpen();
    await flushPromises();
    expect(await staleGuest.getSentMessages()).toContainEqual({
      type: "reconnect_rejected",
      reason: "Host session superseded",
    });

    const currentGuest = await joinGuest(currentPeer.emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: ["Plains"], sideboard: [] } },
    });
    await flushPromises();
    expect(current.getPlayerSlots()[1]?.kind.type).toBe("JoinedHuman");
    expect((await currentGuest.getSentMessages()).some(
      (message) => (message as { authority?: { sessionKey?: string } }).authority?.sessionKey === "stable-p2p-session",
    )).toBe(true);

    stale.dispose();
    current.dispose();
  });

  it("retries failed initialization without duplicating guest connections", async () => {
    const { adapter, emitConnection } = makeHost(2);
    mockInitialize
      .mockRejectedValueOnce(new Error("worker startup failed"))
      .mockResolvedValueOnce(undefined);

    await expect(adapter.initialize()).rejects.toThrow("worker startup failed");
    await adapter.initialize();

    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: ["Plains"], sideboard: [] } },
    });
    await flushPromises(20);

    expect(mockInitialize).toHaveBeenCalledTimes(2);
    expect(adapter.getPlayerSlots().map((slot) => slot.kind.type)).toEqual([
      "HostHuman",
      "JoinedHuman",
    ]);
    const messages = await guest.getSentMessages();
    expect(messages.filter((message) => (message as { type?: string }).type === "seat_snapshot"))
      .toHaveLength(1);
    expect(messages.some((message) => (message as { type?: string }).type === "kick")).toBe(false);
  });

  it("rejects a non-Oathbreaker guest signature spell before game setup", async () => {
    mockCheckDeckCompatibility.mockResolvedValueOnce({
      selected_format_compatible: false,
      selected_format_reasons: ["Commander does not use a signature spell slot"],
    });
    const { adapter, emitConnection } = makeHost(2, 5_000, commanderConfig());
    await adapter.initialize();

    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: {
        player: {
          main_deck: ["Plains"],
          sideboard: [],
          commander: ["Legal Commander"],
          companion: [],
          signature_spell: ["Invalid Signature Spell"],
        },
      },
    });
    await flushPromises(20);

    expect(mockCheckDeckCompatibility).toHaveBeenCalledWith({
      main_deck: ["Plains"],
      sideboard: [],
      commander: ["Legal Commander"],
      companion: [],
      signature_spell: ["Invalid Signature Spell"],
      selected_format: "Commander",
    });
    expect(mockInitializeHostGame).not.toHaveBeenCalled();

    const kicked = (await guest.getSentMessages()).find(
      (message) =>
        typeof message === "object"
        && message !== null
        && (message as { type: string }).type === "kick",
    );
    expect(kicked).toMatchObject({
      type: "kick",
      reason: "Deck rejected: Commander does not use a signature spell slot",
      format: "Commander",
    });
    expect(guest.open).toBe(false);
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
    mockGetAiActionProposal.mockResolvedValueOnce({
      token: "proposal-mulligan",
      semanticOwner: 1,
      actor: 1,
      action: { type: "MulliganDecision", data: { choice: { type: "Keep" } } },
    });

    await adapter.initializeGame();

    expect(mockGetAiActionProposal).toHaveBeenCalledWith("Medium", 1);
    expect(mocks.submitAiActionProposal).toHaveBeenCalledWith(expect.objectContaining({
      token: "proposal-mulligan",
    }));
  });

  it("bounds repeated stale AI proposals", async () => {
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

    mockGetState.mockResolvedValue({
      waiting_for: { type: "Priority", data: { player: 1 } },
      priority_player: 1,
    });
    mockGetAiActionProposal.mockResolvedValue({
      token: "proposal-stale",
      semanticOwner: 1,
      actor: 1,
      action: { type: "PassPriority" },
    });
    mockSubmitAiActionProposal.mockResolvedValue({
      status: "stale",
      reason: "decision_changed_or_action_outside_issued_bounds",
    });

    await expect(adapter.initializeGame()).rejects.toMatchObject({
      code: "P2P_ERROR",
    });
    expect(mocks.submitAiActionProposal).toHaveBeenCalledTimes(4);
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

    expect(mockGetAiActionProposal).not.toHaveBeenCalled();
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
    mockGetAiActionProposal.mockResolvedValueOnce({
      token: "proposal-priority",
      semanticOwner: 0,
      actor: 1,
      action: { type: "PassPriority" },
    });

    await adapter.initializeGame();

    expect(mockGetAiActionProposal).toHaveBeenCalledWith("Medium", 1);
    expect(mocks.submitAiActionProposal).toHaveBeenCalledWith(expect.objectContaining({
      token: "proposal-priority",
    }));
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

  it("keeps a host zero-count debug create out of transition side effects", async () => {
    const { adapter } = makeHost(2);
    await adapter.initialize();
    const revisionBefore = (adapter as unknown as { authoritativeRevision: number })
      .authoritativeRevision;

    await expect(adapter.submitAction({
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
    }, 0)).resolves.toEqual({ events: [] });

    expect(mockSubmitAction).toHaveBeenCalledOnce();
    expect((adapter as unknown as { authoritativeRevision: number }).authoritativeRevision)
      .toBe(revisionBefore);
    expect(mockGetViewerSnapshot).not.toHaveBeenCalled();
    expect(mockGetState).not.toHaveBeenCalled();
  });

  it("acknowledges a guest zero-count debug create without broadcasting a transition", async () => {
    const { adapter, emitConnection } = makeHost(2);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    guest.sent.length = 0;
    mockGetViewerSnapshot.mockClear();
    mockGetState.mockClear();
    const revisionBefore = (adapter as unknown as { authoritativeRevision: number })
      .authoritativeRevision;

    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: {
        type: "Debug",
        data: {
          type: "CreateTokenCopy",
          data: { source_id: 1, owner: 1, nonlegendary: false, count: 0 },
        },
      },
    });

    expect(await guest.getSentMessages()).toEqual([
      expect.objectContaining({ type: "action_noop" }),
    ]);
    expect((adapter as unknown as { authoritativeRevision: number }).authoritativeRevision)
      .toBe(revisionBefore);
    expect(mockGetViewerSnapshot).not.toHaveBeenCalled();
    expect(mockGetState).not.toHaveBeenCalled();
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

  it("blocks AI proposal submission while paused-disconnect", async () => {
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

    await expect(adapter.submitAiActionProposal({
      token: "proposal-paused",
      semanticOwner: 0,
      actor: 0,
      action: { type: "PassPriority" },
    })).rejects.toMatchObject({
      code: "P2P_PAUSED",
    });
    expect(mocks.submitAiActionProposal).not.toHaveBeenCalled();
  });

  // Regression guard: the wire must carry legalActionsByObject, spellCosts,
  // engine-authored mana-payment shortcut actions, and derived copy views
  // across game_setup, state_update, and reconnect_ack. Dropping these fields
  // — even though the flat `legalActions` array still arrives — leaves guests
  // unable to click cards in their hand, because the frontend card-click
  // dispatch (PlayerHand.tsx et al.) routes through
  // collectObjectActions(legalActionsByObject, objectId), which returns []
  // when the map is undefined. Mulligan / pass-priority still worked pre-fix
  // because those dispatch as plain GameActions, which is why the original
  // bug evaded detection for so long. This test locks in the fix at every
  // wire site so a future refactor cannot silently regress.
  it("wire protocol round-trips legal projections on every send site", async () => {
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
    const manaPaymentShortcutActions: GameAction[] = [{ type: "PassPriority" }];
    const copiedPermanents = [42];
    const legendCandidateIdentities = { "42": "TokenCopy" as const };
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
      state: {
        filteredFor: pid,
        players: [],
        derived: {
          copied_permanents: copiedPermanents,
          legend_candidate_identities: legendCandidateIdentities,
        },
      },
      actions: [
        { type: "CastSpell", data: { object_id: 42, targets: [] } },
        { type: "PlayLand", data: { object_id: 43 } },
        { type: "PassPriority" },
      ],
      autoPassRecommended: false,
      manaPaymentShortcutActions,
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
        manaPaymentShortcutActions?: GameAction[];
        state: {
          derived?: {
            copied_permanents?: number[];
            legend_candidate_identities?: Record<string, string>;
          };
        };
      } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "game_setup",
    );
    expect(setup).toBeDefined();
    expect(setup!.legalActionsByObject).toEqual(legalActionsByObject);
    expect(setup!.spellCosts).toEqual(spellCosts);
    expect(setup!.manaPaymentShortcutActions).toEqual(manaPaymentShortcutActions);
    expect(setup!.state.derived?.copied_permanents).toEqual(copiedPermanents);
    expect(setup!.state.derived?.legend_candidate_identities).toEqual(legendCandidateIdentities);
    const playerToken = setup!.playerToken;

    // ── state_update ───────────────────────────────────────────────────────
    g1.sent.length = 0;
    await adapter.submitAction({ type: "PassPriority" }, 0);

    const stateUpdate = (await g1.getSentMessages()).find(
      (m): m is {
        type: "state_update";
        legalActionsByObject?: Record<string, unknown>;
        spellCosts?: Record<string, unknown>;
        manaPaymentShortcutActions?: GameAction[];
        state: {
          derived?: {
            copied_permanents?: number[];
            legend_candidate_identities?: Record<string, string>;
          };
        };
      } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "state_update",
    );
    expect(stateUpdate).toBeDefined();
    expect(stateUpdate!.legalActionsByObject).toEqual(legalActionsByObject);
    expect(stateUpdate!.spellCosts).toEqual(spellCosts);
    expect(stateUpdate!.manaPaymentShortcutActions).toEqual(manaPaymentShortcutActions);
    expect(stateUpdate!.state.derived?.copied_permanents).toEqual(copiedPermanents);
    expect(stateUpdate!.state.derived?.legend_candidate_identities).toEqual(legendCandidateIdentities);

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
        manaPaymentShortcutActions?: GameAction[];
        state: {
          derived?: {
            copied_permanents?: number[];
            legend_candidate_identities?: Record<string, string>;
          };
        };
      } =>
        typeof m === "object" && m !== null && (m as { type: string }).type === "reconnect_ack",
    );
    expect(ack).toBeDefined();
    expect(ack!.legalActionsByObject).toEqual(legalActionsByObject);
    expect(ack!.spellCosts).toEqual(spellCosts);
    expect(ack!.manaPaymentShortcutActions).toEqual(manaPaymentShortcutActions);
    expect(ack!.state.derived?.copied_permanents).toEqual(copiedPermanents);
    expect(ack!.state.derived?.legend_candidate_identities).toEqual(legendCandidateIdentities);
  });

  it("keeps turn-controller auto-pass recommendations viewer-scoped on setup, update, and reconnect", async () => {
    const viewerSnapshot = (pid: number) => ({
      state: {
        filteredFor: pid,
        players: [],
        active_player: 2,
        priority_player: 1,
        phase: "Upkeep",
        waiting_for: { type: "Priority", data: { player: 2 } },
        turn_decision_controller: 1,
        priority_passing_modes: pid === 1 ? { "1": "SkipLowUseWindows" } : {},
      },
      actions: pid === 1 ? [{ type: "PassPriority" }] : [],
      autoPassRecommended: pid === 1,
    });
    (mocks.getViewerSnapshot as unknown as {
      mockImplementation: (fn: (pid: number) => Promise<unknown>) => void;
    }).mockImplementation(async (pid: number) => viewerSnapshot(pid));

    const messageOfType = async <T extends { type: string }>(
      conn: FakeOpenableConnection,
      type: T["type"],
    ): Promise<T> => {
      const message = (await conn.getSentMessages()).find(
        (candidate) =>
          typeof candidate === "object"
          && candidate !== null
          && (candidate as { type: string }).type === type,
      );
      expect(message).toBeDefined();
      return message as T;
    };
    type ViewerMessage = {
      type: "game_setup" | "state_update" | "reconnect_ack";
      playerToken?: string;
      state: { priority_passing_modes?: Record<string, string> };
      legalActions: GameAction[];
      autoPassRecommended: boolean;
    };
    const expectControllerView = (message: ViewerMessage) => {
      expect(message.autoPassRecommended).toBe(true);
      expect(message.legalActions).toEqual([{ type: "PassPriority" }]);
      expect(message.state.priority_passing_modes).toEqual({
        "1": "SkipLowUseWindows",
      });
    };
    const expectControlledView = (message: ViewerMessage) => {
      expect(message.autoPassRecommended).toBe(false);
      expect(message.legalActions).toEqual([]);
      expect(message.state.priority_passing_modes).toEqual({});
    };

    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const controller = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    const controlled = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    const controllerSetup = await messageOfType<ViewerMessage & { playerToken: string }>(
      controller,
      "game_setup",
    );
    const controlledSetup = await messageOfType<ViewerMessage & { playerToken: string }>(
      controlled,
      "game_setup",
    );
    expectControllerView(controllerSetup);
    expectControlledView(controlledSetup);

    controller.sent.length = 0;
    controlled.sent.length = 0;
    await adapter.submitAction({ type: "PassPriority" }, 0);
    expectControllerView(await messageOfType<ViewerMessage>(controller, "state_update"));
    expectControlledView(await messageOfType<ViewerMessage>(controlled, "state_update"));

    controller.simulateClose();
    const reconnectedController = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: controllerSetup.playerToken,
    });
    await flushPromises();
    expectControllerView(
      await messageOfType<ViewerMessage>(reconnectedController, "reconnect_ack"),
    );

    controlled.simulateClose();
    const reconnectedControlled = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: controlledSetup.playerToken,
    });
    await flushPromises();
    expectControlledView(
      await messageOfType<ViewerMessage>(reconnectedControlled, "reconnect_ack"),
    );
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
      manaPaymentShortcutActions: [],
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
      manaPaymentShortcutActions: [],
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
      manaPaymentShortcutActions: [],
    });

    // The engine pair now travels as one seq-stamped `EngineSnapshot`.
    expect(emitted).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "stateChanged",
        snapshot: expect.objectContaining({
          state: unsolicitedState,
          seq: expect.any(Number),
        }),
        events: unsolicitedEvents,
        logEntries: unsolicitedLogs,
      }),
    );
  });

  it("guest receive path resolves action_noop without replacing its cached snapshot", async () => {
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
    const setupState = remoteState("setup");
    await conn.simulateData({
      type: "game_setup",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 1,
      playerToken: "seat-token",
      state: setupState,
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    await adapter.initializeGame();
    const cachedSnapshot = await adapter.getSnapshot();
    emitted.mockClear();

    const pending = adapter.submitAction({
      type: "Debug",
      data: {
        type: "CreateCard",
        data: {
          card_name: "Lightning Bolt",
          owner: 1,
          zone: "Hand",
          run_etb: false,
          nonlegendary: false,
          count: 0,
        },
      },
    }, 1);
    await conn.simulateData({ type: "action_noop" });

    await expect(pending).resolves.toEqual({ events: [], log_entries: [] });
    expect(await adapter.getSnapshot()).toBe(cachedSnapshot);
    expect(emitted).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "stateChanged" }),
    );
  });

  // Issue #5913: the host relays the engine's verdict verbatim, so a guest must
  // classify a stale ReorderHand exactly as the local-WASM seat does. Before the
  // shared classifier this path built a generic ACTION_REJECTED, and
  // `dispatchAction` — which suppresses only STALE_ACTION — still surfaced the
  // red error to P2P guests.
  it("guest classifies a stale ReorderHand rejection from the host as STALE_ACTION", async () => {
    const { peer } = createFakePeer();
    const conn = new FakeDataConnection();
    const adapter = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      peer as unknown as Peer,
      "host-peer",
      conn as unknown as DataConnection,
    );
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
      manaPaymentShortcutActions: [],
    });
    await adapter.initializeGame();

    const stale = adapter.submitAction(
      { type: "ReorderHand", data: { order: [1, 2, 3] } } as unknown as GameAction,
      1,
    );
    await conn.simulateData({
      type: "action_rejected",
      reason: "Engine error: ReorderHand: expected 6 ids, got 5",
    });
    await expect(stale).rejects.toMatchObject({
      code: "STALE_ACTION",
      recoverable: false,
    });

    // A genuine rejection must still surface as a recoverable ACTION_REJECTED.
    const real = adapter.submitAction({ type: "PassPriority" }, 1);
    await conn.simulateData({
      type: "action_rejected",
      reason: "Engine error: Something genuinely wrong",
    });
    await expect(real).rejects.toMatchObject({
      code: "ACTION_REJECTED",
      recoverable: true,
    });
  });

  it("guest snapshots stay coherent and strictly ordered across successive state updates", async () => {
    const { peer } = createFakePeer();
    const conn = new FakeDataConnection();
    const adapter = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      peer as unknown as Peer,
      "host-peer",
      conn as unknown as DataConnection,
    );
    await adapter.initialize();

    /** One inbound host update carrying a state and the legal actions derived from it. */
    const pushUpdate = (label: string, actions: GameAction[]) =>
      conn.simulateData({
        type: "state_update",
        state: remoteState(label),
        events: [],
        legalActions: actions,
        autoPassRecommended: false,
        manaPaymentShortcutActions: [],
      });

    const passPriority = [{ type: "PassPriority" }] as unknown as GameAction[];
    const decideOptional = [
      { type: "DecideOptionalEffect", data: { accept: true } },
    ] as unknown as GameAction[];

    await pushUpdate("first", passPriority);
    const first = await adapter.getSnapshot();

    // Coherence: the pair in a snapshot is the pair that arrived together.
    expect((first.state as unknown as { label: string }).label).toBe("first");
    expect(first.legalResult.actions).toEqual(passPriority);

    // And the un-paired reads are served from that SAME cached snapshot, so they
    // cannot straddle two updates the way two independent fields could.
    expect(await adapter.getState()).toBe(first.state);
    expect(await adapter.getLegalActions()).toBe(first.legalResult);

    await pushUpdate("second", decideOptional);
    const second = await adapter.getSnapshot();

    // The second update replaces BOTH halves together — never one without the
    // other. A `state:"second"` paired with the first update's `PassPriority`
    // actions is precisely the mixed pair that softlocked the host.
    expect((second.state as unknown as { label: string }).label).toBe("second");
    expect(second.legalResult.actions).toEqual(decideOptional);

    // Strictly increasing stamps let the store's gate order these commits.
    expect(second.seq).toBeGreaterThan(first.seq);
  });

  it("commits each terminal result to the recipient's filtered final state", async () => {
    const { adapter, emitConnection } = makeHost(2);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    const hostState = {
      players: [],
      objects: { 7: { name: "Secret Hand Card" } },
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    } as unknown as GameState;
    const guestState = {
      players: [],
      objects: { 7: { name: "Hidden Card" } },
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    } as unknown as GameState;
    (mockGetViewerSnapshot as unknown as {
      mockImplementation: (implementation: (playerId: number) => Promise<unknown>) => void;
    }).mockImplementation(async (playerId: number) => ({
      state: playerId === 1 ? guestState : hostState,
      actions: [],
      autoPassRecommended: false,
    }));

    await (adapter as unknown as {
      commitTerminalIfComplete: (snapshot: unknown, revision: number) => Promise<void>;
    }).commitTerminalIfComplete({
      state: hostState,
      legalResult: { actions: [], autoPassRecommended: false },
      seq: 42,
    }, 42);

    const terminal = (await guest.getSentMessages()).find(
      (message) => (message as { type?: string }).type === "terminal_result",
    ) as { type: "terminal_result"; result: { recipient: number; finalStateCommitment: string } } | undefined;
    expect(terminal?.result.recipient).toBe(1);
    expect(terminal?.result.finalStateCommitment).toBe(
      await p2pFinalStateCommitment(guestState),
    );
    expect(terminal?.result.finalStateCommitment).not.toBe(
      await p2pFinalStateCommitment(hostState),
    );
    adapter.dispose();
  });

  it("redelivers a recipient-bound terminal result after a guest reconnects", async () => {
    const { adapter, emitConnection } = makeHost(2);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    const setup = (await guest.getSentMessages()).find(
      (message): message is { type: "game_setup"; playerToken: string } =>
        typeof message === "object"
        && message !== null
        && (message as { type?: string }).type === "game_setup",
    );
    const terminalState = {
      players: [],
      objects: { 7: { name: "Hidden Card" } },
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    } as unknown as GameState;
    (mockGetViewerSnapshot as unknown as { mockResolvedValue: (value: unknown) => void }).mockResolvedValue({
      state: terminalState,
      actions: [],
      autoPassRecommended: false,
    });
    await (adapter as unknown as {
      commitTerminalIfComplete: (snapshot: unknown, revision: number) => Promise<void>;
    }).commitTerminalIfComplete({
      state: terminalState,
      legalResult: { actions: [], autoPassRecommended: false },
      seq: 42,
    }, 42);

    guest.simulateClose();
    const reconnect = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: setup!.playerToken,
    });
    await vi.waitFor(async () => {
      const messages = await reconnect.getSentMessages();
      expect(messages.some((message) =>
        typeof message === "object"
        && message !== null
        && (message as { type?: string }).type === "terminal_result")).toBe(true);
    });
    const messages = await reconnect.getSentMessages();
    const ackIndex = messages.findIndex((message) =>
      typeof message === "object"
      && message !== null
      && (message as { type?: string }).type === "reconnect_ack");
    const terminal = messages.find(
      (message): message is { type: "terminal_result"; result: { recipient: number; finalStateCommitment: string } } =>
        typeof message === "object"
        && message !== null
        && (message as { type?: string }).type === "terminal_result",
    );
    expect(ackIndex).toBeGreaterThanOrEqual(0);
    expect(messages.indexOf(terminal!)).toBeGreaterThan(ackIndex);
    expect(terminal?.result.recipient).toBe(1);
    expect(terminal?.result.finalStateCommitment).toBe(
      await p2pFinalStateCommitment(terminalState),
    );
    adapter.dispose();
  });
});

describe("P2PHostAdapter — bound draft match concession", () => {
  it("installs the capability only when a pod binding supplies its forwarder", async () => {
    const { peer, onGuestConnected } = createFakePeer();
    const onConcede = vi.fn();
    const adapter = new P2PHostAdapter(
      { player: { main_deck: [], sideboard: [] }, opponent: { main_deck: [], sideboard: [] }, ai_decks: [] },
      peer as unknown as Peer,
      onGuestConnected,
      2,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      { onConcede },
    );

    expect(supportsMatchConcede(adapter)).toBe(true);
    await adapter.initialize();
    (adapter as unknown as { gameStarted: boolean }).gameStarted = true;
    adapter.sendMatchConcede();
    adapter.sendMatchConcede();
    expect(onConcede).toHaveBeenCalledTimes(1);
    expect(onConcede).toHaveBeenCalledWith(0);
    adapter.dispose();
  });

  it("routes a bound guest request to match settlement without conceding the engine game", async () => {
    const { peer, onGuestConnected, emitConnection } = createFakePeer();
    const onConcede = vi.fn();
    const adapter = new P2PHostAdapter(
      { player: { main_deck: [], sideboard: [] }, opponent: { main_deck: [], sideboard: [] }, ai_decks: [] },
      peer as unknown as Peer,
      onGuestConnected,
      2,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      { onConcede },
    );
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    mockSubmitAction.mockClear();

    await guest.simulateData({ type: "match_concede" });

    expect(onConcede).toHaveBeenCalledTimes(1);
    expect(onConcede).toHaveBeenCalledWith(1);
    expect(mockSubmitAction).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "Concede" }),
      expect.anything(),
    );
    adapter.dispose();
  });

  it("rejects a protected match request when no draft binding was installed", async () => {
    const { adapter, emitConnection } = makeHost(2);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    guest.sent.length = 0;

    await guest.simulateData({ type: "match_concede" });

    expect(await guest.getSentMessages()).toContainEqual(expect.objectContaining({
      type: "action_rejected",
      reason: "Whole-match concession is unavailable for this game",
    }));
    adapter.dispose();
  });
});

/**
 * On a memory-constrained device the host's engine is the same worker local
 * play uses, so teardown must clear engine state for the claimant and only the
 * claimant, and a start must never overwrite a game that is already live.
 */
describe("P2PHostAdapter — shared-engine ownership", () => {
  beforeEach(() => {
    // Earlier suites leave persistent `mockResolvedValue` overrides on the AI
    // mocks (`mockClear` does not undo those). Restore a board where the host
    // holds priority and no AI proposal is pending, so `runAiLoop` returns
    // immediately and these tests observe only the ownership bookkeeping.
    mockGetState.mockResolvedValue({
      players: [],
      objects: {},
      priority_player: 0,
      waiting_for: { type: "Priority", data: { player: 0 } },
    });
    mockGetAiActionProposal.mockResolvedValue(null);
  });

  async function seatAi(adapter: P2PHostAdapter): Promise<void> {
    await adapter.applySeatMutation({
      type: "SetKind",
      data: {
        seatIndex: 1,
        kind: { type: "Ai", data: { difficulty: "Medium", deck: { type: "Random" } } },
      },
    });
  }

  async function startedHost(): Promise<P2PHostAdapter> {
    const { adapter } = makeHost(2);
    await adapter.initialize();
    await seatAi(adapter);
    await adapter.initializeGame();
    return adapter;
  }

  it("clears the engine state it installed when the host tears down", async () => {
    const adapter = await startedHost();
    mocks.releaseHostSession.mockClear();

    adapter.dispose();

    expect(mocks.releaseHostSession).toHaveBeenCalledWith(true);
  });

  it("leaves the engine untouched when a host that never started tears down", async () => {
    const { adapter } = makeHost(2);
    await adapter.initialize();
    mocks.releaseHostSession.mockClear();

    adapter.dispose();

    expect(mocks.releaseHostSession).toHaveBeenCalledWith(false);
  });

  it("does not let another host's teardown clear the claimant's game", async () => {
    const claimant = await startedHost();
    const { adapter: other } = makeHost(2);
    await other.initialize();

    mocks.releaseHostSession.mockClear();
    other.dispose();
    expect(mocks.releaseHostSession).toHaveBeenCalledWith(false);

    mocks.releaseHostSession.mockClear();
    claimant.dispose();
    expect(mocks.releaseHostSession).toHaveBeenCalledWith(true);
  });

  function occupiedRefusal(): AdapterError {
    return new AdapterError(
      AdapterErrorCode.ENGINE_OCCUPIED,
      "Finish or leave your current game before starting a new one.",
      false,
    );
  }

  it("surfaces the engine's refusal when it already holds a game", async () => {
    const { adapter } = makeHost(2);
    await adapter.initialize();
    await seatAi(adapter);
    // The engine is the authority, not a client-side probe: it tests occupancy
    // and installs inside one synchronous worker task, so a local
    // `initializeGame` on the same shared worker cannot land in between.
    mockInitializeHostGame.mockRejectedValueOnce(occupiedRefusal());

    await expect(adapter.initializeGame()).rejects.toThrow(
      /Finish or leave your current game/,
    );
    // A refused claim installed nothing, so there is nothing to compensate.
    // `releaseHostSession(true)` here would run `resetGameState()` on the
    // shared engine and destroy the live local game the refusal just protected.
    expect(mocks.releaseHostSession).toHaveBeenCalledWith(false);
    expect(mocks.releaseHostSession).not.toHaveBeenCalledWith(true);
    expect(mockSetMultiplayerMode).not.toHaveBeenCalled();
    adapter.dispose();
  });

  it("leaves the engine untouched when a refused claim is disposed concurrently", async () => {
    const { adapter } = makeHost(2);
    await adapter.initialize();
    await seatAi(adapter);
    // "Cancel hosting" clicked while a refused start is still in flight:
    // `dispose()` sets `disposed` synchronously, so the catch takes its
    // *disposed* branch — which routes to `releaseHostSession` as well. With
    // `true` that branch would reset the very game the refusal protected. The
    // test above never disposes, so only this one covers that exit.
    const gate = deferred<undefined>();
    mockInitializeHostGame.mockImplementationOnce(async () => {
      await gate.promise;
      throw occupiedRefusal();
    });
    const start = adapter.initializeGame();
    await vi.waitFor(() => {
      expect(mockInitializeHostGame).toHaveBeenCalled();
    });

    adapter.dispose();
    mocks.releaseHostSession.mockClear();
    gate.resolve(undefined);

    await expect(start).rejects.toThrow(/disposed during start/);
    expect(mocks.releaseHostSession).toHaveBeenCalledWith(false);
    expect(mocks.releaseHostSession).not.toHaveBeenCalledWith(true);
  });

  it("hands the engine back when teardown lands while the start call is in flight", async () => {
    const { adapter } = makeHost(2);
    await adapter.initialize();
    await seatAi(adapter);
    // Park inside the host-start call — the real window is the card-DB load it
    // awaits, seconds wide with "Cancel hosting" one click away. Here the
    // engine *accepted*, so this bail owns the state it installed and has to
    // hand it back, flag included; nothing else ever clears it.
    const install = deferred<{ events: [] }>();
    mockInitializeHostGame.mockReturnValueOnce(install.promise);
    const start = adapter.initializeGame();
    await vi.waitFor(() => {
      expect(mockInitializeHostGame).toHaveBeenCalled();
    });

    adapter.dispose();
    mocks.releaseHostSession.mockClear();
    install.resolve({ events: [] });

    await expect(start).rejects.toThrow(/disposed during start/);
    expect(mocks.releaseHostSession).toHaveBeenCalledWith(true);
  });

  it("leaves the engine untouched when the start call rejects for any other reason", async () => {
    const { adapter } = makeHost(2);
    await adapter.initialize();
    await seatAi(adapter);
    // The engine also rejects on deck validation, and on "Card database not
    // loaded" whenever `ensureCardDb` swallowed a fetch failure — routine on
    // the flaky-network devices that share this worker. No engine state is
    // installed on any of those paths either.
    const refusal = new Error("Card database not loaded");
    mockInitializeHostGame.mockRejectedValueOnce(refusal);

    await expect(adapter.initializeGame()).rejects.toThrow(refusal);

    expect(mockSetMultiplayerMode).not.toHaveBeenCalled();
    expect(mocks.releaseHostSession).toHaveBeenCalledWith(false);
    expect(mocks.releaseHostSession).not.toHaveBeenCalledWith(true);
    adapter.dispose();
  });

  it("fails loud on engine calls after teardown", async () => {
    const { adapter } = makeHost(2);
    await adapter.initialize();

    adapter.dispose();

    await expect(adapter.getState()).rejects.toThrow("P2P host adapter has been disposed");
    await expect(adapter.submitAction({ type: "PassPriority" }, 0)).rejects.toThrow(
      "P2P host adapter has been disposed",
    );
  });
});
