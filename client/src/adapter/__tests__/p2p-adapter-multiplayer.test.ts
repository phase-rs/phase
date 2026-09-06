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

import { P2PGuestAdapter, P2PHostAdapter, playerSlotsFromSeatView, type P2PAdapterEvent } from "../p2p-adapter";
import { AdapterError, AdapterErrorCode, supportsAiDecisionDiagnostics, supportsMatchConcede, type EngineSnapshot, type FormatConfig, type GameAction, type GameEvent, type GameLogEntry, type GameState, type PersistedGameState, type RestoredGameStateResult } from "../types";
import type { WsAdapterEvent } from "../ws-adapter";
import { FakeDataConnection } from "../../network/__tests__/fakeDataConnection";
import { PEER_CONNECT_OPTIONS } from "../../network/connection";
import { WIRE_PROTOCOL_VERSION, type P2PMessage } from "../../network/protocol";
import { p2pFinalStateCommitment } from "../../services/p2pTerminalResult";
import { ownsP2PHostLease } from "../../services/p2pSession";

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

const terminalMocks = vi.hoisted(() => ({
  clearP2PTerminalResult: vi.fn(async () => undefined),
  commitP2PTerminalResult: vi.fn(async () => true),
}));

vi.mock("../../services/p2pTerminalResult", async (orig) => {
  const actual = await orig<typeof import("../../services/p2pTerminalResult")>();
  return {
    ...actual,
    clearP2PTerminalResult: terminalMocks.clearP2PTerminalResult,
    commitP2PTerminalResult: terminalMocks.commitP2PTerminalResult,
  };
});

const persistenceMocks = vi.hoisted(() => ({
  clearGame: vi.fn(async () => undefined),
  clearP2PHostSession: vi.fn(async () => undefined),
  saveGame: vi.fn(async () => undefined),
  saveP2PHostSession: vi.fn(async () => undefined),
  saveResumableGameStrict: vi.fn<() => Promise<void>>(async () => undefined),
}));

vi.mock("../../services/gamePersistence", async (orig) => {
  const actual = await orig<typeof import("../../services/gamePersistence")>();
  return {
    ...actual,
    clearGame: persistenceMocks.clearGame,
    clearP2PHostSession: persistenceMocks.clearP2PHostSession,
    saveGame: persistenceMocks.saveGame,
    saveP2PHostSession: persistenceMocks.saveP2PHostSession,
    saveResumableGameStrict: persistenceMocks.saveResumableGameStrict,
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
  // The ENFORCING gate the host's per-guest deck check uses. Deliberately a
  // separate mock from `checkDeckCompatibility` above: the two return different
  // shapes AND different verdicts for a Custom format (definite `false` here,
  // "no opinion" there), so a test that mocks the wrong one would prove
  // nothing about the kick path.
  const evaluateDeckFormatGate = vi.fn(async () => ({
    compatible: true,
    reasons: [] as string[],
  }));
  // Local monotonic stamp — the hoisted factory runs before imports, so it
  // can't call the adapter module's `nextSnapshotSeq`. Only ordering matters
  // to these assertions, and `seq` is never compared across clients.
  let seq = 0;
  return {
    initialize: vi.fn(async () => undefined),
    submitAction: vi.fn(async (_action: unknown) => ({ events: [] })),
    submitInteraction: vi.fn(async (_submission: unknown) => ({ events: [] })),
    previewInteraction: vi.fn(async (request: { requestId: string }, _actor: number) => ({
      requestId: request.requestId,
      interactionId: "interaction-1",
      status: { type: "confirmable" },
      progress: { selected: 3, minimum: 1, maximum: 3, aggregate: 6, confirmable: true },
      outcome: "advanced",
      summaries: ["confirmAvailable", "progress"],
    })),
    checkDeckCompatibility,
    evaluateDeckFormatGate,
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
    exportPersistenceState: vi.fn(async () => JSON.stringify({ players: [], objects: {} })),
    resumeMultiplayerHostState: vi.fn<() => Promise<RestoredGameStateResult>>(async () => ({
      snapshot: {
        state: { players: [], objects: {}, waiting_for: { type: "Priority", data: { player: 0 } } } as unknown as GameState,
        legalResult: { actions: [], autoPassRecommended: false },
        seq: 1,
      },
      presentation: {
        outcome: "noop",
        automatedResolutionCount: 0,
        omittedEventCount: 0,
        logEntries: [],
      },
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
          deck_size: { type: "Minimum", data: 60 },
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
    let playerId: number | null = null;
    return {
      get playerId() {
        return playerId;
      },
      initializePregame: async () => {
        const attachment = await nativeWebSocketMocks.initializePregame();
        playerId = attachment.playerId;
        return attachment;
      },
      waitForPlayerSlots: nativeWebSocketMocks.waitForPlayerSlots,
      onEvent: nativeWebSocketMocks.onEvent,
      sendAbandonGame: nativeWebSocketMocks.sendAbandonGame,
      sendSeatMutation: nativeWebSocketMocks.sendSeatMutation,
      dispose: nativeWebSocketMocks.dispose,
    };
  }),
}));
const mockSubmitAction = mocks.submitAction;
const mockSubmitInteraction = mocks.submitInteraction;
const mockCheckDeckCompatibility = mocks.checkDeckCompatibility;
const mockEvaluateDeckFormatGate = mocks.evaluateDeckFormatGate;
const mockGetSnapshot = mocks.getSnapshot as unknown as AsyncMockWithResolvedValueOnce;
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
    waiting_for: { type: "Priority", data: { player: 0 } },
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
    submitInteraction: mocks.submitInteraction,
    previewInteraction: mocks.previewInteraction,
    checkDeckCompatibility: mocks.checkDeckCompatibility,
    evaluateDeckFormatGate: mocks.evaluateDeckFormatGate,
    getState: mocks.getState,
    getLegalActions: mocks.getLegalActions,
    getSnapshot: mocks.getSnapshot,
    getLegalActionsForViewer: mocks.getLegalActionsForViewer,
    getFilteredState: mocks.getFilteredState,
    getViewerSnapshot: mocks.getViewerSnapshot,
    getAiActionProposal: mocks.getAiActionProposal,
    submitAiActionProposal: mocks.submitAiActionProposal,
    exportPersistenceState: mocks.exportPersistenceState,
    resumeMultiplayerHostState: mocks.resumeMultiplayerHostState,
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
  mockEvaluateDeckFormatGate.mockClear();
  mockGetViewerSnapshot.mockClear();
  mockSetMultiplayerMode.mockClear();
  mockProjectSeatView.mockClear();
  mockGetState.mockClear();
  mockGetAiActionProposal.mockClear();
  mockSubmitAiActionProposal.mockClear();
  mocks.exportPersistenceState.mockReset();
  mocks.exportPersistenceState.mockResolvedValue(JSON.stringify({ players: [], objects: {} }));
  mocks.resumeMultiplayerHostState.mockReset();
  mocks.resumeMultiplayerHostState.mockResolvedValue({
    snapshot: {
      state: { players: [], objects: {}, waiting_for: { type: "Priority", data: { player: 0 } } } as unknown as GameState,
      legalResult: { actions: [], autoPassRecommended: false },
      seq: 1,
    },
    presentation: {
        outcome: "noop",
        automatedResolutionCount: 0,
        omittedEventCount: 0,
        logEntries: [],
      },
  });
  persistenceMocks.clearGame.mockReset();
  persistenceMocks.clearGame.mockResolvedValue(undefined);
  persistenceMocks.clearP2PHostSession.mockReset();
  persistenceMocks.clearP2PHostSession.mockResolvedValue(undefined);
  persistenceMocks.saveGame.mockReset();
  persistenceMocks.saveGame.mockResolvedValue(undefined);
  persistenceMocks.saveP2PHostSession.mockReset();
  persistenceMocks.saveP2PHostSession.mockResolvedValue(undefined);
  persistenceMocks.saveResumableGameStrict.mockReset();
  persistenceMocks.saveResumableGameStrict.mockResolvedValue(undefined);
  terminalMocks.clearP2PTerminalResult.mockReset();
  terminalMocks.clearP2PTerminalResult.mockResolvedValue(undefined);
  terminalMocks.commitP2PTerminalResult.mockReset();
  terminalMocks.commitP2PTerminalResult.mockResolvedValue(true);
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
  connect(): DataConnection;
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

  /**
   * Every `state_ack` this fake fed back to the host, in emission order.
   * Harness-only bookkeeping: the host's own record (`guestAckedRevisions`) is
   * private, so this is the only direct observation point for what the seat
   * acknowledged, as opposed to what the host did about it.
   */
  readonly acksSent: P2PMessage[] = [];

  /**
   * Stop auto-acking. Callable at any point — including before
   * `initializeGame` — so a test can model a seat that never acknowledges
   * anything, not merely one that stops mid-game.
   */
  stopAcking() {
    this.acking = false;
  }

  /** Resume auto-acking after `stopAcking()`, so a test can show a seat that
   *  missed a window and then recovers. */
  resumeAcking() {
    this.acking = true;
  }

  /**
   * Default ON: a real guest acks every state-bearing frame it applies, and a
   * seat that never acks reads as permanently behind. Defaulting off would
   * change behavior in tests that have nothing to do with acknowledgement.
   */
  private acking = true;

  /**
   * The frame type after which this channel stops taking bytes, or `null`.
   * Consumed once.
   */
  private refuseAfter: P2PMessage["type"] | null = null;

  /**
   * Model a channel that REFUSES bytes — `open = false`, so `PeerSession.send`
   * resolves FALSE with no `close` event. `trySend` gates on `conn.open` both
   * before and inside its queue and returns false at either gate without
   * calling `handleDisconnect`, which is what makes this distinct from a `send`
   * that THROWS (the only other failure the base fake can produce): a throw
   * does call `handleDisconnect`, dropping the seat into `disconnectedSeats`
   * where the sweep skips it — a different scenario entirely.
   *
   * What this is NOT is peerjs PARKING a frame past its buffered-amount
   * budget. `_bufferedSend` returns `undefined` unconditionally and `trySend`
   * returns `true` after any non-throwing `conn.send`, so a parked frame
   * resolves TRUE and is indistinguishable from a delivered one at this
   * boundary. That is exactly why the ADVANCE signal is the guest's own
   * `state_ack` rather than the send result. The residual it leaves is
   * `terminalDelivered`, which still records transmission: a parked terminal
   * frame is recorded as delivered and the sweep stops nominating the seat.
   * That residual is accepted (it is post-game, and acking it would need a
   * second wire bump) and no test here covers it — this knob cannot produce
   * it.
   *
   * The refusal starts AFTER the named frame has been sent and acked, so the
   * seat is provably current on state when its next frame is refused.
   * `reopen()` restores the channel.
   */
  refuseSendsAfter(type: P2PMessage["type"]) {
    this.refuseAfter = type;
  }

  reopen() {
    this.open = true;
  }

  protected override onDecodedSend(msg: P2PMessage): void {
    // `FakeDataConnection.onDecodedSend` documents that an override MUST call
    // super, so the base keep-alive `pong` reply survives. No test in this file
    // currently fails without it — no seat here holds an open channel for a
    // full 10s of fake time — but an override that silently models a dead peer
    // is a trap for the next test that does.
    super.onDecodedSend(msg);
    this.ackDecodedSend(msg);
    if (this.refuseAfter !== null && this.refuseAfter === msg.type) {
      this.refuseAfter = null;
      this.open = false;
    }
  }

  private ackDecodedSend(msg: P2PMessage): void {
    if (!this.acking) return;
    // A decode `.then` from a pre-close send can settle AFTER
    // `simulateClose()`; dispatching an ack into a torn-down session would
    // model something a real guest never does.
    if (!this.open) return;
    if (msg.type !== "game_setup" && msg.type !== "state_update" && msg.type !== "reconnect_ack") {
      return;
    }
    // `revision` is optional on `state_update` and `reconnect_ack`; only
    // `game_setup` always carries one. An ack without a revision is not a
    // frame a real guest could produce.
    if (msg.revision == null) return;
    const ack = {
      type: "state_ack" as const,
      revision: msg.revision,
      // A real guest stamps `authority` on every outbound frame, so an
      // unstamped ack would skip the host's pre-switch authority guard.
      ...(msg.authority ? { authority: msg.authority } : {}),
    };
    this.acksSent.push(ack);
    // Enqueue on the inherited drain so `getSentMessages()` reaches the
    // fixed point: an ack can provoke host sends, which enqueue more decodes.
    // The `.catch` prevents an unhandled rejection when a test never awaits
    // `getSentMessages()`, but it must stay LOUD: a silently dropped ack makes
    // a seat read as un-acked, which would let a redelivery test pass for the
    // wrong reason.
    this.pendingDecodes.push(
      this.simulateData(ack).catch((e) => {
        console.warn("[FakeOpenableConnection] auto-ack dispatch failed:", e);
      }),
    );
  }
}

function twoHeadedGiantConfig(): FormatConfig {
  return {
    format: "TwoHeadedGiant",
    starting_life: 30,
    min_players: 4,
    max_players: 4,
    deck_size: { type: "Minimum", data: 60 },
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: true,
    sideboard_policy: { type: "Unlimited" },
    default_deck_copy_limit: { type: "Unlimited" },
    uses_commander: false,
    allow_debug_actions: false,
  };
}

/**
 * An active Custom-format config, shaped exactly as the engine's
 * `FormatConfig::for_custom_rules` derives it from a saved Axis-A definition:
 * `format` is the `"Custom:<id>"` wire string, and `custom_rules.id` matches.
 * `CustomFormatId(0)` is the reserved lobby-save sentinel every Axis-A save
 * carries.
 */
function customFormatConfig(): FormatConfig {
  return {
    format: "Custom:0",
    starting_life: 20,
    min_players: 2,
    max_players: 2,
    deck_size: { type: "Minimum", data: 60 },
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: false,
    sideboard_policy: { type: "Limited", data: 15 },
    default_deck_copy_limit: { type: "UpTo", data: 4 },
    uses_commander: false,
    supplies_fixed_deck: false,
    allow_debug_actions: false,
    custom_rules: {
      id: 0,
      structural: {
        starting_life: 20,
        min_players: 2,
        max_players: 2,
        deck_size: { type: "Minimum", data: 60 },
        singleton: false,
        command_zone_mode: "Disabled",
        range_of_influence: null,
        team_based: false,
        sideboard_policy: { type: "Limited", data: 15 },
        default_deck_copy_limit: { type: "UpTo", data: 4 },
      },
      legality: {
        legal_sets: null,
        banned: [],
        restricted: [],
        legacy: {
          mana_burn: "Modern",
          damage_timing: "Modern",
          wish_scope: "PostM10SideboardOnly",
          legend_rule_scope: "Modern",
        },
      },
    },
  };
}

function commanderConfig(): FormatConfig {
  return {
    format: "Commander",
    starting_life: 40,
    min_players: 2,
    max_players: 6,
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
}

/**
 * CR 903.13f: Commander Draft deck construction, which differs from Commander
 * in the two ways that show here — at least 60 cards with no maximum (1), and
 * any number of same-named cards from the drafted pool (2).
 *
 * Spelled out locally rather than imported: the real value lives in
 * `FORMAT_DEFAULTS` (`stores/multiplayerStore.ts`), which this suite does not
 * import and which is itself derived from the engine's format registry at
 * runtime. What the cases below need from it is only that a `formatConfig` is
 * present — that is what arms `validateGuestDeck` — and that its format is
 * `CommanderDraft`.
 */
function commanderDraftConfig(): FormatConfig {
  return {
    ...commanderConfig(),
    format: "CommanderDraft",
    deck_size: { type: "Minimum", data: 60 },
    singleton: false,
    default_deck_copy_limit: { type: "Unlimited" },
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

function makeResumedHost() {
  const { peer, onGuestConnected, emitConnection } = createFakePeer();
  const hostDeck = {
    player: { main_deck: ["Mountain"], sideboard: [] },
    opponent: { main_deck: ["Forest"], sideboard: [] },
    ai_decks: [],
  };
  const persistedSession = {
    gameId: "resume-game",
    roomCode: "ABCDE",
    sessionKey: "resume-session",
    useBroker: false,
    playerTokens: { 1: "guest-token" },
    guestDecks: { 1: { main_deck: ["Forest"], sideboard: [] } },
    kickedTokens: [],
    eliminatedSeats: [],
    playerCount: 2,
    hostDeckData: hostDeck,
    gameStarted: true,
  };
  const adapter = new P2PHostAdapter(
    hostDeck,
    peer as unknown as Peer,
    onGuestConnected,
    2,
    undefined,
    undefined,
    5_000,
    undefined,
    true,
    undefined,
    {
      gameId: "resume-game",
      roomCode: "ABCDE",
      resumeData: { state: { persisted: true } as unknown as PersistedGameState, session: persistedSession },
    },
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
  msg:
    | { type: "guest_deck"; deckData: unknown; wireProtocolVersion?: number }
    | { type: "reconnect"; playerToken: string; wireProtocolVersion?: number },
): Promise<FakeOpenableConnection> {
  const conn = new FakeOpenableConnection();
  emitConnection(conn as unknown as DataConnection);
  conn.fireOpen();
  await conn.simulateData({ ...msg, wireProtocolVersion: msg.wireProtocolVersion ?? WIRE_PROTOCOL_VERSION });
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

  it("exposes authoritative export from every P2P host", async () => {
    const { adapter } = makeHost(2);

    expect(adapter.exportPersistenceState).toBeDefined();
    await expect(adapter.exportPersistenceState!()).resolves.toBe(
      JSON.stringify({ players: [], objects: {} }),
    );
    expect(mocks.exportPersistenceState).toHaveBeenCalledOnce();

    const { adapter: nativeHost } = makeNativeHost();
    expect(nativeHost.exportPersistenceState).toBeDefined();
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

  /**
   * CR 903.13f(3): a draft that contained Commander Masters boosters grants the
   * partner ability, for deckbuilding purposes, to any card that can be a
   * commander by itself whose color identity is one or fewer colors. The engine
   * computes that grant from the deck payload's `draft_set_codes`.
   *
   * `startPregameGameInner` REBUILDS the deck payload field-by-field before
   * `initializeMultiplayerHostGame`, so declaring the field on `DeckListPayload`
   * is NOT sufficient on its own — an unnamed field is silently discarded and
   * the grant vanishes with it. This row is red exactly when the reconstruction
   * misses it, and green under the declaration alone would be the bug.
   *
   * BOTH codes, not one: the rule asks what the draft CONTAINED, so a mixed
   * CMM+CLB draft that forwarded a single representative code could drop the
   * very set the grant keys on.
   */
  it("carries the pod's draft set codes through the rebuilt payload to the engine", async () => {
    const { peer, onGuestConnected } = createFakePeer();
    const adapter = new P2PHostAdapter(
      {
        player: { main_deck: ["Mountain"], sideboard: [], commander: ["Human Legend"] },
        opponent: { main_deck: ["Forest"], sideboard: [] },
        ai_decks: [],
        draft_set_codes: ["CMM", "CLB"],
      },
      peer as unknown as Peer,
      onGuestConnected,
      2,
      commanderDraftConfig(),
    );
    await adapter.initialize();

    await adapter.applySeatMutation({
      type: "SetKind",
      data: {
        seatIndex: 1,
        kind: { type: "Ai", data: { difficulty: "Medium", deck: { type: "Random" } } },
      },
    });
    await adapter.startPregameGame();

    // Reach guard: the payload really was built and handed to the engine, so an
    // absent field below is a real omission rather than a dead harness.
    expect(mockInitializeHostGame).toHaveBeenCalledTimes(1);
    // The mock is declared parameterless, so its recorded args need a cast to
    // be read at all — the same reason `nativeWebSocketMocks.onEvent`'s
    // recorded handler is cast where it is read.
    const [payload] = mockInitializeHostGame.mock.calls[0] as unknown as [
      { draft_set_codes?: string[] },
    ];
    expect(payload.draft_set_codes).toEqual(["CMM", "CLB"]);
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

  it("persists resumed authority before acknowledging a reconnect", async () => {
    const persisted = deferred<void>();
    persistenceMocks.saveResumableGameStrict.mockImplementationOnce(() => persisted.promise);
    const { adapter, emitConnection } = makeResumedHost();

    const initialize = adapter.initialize();
    await flushPromises();
    expect(mocks.resumeMultiplayerHostState).toHaveBeenCalledOnce();
    expect(persistenceMocks.saveResumableGameStrict).toHaveBeenCalledOnce();

    const reconnect = new FakeOpenableConnection();
    const send = vi.spyOn(reconnect, "send");
    emitConnection(reconnect as unknown as DataConnection);
    reconnect.fireOpen();
    await reconnect.simulateData({
      type: "reconnect",
      playerToken: "guest-token",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
    });
    await flushPromises();

    expect(send).not.toHaveBeenCalled();

    persisted.resolve();
    await initialize;
    await flushPromises();

    expect(await reconnect.getSentMessages()).toContainEqual(expect.objectContaining({ type: "reconnect_ack" }));
    expect(persistenceMocks.saveResumableGameStrict.mock.invocationCallOrder[0])
      .toBeLessThan(send.mock.invocationCallOrder[0]!);
    adapter.dispose();
  });

  it("releases unpublished resumed authority after a strict-save failure without acknowledging guests", async () => {
    persistenceMocks.saveResumableGameStrict.mockRejectedValueOnce(new Error("IndexedDB unavailable"));
    const { adapter, emitConnection } = makeResumedHost();
    const authority = (adapter as unknown as { authority: { sessionKey: string; hostIncarnation: string } }).authority;

    const initialize = adapter.initialize();
    await flushPromises();
    const reconnect = new FakeOpenableConnection();
    emitConnection(reconnect as unknown as DataConnection);
    reconnect.fireOpen();
    await reconnect.simulateData({
      type: "reconnect",
      playerToken: "guest-token",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
    });

    await expect(initialize).rejects.toThrow("IndexedDB unavailable");
    await flushPromises();

    expect(ownsP2PHostLease(authority)).toBe(false);
    expect(mocks.releaseHostSession).toHaveBeenCalledWith(true);
    expect(await reconnect.getSentMessages()).toEqual([]);
  });

  it("commits a terminal resumed result before clearing its stale resumable save or publishing it", async () => {
    const terminalState = {
      players: [],
      objects: {},
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    } as unknown as GameState;
    mocks.resumeMultiplayerHostState.mockResolvedValueOnce({
      snapshot: {
        state: terminalState,
        legalResult: { actions: [], autoPassRecommended: false },
        seq: 1,
      },
      presentation: {
        outcome: "noop",
        automatedResolutionCount: 0,
        omittedEventCount: 0,
        logEntries: [],
      },
    });
    (mockGetViewerSnapshot as unknown as { mockResolvedValue: (value: unknown) => void }).mockResolvedValue({
      state: terminalState,
      actions: [],
      autoPassRecommended: false,
    });
    const committed = deferred<boolean>();
    terminalMocks.commitP2PTerminalResult.mockImplementationOnce(() => committed.promise);
    const { adapter, emitConnection } = makeResumedHost();

    const initialize = adapter.initialize();
    await vi.waitFor(() => expect(terminalMocks.commitP2PTerminalResult).toHaveBeenCalledOnce());
    expect(persistenceMocks.clearGame).not.toHaveBeenCalled();

    const reconnect = new FakeOpenableConnection();
    const send = vi.spyOn(reconnect, "send");
    emitConnection(reconnect as unknown as DataConnection);
    reconnect.fireOpen();
    await reconnect.simulateData({
      type: "reconnect",
      playerToken: "guest-token",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
    });
    await flushPromises();
    expect(send).not.toHaveBeenCalled();

    committed.resolve(true);
    await initialize;
    await flushPromises();

    expect(persistenceMocks.clearGame).toHaveBeenCalledWith("resume-game");
    expect(terminalMocks.commitP2PTerminalResult.mock.invocationCallOrder[0])
      .toBeLessThan(persistenceMocks.clearGame.mock.invocationCallOrder[0]!);
    expect(persistenceMocks.clearGame.mock.invocationCallOrder[0])
      .toBeLessThan(send.mock.invocationCallOrder[0]!);
    await vi.waitFor(async () => {
      expect((await reconnect.getSentMessages()).map((message) => (message as { type: string }).type))
        .toEqual(["reconnect_ack", "terminal_result"]);
    });
    adapter.dispose();
  });

  it("consumes a resumed automation result exactly once", async () => {
    const restored: RestoredGameStateResult = {
      snapshot: {
        state: remoteState("resumed"),
        legalResult: { actions: [], autoPassRecommended: false },
        seq: 1,
      },
      presentation: {
        outcome: "noop",
        automatedResolutionCount: 0,
        omittedEventCount: 0,
        logEntries: [],
      },
    };
    mocks.resumeMultiplayerHostState.mockResolvedValueOnce(restored);
    const { adapter } = makeResumedHost();

    await adapter.initialize();
    await adapter.initialize();

    expect(mocks.resumeMultiplayerHostState).toHaveBeenCalledOnce();
    await expect(adapter.resumeRestoredGameState()).resolves.toBe(restored);
    await expect(adapter.resumeRestoredGameState()).resolves.toBeNull();
    adapter.dispose();
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
    mockEvaluateDeckFormatGate.mockResolvedValueOnce({
      compatible: false,
      reasons: ["Commander does not use a signature spell slot"],
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

    expect(mockEvaluateDeckFormatGate).toHaveBeenCalledWith({
      main_deck: ["Plains"],
      sideboard: [],
      commander: ["Legal Commander"],
      companion: [],
      signature_spell: ["Invalid Signature Spell"],
      selected_format: "Commander",
    });
    // The UI-hint function must not be what decides a kick.
    expect(mockCheckDeckCompatibility).not.toHaveBeenCalled();
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

  it("admits a legal guest deck through the format gate without kicking", async () => {
    // Positive reach-guard for the two assertions below: it proves the guest
    // actually reached `validateGuestDeck` (the gate was called with this
    // deck), so the "not kicked" assertion cannot pass vacuously by the guest
    // never being validated at all.
    mockEvaluateDeckFormatGate.mockResolvedValueOnce({ compatible: true, reasons: [] });
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
          signature_spell: [],
        },
      },
    });
    await flushPromises(20);

    // The gate — NOT the shared UI-hint function — is what ran.
    expect(mockEvaluateDeckFormatGate).toHaveBeenCalledWith({
      main_deck: ["Plains"],
      sideboard: [],
      commander: ["Legal Commander"],
      companion: [],
      signature_spell: [],
      selected_format: "Commander",
    });
    expect(mockCheckDeckCompatibility).not.toHaveBeenCalled();

    const messages = await guest.getSentMessages();
    expect(messages.some((message) => (message as { type?: string }).type === "kick")).toBe(false);
    expect(guest.open).toBe(true);
    expect(adapter.getPlayerSlots().map((slot) => slot.kind.type)).toEqual([
      "HostHuman",
      "JoinedHuman",
    ]);
  });

  /**
   * CR 903.13f(3): `commander_draft_partner_grant` computes the Commander
   * Masters partner grant from the request's `draft_set_codes`. This gate KICKS
   * a guest whose deck fails it, so a request that omits the field rejects a
   * rules-legal two-commander deck and evicts the player.
   *
   * This asserts the REQUEST only, and is named for that. It cannot assert
   * ADMISSION of a partner-granted deck: the wasm is mocked in this suite, so
   * `commander_draft_partner_grant` never runs and the verdict here is
   * whatever the mock was told to return.
   *
   * `objectContaining` on this NEW case only — the exact `toHaveBeenCalledWith`
   * rows above stay exact. Their `makeHost` deck has no `draft_set_codes`, and
   * `toHaveBeenCalledWith` compares with `toEqual` semantics, which ignores an
   * `undefined` key; they would go red only if the host normalised the absent
   * value to `null` or `[]`, which the passthrough deliberately does not do.
   */
  it("sends the host's draft set codes on the guest deck gate request", async () => {
    mockEvaluateDeckFormatGate.mockResolvedValueOnce({ compatible: true, reasons: [] });
    const { peer, onGuestConnected, emitConnection } = createFakePeer();
    const adapter = new P2PHostAdapter(
      {
        player: { main_deck: ["Mountain"], sideboard: [], commander: ["Human Legend"] },
        opponent: { main_deck: ["Forest"], sideboard: [] },
        ai_decks: [],
        draft_set_codes: ["CMM"],
      },
      peer as unknown as Peer,
      onGuestConnected,
      2,
      commanderDraftConfig(),
    );
    await adapter.initialize();

    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: {
        player: {
          main_deck: ["Plains"],
          sideboard: [],
          commander: ["Mono Red Legend", "Mono Blue Legend"],
          companion: [],
          signature_spell: [],
        },
      },
    });
    await flushPromises(20);

    expect(mockEvaluateDeckFormatGate).toHaveBeenCalledWith(
      expect.objectContaining({
        commander: ["Mono Red Legend", "Mono Blue Legend"],
        selected_format: "CommanderDraft",
        draft_set_codes: ["CMM"],
      }),
    );
    // Reach guard: the guest survived the gate, so the assertion above read a
    // real request rather than one built on a path that then kicked.
    expect(guest.open).toBe(true);
  });

  it("kicks a guest whose deck is for a Custom format the engine cannot evaluate", async () => {
    // THE regression this whole split exists for. The shared
    // `checkDeckCompatibility` deliberately answers "no opinion"
    // (`selected_format_compatible: null`) for a Custom format, which the old
    // `=== false` kick test would have read as "not illegal" — silently
    // admitting every Custom-format guest deck. The dedicated gate returns a
    // definite `false` instead, so the guest is kicked.
    mockEvaluateDeckFormatGate.mockResolvedValueOnce({
      compatible: false,
      reasons: ["Custom format deck-compatibility checks are not yet supported."],
    });
    const { adapter, emitConnection } = makeHost(2, 5_000, customFormatConfig());
    await adapter.initialize();

    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: {
        player: { main_deck: ["Plains"], sideboard: [] },
      },
    });
    await flushPromises(20);

    expect(mockEvaluateDeckFormatGate).toHaveBeenCalledWith({
      main_deck: ["Plains"],
      sideboard: [],
      commander: [],
      companion: [],
      signature_spell: [],
      selected_format: "Custom:0",
    });
    expect(mockCheckDeckCompatibility).not.toHaveBeenCalled();

    const kicked = (await guest.getSentMessages()).find(
      (message) =>
        typeof message === "object"
        && message !== null
        && (message as { type: string }).type === "kick",
    );
    expect(kicked).toMatchObject({
      type: "kick",
      reason: "Deck rejected: Custom format deck-compatibility checks are not yet supported.",
      format: "Custom:0",
    });
    // Session closed and the seat reverted to waiting.
    expect(guest.open).toBe(false);
    expect(adapter.getPlayerSlots().map((slot) => slot.kind.type)).toEqual([
      "HostHuman",
      "WaitingHuman",
    ]);
    expect(mockInitializeHostGame).not.toHaveBeenCalled();
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
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
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

    // Envelope spoofing is an operational transport fault, not an engine
    // rejection payload.
    const rejected = (await g2.getSentMessages()).find(
      (m) =>
        typeof m === "object" &&
        m !== null &&
        (m as { type: string }).type === "action_failed",
    );
    expect(rejected).toBeDefined();
    // And the spoofed action did NOT reach the engine.
    expect(mockSubmitAction).not.toHaveBeenCalled();
  });

  it("separates engine rejections from host operational action failures", async () => {
    const { adapter, emitConnection } = makeHost(2);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    guest.sent.length = 0;

    const rejection = {
      code: "invalid_action" as const,
      disposition: "invalid" as const,
      message: "Engine error: invalid action",
      related_object_ids: [42],
    };
    mockSubmitAction.mockRejectedValueOnce(
      new AdapterError(AdapterErrorCode.ACTION_REJECTED, rejection.message, true, undefined, rejection),
    );
    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    expect(await guest.getSentMessages()).toContainEqual(expect.objectContaining({
      type: "action_rejected",
      rejection,
    }));

    guest.sent.length = 0;
    mockSubmitAction.mockRejectedValueOnce(new Error("engine transport unavailable"));
    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    expect(await guest.getSentMessages()).toContainEqual(expect.objectContaining({
      type: "action_failed",
      message: "engine transport unavailable",
    }));
    adapter.dispose();
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

  const isStateBearingWithRevision = (m: unknown): m is P2PMessage & { revision: number } =>
    typeof m === "object"
    && m !== null
    && ["game_setup", "state_update", "reconnect_ack"].includes((m as { type: string }).type)
    && typeof (m as { revision?: unknown }).revision === "number";

  // Harness coverage for `FakeOpenableConnection`'s auto-ack. Nothing else in
  // the suite observes it directly, so this test is the only thing standing
  // between a silently-broken fake and every acceptance-semantics test built
  // on top of it.
  it("auto-acks every state-bearing frame the host sends, echoing its authority", async () => {
    const { adapter, emitConnection } = makeHost(2);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await adapter.submitAction({ type: "PassPriority" }, 0);

    const sent = await guest.getSentMessages();
    const stateBearing = sent.filter(isStateBearingWithRevision);
    // Both a `game_setup` and at least one `state_update` must have flowed,
    // or the ack assertion below would be vacuously satisfiable.
    expect(stateBearing.map((m) => m.type)).toEqual(
      expect.arrayContaining(["game_setup", "state_update"]),
    );
    // One ack per state-bearing frame, in order, carrying that frame's
    // revision and the host's authority stamp verbatim.
    expect(guest.acksSent).toEqual(
      stateBearing.map((m) => ({
        type: "state_ack",
        revision: m.revision,
        authority: m.authority,
      })),
    );

    // The knob silences it: further host state changes provoke no new ack.
    guest.stopAcking();
    const ackCount = guest.acksSent.length;
    const stateBearingCount = stateBearing.length;
    await adapter.submitAction({ type: "PassPriority" }, 0);
    const afterStop = await guest.getSentMessages();
    // Non-vacuity: the host really did send another state-bearing frame that
    // the fake would have acked had the knob not been flipped.
    expect(afterStop.filter(isStateBearingWithRevision).length).toBeGreaterThan(stateBearingCount);
    expect(guest.acksSent).toHaveLength(ackCount);
    adapter.dispose();
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

  it("reserves a reconnect seat through its ACK and ignores duplicate or pending actions", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
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
    const pendingSnapshot = deferred<{
      state: GameState;
      actions: GameAction[];
      autoPassRecommended: boolean;
    }>();
    (mockGetViewerSnapshot as unknown as {
      mockImplementationOnce: (implementation: () => Promise<unknown>) => void;
    }).mockImplementationOnce(() => pendingSnapshot.promise);
    guest.simulateClose();

    const reconnect = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: setup!.playerToken,
    });
    await reconnect.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    const duplicate = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: setup!.playerToken,
    });
    await flushPromises();

    expect(mockSubmitAction).not.toHaveBeenCalled();
    expect((await duplicate.getSentMessages()).some(
      (message) => (message as { type?: string }).type === "reconnect_rejected",
    )).toBe(true);

    // Closing the reserved channel must release only its reservation, leaving
    // the seat disconnected and eligible for a later retry.
    reconnect.simulateClose();
    pendingSnapshot.resolve({
      state: { players: [], objects: {}, waiting_for: { type: "Priority", data: { player: 1 } } } as unknown as GameState,
      actions: [{ type: "PassPriority" }],
      autoPassRecommended: false,
    });
    await flushPromises();

    const retry = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: setup!.playerToken,
    });
    await flushPromises();
    const messages = await retry.getSentMessages();
    expect(messages.map((message) => (message as { type?: string }).type)).toContain("reconnect_ack");
    const action = retry.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await action;
    expect(mockSubmitAction).toHaveBeenCalledWith({ type: "PassPriority" }, 1);
  });

  it("keeps a seat disconnected when its reconnect ACK is dropped before the write", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
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
    const pendingSnapshot = deferred<{
      state: GameState;
      actions: GameAction[];
      autoPassRecommended: boolean;
    }>();
    (mockGetViewerSnapshot as unknown as {
      mockImplementationOnce: (implementation: () => Promise<unknown>) => void;
    }).mockImplementationOnce(() => pendingSnapshot.promise);
    guest.simulateClose();

    const reconnect = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: setup!.playerToken,
    });
    // The handoff is still resolving, then the data channel drops before the
    // queued reconnect_ack can reach `conn.send`.
    reconnect.open = false;
    pendingSnapshot.resolve({
      state: remoteState("dropped reconnect acknowledgement"),
      actions: [{ type: "PassPriority" }],
      autoPassRecommended: false,
    });
    await flushPromises();

    const host = adapter as unknown as {
      guestSessions: Map<number, unknown>;
      pendingReconnectSessions: Map<number, unknown>;
      disconnectedSeats: Map<number, unknown>;
      gameRunState: string;
    };
    expect(await reconnect.getSentMessages()).toEqual([]);
    expect(host.guestSessions.has(1)).toBe(false);
    expect(host.pendingReconnectSessions.has(1)).toBe(false);
    expect(host.disconnectedSeats.has(1)).toBe(true);
    expect(host.gameRunState).toBe("paused-disconnect");

    const retry = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: setup!.playerToken,
    });
    await flushPromises();
    expect((await retry.getSentMessages()).some(
      (message) => (message as { type?: string }).type === "reconnect_ack",
    )).toBe(true);
  });

  it("rejects a reconnect when its native handoff cannot be built", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
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
    guest.simulateClose();

    const host = adapter as unknown as { nativeBridge: object | null };
    host.nativeBridge = {};
    const reconnect = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: setup!.playerToken,
    });
    await flushPromises();

    expect((await reconnect.getSentMessages()).find(
      (message) => (message as { type?: string }).type === "reconnect_rejected",
    )).toMatchObject({
      type: "reconnect_rejected",
      reason: "Reconnect acknowledgement failed",
    });
  });

  it("serializes a native reconnect behind an in-flight native revision delivery", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
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
    guest.simulateClose();

    const host = adapter as unknown as {
      nativeBridge: object | null;
      nativeDeliveredViews: Map<number, { revision: number; snapshot: EngineSnapshot }>;
      authoritativeRevision: number;
      enqueueDelivery: (operation: () => Promise<void>) => Promise<void>;
    };
    const oldSnapshot: EngineSnapshot = {
      state: remoteState("native revision one"),
      legalResult: { actions: [], autoPassRecommended: false },
      seq: 1,
    };
    const currentSnapshot: EngineSnapshot = {
      state: remoteState("native revision two"),
      legalResult: { actions: [{ type: "PassPriority" }], autoPassRecommended: false },
      seq: 2,
    };
    host.nativeBridge = {};
    host.nativeDeliveredViews.set(1, { revision: 1, snapshot: oldSnapshot });
    host.authoritativeRevision = 1;
    const revisionDelivery = deferred<void>();
    const inFlightRevision = host.enqueueDelivery(async () => {
      host.authoritativeRevision = 2;
      await revisionDelivery.promise;
      host.nativeDeliveredViews.set(1, { revision: 2, snapshot: currentSnapshot });
    });

    const reconnect = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: setup!.playerToken,
    });
    await flushPromises();
    expect((await reconnect.getSentMessages()).some(
      (message) => (message as { type?: string }).type === "reconnect_ack",
    )).toBe(false);

    revisionDelivery.resolve();
    await inFlightRevision;
    await flushPromises();

    const ack = (await reconnect.getSentMessages()).find(
      (message): message is { type: "reconnect_ack"; revision: number; state: { label: string } } =>
        typeof message === "object"
        && message !== null
        && (message as { type?: string }).type === "reconnect_ack",
    );
    expect(ack).toMatchObject({
      revision: 2,
      state: { label: "native revision two" },
    });
  });

  it("serializes a WASM reconnect with a queued final state and terminal delivery", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
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
    const oldState = remoteState("before final state");
    const finalState = {
      ...remoteState("final state"),
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    } as unknown as GameState;
    let viewerState = oldState;
    (mockGetViewerSnapshot as unknown as {
      mockImplementation: (implementation: () => Promise<unknown>) => void;
    }).mockImplementation(async () => ({
      state: viewerState,
      actions: [],
      autoPassRecommended: false,
    }));
    mockGetSnapshot.mockResolvedValueOnce({
      state: finalState,
      legalResult: { actions: [], autoPassRecommended: false },
      seq: 99,
    });

    const host = adapter as unknown as {
      enqueueDelivery: (operation: () => Promise<void>) => Promise<void>;
      broadcastStateUpdate: (
        events: GameEvent[],
        logEntries?: GameLogEntry[],
        terminalReason?: string,
      ) => Promise<void>;
    };
    const releaseDelivery = deferred<void>();
    const inFlightDelivery = host.enqueueDelivery(async () => {
      await releaseDelivery.promise;
    });

    guest.simulateClose();
    const reconnect = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: setup!.playerToken,
    });
    viewerState = finalState;
    const finalBroadcast = host.broadcastStateUpdate([], [], "Game complete");

    releaseDelivery.resolve();
    await inFlightDelivery;
    await finalBroadcast;
    await flushPromises();

    const messages = await reconnect.getSentMessages();
    const ack = messages.find(
      (message): message is { type: "reconnect_ack"; revision: number; state: { label: string } } =>
        typeof message === "object"
        && message !== null
        && (message as { type?: string }).type === "reconnect_ack",
    );
    const update = messages.find(
      (message): message is { type: "state_update"; revision: number; state: { label: string } } =>
        typeof message === "object"
        && message !== null
        && (message as { type?: string }).type === "state_update",
    );
    const terminalIndex = messages.findIndex(
      (message) => typeof message === "object"
        && message !== null
        && (message as { type?: string }).type === "terminal_result",
    );

    expect(ack).toMatchObject({ state: { label: "final state" } });
    expect(update).toMatchObject({ state: { label: "final state" } });
    expect(update!.revision).toBeGreaterThan(ack!.revision);
    expect(terminalIndex).toBeGreaterThan(messages.indexOf(update!));
  });

  it("resumes a manual pause only after the last disconnected seat is resolved", async () => {
    const { adapter } = makeHost(2, 5_000);
    await adapter.initialize();
    const events: P2PAdapterEvent[] = [];
    adapter.onEvent((event) => events.push(event));
    adapter.requestPause();
    adapter.requestResume();

    expect(events.map((event) => event.type)).toEqual(["gamePaused", "gameResumed"]);

    const host = adapter as unknown as {
      disconnectedSeats: Map<number, { disconnectedAt: number; timer: null }>;
    };
    host.disconnectedSeats.set(1, { disconnectedAt: Date.now(), timer: null });
    adapter.requestPause();
    adapter.requestResume();

    expect(events.filter((event) => event.type === "gameResumed")).toHaveLength(1);
  });

  it("replays a native AI driver fault after reconnecting guest's snapshot", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
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
        && (message as { type: string }).type === "game_setup",
    );
    expect(setup).toBeDefined();
    guest.simulateClose();

    const fault: { id: number; revision: number; message: string } = {
      id: 7,
      revision: 3,
      message: "Native AI driver stopped",
    };
    const host = adapter as unknown as {
      authoritativeRevision: number;
      handleNativeAiDriverFault: (driverFault: typeof fault) => Promise<void>;
    };
    host.authoritativeRevision = fault.revision;
    await host.handleNativeAiDriverFault(fault);

    const reconnectedGuest = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: setup!.playerToken,
    });
    await flushPromises();

    const messages = await reconnectedGuest.getSentMessages();
    expect(messages.map((message) => (message as { type: string }).type)).toEqual([
      "reconnect_ack",
      "ai_driver_fault",
    ]);
    expect(messages[1]).toMatchObject({ type: "ai_driver_fault", ...fault });
  });

  it("waits for a resumed native fault's final revision before replaying it to a reconnecting guest", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
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
        && (message as { type: string }).type === "game_setup",
    );
    expect(setup).toBeDefined();
    guest.simulateClose();

    const host = adapter as unknown as {
      nativeAiDriverFault: { id: number; revision: number; message: string } | null;
      deliveredNativeAiDriverFault: { id: number; revision: number; message: string } | null;
    };
    host.nativeAiDriverFault = { id: 7, revision: 3, message: "Native AI driver stopped" };
    host.deliveredNativeAiDriverFault = null;

    const reconnectedGuest = await joinGuest(emitConnection, {
      type: "reconnect",
      playerToken: setup!.playerToken,
    });
    await flushPromises();

    const messageTypes = (await reconnectedGuest.getSentMessages()).map(
      (message) => (message as { type: string }).type,
    );
    expect(messageTypes).toContain("reconnect_ack");
    expect(messageTypes).not.toContain("ai_driver_fault");
  });

  it("renders a persisted native AI driver fault once when the host resumes", async () => {
    const { peer, onGuestConnected } = createFakePeer();
    const fault = { id: 7, revision: 3, message: "Native AI driver stopped" };
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
      {
        gameId: "native-resume-fault",
        roomCode: "ABCDE",
        resumeData: {
          session: {
            gameId: "native-resume-fault",
            roomCode: "ABCDE",
            sessionKey: "native-resume-fault-session",
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
            gameStarted: true,
            nativeAiDriverFault: fault,
            nativeSession: {
              gameCode: "native-game",
              fullKey: { game_code: "native-game", generation: 1 },
              playerTokens: { 0: "native-host-token" },
            },
          },
        },
      },
      {},
    );
    nativeWebSocketMocks.initializePregame.mockResolvedValue(NATIVE_HOST_ATTACHMENT);

    const events: P2PAdapterEvent[] = [];
    adapter.onEvent((event) => events.push(event));
    await adapter.initialize();

    const onNativeEvent = nativeWebSocketMocks.onEvent.mock.calls[0]?.[0] as
      | ((event: WsAdapterEvent) => void)
      | undefined;
    if (!onNativeEvent) throw new Error("Native bridge did not register a WebSocket event listener");

    const finalSnapshot: EngineSnapshot = {
      state: remoteState("native AI final state"),
      legalResult: { actions: [], autoPassRecommended: false },
      seq: 3,
    };
    onNativeEvent({
      type: "stateChanged",
      snapshot: finalSnapshot,
      events: [],
      serverRevision: fault.revision,
    });
    await flushPromises();
    const host = adapter as unknown as {
      handleNativeAiDriverFault: (driverFault: typeof fault) => Promise<void>;
    };
    await host.handleNativeAiDriverFault(fault);
    await host.handleNativeAiDriverFault(fault);
    await host.handleNativeAiDriverFault({ ...fault, id: fault.id + 1 });

    expect(events).toContainEqual(expect.objectContaining({
      type: "stateChanged",
      snapshot: finalSnapshot,
      events: [],
    }));
    expect(events).toContainEqual({ type: "error", message: fault.message });
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
    const legendCandidateIdentities = {
      "42": "TokenCopy" as const,
      "43": "Unknown" as const,
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

  it("does not replace a guest's newer snapshot with a stale state revision", async () => {
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
      revision: 8,
      state: remoteState("setup"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    await adapter.initializeGame();
    emitted.mockClear();

    await conn.simulateData({
      type: "state_update",
      revision: 9,
      state: remoteState("current"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    emitted.mockClear();

    await conn.simulateData({
      type: "state_update",
      revision: 8,
      state: remoteState("stale"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });

    expect(((await adapter.getState()) as unknown as { label: string }).label).toBe("current");
    expect(emitted).not.toHaveBeenCalled();
  });

  it("acks the revision the guest has applied, including the held one on a stale drop", async () => {
    const { peer } = createFakePeer();
    const conn = new FakeDataConnection();
    const adapter = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      peer as unknown as Peer,
      "host-peer",
      conn as unknown as DataConnection,
    );
    await adapter.initialize();

    const ackedRevisions = async (): Promise<number[]> => {
      await flushPromises();
      return (await conn.getSentMessages())
        .filter(
          (m): m is { type: "state_ack"; revision: number } =>
            typeof m === "object" && m !== null && (m as { type: string }).type === "state_ack",
        )
        .map((m) => m.revision);
    };

    await conn.simulateData({
      type: "game_setup",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 1,
      playerToken: "seat-token",
      revision: 8,
      state: remoteState("setup"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    await adapter.initializeGame();
    expect(await ackedRevisions()).toEqual([8]);

    await conn.simulateData({
      type: "state_update",
      revision: 9,
      state: remoteState("current"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    expect(await ackedRevisions()).toEqual([8, 9]);

    // The stale frame is dropped, but the guest still answers — with the
    // NEWER revision it holds. A host ledger that records transmission cannot
    // observe this; the ack is the only way it learns the seat is ahead.
    await conn.simulateData({
      type: "state_update",
      revision: 8,
      state: remoteState("stale"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    expect(await ackedRevisions()).toEqual([8, 9, 9]);

    // A resumed session re-declares the seat's revision. This test is the
    // guest half only: it drives `P2PGuestAdapter` directly, with no host on
    // the other end. The host half — `recordGuestAck` and the redelivery
    // predicate it feeds — is pinned by the eventual-delivery block below.
    await conn.simulateData({
      type: "reconnect_ack",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 1,
      revision: 12,
      state: remoteState("resumed"),
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    expect(await ackedRevisions()).toEqual([8, 9, 9, 12]);
  });

  it("still acks an applied state_update when a stateChanged listener throws", async () => {
    const { peer } = createFakePeer();
    const conn = new FakeDataConnection();
    const adapter = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      peer as unknown as Peer,
      "host-peer",
      conn as unknown as DataConnection,
    );
    await adapter.initialize();

    // `P2PGuestAdapter.emit` does not wrap its listeners, so a throwing
    // subscriber unwinds the rest of the arm — here, only an ack placed after
    // the emit. It must already be on the wire: a resent EQUAL revision is not
    // `<` the cached one, so it re-enters this same arm and throws again, and
    // and since the host drives redelivery off these acks, that is a resend
    // loop rather than a heal. The
    // seat itself is fine throughout; it holds the state and serves
    // `getState()`.
    adapter.onEvent((event) => {
      if (event.type === "stateChanged") throw new Error("subscriber blew up");
    });

    await conn.simulateData({
      type: "game_setup",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 1,
      playerToken: "seat-token",
      revision: 3,
      state: remoteState("setup"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    await adapter.initializeGame();

    await conn.simulateData({
      type: "state_update",
      revision: 4,
      state: remoteState("current"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    await flushPromises();

    const acked = (await conn.getSentMessages())
      .filter(
        (m): m is { type: "state_ack"; revision: number } =>
          typeof m === "object" && m !== null && (m as { type: string }).type === "state_ack",
      )
      .map((m) => m.revision);
    expect(acked).toEqual([3, 4]);
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

  /** A guest adapter past its `game_setup` handshake, ready to submit. */
  async function joinedGuest(): Promise<{ adapter: P2PGuestAdapter; conn: FakeDataConnection }> {
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
    return { adapter, conn };
  }

  it("guest submission the host never answers rejects at the timeout instead of parking forever", async () => {
    const { adapter, conn } = await joinedGuest();

    // The host lease fence applies the action and then sends NOTHING: no
    // `state_update` (so no ack, no ledger entry, no redelivery sweep), and no
    // `action_rejected`/`action_failed` either. The channel stays open and
    // healthy — a liveness detector has nothing to find here.
    const stranded = adapter.submitAction({ type: "PassPriority" }, 1);
    let settled = false;
    void stranded.catch(() => {
      settled = true;
    });

    await vi.advanceTimersByTimeAsync(29_000);
    expect(settled).toBe(false);

    await vi.advanceTimersByTimeAsync(1_000);
    await expect(stranded).rejects.toMatchObject({
      code: "P2P_ERROR",
      recoverable: true,
    });
    // The host is silent, not gone: nothing was sent after the action frame.
    expect((await conn.getSentMessages()).filter(
      (message) => (message as { type?: string }).type === "action",
    )).toHaveLength(1);
  });

  it("guest settles an action submission displaced by an interaction submission", async () => {
    const { adapter, conn } = await joinedGuest();

    // `dispatchInteraction` never touches `isAnimating`/`inFlightLocalAction`
    // while `dispatchActionInternal` does, so an interaction submit can overlap
    // an action submit and take the single slot from under it.
    const displaced = adapter.submitAction({ type: "PassPriority" }, 1);
    const displacing = adapter.submitInteraction({} as never, 1);

    await expect(displaced).rejects.toMatchObject({
      code: "P2P_ERROR",
      recoverable: true,
    });

    // The displacing submission still settles normally off the next reply. The
    // slot remains single and unkeyed: this does NOT make reply routing correct
    // after a displacement, it only stops the displaced caller from parking.
    await conn.simulateData({ type: "action_noop" });
    await expect(displacing).resolves.toEqual({ events: [], log_entries: [] });
  });

  it("guest timeout from a settled submission never rejects a later, unrelated one", async () => {
    const { adapter, conn } = await joinedGuest();

    const first = adapter.submitAction({ type: "PassPriority" }, 1);
    await conn.simulateData({ type: "action_noop" });
    await expect(first).resolves.toEqual({ events: [], log_entries: [] });

    // Sit just short of the FIRST submission's original 30s deadline, then park
    // a second submission. A timeout armed on park but cleared only in the
    // teardown helper survives the successful settle and fires below — against
    // a submission the host has had barely a second to answer. The slot is
    // unkeyed, so that rejection would hit whatever is parked, breaking normal
    // play rather than fixing the freeze.
    await vi.advanceTimersByTimeAsync(29_000);
    const second = adapter.submitInteraction({} as never, 1);
    await vi.advanceTimersByTimeAsync(2_000);

    await conn.simulateData({ type: "action_noop" });
    await expect(second).resolves.toEqual({ events: [], log_entries: [] });
  });

  it("guest preserves a structured stale engine rejection from the host", async () => {
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

    const stale = adapter.submitAction(
      { type: "ReorderHand", data: { order: [1, 2, 3] } } as unknown as GameAction,
      1,
    );
    await conn.simulateData({
      type: "action_rejected",
      rejection: {
        code: "stale_action",
        disposition: "stale",
        message: "Engine error: ReorderHand no longer matches the hand",
        related_object_ids: [1],
      },
    });
    await expect(stale).rejects.toMatchObject({
      code: "STALE_ACTION",
      recoverable: false,
      rejection: expect.objectContaining({ related_object_ids: [1] }),
    });

    // A genuine rejection must still surface as a recoverable ACTION_REJECTED.
    const real = adapter.submitAction({ type: "PassPriority" }, 1);
    await conn.simulateData({
      type: "action_rejected",
      rejection: {
        code: "invalid_action",
        disposition: "invalid",
        message: "Engine error: Something genuinely wrong",
        related_object_ids: [],
      },
    });
    await expect(real).rejects.toMatchObject({
      code: "ACTION_REJECTED",
      recoverable: true,
    });
  });

  it("guest rejects a malformed rejection DTO without exposing its arbitrary message", async () => {
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

    const pending = adapter.submitAction({ type: "PassPriority" }, 1);
    await conn.simulateData({
      type: "action_rejected",
      rejection: { message: "untrusted host payload" },
    } as never);
    await expect(pending).rejects.toMatchObject({
      code: "ACTION_REJECTED",
      message: "Host sent an invalid action rejection",
      rejection: undefined,
    });

    await conn.simulateData({ type: "action_failed", message: "Match concession unavailable" });
    await conn.simulateData({ type: "action_rejected", rejection: { message: "untrusted" } } as never);
    expect(emitted).toHaveBeenCalledWith({ type: "error", message: "Match concession unavailable" });
    expect(emitted).toHaveBeenCalledWith({ type: "error", message: "Host sent an invalid action rejection" });
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

  it("accepts guest state and outbound actions only from the current authenticated session", async () => {
    const { peer } = createFakePeer();
    const firstConn = new FakeDataConnection();
    const adapter = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      peer as unknown as Peer,
      "host-peer",
      firstConn as unknown as DataConnection,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      true,
    );
    const emitted = vi.fn();
    adapter.onEvent(emitted);
    await adapter.initialize();
    await firstConn.simulateData({
      type: "game_setup",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 1,
      playerToken: "seat-token",
      state: remoteState("first-session"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    await adapter.initializeGame();

    const pendingFirstSessionAction = adapter.submitAction({ type: "PassPriority" }, 1);

    const secondConn = new FakeDataConnection();
    (adapter as unknown as { attachSession(conn: DataConnection): void }).attachSession(
      secondConn as unknown as DataConnection,
    );
    await expect(pendingFirstSessionAction).rejects.toThrow(
      "Host disconnected while submitting an action",
    );
    emitted.mockClear();

    await firstConn.simulateData({
      type: "state_update",
      state: remoteState("stale-state"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    await firstConn.simulateData({
      type: "reconnect_ack",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 2,
      state: remoteState("stale-ack"),
      playerNames: {},
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    firstConn.simulateClose();
    await secondConn.simulateData({
      type: "state_update",
      state: remoteState("pre-ack"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    await secondConn.simulateData({ type: "action_noop" });
    await secondConn.simulateData({ type: "action_rejected", reason: "stale response" });
    await secondConn.simulateData({ type: "mana_payment_preview", requestId: 1, sourceIds: [] });
    await secondConn.simulateData({ type: "terminal_result" } as never);
    await secondConn.simulateData({ type: "player_disconnected", playerId: 1 });

    expect(((await adapter.getState()) as unknown as { label: string }).label).toBe("first-session");
    expect(emitted).not.toHaveBeenCalled();
    await expect(adapter.submitAction({ type: "PassPriority" }, 1)).rejects.toThrow(
      "Not yet assigned a player ID",
    );
    await expect(adapter.submitInteraction({} as never, 1)).rejects.toThrow(
      "Not yet assigned a player ID",
    );
    await expect(adapter.previewManaPayment({ type: "PassPriority" }, 1)).rejects.toThrow(
      "Not yet assigned a player ID",
    );
    adapter.sendConcede();
    adapter.sendMatchConcede();
    expect(await secondConn.getSentMessages()).toEqual([]);

    await secondConn.simulateData({
      type: "reconnect_ack",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 3,
      state: remoteState("second-session"),
      playerNames: {},
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    expect(((await adapter.getState()) as unknown as { label: string }).label).toBe("second-session");

    const action = adapter.submitAction({ type: "PassPriority" }, 3);
    await flushPromises();
    expect((await secondConn.getSentMessages()).some(
      (message) => (message as { type?: string; senderPlayerId?: number }).type === "action"
        && (message as { senderPlayerId?: number }).senderPlayerId === 3,
    )).toBe(true);
    await secondConn.simulateData({ type: "action_noop" });
    await expect(action).resolves.toEqual({ events: [], log_entries: [] });

    const interaction = adapter.submitInteraction({} as never, 3);
    await flushPromises();
    expect((await secondConn.getSentMessages()).some(
      (message) => (message as { type?: string; senderPlayerId?: number }).type === "interaction"
        && (message as { senderPlayerId?: number }).senderPlayerId === 3,
    )).toBe(true);
    await secondConn.simulateData({ type: "action_noop" });
    await expect(interaction).resolves.toEqual({ events: [], log_entries: [] });

    const preview = adapter.previewManaPayment({ type: "PassPriority" }, 3);
    await flushPromises();
    expect((await secondConn.getSentMessages()).some(
      (message) => (message as { type?: string; requestId?: number }).type === "preview_mana_payment"
        && (message as { requestId?: number }).requestId === 1,
    )).toBe(true);
    await secondConn.simulateData({ type: "mana_payment_preview", requestId: 1, sourceIds: [] });
    await expect(preview).resolves.toEqual([]);

    adapter.sendConcede();
    adapter.sendMatchConcede();
    adapter.sendMatchConcede();
    await flushPromises();
    const messages = await secondConn.getSentMessages();
    expect(messages.map((message) => (message as { type?: string }).type)).toEqual(
      expect.arrayContaining(["action", "interaction", "preview_mana_payment"]),
    );
    expect(messages.filter((message) => (message as { type?: string }).type === "concede"))
      .toHaveLength(1);
    expect(messages.filter((message) => (message as { type?: string }).type === "match_concede"))
      .toHaveLength(1);

    secondConn.simulateClose();
    const thirdConn = new FakeDataConnection();
    (adapter as unknown as { attachSession(conn: DataConnection): void }).attachSession(
      thirdConn as unknown as DataConnection,
    );
    await thirdConn.simulateData({
      type: "reconnect_ack",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 3,
      state: remoteState("third-session"),
      playerNames: {},
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    adapter.sendMatchConcede();
    await flushPromises();
    expect((await thirdConn.getSentMessages()).some(
      (message) => (message as { type?: string }).type === "match_concede",
    )).toBe(true);
  });

  it("stops guest reconnect attempts when disposed during backoff", async () => {
    const { peer } = createFakePeer();
    const connect = vi.fn();
    const reconnectPeer = { ...peer, connect };
    const conn = new FakeDataConnection();
    const adapter = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      reconnectPeer as unknown as Peer,
      "host-peer",
      conn as unknown as DataConnection,
    );
    await adapter.initialize();

    conn.simulateClose();
    adapter.dispose();
    await vi.advanceTimersByTimeAsync(1_000);

    expect(connect).not.toHaveBeenCalled();
  });

  it("stops an unauthenticated guest reconnecting after host_left", async () => {
    const { peer } = createFakePeer();
    const connect = vi.fn();
    const reconnectPeer = { ...peer, connect };
    const conn = new FakeDataConnection();
    const adapter = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      reconnectPeer as unknown as Peer,
      "host-peer",
      conn as unknown as DataConnection,
    );
    const emitted = vi.fn();
    adapter.onEvent(emitted);
    await adapter.initialize();
    const setupRejection = expect(adapter.initializeGame()).rejects.toMatchObject({
      code: "P2P_REJECTED",
      message: "Host left",
    });

    await conn.simulateData({ type: "host_left", reason: "Host left" });
    await setupRejection;
    expect(conn.open).toBe(false);
    expect(emitted).toHaveBeenCalledWith({ type: "gameOver", winner: null, reason: "Host left" });
    adapter.sendConcede();
    const sentAfterTerminal = await conn.getSentMessages();
    expect(sentAfterTerminal.some((message) =>
      (message as { type?: string }).type === "concede"
      || (message as { type?: string }).type === "match_concede",
    )).toBe(false);
    await vi.advanceTimersByTimeAsync(1_000);

    expect(connect).not.toHaveBeenCalled();
  });

  it("closes a reconnect transport if disposed before its open continuation", async () => {
    const { peer } = createFakePeer();
    const reconnectConn = new FakeOpenableConnection();
    const connect = vi.fn(() => reconnectConn as unknown as DataConnection);
    const reconnectPeer = { ...peer, connect };
    const conn = new FakeDataConnection();
    const adapter = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      reconnectPeer as unknown as Peer,
      "host-peer",
      conn as unknown as DataConnection,
    );
    await adapter.initialize();

    conn.simulateClose();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(connect).toHaveBeenCalledOnce();
    // The reconnect dial must carry the same options as the initial join.
    // Without `reliable: true` PeerJS builds the channel `ordered: false`, and
    // the guest's revision guards drop reordered frames rather than
    // resequencing them.
    expect(connect).toHaveBeenCalledWith("host-peer", PEER_CONNECT_OPTIONS);

    adapter.dispose();
    reconnectConn.fireOpen();
    await flushPromises();

    expect(reconnectConn.open).toBe(false);
    expect(await reconnectConn.getSentMessages()).toEqual([]);
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
      type: "action_failed",
      message: "Whole-match concession is unavailable for this game",
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

describe("P2P wire-protocol version gate", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const setupFrameAt = (wireProtocolVersion: number) => ({
    type: "game_setup" as const,
    wireProtocolVersion,
    assignedPlayerId: 1,
    playerToken: "seat-token",
    state: remoteState("setup"),
    events: [],
    legalActions: [],
    autoPassRecommended: false,
    manaPaymentShortcutActions: [],
  });

  function makeGuest(
    conn: FakeDataConnection = new FakeDataConnection(),
    peer?: unknown,
    existingPlayerToken?: string,
  ) {
    const adapter = new P2PGuestAdapter(
      { player: { main_deck: [], sideboard: [] } },
      (peer ?? createFakePeer().peer) as unknown as Peer,
      "host-peer",
      conn as unknown as DataConnection,
      existingPlayerToken,
    );
    const emitted = vi.fn();
    adapter.onEvent(emitted);
    return { conn, adapter, emitted };
  }

  const sentOfType = async (conn: FakeDataConnection, type: string) =>
    (await conn.getSentMessages()).find(
      (m) => typeof m === "object" && m !== null && (m as { type: string }).type === type,
    ) as { type: string; reason?: string; wireProtocolVersion?: number } | undefined;

  // Relocated here from `protocol.test.ts`, where this pair guarded
  // `validateMessage`. The version rule now lives solely in
  // `P2PGuestAdapter.handleHostMessage`, so an instrument pointed at the
  // validator would guard nothing; this one points at the adapter's DECISION.
  // The frame also traverses the real validator on the way in — this file's
  // decode stub ends in `real.validateMessage` — so a regression that put the
  // check back into the transport would surface here as the refusing half
  // never emitting.
  //
  // Both halves stamp LITERALS. A frame built from WIRE_PROTOCOL_VERSION
  // cannot tell a bumped client from an unbumped one, which is why every
  // other handshake fixture in the suite is useless as an instrument for a
  // bump. Revert 48 → 47 and BOTH halves red: the v47 frame stops being
  // refused, and the v48 frame stops being admitted. The admitting half is
  // the reach-guard — without it "refuses v46" is also satisfied by a client
  // that refuses everything.
  it("refuses the previous wire protocol (v47) and admits its own (v48)", async () => {
    const refusing = makeGuest();
    await refusing.adapter.initialize();
    await refusing.conn.simulateData(setupFrameAt(47));

    await expect(refusing.adapter.initializeGame()).rejects.toMatchObject({
      code: "P2P_REJECTED",
      message: expect.stringContaining("Wire protocol mismatch"),
    });
    expect(refusing.emitted).toHaveBeenCalledWith(
      expect.objectContaining({ type: "reconnectFailed" }),
    );

    const admitting = makeGuest();
    await admitting.adapter.initialize();
    await admitting.conn.simulateData(setupFrameAt(48));

    await expect(admitting.adapter.initializeGame()).resolves.toBeDefined();
    expect(admitting.emitted).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "reconnectFailed" }),
    );
  });

  it("rejects a malformed present authority before bootstrap adoption", async () => {
    const guest = makeGuest();
    await guest.adapter.initialize();
    await guest.conn.simulateData({
      ...setupFrameAt(WIRE_PROTOCOL_VERSION),
      authority: { sessionKey: "session", hostIncarnation: "host", extra: true },
    } as never);

    await expect(guest.adapter.initializeGame()).rejects.toMatchObject({
      code: "P2P_REJECTED",
      message: "Host sent a malformed P2P authority",
    });
    expect(guest.emitted).toHaveBeenCalledWith(expect.objectContaining({
      type: "reconnectFailed",
      reason: "Host sent a malformed P2P authority",
    }));
  });

  // Derived, not a literal: this gate tests "unequal", not any particular
  // version, so it must not become a second bump tripwire. The literal-stamped
  // instrument is the guest-side pair above.
  const SKEWED_GUEST_VERSION = WIRE_PROTOCOL_VERSION + 1;

  it("refuses a guest whose first contact stamps a different wire version", async () => {
    const { adapter, emitConnection } = makeHost(2);
    await adapter.initialize();

    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
      wireProtocolVersion: SKEWED_GUEST_VERSION,
    });

    const rejected = await sentOfType(guest, "reconnect_rejected");
    expect(rejected).toBeDefined();
    // Mirrors the guest-side wording so both peers read the same sentence.
    expect(rejected!.reason).toBe(
      `Wire protocol mismatch: guest sent v${SKEWED_GUEST_VERSION}, this host speaks v${WIRE_PROTOCOL_VERSION}. Refresh both windows.`,
    );
    expect(guest.open).toBe(false);
    expect(await sentOfType(guest, "game_setup")).toBeUndefined();
    adapter.dispose();
  });

  it("refuses a guest that omits the mandatory wire version", async () => {
    const { adapter, emitConnection } = makeHost(2);
    await adapter.initialize();

    const guest = new FakeOpenableConnection();
    emitConnection(guest as unknown as DataConnection);
    guest.fireOpen();
    await guest.simulateData({
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });

    expect(await sentOfType(guest, "reconnect_rejected")).toEqual(expect.objectContaining({
      reason: expect.stringContaining("Wire protocol version required"),
    }));
    expect(guest.open).toBe(false);
    expect(await sentOfType(guest, "game_setup")).toBeUndefined();
    adapter.dispose();
  });

  // The two first-contact stamp sites, asserted on the OUTBOUND frame. Neither
  // was covered before: deleting either stamp left the whole suite green,
  // because a guest that fails to stamp is rejected before the host allocates
  // a seat. A
  // refactor dropping the `guest_deck` stamp would turn Unit 2 into a no-op
  // for every fresh join — the common path — with nothing red.
  it("stamps the wire version on both first-contact guest messages", async () => {
    const fresh = makeGuest();
    await fresh.adapter.initialize();
    // `initialize` does not await its own send: `peer.ts` queues the encode,
    // and `getSentMessages` drains decode-side work only. Flush the queue
    // before reading, or the frame has not reached `conn.send` yet.
    await vi.advanceTimersByTimeAsync(0);
    const guestDeck = await sentOfType(fresh.conn, "guest_deck");
    expect(guestDeck).toBeDefined();
    expect(guestDeck!.wireProtocolVersion).toBe(WIRE_PROTOCOL_VERSION);

    const resuming = makeGuest(new FakeDataConnection(), undefined, "prior-token");
    await resuming.adapter.initialize();
    await vi.advanceTimersByTimeAsync(0);
    const reconnect = await sentOfType(resuming.conn, "reconnect");
    expect(reconnect).toBeDefined();
    expect(reconnect!.wireProtocolVersion).toBe(WIRE_PROTOCOL_VERSION);
  });

  // The third stamp site: a guest that drops and comes back through
  // `attemptReconnect` must still carry the version, or every reconnection
  // after a transient blip would look like an old bundle to the host.
  it("stamps the wire version on a reconnect after a transient drop", async () => {
    const initialConn = new FakeDataConnection();
    const rejoinConn = new FakeOpenableConnection();
    const peer = {
      on() {},
      off() {},
      destroy() {},
      connect: () => rejoinConn,
    };
    const { adapter, conn } = makeGuest(initialConn, peer);
    await adapter.initialize();
    await conn.simulateData(setupFrameAt(WIRE_PROTOCOL_VERSION));
    await adapter.initializeGame();

    conn.simulateClose();
    // Drain the first backoff step, then complete the WebRTC open handshake
    // the reconnect path awaits.
    await vi.advanceTimersByTimeAsync(1_000);
    rejoinConn.fireOpen();
    await vi.advanceTimersByTimeAsync(0);

    const reconnect = await sentOfType(rejoinConn, "reconnect");
    expect(reconnect).toBeDefined();
    expect(reconnect!.wireProtocolVersion).toBe(WIRE_PROTOCOL_VERSION);
    adapter.dispose();
  });
});

/**
 * #7924: the host's own screen must not depend on the guest fan-out.
 *
 * `broadcastStateUpdate` closes on `commitTerminalIfComplete`, fed by a host
 * snapshot read; on the guest-message paths that fan-out runs inside a `try`.
 * While the host's `stateChanged` came after it, that close rejecting froze
 * the host on a board its own engine had already advanced, and the acting
 * guest was told an applied action had failed. (`broadcastStateUpdateInner`'s
 * own per-seat viewer reads stopped being a rejection source when the
 * delivery contract isolated them, and so is the terminal close's own
 * per-recipient read. The eventual-delivery describe below owns that half.)
 *
 * Not the dead-channel case: `trySend` resolves `false` rather than rejecting
 * (`network/peer.ts:69-106`), so a broken link degrades the fan-out silently.
 *
 * Measured, so the claim stays honest: moving the emission back behind the
 * fan-out reds three of the four tests here — the ordering pair and the
 * unclosable-game test. The fourth pins a different guard, the swallowing
 * `try` inside `publishHostSnapshot`, and reds on its own probe: remove that
 * `try` and only that test falls.
 */
describe("P2PHostAdapter — host emission precedes the guest fan-out", () => {
  /**
   * Returns a reach guard: `consumed()` is true only if the scripted rejection
   * actually ran. Without it every assertion below is also the picture of a
   * completely healthy run, so an injection that silently stops being reached
   * would leave the test green and measuring nothing.
   *
   * Rejects the SECOND host snapshot read. The injection is POSITIONAL, and
   * the ordering under test decides which call that is: with the emission
   * first it is the terminal close's read, and with the emission moved back
   * behind the fan-out it becomes `publishHostSnapshot`'s own. That is exactly
   * why the pair discriminates.
   */
  function failTerminalCloseRead(): { consumed: () => boolean } {
    let reached = false;
    const m = mockGetSnapshot as unknown as {
      getMockImplementation: () => (() => Promise<unknown>) | undefined;
      mockImplementationOnce: (implementation: () => Promise<unknown>) => void;
    };
    const original = m.getMockImplementation()!;
    m.mockImplementationOnce(original);
    m.mockImplementationOnce(async () => {
      reached = true;
      throw new Error("terminal close read failed");
    });
    return { consumed: () => reached };
  }

  /** Same guard for the host's own snapshot read. */
  function failNextHostSnapshotRead(): { consumed: () => boolean } {
    let reached = false;
    (mockGetSnapshot as unknown as {
      mockImplementationOnce: (implementation: () => Promise<unknown>) => void;
    }).mockImplementationOnce(async () => {
      reached = true;
      throw new Error("host snapshot read failed");
    });
    return { consumed: () => reached };
  }

  it("still updates the host and does not reject the guest's applied action", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    const events: Array<{ type: string }> = [];
    adapter.onEvent((e) => events.push(e));
    mockSubmitAction.mockClear();
    const injection = failTerminalCloseRead();

    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();

    // The scripted failure actually happened,
    expect(injection.consumed()).toBe(true);
    // the engine applied the action,
    expect(mockSubmitAction).toHaveBeenCalledWith({ type: "PassPriority" }, 1);
    // so the host's own screen must have advanced despite the fan-out,
    expect(events.some((e) => e.type === "stateChanged")).toBe(true);
    // and the guest must not hear that its applied action failed.
    const sent = await guest.getSentMessages();
    expect(sent.some((m) => {
      const type = (m as { type?: string }).type;
      return type === "action_rejected" || type === "action_failed";
    })).toBe(false);
    adapter.dispose();
  });

  it("still updates the host and does not reject the guest's applied interaction", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    const events: Array<{ type: string }> = [];
    adapter.onEvent((e) => events.push(e));
    mockSubmitInteraction.mockClear();
    const injection = failTerminalCloseRead();

    await guest.simulateData({
      type: "interaction",
      senderPlayerId: 1,
      submission: {},
    });
    await flushPromises();

    expect(injection.consumed()).toBe(true);
    expect(mockSubmitInteraction).toHaveBeenCalled();
    expect(events.some((e) => e.type === "stateChanged")).toBe(true);
    const sent = await guest.getSentMessages();
    expect(sent.some((m) => {
      const type = (m as { type?: string }).type;
      return type === "action_rejected" || type === "action_failed";
    })).toBe(false);
    adapter.dispose();
  });

  // The ordering must not starve the other side either: the host's own read
  // runs first, so a rejection there would stop the fan-out for an action the
  // engine has already applied — and the guest watchdog cannot recover a state
  // its adapter cache never held.
  it("still serves the guests when the host's own snapshot read fails", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();

    mockSubmitAction.mockClear();
    const before = (await guest.getSentMessages()).length;
    // `publishHostSnapshot` is the first `getSnapshot` caller after this point;
    // the fan-out's own per-guest reads use `getViewerSnapshot` and still work.
    const injection = failNextHostSnapshotRead();

    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();

    expect(injection.consumed()).toBe(true);
    expect(mockSubmitAction).toHaveBeenCalledWith({ type: "PassPriority" }, 1);
    const sent = (await guest.getSentMessages()).slice(before);
    expect(sent.some((m) => (m as { type?: string }).type === "state_update")).toBe(true);
    expect(sent.some((m) => {
      const type = (m as { type?: string }).type;
      return type === "action_rejected" || type === "action_failed";
    })).toBe(false);
    adapter.dispose();
  });

  // The terminal close reads the HOST's own snapshot, and that read is not part
  // of the per-seat isolation — it decides whether the game closes at all. The
  // `try` this PR puts around the guest-path delivery would otherwise swallow
  // it, and nothing would ever retry: `terminalResult` stays null, so the sweep
  // has no statement to hand out, and a finished game produces no further
  // action to drive another close. Silent and final — so it has to surface,
  // while the guest's already-applied action still must not be rejected.
  it("reports an unclosable game when the terminal close cannot read the final state", async () => {
    const { adapter, emitConnection } = makeHost(2);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();

    const before = (await guest.getSentMessages()).length;
    (mockGetState as unknown as { mockResolvedValue: (v: unknown) => void }).mockResolvedValue({
      players: [],
      objects: {},
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    });
    const injection = failTerminalCloseRead();

    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();

    expect(injection.consumed()).toBe(true);
    const typesOf = (list: unknown[]) => list.map((e) => (e as { type?: string }).type);
    expect(typesOf(events).filter((t) => t === "terminalUnavailable")).toHaveLength(1);
    // Not closed behind the host's back, and no seat was handed a statement.
    expect(typesOf(events).filter((t) => t === "terminalResult")).toHaveLength(0);
    const sent = (await guest.getSentMessages()).slice(before);
    expect(typesOf(sent).filter((t) => t === "terminal_result")).toHaveLength(0);
    // The PR's own contract still holds: the applied action is not rejected.
    expect(typesOf(sent).some((t) => t === "action_rejected" || t === "action_failed")).toBe(false);
    adapter.dispose();
  });
});

/**
 * #7924, delivery contract: an applied action must reach every guest
 * EVENTUALLY, not just when the immediate fan-out succeeds.
 *
 * The immediate fan-out isolates a failed per-guest viewer read, but that
 * alone still strands the seat: its adapter cache never receives the applied
 * state, and the guest watchdog compares the screen against exactly that
 * cache. Two host-side structures plus the 5s redelivery sweep close the gap:
 * `guestAckedRevisions` (how far along each seat is) and `terminalDelivered`
 * (whose statement reached the channel). `shouldRedeliver` reads both, and
 * BOTH lag checks — the sweep's nomination and `redeliverGuestState`'s own
 * re-check — go through it.
 *
 * `guestAckedRevisions` uses two different signals on purpose, because the two
 * failure directions are not symmetric. It ADVANCES only on the seat's own
 * `state_ack`: a `send` resolving true only means the bytes reached the
 * channel (peerjs parks anything past its buffered-amount budget in a buffer
 * `close()` discards), so a ledger that advanced on transmission marked a seat
 * delivered while the host was still waiting on that seat to act, and the
 * sweep skipped the very seat that deadlocked the match. It is CREATED on an
 * accepted handshake send — `game_setup` or `reconnect_ack`, never
 * `state_update` — because a seat with no entry is one the sweep can never
 * nominate. The asymmetry is one-sided: a false negative disarms the sweep for
 * good, while a false positive is merely no worse than the handshake never
 * having been sent — a parked handshake never authenticates the guest, so the
 * redelivered `state_update`s it would receive are discarded anyway.
 *
 * A consequence worth stating, because it changes what "duplicate frame"
 * means here: a seat nominated ONLY by the terminal clause still runs the full
 * `redeliverGuestState`, so it gets a state frame at its CURRENT revision
 * before the terminal frame. That duplicate is expected and harmless — the
 * guest's stale guard is a strict `<`, so an equal-revision frame passes
 * through, settles `pendingResolve` and produces a fresh ack.
 *
 * Measured, so the claim stays honest. Each probe below was applied to
 * `p2p-adapter.ts` alone and the whole frontend suite re-run; the counts are
 * that run's, not an inference:
 *
 *   P1  no-op `queueLaggingRedeliveries`      → 10 of the 13 tests here red.
 *       The three left green are the two setup tests and the
 *       reconnect-stops-nominating one, all NEGATIVE claims about the sweep.
 *   P2  drop the rethrow after the setup loop → both setup tests red.
 *   P3  drop `shouldRedeliver`'s create-guard → "does not adopt a seat that
 *       never took its game_setup" reds, and only it. That test's seat never
 *       gets a `game_setup` send at all (its viewer read throws), so no
 *       accepted send seeds it and the guard stays the only thing standing
 *       between it and the terminal clause.
 *   P4  leave `redeliverGuestState`'s own re-check on the lag-only inline
 *       form                                  → the two terminal-clause tests
 *       red ("heals a seat whose terminal-close viewer read rejected" and
 *       "re-sends a terminal statement whose bytes never reached the
 *       channel"): the seat is nominated and then returns before sending.
 *   P5  restore TRANSMISSION semantics on the state fan-out (advance the
 *       entry when `send` resolves true)      → 4 red: "resyncs a seat whose
 *       channel took the frame but never applied it", "ignores a guest ack
 *       claiming a revision the host never reached", and both new tests
 *       below, each of which needs a seat that a true send does NOT make
 *       current. This probe IS the shipped bug.
 *   P6  drop the reconnect path's `terminalDelivered` write → "stops
 *       nominating a seat that took its terminal result on reconnect" reds.
 *   P7  drop `seedGuestEntry` from the setup fan-out → "heals a seat whose
 *       handshake ack never reached the host" reds, and only it: with no
 *       entry the create-guard disarms the sweep, and the seat cannot ack its
 *       way back in because the frame that would let it never arrives.
 *   P8  drop `recordGuestAck`'s `Number.isInteger` gate → "ignores a guest ack
 *       whose revision is not a number" reds, and only it.
 *
 * Every test in this block is red under at least one probe.
 */
describe("P2PHostAdapter — per-guest eventual state delivery", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // Re-pin the hoisted defaults: earlier tests in this file replace both
    // wholesale via `mockResolvedValue` (a terminal state without
    // `filteredFor`, and a GameOver `getState`), and neither `mockClear` nor
    // `vi.clearAllMocks` restores a replaced implementation. The per-seat
    // `filteredFor` assertions below need the per-viewer default, and a leaked
    // GameOver `getState` would close the game before these tests act.
    (mockGetViewerSnapshot as unknown as {
      mockImplementation: (implementation: (pid: number) => Promise<unknown>) => void;
    }).mockImplementation(async (pid: number) => ({
      state: { filteredFor: pid, players: [] },
      actions: [],
      autoPassRecommended: false,
    }));
    (mockGetState as unknown as {
      mockImplementation: (implementation: () => Promise<unknown>) => void;
    }).mockImplementation(async () => ({
      players: [],
      objects: {},
      waiting_for: { type: "Priority", data: { player: 0 } },
    }));
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  /** Reach guard, same shape as `failNextHostSnapshotRead` above:
   * `consumed()` is true only if the scripted rejection actually ran. */
  function failNextViewerSnapshot(): { consumed: () => boolean } {
    let reached = false;
    (mockGetViewerSnapshot as unknown as {
      mockImplementationOnce: (implementation: () => Promise<unknown>) => void;
    }).mockImplementationOnce(async () => {
      reached = true;
      throw new Error("viewer snapshot failed");
    });
    return { consumed: () => reached };
  }

  /** Rejects the SECOND per-recipient viewer read: the first belongs to the
   * state fan-out in `broadcastStateUpdateInner`, the second to the terminal
   * close in `commitTerminalIfComplete` — the last per-seat read the fan-out
   * isolation did not cover. */
  function failTerminalViewerRead(): { consumed: () => boolean } {
    let reached = false;
    const m = mockGetViewerSnapshot as unknown as {
      getMockImplementation: () => ((pid: number) => Promise<unknown>) | undefined;
      mockImplementationOnce: (implementation: (pid: number) => Promise<unknown>) => void;
    };
    m.mockImplementationOnce(m.getMockImplementation()!);
    m.mockImplementationOnce(async () => {
      reached = true;
      throw new Error("terminal viewer read failed");
    });
    return { consumed: () => reached };
  }

  /** Advance to the next sweep and then WAIT FOR ITS OUTPUT, instead of
   * draining a fixed number of microtask turns.
   *
   * The delivery chain needs more await turns than a fixed five-microtask
   * drain (queue, handoff, viewer read, send queue, encode, decode), and for a
   * closed game the terminal statement additionally crosses a REAL macrotask:
   * the `crypto.subtle.digest` inside `p2pFinalStateCommitment`, which no
   * microtask drain can await. (Not the gzip encode — this file stubs
   * `encodeWireMessage`, see the `vi.mock` at the top.) So a fixed drain after
   * the timer is a race — green on an idle machine, red under CI load, which
   * is how the terminal test below failed on the maintainer's merge head.
   *
   * Measured, not assumed: chaining 50 extra digests into
   * `p2pFinalStateCommitment` reproduces that CI failure verbatim against a
   * fixed drain (`expected … to have a length of 2 but got 1`); the same delay
   * in the viewer-snapshot mock reds the other tests of this block, which are
   * green in CI today but rest on the same fixed drain. With this helper they
   * stay green under either delay and under both at once. */
  async function sweepAndWaitFor(check: () => void | Promise<void>): Promise<void> {
    await vi.advanceTimersByTimeAsync(5_000);
    // `waitFor` polls on REAL timers and gives up after `timeout` of real time,
    // while each poll advances the FAKE clock by `interval`. 2 s of real budget
    // is sized for a loaded CI runner; the fake clock then reaches at most
    // 5 s + 2 s, still short of the next 5 s sweep tick at 10 s, so polling can
    // never fire a second sweep behind the assertion's back.
    await vi.waitFor(check, { interval: 10, timeout: 2_000 });
  }

  function statesSentTo(messages: unknown[]): Array<{ revision?: number; state?: { filteredFor?: number } }> {
    return messages.filter((m) => (m as { type?: string }).type === "state_update") as
      Array<{ revision?: number; state?: { filteredFor?: number } }>;
  }

  it("redelivers the applied action's state after the guest's viewer read rejected", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();

    mockSubmitAction.mockClear();
    const before = (await guest.getSentMessages()).length;
    const injection = failNextViewerSnapshot();

    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();

    // The scripted failure ran, the engine applied the action,
    expect(injection.consumed()).toBe(true);
    expect(mockSubmitAction).toHaveBeenCalledWith({ type: "PassPriority" }, 1);
    // and the immediate fan-out could not serve this seat — no state frame,
    // but no failure report either (the action HAS applied).
    const immediate = (await guest.getSentMessages()).slice(before);
    expect(statesSentTo(immediate)).toHaveLength(0);
    expect(immediate.some((m) => {
      const type = (m as { type?: string }).type;
      return type === "action_rejected" || type === "action_failed";
    })).toBe(false);

    // One sweep later the seat holds its own filtered authoritative state.
    await sweepAndWaitFor(async () => {
      expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(1);
    });
    const redelivered = statesSentTo((await guest.getSentMessages()).slice(before));
    expect(redelivered[0].state?.filteredFor).toBe(1);

    // The fake ACKED the redelivery, which is what advances the seat's entry
    // to the authoritative revision: further sweeps stay quiet. (The send
    // resolving true would not have been enough — this assertion depends on
    // the harness's auto-ack, not on the redelivery alone.)
    await vi.advanceTimersByTimeAsync(5_000);
    // A negative ("nothing more went out") cannot be polled for, so cross the
    // real event loop the way this file already does elsewhere.
    await vi.advanceTimersByTimeAsync(0);
    expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(1);
    adapter.dispose();
  });

  it("redelivers the applied interaction's state after the guest's viewer read rejected", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();

    mockSubmitInteraction.mockClear();
    const before = (await guest.getSentMessages()).length;
    const injection = failNextViewerSnapshot();

    await guest.simulateData({
      type: "interaction",
      senderPlayerId: 1,
      submission: {},
    });
    await flushPromises();

    expect(injection.consumed()).toBe(true);
    expect(mockSubmitInteraction).toHaveBeenCalled();
    const immediate = (await guest.getSentMessages()).slice(before);
    expect(statesSentTo(immediate)).toHaveLength(0);
    expect(immediate.some((m) => {
      const type = (m as { type?: string }).type;
      return type === "action_rejected" || type === "action_failed";
    })).toBe(false);

    await sweepAndWaitFor(async () => {
      expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(1);
    });
    const redelivered = statesSentTo((await guest.getSentMessages()).slice(before));
    expect(redelivered[0].state?.filteredFor).toBe(1);
    adapter.dispose();
  });

  // The isolation half of the contract: seat 1's failed viewer read must not
  // abort seat 2's IMMEDIATE delivery (the pre-fix fan-out awaited the reads
  // serially, so one rejection starved every later seat). Seat 2's single
  // frame also pins that a broadcast the seat ACKS leaves it current — without
  // that, the sweep would resend to a seat that missed nothing.
  it("serves the healthy seat immediately while the failed seat waits for the sweep", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const guestOne = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    const guestTwo = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();

    const beforeOne = (await guestOne.getSentMessages()).length;
    const beforeTwo = (await guestTwo.getSentMessages()).length;
    // Seat 1 joined first, so the fan-out reads its view first.
    const injection = failNextViewerSnapshot();

    await guestOne.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();

    expect(injection.consumed()).toBe(true);
    const immediateTwo = statesSentTo((await guestTwo.getSentMessages()).slice(beforeTwo));
    expect(immediateTwo).toHaveLength(1);
    expect(immediateTwo[0].state?.filteredFor).toBe(2);
    expect(statesSentTo((await guestOne.getSentMessages()).slice(beforeOne))).toHaveLength(0);

    // The sweep heals seat 1 and leaves the already-served seat 2 alone.
    await sweepAndWaitFor(async () => {
      expect(statesSentTo((await guestOne.getSentMessages()).slice(beforeOne))).toHaveLength(1);
    });
    const healedOne = statesSentTo((await guestOne.getSentMessages()).slice(beforeOne));
    expect(healedOne[0].state?.filteredFor).toBe(1);
    await vi.advanceTimersByTimeAsync(0);
    expect(statesSentTo((await guestTwo.getSentMessages()).slice(beforeTwo))).toHaveLength(1);
    adapter.dispose();
  });

  // The same isolation for the SETUP fan-out, which the sweep cannot repair.
  // Unisolated, one rejected read starved every LATER seat of its `game_setup`
  // — permanently: with no `game_setup` send there is nothing to seed the
  // seat's entry, so the sweep skips it by design, and a second start returns
  // early because `gameStarted` is already true.
  //
  // The isolation buys those later seats their frame. It must NOT also buy
  // silence: the seat whose own read failed waits on a promise that never
  // settles (the guest resolves `initializeGame` only on
  // `game_setup`/`reconnect_ack`), so the host stays the only party that can
  // learn of it — hence the rethrow after the loop. This pins both halves.
  it("still sends game_setup to the later seat, and still fails loudly", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const guestOne = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    const guestTwo = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    // Seat 1 joined first, so the setup fan-out reads its view first.
    const injection = failNextViewerSnapshot();

    await expect(adapter.initializeGame()).rejects.toThrow("viewer snapshot failed");
    await flushPromises();

    expect(injection.consumed()).toBe(true);
    const setupsFor = async (guest: typeof guestOne): Promise<unknown[]> =>
      (await guest.getSentMessages()).filter((m) => (m as { type?: string }).type === "game_setup");
    expect(await setupsFor(guestTwo)).toHaveLength(1);
    expect(await setupsFor(guestOne)).toHaveLength(0);

    // The starved seat stays with the setup/reconnect path on purpose: with no
    // `game_setup` send there is no accepted handshake to seed its entry, and
    // a `state_update` cannot stand in for the handshake — a guest discards
    // state frames it cannot authenticate. So the sweep must NOT adopt it.
    await vi.advanceTimersByTimeAsync(5_000);
    await vi.advanceTimersByTimeAsync(0);
    expect(statesSentTo(await guestOne.getSentMessages())).toHaveLength(0);
    adapter.dispose();
  });

  // The create-guard: `shouldRedeliver` returns false for a seat with no
  // `guestAckedRevisions` entry. Such a seat was never handed a handshake
  // frame its channel accepted — here its viewer read throws, so no
  // `game_setup` is ever sent and `seedGuestEntry` never runs — and the
  // contract leaves it to the setup/reconnect path, because a `state_update`
  // cannot stand in for the handshake (the guest discards state frames it
  // cannot authenticate).
  //
  // The guard now gates BOTH clauses, and the terminal one is why it must be
  // explicit: `undefined < N` is already false, so the lag clause was
  // accidentally safe, but `terminalResult !== null && !terminalDelivered.has(pid)`
  // is TRUE for exactly this seat once the game closes — the close ran, and
  // this seat's terminal send never did. Dropping `acked === undefined` from
  // `shouldRedeliver` is this test's probe.
  it("does not adopt a seat that never took its game_setup", async () => {
    const { adapter, emitConnection } = makeHost(3, 5_000);
    await adapter.initialize();
    const guestOne = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    const guestTwo = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    // Seat 1's setup frame is never sent, so nothing seeds its entry. The host
    // is told (see the test above); here only that consequence matters.
    const setupInjection = failNextViewerSnapshot();
    await expect(adapter.initializeGame()).rejects.toThrow("viewer snapshot failed");
    await flushPromises();
    expect(setupInjection.consumed()).toBe(true);

    const gameOverView = (pid: number) => ({
      state: { filteredFor: pid, players: [], waiting_for: { type: "GameOver", data: { winner: 0 } } },
      actions: [],
      autoPassRecommended: false,
    });
    (mockGetState as unknown as { mockResolvedValue: (v: unknown) => void }).mockResolvedValue({
      players: [],
      objects: {},
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    });
    // Seat 1's viewer stays broken for the closing broadcast's two reads (the
    // state fan-out and the terminal close) and is healthy again afterwards.
    // That last part is what makes this discriminate: with an entry the sweep
    // WOULD get a snapshot and send. Without one it never asks.
    let failuresLeft = 2;
    (mockGetViewerSnapshot as unknown as {
      mockImplementation: (impl: (pid: number) => Promise<unknown>) => void;
    }).mockImplementation(async (pid: number) => {
      if (pid === 1 && failuresLeft > 0) {
        failuresLeft -= 1;
        throw new Error("seat 1 viewer read failed");
      }
      return gameOverView(pid);
    });

    await guestTwo.simulateData({
      type: "action",
      senderPlayerId: 2,
      action: { type: "PassPriority" },
    });
    await flushPromises();
    expect(failuresLeft).toBe(0);

    await vi.advanceTimersByTimeAsync(5_000);
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(5_000);
    await vi.advanceTimersByTimeAsync(0);
    // Never adopted, across two sweeps, although its viewer would answer now.
    expect(statesSentTo(await guestOne.getSentMessages())).toHaveLength(0);
    adapter.dispose();
  });

  // The game-ending broadcast is the one delivery a seat cannot afford to
  // miss twice: the terminal fan-out is one-shot, and the guest refuses a
  // `terminal_result` whose revision it never received. The sweep therefore
  // re-commits the terminal statement AFTER the healing state frame. The two
  // frames settle independently: the seat's ack clears the lag clause, and
  // only an accepted terminal send clears `terminalDelivered`.
  it("re-commits the terminal result for the seat that missed the game-ending broadcast", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();

    const before = (await guest.getSentMessages()).length;
    const gameOver = {
      players: [],
      objects: {},
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    };
    // Both host snapshot reads (publish + terminal close) see the final
    // board, and so does every viewer read after the scripted rejection.
    (mockGetState as unknown as { mockResolvedValue: (v: unknown) => void }).mockResolvedValue(gameOver);
    (mockGetViewerSnapshot as unknown as {
      mockImplementation: (implementation: (pid: number) => Promise<unknown>) => void;
    }).mockImplementation(async (pid: number) => ({
      state: { filteredFor: pid, players: [], waiting_for: { type: "GameOver", data: { winner: 0 } } },
      actions: [],
      autoPassRecommended: false,
    }));
    const injection = failNextViewerSnapshot();

    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();

    expect(injection.consumed()).toBe(true);
    // The one-shot terminal fan already fired, at a revision this seat never
    // cached — unusable for that guest — and no state frame went out.
    const immediate = (await guest.getSentMessages()).slice(before);
    expect(immediate.filter((m) => (m as { type?: string }).type === "terminal_result")).toHaveLength(1);
    expect(statesSentTo(immediate)).toHaveLength(0);

    await sweepAndWaitFor(async () => {
      const sent = (await guest.getSentMessages()).slice(before);
      expect(sent.filter((m) => (m as { type?: string }).type === "terminal_result")).toHaveLength(2);
    });
    const afterSweep = (await guest.getSentMessages()).slice(before);
    const states = statesSentTo(afterSweep);
    expect(states).toHaveLength(1);
    const terminals = afterSweep.filter((m) => (m as { type?: string }).type === "terminal_result") as
      Array<{ result?: { recipient?: number; revision?: number; finalStateCommitment?: string } }>;
    expect(terminals).toHaveLength(2);
    expect(terminals[1].result?.recipient).toBe(1);
    expect(terminals[1].result?.revision).toBe(states[0].revision);
    // The binding that decides acceptance on the guest: the fresh statement
    // must commit to exactly the state the healing frame delivered.
    expect(terminals[1].result?.finalStateCommitment).toBe(
      await p2pFinalStateCommitment(states[0].state as unknown as GameState),
    );
    // The healing state frame precedes the fresh terminal statement (FIFO).
    expect(afterSweep.indexOf(states[0] as never)).toBeLessThan(afterSweep.indexOf(terminals[1] as never));
    adapter.dispose();
  });

  // The terminal close reads a viewer snapshot PER RECIPIENT, and that read was
  // the last per-seat read outside any `try`. One rejection there rejected the
  // whole `Promise.all`, which cost three different things at once:
  //   1. the seat never got its `terminal_result`,
  //   2. no sweep ever nominated it — the state fan-out had already recorded
  //      the seat at this revision, so the ledger called it current,
  //   3. the host's own `terminalResult` emit never ran, which costs the prompt
  //      overlay cleanup, the resumable save and the closing reason line. (The
  //      board itself already reads GameOver from the published snapshot, so
  //      the host does reach its result screen.)
  // A retry is also impossible: `commitTerminalIfComplete` returns early once
  // `this.terminalResult` is set. This pins all three victims.
  it("heals a seat whose terminal-close viewer read rejected, and still closes the host", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    const events: unknown[] = [];
    adapter.onEvent((event) => events.push(event));
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();

    const before = (await guest.getSentMessages()).length;
    (mockGetState as unknown as { mockResolvedValue: (v: unknown) => void }).mockResolvedValue({
      players: [],
      objects: {},
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    });
    (mockGetViewerSnapshot as unknown as {
      mockImplementation: (implementation: (pid: number) => Promise<unknown>) => void;
    }).mockImplementation(async (pid: number) => ({
      state: { filteredFor: pid, players: [], waiting_for: { type: "GameOver", data: { winner: 0 } } },
      actions: [],
      autoPassRecommended: false,
    }));
    // Scripted AFTER the game-over implementation, so the pass-through arm
    // replays that one and not the pre-game default.
    const injection = failTerminalViewerRead();

    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();

    expect(injection.consumed()).toBe(true);
    // Victim 3: the host closes regardless of one seat's failed read.
    expect(events.filter((e) => (e as { type?: string }).type === "terminalResult")).toHaveLength(1);
    // Victims 1 and 2: the seat holds the final board but no statement yet.
    const immediate = (await guest.getSentMessages()).slice(before);
    expect(statesSentTo(immediate)).toHaveLength(1);
    expect(immediate.filter((m) => (m as { type?: string }).type === "terminal_result")).toHaveLength(0);

    await sweepAndWaitFor(async () => {
      const sent = (await guest.getSentMessages()).slice(before);
      expect(sent.filter((m) => (m as { type?: string }).type === "terminal_result")).toHaveLength(1);
    });
    const afterSweep = (await guest.getSentMessages()).slice(before);
    const states = statesSentTo(afterSweep);
    const healing = states[states.length - 1];
    const terminals = afterSweep.filter((m) => (m as { type?: string }).type === "terminal_result") as
      Array<{ result?: { recipient?: number; revision?: number; finalStateCommitment?: string } }>;
    expect(terminals[0].result?.recipient).toBe(1);
    expect(terminals[0].result?.revision).toBe(healing.revision);
    // Same binding the guest checks: the statement commits to exactly the state
    // the healing frame delivered.
    expect(terminals[0].result?.finalStateCommitment).toBe(
      await p2pFinalStateCommitment(healing.state as unknown as GameState),
    );
    expect(afterSweep.indexOf(healing as never)).toBeLessThan(afterSweep.indexOf(terminals[0] as never));
    adapter.dispose();
  });

  /** Keepalive `ping`s ride the same channel and land in `sent` whenever the
   * fake clock crosses 5 s. They are transport, not delivery, so an
   * exact-zero assertion about what a seat received must exclude them. */
  function deliveryFramesIn(messages: unknown[]): unknown[] {
    return messages.filter((m) => {
      const type = (m as { type?: string }).type;
      return type !== "ping" && type !== "pong";
    });
  }

  // THE BUG. `send` resolving true only means the bytes reached the channel;
  // peerjs parks anything past its buffered-amount budget in a buffer that
  // `close()` discards. A ledger recording transmission therefore called this
  // seat current while the host was still waiting on it to act — the sweep
  // skipped it, the host could not advance its revision, and the match
  // deadlocked. Acceptance semantics make the sweep the way out.
  //
  // `stopAcking()` models exactly that seat: every send succeeds, nothing is
  // ever applied.
  it("resyncs a seat whose channel took the frame but never applied it", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();

    // The setup frame WAS applied, so the seat has an entry and the
    // create-guard is not what this test measures.
    expect(guest.acksSent).not.toHaveLength(0);
    guest.stopAcking();
    const before = (await guest.getSentMessages()).length;

    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();

    // Non-vacuity: the fan-out succeeded. Every viewer read resolved and the
    // channel took the bytes — a transmission ledger would record this seat as
    // current here, and never send it anything again.
    expect(mockSubmitAction).toHaveBeenCalledWith({ type: "PassPriority" }, 1);
    expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(1);

    // Acceptance semantics: no ack, so the seat is still behind and the sweep
    // resends the current authoritative state.
    await sweepAndWaitFor(async () => {
      expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(2);
    });
    const resent = statesSentTo((await guest.getSentMessages()).slice(before));
    expect(resent[1].state?.filteredFor).toBe(1);
    adapter.dispose();
  });

  // `revision` crosses the trust boundary: `validateMessage` checks type
  // membership only, and nothing on the decode path validates a field. An
  // unclamped `1e9` makes the lag clause false forever, which strands the seat
  // permanently — this bug's own symptom, reachable from one frame.
  it("ignores a guest ack claiming a revision the host never reached", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();

    guest.stopAcking();
    await guest.simulateData({ type: "state_ack", revision: 1e9 });
    const before = (await guest.getSentMessages()).length;

    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();
    expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(1);

    // Clamped to the host's own revision, so the seat is still behind.
    await sweepAndWaitFor(async () => {
      expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(2);
    });
    adapter.dispose();
  });

  // Terminal delivery gets its own bit rather than a revision, and this is
  // why. Here the seat is FULLY current on state — its ack equals the
  // authoritative revision — and must still be nominated, because its
  // `terminal_result` never reached the channel. No revision ledger can
  // express that, which is what the old ledger's `revision - 1` back-dating was
  // simulating; and once the redelivered state frame is acked (the late ack),
  // the simulation would be undone before the terminal frame ever went out.
  it("re-sends a terminal statement whose bytes never reached the channel", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();

    const before = (await guest.getSentMessages()).length;
    (mockGetState as unknown as { mockResolvedValue: (v: unknown) => void }).mockResolvedValue({
      players: [],
      objects: {},
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    });
    (mockGetViewerSnapshot as unknown as {
      mockImplementation: (implementation: (pid: number) => Promise<unknown>) => void;
    }).mockImplementation(async (pid: number) => ({
      state: { filteredFor: pid, players: [], waiting_for: { type: "GameOver", data: { winner: 0 } } },
      actions: [],
      autoPassRecommended: false,
    }));
    // The closing broadcast's state frame lands and is acked; the terminal
    // frame that follows it finds a channel that cannot take bytes.
    guest.refuseSendsAfter("state_update");

    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();

    const immediate = (await guest.getSentMessages()).slice(before);
    expect(statesSentTo(immediate)).toHaveLength(1);
    expect(immediate.filter((m) => (m as { type?: string }).type === "terminal_result")).toHaveLength(0);
    // The seat is provably current on state, so ONLY the terminal clause can
    // nominate it below.
    expect(guest.acksSent[guest.acksSent.length - 1])
      .toMatchObject({ revision: statesSentTo(immediate)[0].revision });

    guest.reopen();
    await sweepAndWaitFor(async () => {
      const sent = (await guest.getSentMessages()).slice(before);
      expect(sent.filter((m) => (m as { type?: string }).type === "terminal_result")).toHaveLength(1);
    });
    const afterSweep = (await guest.getSentMessages()).slice(before);
    const states = statesSentTo(afterSweep);
    const terminals = afterSweep.filter((m) => (m as { type?: string }).type === "terminal_result") as
      Array<{ result?: { recipient?: number; revision?: number } }>;
    expect(states).toHaveLength(2);
    expect(terminals[0].result?.recipient).toBe(1);
    expect(terminals[0].result?.revision).toBe(states[1].revision);
    expect(afterSweep.indexOf(states[1] as never)).toBeLessThan(afterSweep.indexOf(terminals[0] as never));

    // And the flag terminates it: the delivered statement stops the sweep,
    // even though `terminalResult` stays armed for the rest of the session.
    await vi.advanceTimersByTimeAsync(5_000);
    await vi.advanceTimersByTimeAsync(0);
    expect(deliveryFramesIn((await guest.getSentMessages()).slice(before)))
      .toHaveLength(deliveryFramesIn(afterSweep).length);
    adapter.dispose();
  });

  // `terminalResult` is never cleared for an adapter incarnation, so the
  // terminal clause stays armed forever after a close. A seat that takes its
  // statement on the reconnect path must therefore be recorded there too —
  // otherwise it is nominated every 5 s for the life of the session, each tick
  // costing a `reconnectHandoff` viewer-snapshot round trip plus a duplicate
  // pair of frames.
  //
  // The FIRST tick is the measurement. Two ticks would not discriminate: the
  // redelivery's own terminal write self-terminates the loop after one tick,
  // so a missing reconnect-path write shows up as one duplicate pair and then
  // silence.
  it("stops nominating a seat that took its terminal result on reconnect", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();

    const setup = (await guest.getSentMessages()).find(
      (m): m is { type: "game_setup"; playerToken: string } =>
        (m as { type?: string }).type === "game_setup",
    );
    const token = setup!.playerToken;

    (mockGetState as unknown as { mockResolvedValue: (v: unknown) => void }).mockResolvedValue({
      players: [],
      objects: {},
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    });
    (mockGetViewerSnapshot as unknown as {
      mockImplementation: (implementation: (pid: number) => Promise<unknown>) => void;
    }).mockImplementation(async (pid: number) => ({
      state: { filteredFor: pid, players: [], waiting_for: { type: "GameOver", data: { winner: 0 } } },
      actions: [],
      autoPassRecommended: false,
    }));
    // The one-shot terminal fan-out cannot reach this seat, so the reconnect
    // path is the ONLY place it can be recorded as having taken its statement.
    // (Disconnecting the seat BEFORE the close is not an option: a
    // disconnected seat pauses the host and `submitAction` refuses.)
    guest.refuseSendsAfter("state_update");
    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();
    const closed = await guest.getSentMessages();
    expect(closed.filter((m) => (m as { type?: string }).type === "terminal_result")).toHaveLength(0);

    // Now the seat drops and comes back. The game is already terminal, so the
    // seat is held without a grace timer.
    guest.simulateClose();
    const rejoin = await joinGuest(emitConnection, { type: "reconnect", playerToken: token });
    // `handleFirstContact` fires the reconnect completion and forgets it, and
    // the statement it commits crosses a real `crypto.subtle.digest`, so no
    // microtask drain can await it. Poll, the way `sweepAndWaitFor` does — and
    // well short of the 5 s sweep tick this test then fires deliberately.
    await vi.waitFor(async () => {
      const sent = await rejoin.getSentMessages();
      expect(sent.filter((m) => (m as { type?: string }).type === "terminal_result")).toHaveLength(1);
    }, { interval: 10, timeout: 2_000 });
    const handshake = await rejoin.getSentMessages();
    expect(handshake.filter((m) => (m as { type?: string }).type === "reconnect_ack")).toHaveLength(1);
    // The seat is current on state because it acked the CLOSING `state_update`
    // before it dropped, so its entry already equals `handoff.revision`. (Not
    // because the reconnect ack was recorded: the host's real message handler
    // is installed only after the `reconnect_ack` send, the seed, and the
    // terminal send, and `peer.ts` buffers nothing while the drain handler is
    // attached — so that ack races, and this test does not depend on it.)
    // The lag clause is therefore provably false before the tick — exact zero
    // assertion, not "no growth".
    const before = handshake.length;

    await vi.advanceTimersByTimeAsync(5_000);
    await vi.advanceTimersByTimeAsync(0);
    expect(deliveryFramesIn((await rejoin.getSentMessages()).slice(before))).toHaveLength(0);
    adapter.dispose();
  });

  // Why the ENTRY is created on transmission even though it ADVANCES on
  // acceptance. The guest's ack is fire-and-forget — `P2PGuestAdapter.send`
  // returns `void` and discards `trySend`'s boolean — so a handshake ack can
  // be lost with nothing anywhere retrying it. Were the ack also the create,
  // that single loss would disarm the sweep permanently: the host holds no
  // entry, `shouldRedeliver` returns false at the create-guard, and the seat
  // cannot produce another ack because every remaining ack site needs a frame
  // the sweep will now never send. That is the reported deadlock verbatim — a
  // guest stuck on "Waiting for another player to decide their opening hand"
  // while the host waits on that same seat.
  //
  // `seedGuestEntry` closes it: the accepted `game_setup` send creates the
  // entry at revision 1, so one revision later the lag clause is true and the
  // sweep serves the seat the frame it needs to rejoin the conversation.
  it("heals a seat whose handshake ack never reached the host", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    // Every ack from this seat is lost, the handshake's included.
    guest.stopAcking();
    await adapter.initializeGame();
    await flushPromises();

    // Non-vacuity, and the line that separates this from the create-guard
    // test: that seat's viewer read throws so no `game_setup` is ever sent,
    // while this one WAS handed its frame and the channel took it.
    const setups = (await guest.getSentMessages()).filter(
      (m) => (m as { type?: string }).type === "game_setup",
    );
    expect(setups).toHaveLength(1);
    expect(guest.acksSent).toHaveLength(0);

    const before = (await guest.getSentMessages()).length;
    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();
    expect(mockSubmitAction).toHaveBeenCalledWith({ type: "PassPriority" }, 1);
    expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(1);

    // Acks work again, but nothing on the guest side can restart the exchange:
    // the host publishes no new revision while it waits on this seat, so the
    // sweep is the only way out.
    guest.resumeAcking();
    await sweepAndWaitFor(async () => {
      expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(2);
    });
    const resent = statesSentTo((await guest.getSentMessages()).slice(before));
    expect(resent[1].state?.filteredFor).toBe(1);

    // Healed, not merely retried: the resend was acked, so the next sweep is
    // quiet rather than resending forever.
    await vi.advanceTimersByTimeAsync(5_000);
    await vi.advanceTimersByTimeAsync(0);
    expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(2);
    adapter.dispose();
  });

  // `Number.isInteger`, which the `1e9` test above cannot reach: `1e9` IS an
  // integer, so that test probes only the clamp. `Math.min` applies ToNumber,
  // so a non-numeric revision yields `NaN`, and a stored `NaN` is the mirror
  // image of an unclamped `1e9` — neither `undefined` (the create-guard lets
  // it through) nor `< authoritativeRevision` (the lag clause is false
  // forever). It is also self-sealing: `capped > NaN` is false for every
  // later honest ack, and `seedGuestEntry` sees an entry already present, so
  // nothing can ever overwrite it.
  //
  // Sent BEFORE the handshake on purpose. That is when the seat has no entry,
  // which is the one branch that would store the `NaN`.
  it("ignores a guest ack whose revision is not a number", async () => {
    const { adapter, emitConnection } = makeHost(2, 5_000);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    // `validateMessage` checks type membership only — no frame's fields are
    // validated anywhere on the decode path — so this arrives verbatim.
    await guest.simulateData({ type: "state_ack", revision: "not-a-number" });

    await adapter.initializeGame();
    await flushPromises();
    guest.stopAcking();
    const before = (await guest.getSentMessages()).length;

    await guest.simulateData({
      type: "action",
      senderPlayerId: 1,
      action: { type: "PassPriority" },
    });
    await flushPromises();
    expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(1);

    // Unpoisoned: the seat still reads as behind, so the sweep serves it.
    await sweepAndWaitFor(async () => {
      expect(statesSentTo((await guest.getSentMessages()).slice(before))).toHaveLength(2);
    });
    adapter.dispose();
  });
});

/**
 * Rows 6, 7 (P2P half), 8 (guest leg) and 12 (guest leg).
 *
 * Wire-format note, disclosed: this file stubs `encodeWireMessage` /
 * `decodeWireMessage` (see the `vi.mock` at the top), but the stub's decode
 * ends in the REAL `validateMessage`, so an unknown tag still throws and the
 * real `peer.ts` catch still runs. The real gzip wire format is exercised by
 * `client/src/network/__tests__/protocol.test.ts`.
 */
describe("P2P interaction preview", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  /**
   * An allocation whose segments are UNEQUAL and whose `choiceId` order is NOT
   * the candidate publication order, so a sort or a canonicalisation anywhere
   * on the P2P path is caught rather than coinciding with the input.
   */
  const request = (requestId: string) => ({
    requestId,
    interactionId: "interaction-1",
    response: {
      type: "shortcut",
      data: {
        decision: { type: "fixed", data: { iterations: 6 } },
        pins: [{
          group: 0,
          choiceIds: ["choice-c", "choice-a", "choice-b"],
          amounts: [
            { choiceId: "choice-c", amount: 3 },
            { choiceId: "choice-a", amount: 1 },
            { choiceId: "choice-b", amount: 2 },
          ],
        }],
      },
    },
  });

  const previewAnswer = (requestId: string) => ({
    requestId,
    interactionId: "interaction-1",
    status: { type: "confirmable" },
    progress: { selected: 3, minimum: 1, maximum: 3, aggregate: 6, confirmable: true },
    outcome: "advanced",
    summaries: ["confirmAvailable", "progress"],
  });

  async function authenticatedGuest(conn: FakeDataConnection) {
    const { peer } = createFakePeer();
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
      state: remoteState("live"),
      events: [],
      legalActions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
    });
    return adapter;
  }

  // Row 8, guest leg: the guest sends the authored request VERBATIM.
  it("sends the guest's authored request verbatim", async () => {
    const conn = new FakeDataConnection();
    const adapter = await authenticatedGuest(conn);

    const pending = adapter.previewInteraction(request("req-1") as never, 1);
    await flushPromises();

    const sent = (await conn.getSentMessages()).find(
      (message) => (message as { type?: string }).type === "preview_interaction",
    ) as { type: string; request: ReturnType<typeof request> } | undefined;
    expect(sent).toBeDefined();
    expect(sent!.request).toEqual(request("req-1"));
    const pin = sent!.request.response.data.pins[0];
    // Reach guard: the asserted allocation has more than one segment.
    expect(pin.amounts.length).toBeGreaterThan(1);
    expect(pin.amounts.map((a) => a.choiceId)).toEqual(pin.choiceIds);

    await conn.simulateData({
      type: "interaction_preview",
      requestId: "req-1",
      answer: { type: "preview", preview: previewAnswer("req-1") },
    });
    await expect(pending).resolves.toMatchObject({ requestId: "req-1" });
  });

  // Row 6: two in flight, answered OUT OF ORDER; each promise gets its own
  // answer, and an id that was never sent settles nothing.
  it("correlates out-of-order answers and drops an id it never sent", async () => {
    const conn = new FakeDataConnection();
    const adapter = await authenticatedGuest(conn);

    const first = adapter.previewInteraction(request("req-1") as never, 1);
    const second = adapter.previewInteraction(request("req-2") as never, 1);
    const settledFirst = vi.fn();
    const settledSecond = vi.fn();
    void first.then(settledFirst, settledFirst);
    void second.then(settledSecond, settledSecond);
    await flushPromises();

    await conn.simulateData({
      type: "interaction_preview",
      requestId: "req-never-sent",
      answer: { type: "preview", preview: previewAnswer("req-never-sent") },
    });
    await flushPromises();
    expect(settledFirst).not.toHaveBeenCalled();
    expect(settledSecond).not.toHaveBeenCalled();

    await conn.simulateData({
      type: "interaction_preview",
      requestId: "req-2",
      answer: { type: "preview", preview: previewAnswer("req-2") },
    });
    await expect(second).resolves.toMatchObject({ requestId: "req-2" });
    await flushPromises();
    expect(settledFirst).not.toHaveBeenCalled();

    await conn.simulateData({
      type: "interaction_preview",
      requestId: "req-1",
      answer: { type: "preview", preview: previewAnswer("req-1") },
    });
    await expect(first).resolves.toMatchObject({ requestId: "req-1" });
  });

  it("rejects a correlated host failure without disturbing the other entry", async () => {
    const conn = new FakeDataConnection();
    const adapter = await authenticatedGuest(conn);

    const failing = adapter.previewInteraction(request("req-1") as never, 1);
    const surviving = adapter.previewInteraction(request("req-2") as never, 1);
    const settledSurviving = vi.fn();
    void surviving.then(settledSurviving, settledSurviving);
    await flushPromises();

    await conn.simulateData({
      type: "interaction_preview",
      requestId: "req-1",
      answer: { type: "failed", message: "Game paused-manual" },
    });
    await expect(failing).rejects.toMatchObject({
      code: "P2P_ERROR",
      message: "Game paused-manual",
    });
    await flushPromises();
    expect(settledSurviving).not.toHaveBeenCalled();

    await conn.simulateData({
      type: "interaction_preview",
      requestId: "req-2",
      answer: { type: "preview", preview: previewAnswer("req-2") },
    });
    await expect(surviving).resolves.toMatchObject({ requestId: "req-2" });
  });

  // Row 12, guest leg: two in flight, one answered on a live channel, then the
  // channel closes. Delete the `handleHostDisconnect` clearing call and the
  // unanswered promise never settles.
  it("rejects every unanswered preview when the host channel goes away", async () => {
    const conn = new FakeDataConnection();
    const adapter = await authenticatedGuest(conn);

    const answered = adapter.previewInteraction(request("req-1") as never, 1);
    const unanswered = adapter.previewInteraction(request("req-2") as never, 1);
    await flushPromises();

    await conn.simulateData({
      type: "interaction_preview",
      requestId: "req-1",
      answer: { type: "preview", preview: previewAnswer("req-1") },
    });
    await expect(answered).resolves.toMatchObject({ requestId: "req-1" });

    conn.simulateClose();
    // The clearing loop walks the map rather than rejecting indiscriminately.
    await expect(answered).resolves.toMatchObject({ requestId: "req-1" });
    await expect(unanswered).rejects.toMatchObject({
      code: "P2P_ERROR",
      message: "Host disconnected during interaction preview",
    });
    adapter.dispose();
  });

  // Row 7, P2P half, DRIVEN: an undecodable frame is dropped and the session
  // survives it. The control that distinguishes "dropped" from "died" is that
  // a SUBSEQUENT known frame on the same connection is still delivered.
  it("survives an undecodable frame and still delivers the next known one", async () => {
    const conn = new FakeDataConnection();
    const adapter = await authenticatedGuest(conn);

    const pending = adapter.previewInteraction(request("req-1") as never, 1);
    await flushPromises();

    await conn.simulateData({
      type: "interaction_preview_from_the_future",
      requestId: "req-1",
      answer: { type: "preview", preview: previewAnswer("req-1") },
    } as never);
    await flushPromises();

    await conn.simulateData({
      type: "interaction_preview",
      requestId: "req-1",
      answer: { type: "preview", preview: previewAnswer("req-1") },
    });
    await expect(pending).resolves.toMatchObject({ requestId: "req-1" });
    expect(conn.open).toBe(true);
    adapter.dispose();
  });

  // Row 6, host half: the host answers the ASKING guest and no other peer.
  it("answers only the asking guest", async () => {
    const { adapter, emitConnection } = makeHost(3);
    await adapter.initialize();
    const asker = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    const bystander = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();
    mocks.previewInteraction.mockClear();

    await asker.simulateData({ type: "preview_interaction", request: request("req-1") as never });
    await flushPromises();

    // The seat the preview was EVALUATED as, not merely the channel it was
    // answered on: a preview bound to another seat reads that seat's hidden
    // zones. Each seat is read off its own `game_setup` rather than assumed.
    const assignedSeat = async (conn: FakeOpenableConnection) =>
      (
        (await conn.getSentMessages()).find(
          (message) => (message as { type?: string }).type === "game_setup",
        ) as { assignedPlayerId: number }
      ).assignedPlayerId;
    const askerSeat = await assignedSeat(asker);
    expect(askerSeat).not.toBe(await assignedSeat(bystander));
    expect(mocks.previewInteraction).toHaveBeenCalledOnce();
    expect(mocks.previewInteraction).toHaveBeenCalledWith(request("req-1"), askerSeat);

    const answers = (await asker.getSentMessages()).filter(
      (message) => (message as { type?: string }).type === "interaction_preview",
    ) as { requestId: string; answer: { type: string; preview?: { requestId: string } } }[];
    expect(answers).toHaveLength(1);
    expect(answers[0].requestId).toBe("req-1");
    expect(answers[0].answer.type).toBe("preview");
    expect(answers[0].answer.preview!.requestId).toBe("req-1");

    // The negative — and its live-instrument control in the same assertion:
    // the bystander's channel DID carry other host traffic, so an empty
    // preview-frame filter is a measured absence, not a dead capture.
    const bystanderMessages = await bystander.getSentMessages();
    expect(bystanderMessages.length).toBeGreaterThan(0);
    expect(bystanderMessages.filter(
      (message) => (message as { type?: string }).type === "interaction_preview",
    )).toHaveLength(0);
    adapter.dispose();
  });

  // Row 6, host refusals: each is a CORRELATED `failed` frame, never silence.
  // The positive is the same request on a live, running seat, above.
  it("answers a correlated failure for an eliminated seat and a non-running game", async () => {
    const { adapter, emitConnection } = makeHost(2);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();

    const host = adapter as unknown as {
      eliminatedSeats: Set<number>;
      gameRunState: string;
    };

    host.gameRunState = "paused-manual";
    await guest.simulateData({ type: "preview_interaction", request: request("paused") as never });
    await flushPromises();
    host.gameRunState = "running";

    host.eliminatedSeats.add(1);
    await guest.simulateData({ type: "preview_interaction", request: request("gone") as never });
    await flushPromises();
    host.eliminatedSeats.delete(1);

    const answers = (await guest.getSentMessages()).filter(
      (message) => (message as { type?: string }).type === "interaction_preview",
    ) as { requestId: string; answer: { type: string; message?: string } }[];
    expect(answers.map((a) => [a.requestId, a.answer.type, a.answer.message])).toEqual([
      ["paused", "failed", "Game paused-manual"],
      ["gone", "failed", "Player has conceded and can no longer act"],
    ]);

    // Reach guard: the identical request on a live, running seat IS answered
    // with a preview, so the refusals above are the guards and not a host that
    // fails every preview.
    await guest.simulateData({ type: "preview_interaction", request: request("live") as never });
    await flushPromises();
    const live = (await guest.getSentMessages()).filter(
      (message) => (message as { type?: string }).type === "interaction_preview",
    ) as { requestId: string; answer: { type: string } }[];
    expect(live[live.length - 1]).toMatchObject({
      requestId: "live",
      answer: { type: "preview" },
    });
    adapter.dispose();
  });

  // The preview must NOT take the mutating arm's delivery path.
  it("does not broadcast, persist or run the AI loop for a preview", async () => {
    const { adapter, emitConnection } = makeHost(2);
    await adapter.initialize();
    const guest = await joinGuest(emitConnection, {
      type: "guest_deck",
      deckData: { player: { main_deck: [], sideboard: [] } },
    });
    await adapter.initializeGame();
    await flushPromises();
    persistenceMocks.saveP2PHostSession.mockClear();
    mocks.getViewerSnapshot.mockClear();
    const before = (await guest.getSentMessages()).length;

    await guest.simulateData({ type: "preview_interaction", request: request("req-1") as never });
    await flushPromises();

    const after = await guest.getSentMessages();
    // Exactly one new frame: the answer. A `state_update` fan-out or a snapshot
    // publish would add more.
    expect(after.length).toBe(before + 1);
    expect((after[after.length - 1] as { type?: string }).type).toBe("interaction_preview");
    expect(persistenceMocks.saveP2PHostSession).not.toHaveBeenCalled();
    expect(mocks.getViewerSnapshot).not.toHaveBeenCalled();

    // Reach guard: a real submission on the same connection DOES take those
    // paths, so the absences above are the preview arm and not a dead host.
    await guest.simulateData({
      type: "interaction",
      senderPlayerId: 1,
      submission: { interactionId: "interaction-1", response: { type: "choose", data: { choiceId: "a" } } } as never,
    });
    await flushPromises();
    expect(mocks.getViewerSnapshot).toHaveBeenCalled();
    adapter.dispose();
  });
});
