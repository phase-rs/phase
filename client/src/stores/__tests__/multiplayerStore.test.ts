import { act, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const localStorageItems = vi.hoisted(() => {
  const items = new Map<string, string>();
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => items.get(key) ?? null,
      setItem: (key: string, value: string) => {
        items.set(key, value);
      },
      removeItem: (key: string) => {
        items.delete(key);
      },
      clear: () => {
        items.clear();
      },
      key: (index: number) => [...items.keys()][index] ?? null,
      get length() {
        return items.size;
      },
    },
  });
  return items;
});

import type { PlayerSlot } from "../../multiplayer/seatTypes";
import { formatMetadata } from "../../data/formatRegistry";
import {
  FORMAT_DEFAULTS,
  MAX_USER_LOBBY_SOURCES,
  adHocLobbySource,
  compareLobbyGameEntries,
  findLobbyGameByCode,
  hostingLobbySource,
  isServerCompatible,
  lobbySources,
  migrateLegacyLoopDetectionOn,
  migrateOfficialServerAddress,
  migratePersistedMultiplayerState,
  migrateServerAddressToSources,
  normalizeDisabledDirectorySources,
  normalizeRememberedHostConfig,
  normalizeUserLobbySources,
  userLobbySource,
  type AmbientLobbyFrame,
  type HostingSettings,
  type LobbyGameEntry,
  type LobbySource,
  useMultiplayerStore,
} from "../multiplayerStore";
import { SERVER_PRESETS } from "../../services/serverDetection";
import {
  DIRECTORY_VERSION,
  projectDirectoryBody,
  type DirectoryRow,
  type DirectorySource,
} from "../../services/serverDirectory";
import type { LobbyGame } from "../../adapter/types";
import {
  LOBBY_PROTOCOL_VERSION,
  MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL,
  PROTOCOL_VERSION,
  type ServerInfo,
} from "../../adapter/ws-adapter";
import { AdapterError, AdapterErrorCode } from "../../adapter/types";
import { DEFAULT_MULTIPLAYER_SERVER_URL } from "../../config/multiplayerServer";
import {
  clearWsSession,
  loadWsSession,
  saveWsSession,
} from "../../services/multiplayerSession";
import { HandshakeError, openPhaseSocket, withReconnect } from "../../services/openPhaseSocket";

const p2pMocks = vi.hoisted(() => ({
  hostDestroy: vi.fn(),
  initialize: vi.fn(async () => undefined),
  applySeatMutation: vi.fn(async () => undefined),
  startNow: vi.fn(),
  startPregameGame: vi.fn(async () => undefined),
  getPlayerSlots: vi.fn(() => []),
  dispose: vi.fn(),
}));

const brokerMocks = vi.hoisted(() => ({
  /** Lobby-update callbacks the store registered, keyed by the URL of the
   * socket they were attached to. A URL missing from this map has no lobby
   * listener at all — which is how an ad-hoc join-origin channel is
   * distinguished from a browsed source. */
  lobbyUpdaters: new Map<string, (games: unknown[]) => void>(),
  subscribeLobbyOver: vi.fn(),
  lookupJoinTargetOver: vi.fn(),
  resolveGuestOver: vi.fn(),
  openBrokerClient: vi.fn(),
  registerHost: vi.fn(async () => ({
    gameCode: "ABCDE",
    playerToken: "host-token",
  })),
  updateMetadata: vi.fn(),
  unregister: vi.fn(async () => undefined),
  close: vi.fn(),
}));

const socketMocks = vi.hoisted(() => ({
  send: vi.fn(),
  close: vi.fn(),
  currentWs: null as {
    send: ReturnType<typeof vi.fn>;
    close: ReturnType<typeof vi.fn>;
    onmessage: ((event: MessageEvent) => void) | null;
    onerror: (() => void) | null;
    onclose: (() => void) | null;
  } | null,
}));

vi.mock("../../network/connection", () => ({
  hostRoom: vi.fn(async () => ({
    peer: { id: "peer-id", destroy: p2pMocks.hostDestroy },
    destroy: p2pMocks.hostDestroy,
    roomCode: "ABCDE",
    onGuestConnected: vi.fn(),
  })),
}));

vi.mock("../../adapter/p2p-adapter", () => ({
  P2PHostAdapter: vi.fn().mockImplementation(function () {
    return {
      onEvent: vi.fn(),
      initialize: p2pMocks.initialize,
      applySeatMutation: p2pMocks.applySeatMutation,
      startNow: p2pMocks.startNow,
      startPregameGame: p2pMocks.startPregameGame,
      getPlayerSlots: p2pMocks.getPlayerSlots,
      dispose: p2pMocks.dispose,
    };
  }),
}));

vi.mock("../../services/brokerClient", () => ({
  openBrokerClient: brokerMocks.openBrokerClient,
  subscribeLobbyOver: brokerMocks.subscribeLobbyOver,
  lookupJoinTargetOver: brokerMocks.lookupJoinTargetOver,
  resolveGuestOver: brokerMocks.resolveGuestOver,
}));

/**
 * The metrics module is MOCKED here, module-level, and that is the mitigation —
 * not the stubbed socket factories.
 *
 * The store's own wrapper around the factory enqueues on the RESOLVE leg, so an
 * unmocked module would build a real queue against `metricsUrl()` on the REAL
 * official host (`vitest.config.ts` defines
 * `__OFFICIAL_MULTIPLAYER_SERVER_URL__` as the production URL, unlike
 * `__TELEMETRY_URL__`, which is `""`), with `telemetryEnabled` defaulting true —
 * and this suite dials directory sources in a dozen cases. The recording stub
 * lets every row below assert on calls instead, and V-U12d additionally asserts
 * that neither transport was reached.
 */
const metricsMocks = vi.hoisted(() => ({
  reportConnectOutcome: vi.fn(),
  flushMetricsNow: vi.fn(),
  installServerMetricsLifecycle: vi.fn(),
  metricsUrl: vi.fn(() => "https://metrics.test/servers/metrics"),
}));
vi.mock("../../services/serverMetrics", () => metricsMocks);

vi.mock("../../services/openPhaseSocket", () => ({
  // Argument order mirrors the production class: `(kind, message)`.
  HandshakeError: class HandshakeError extends Error {
    kind: string;

    constructor(kind: string, message: string) {
      super(message);
      this.kind = kind;
    }
  },
  openPhaseSocket: vi.fn(async () => ({
    serverInfo: { mode: "Full", protocolVersion: 14 },
    ws: (() => {
      const ws = {
      send: socketMocks.send,
      close: vi.fn(),
      onmessage: null,
      onerror: null,
      onclose: null,
      };
      socketMocks.currentWs = ws;
      return ws;
    })(),
  })),
  withReconnect: vi.fn(),
}));

function hostingSettings(
  overrides: Partial<HostingSettings> = {},
): HostingSettings {
  return {
    displayName: "Host",
    public: true,
    password: "",
    timerSeconds: null,
    formatConfig: FORMAT_DEFAULTS.Commander,
    matchType: "Bo1",
    loopDetection: { type: "Off" },
    aiSeats: [],
    startWhenFull: false,
    ranked: false,
    roomName: "Test room",
    ...overrides,
  };
}

function emitServerMessage(type: string, data?: unknown): void {
  socketMocks.currentWs?.onmessage?.({
    data: JSON.stringify({ type, data }),
  } as MessageEvent);
}

/** A fake `PhaseSocket` that remembers the URL it was opened on, so the
 * `subscribeLobbyOver` mock can key its listeners per source. `serverInfo`
 * overrides let one test give each source a distinguishable identity. */
function fakeSocket(url: string, serverInfo: Partial<ServerInfo> = {}) {
  return {
    url,
    serverInfo: {
      version: "test",
      buildCommit: "test",
      mode: "Full",
      protocolVersion: PROTOCOL_VERSION,
      lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
      ...serverInfo,
    } satisfies ServerInfo,
    ws: {
      readyState: 1,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      send: vi.fn(),
    },
    close: vi.fn(),
  };
}

/**
 * Drive `withReconnect` the way the real one does: notify from an async
 * continuation (so the store has stored the handle before `current()` is
 * read), `"open"` when the factory resolves and `"offline"` when it has
 * exhausted its attempts.
 */
function driveReconnect(): void {
  vi.mocked(withReconnect).mockImplementation((factory, opts) => {
    let current: Awaited<ReturnType<typeof factory>> | null = null;
    void (async () => {
      try {
        current = await factory(0);
        opts?.onStateChange?.("open");
      } catch {
        opts?.onStateChange?.("offline");
      }
    })();
    return { current: () => current, close: vi.fn() };
  });
}

/**
 * `driveReconnect` plus a per-URL handle that drives a flap: `drop()` puts
 * the channel in `"reconnecting"`, `reopen(socket)` brings it back `"open"`
 * on a DIFFERENT socket — which is what a real reconnect does
 * (`withReconnect` assigns the brand-new `PhaseSocket` before it notifies).
 * Keyed off the URL each fake socket remembers.
 */
function driveReconnectWithFlaps(): Map<
  string,
  {
    drop: () => void;
    reopen: (socket: ReturnType<typeof fakeSocket>) => void;
    reopenViaFactory: () => Promise<void>;
  }
> {
  const controls = new Map<
    string,
    {
      drop: () => void;
      reopen: (socket: ReturnType<typeof fakeSocket>) => void;
      reopenViaFactory: () => Promise<void>;
    }
  >();
  vi.mocked(withReconnect).mockImplementation((factory, opts) => {
    let current: Awaited<ReturnType<typeof factory>> | null = null;
    void (async () => {
      try {
        current = await factory(0);
        const { url } = current as unknown as { url: string };
        controls.set(url, {
          drop: () => opts?.onStateChange?.("reconnecting"),
          reopen: (socket) => {
            current = socket as unknown as Awaited<ReturnType<typeof factory>>;
            opts?.onStateChange?.("open");
          },
          // The factory-faithful control, beside `reopen` rather than
          // replacing it: the real `withReconnect` RE-INVOKES the factory on a
          // re-dial, while `reopen` assigns a caller-supplied socket the two
          // flap tests below then push frames at. Anything that asserts on
          // what the factory does across a flap must drive this one.
          //
          // Production passes 1: `attempt` is reset to 0 on each successful
          // open (`client/src/services/openPhaseSocket.ts:400`) and bumped once
          // by `scheduleRetry` (`:422`) before `connect` awaits
          // `factory(attempt)` (`:393`).
          reopenViaFactory: async () => {
            current = await factory(1);
            opts?.onStateChange?.("open");
          },
        });
        opts?.onStateChange?.("open");
      } catch {
        opts?.onStateChange?.("offline");
      }
    })();
    return { current: () => current, close: vi.fn() };
  });
  return controls;
}

/** Push a raw server frame at a fake socket. The store's ambient listener is
 * a real `message` listener registered through `addEventListener`, so this
 * delivers it exactly the way a broker frame would — and only to listeners
 * bound to THIS socket, which is what makes a rebinding claim measurable. */
function emitAmbient(
  socket: ReturnType<typeof fakeSocket>,
  type: string,
  data: unknown,
): void {
  const event = { data: JSON.stringify({ type, data }) } as MessageEvent;
  for (const [, listener] of socket.ws.addEventListener.mock.calls) {
    (listener as (e: MessageEvent) => void)(event);
  }
}

function lobbyGame(overrides: Partial<LobbyGame> & { game_code: string }): LobbyGame {
  return {
    host_name: "Alice",
    created_at: 1_700_000_000,
    has_password: false,
    ...overrides,
  };
}

const PRESET_URL = SERVER_PRESETS[0].url;
/** The URL `beforeEach` puts in `hostingServer`. Passed explicitly to
 * `startHosting` by the cases that predate the host-target parameter, so their
 * assertions keep measuring what they always measured. */
const HOST_URL = "ws://localhost:8787";

describe("multiplayerStore", () => {
  beforeEach(() => {
    useMultiplayerStore.getState().cancelHosting();
    useMultiplayerStore.getState().closeSubscriptionSocket();
    vi.clearAllMocks();
    brokerMocks.lobbyUpdaters.clear();
    brokerMocks.subscribeLobbyOver.mockImplementation(
      (socket: unknown, onUpdate: (games: unknown[]) => void) => {
        const { url } = socket as { url: string };
        brokerMocks.lobbyUpdaters.set(url, onUpdate);
        return () => {
          brokerMocks.lobbyUpdaters.delete(url);
        };
      },
    );
    brokerMocks.openBrokerClient.mockResolvedValue({
      serverInfo: { mode: "LobbyOnly", protocolVersion: 14 },
      registerHost: brokerMocks.registerHost,
      updateMetadata: brokerMocks.updateMetadata,
      unregister: brokerMocks.unregister,
      close: brokerMocks.close,
    });
    // Egress guards — DEFENCE-IN-DEPTH; the real mitigation is the module
    // mock above, which means no queue, timer or body is ever constructed
    // here. These stubs only make that observable, and would catch a future
    // transport reached by some path the mock does not cover.
    vi.stubGlobal("navigator", { sendBeacon: vi.fn(() => true) });
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: false, status: 599 })));
    socketMocks.currentWs = null;
    localStorageItems.clear();
    clearWsSession();
    useMultiplayerStore.setState({
      displayName: "",
      connectionStatus: "disconnected",
      activePlayerId: null,
      opponentDisplayName: null,
      hostingServer: "ws://localhost:8787",
      userLobbySources: [],
      sourceStatus: new Map(),
      // Without these three a directory fixture leaks into every following
      // test in this file.
      directorySources: [],
      directoryFetchedAtMs: null,
      disabledDirectorySources: [],
    });
  });

  it("initializes with a stable UUID playerId", () => {
    const id1 = useMultiplayerStore.getState().playerId;
    expect(id1).toMatch(/^[0-9a-f]{8}-/);
    const id2 = useMultiplayerStore.getState().playerId;
    expect(id2).toBe(id1);
  });

  const server = (
    mode: ServerInfo["mode"],
    protocolVersion: number,
    lobbyProtocolVersion?: number,
  ): ServerInfo => ({
    version: "test",
    buildCommit: "test",
    mode,
    protocolVersion,
    lobbyProtocolVersion,
  });

  // LEGACY PATH: brokers that advertise no lobby version keep the derived
  // one-version window, so already-deployed brokers stay reachable.
  it("keeps LobbyOnly compatibility to the derived one-version rollout window", () => {
    expect(isServerCompatible(server("LobbyOnly", PROTOCOL_VERSION))).toBe(true);
    expect(isServerCompatible(server("LobbyOnly", PROTOCOL_VERSION - 1))).toBe(true);
    expect(isServerCompatible(server("LobbyOnly", PROTOCOL_VERSION - 2))).toBe(false);
    expect(isServerCompatible(server("Full", PROTOCOL_VERSION - 1))).toBe(false);
  });

  it("judges a lobby broker by its lobby version, not its full-game version", () => {
    // The badge must agree with the handshake: a broker whose full-game number
    // is many bumps stale is still fully usable when the lobby surface matches.
    expect(
      isServerCompatible(
        server("LobbyOnly", PROTOCOL_VERSION - 9, LOBBY_PROTOCOL_VERSION),
      ),
    ).toBe(true);
    // No ceiling — a newer broker must not strand this client.
    expect(
      isServerCompatible(
        server("LobbyOnly", PROTOCOL_VERSION, LOBBY_PROTOCOL_VERSION + 5),
      ),
    ).toBe(true);
    // The floor still bites — and "below the floor" is measured from the floor
    // itself, not from this client's own version: an additive lobby bump moves
    // LOBBY_PROTOCOL_VERSION while MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL stays
    // put, so `LOBBY_PROTOCOL_VERSION - 1` can be a perfectly acceptable
    // broker.
    expect(
      isServerCompatible(
        server("LobbyOnly", PROTOCOL_VERSION, MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL - 1),
      ),
    ).toBe(false);
    // Full servers ignore the lobby field entirely.
    expect(
      isServerCompatible(server("Full", PROTOCOL_VERSION - 1, LOBBY_PROTOCOL_VERSION)),
    ).toBe(false);
  });

  // Guards the wiring, not the window: `serverProtocolRejection` can be
  // surface-aware and the lobby still unreachable if the one socket that
  // browses it forgets to say which surface it is on.
  it("opens the shared subscription socket on the lobby surface", async () => {
    const socket = {
      serverInfo: {
        version: "test",
        buildCommit: "test",
        mode: "Full" as const,
        protocolVersion: PROTOCOL_VERSION - 2,
        lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
      },
      ws: { readyState: 1, addEventListener: vi.fn(), removeEventListener: vi.fn(), send: vi.fn() },
      close: vi.fn(),
    };
    vi.mocked(withReconnect).mockImplementationOnce((factory, opts) => {
      let current: Awaited<ReturnType<typeof factory>> | null = null;
      // The real implementation notifies from an async continuation, after it
      // has returned the handle the store stores. Reproduce that ordering —
      // notifying synchronously would find no handle to read `current()` from.
      void (async () => {
        current = await factory(0);
        opts?.onStateChange?.("open");
      })();
      return { current: () => current, close: vi.fn() };
    });
    vi.mocked(openPhaseSocket).mockResolvedValueOnce(
      socket as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );

    const opened = await useMultiplayerStore
      .getState()
      .ensureSubscriptionSocket("ws://localhost:8787");

    expect(opened).toBe(socket);
    expect(openPhaseSocket).toHaveBeenCalledWith(
      "ws://localhost:8787",
      expect.objectContaining({ surface: "lobby" }),
    );
    useMultiplayerStore.getState().closeSubscriptionSocket();
  });

  it("persists displayName across store resets", () => {
    useMultiplayerStore.getState().setDisplayName("TestPlayer");
    expect(useMultiplayerStore.getState().displayName).toBe("TestPlayer");
  });

  it("does not persist connectionStatus or activePlayerId", () => {
    useMultiplayerStore.getState().setConnectionStatus("connected");
    expect(useMultiplayerStore.getState().connectionStatus).toBe("connected");
    useMultiplayerStore.getState().setActivePlayerId(1);
    expect(useMultiplayerStore.getState().activePlayerId).toBe(1);
  });

  it("setActivePlayerId updates activePlayerId", () => {
    useMultiplayerStore.getState().setActivePlayerId(1);
    expect(useMultiplayerStore.getState().activePlayerId).toBe(1);
    useMultiplayerStore.getState().setActivePlayerId(null);
    expect(useMultiplayerStore.getState().activePlayerId).toBeNull();
  });

  it("derives Two-Headed Giant defaults from the registry metadata", () => {
    expect(FORMAT_DEFAULTS.TwoHeadedGiant).toBe(
      formatMetadata("TwoHeadedGiant")?.default_config,
    );
    for (const metadata of Object.values(FORMAT_DEFAULTS)) {
      expect(FORMAT_DEFAULTS[metadata.format]).toBe(
        formatMetadata(metadata.format)?.default_config,
      );
    }
  });

  it("migrates official persisted server addresses to the configured deployment default", () => {
    expect(
      migrateOfficialServerAddress(
        "wss://lobby.phase-rs.dev/ws",
        "wss://selfhost.example/ws",
      ),
    ).toBe("wss://selfhost.example/ws");
    expect(
      migrateOfficialServerAddress(
        "wss://us.phase-rs.dev/ws",
        "wss://selfhost.example/ws",
      ),
    ).toBe("wss://selfhost.example/ws");
  });

  it("does not migrate custom self-hosted server addresses", () => {
    expect(
      migrateOfficialServerAddress(
        "wss://play.example.com/ws",
        "wss://selfhost.example/ws",
      ),
    ).toBe("wss://play.example.com/ws");
  });

  // Every channel's broker is an official host. A returning preview browser
  // holds a persisted PRODUCTION address, and detectServerUrl honours any
  // stored address whose /health answers — production's does — so without this
  // it stays pinned to a lobby its build cannot handshake with.
  it("migrates the other channel's official lobby to this build's default", () => {
    expect(
      migrateOfficialServerAddress(
        "wss://lobby.phase-rs.dev/ws",
        "wss://lobby-preview.phase-rs.dev/ws",
      ),
    ).toBe("wss://lobby-preview.phase-rs.dev/ws");
    expect(
      migrateOfficialServerAddress(
        "wss://lobby-preview.phase-rs.dev/ws",
        "wss://lobby.phase-rs.dev/ws",
      ),
    ).toBe("wss://lobby.phase-rs.dev/ws");
  });

  // Both the v3 remap and the v6 source split run on an old blob, in that
  // order: the address is repointed at this channel's broker first, and the
  // result is an official address, so it yields no user source.
  it("re-runs the official-address migration for v2 stores (v2 -> v3)", () => {
    expect(
      migratePersistedMultiplayerState(
        { serverAddress: "wss://lobby.phase-rs.dev/ws" },
        2,
      ),
    ).toEqual({
      hostingServer: DEFAULT_MULTIPLAYER_SERVER_URL,
      userLobbySources: [],
    });
  });

  it("leaves a user-typed address alone across the v3 migration", () => {
    expect(
      migratePersistedMultiplayerState(
        { serverAddress: "wss://play.example.com/ws" },
        2,
      ),
    ).toEqual({
      hostingServer: "wss://play.example.com/ws",
      userLobbySources: [
        { url: "wss://play.example.com/ws", name: "play.example.com", origin: "user" },
      ],
    });
  });

  it("does not re-run the official-address remap for a v3 store", () => {
    expect(
      migratePersistedMultiplayerState(
        { serverAddress: "wss://lobby.phase-rs.dev/ws" },
        3,
      ),
    ).toEqual({
      hostingServer: "wss://lobby.phase-rs.dev/ws",
      userLobbySources: [],
    });
  });

  // v5 -> v6: the single address splits into "where I host" plus "what I
  // browse". A hand-typed address is both; a built-in one is only the first,
  // because it is already derived as a preset source every session.
  it("migrates a custom v5 serverAddress into hostingServer plus one user source", () => {
    expect(
      migratePersistedMultiplayerState(
        { serverAddress: "wss://play.example.com/ws", displayName: "Tester" },
        5,
      ),
    ).toEqual({
      displayName: "Tester",
      hostingServer: "wss://play.example.com/ws",
      userLobbySources: [
        { url: "wss://play.example.com/ws", name: "play.example.com", origin: "user" },
      ],
    });
  });

  it("migrates an official v5 serverAddress without adding a user source", () => {
    expect(migrateServerAddressToSources("wss://lobby.phase-rs.dev/ws")).toEqual({
      hostingServer: "wss://lobby.phase-rs.dev/ws",
      userLobbySources: [],
    });
    expect(migrateServerAddressToSources(DEFAULT_MULTIPLAYER_SERVER_URL)).toEqual({
      hostingServer: DEFAULT_MULTIPLAYER_SERVER_URL,
      userLobbySources: [],
    });
  });

  it("migrates the None sentinel to a null hosting server", () => {
    expect(migrateServerAddressToSources("")).toEqual({
      hostingServer: null,
      userLobbySources: [],
    });
  });

  it("migrates a malformed stored address to this build's default", () => {
    expect(migrateServerAddressToSources("wss:")).toEqual({
      hostingServer: DEFAULT_MULTIPLAYER_SERVER_URL,
      userLobbySources: [],
    });
    expect(migrateServerAddressToSources(undefined)).toEqual({
      hostingServer: DEFAULT_MULTIPLAYER_SERVER_URL,
      userLobbySources: [],
    });
  });

  it("does not re-migrate a store already at v6", () => {
    const blob = {
      hostingServer: "wss://play.example.com/ws",
      userLobbySources: [
        { url: "wss://play.example.com/ws", name: "play.example.com", origin: "user" },
      ],
    };
    expect(migratePersistedMultiplayerState(blob, 6)).toEqual(blob);
  });

  it("drops malformed persisted user sources on hydration", () => {
    const normalized = normalizeUserLobbySources([
      { url: "wss://keep.example/ws", name: "keep.example", origin: "user" },
      // Not a user entry: presets are rebuilt per session, never hydrated.
      { url: "wss://lobby.phase-rs.dev/ws", name: "lobby.phase-rs.dev", origin: "official" },
      { url: "not a url", name: "junk", origin: "user" },
      { url: "wss://keep.example/ws", name: "dupe", origin: "user" },
      "nonsense",
    ]);

    expect(normalized).toEqual([
      { url: "wss://keep.example/ws", name: "keep.example", origin: "user" },
    ]);
  });

  it("trims a hydrated blob to the same cap the add path enforces", () => {
    const persisted = Array.from({ length: MAX_USER_LOBBY_SOURCES + 1 }, (_, i) => ({
      url: `wss://s${i}.example/ws`,
      name: `s${i}.example`,
      origin: "user",
    }));

    expect(normalizeUserLobbySources(persisted)).toHaveLength(MAX_USER_LOBBY_SOURCES);
    expect(normalizeUserLobbySources("not an array")).toEqual([]);
  });

  it("forwards a legacy 'On' loop-detection choice to Interactive", () => {
    expect(
      migrateLegacyLoopDetectionOn({
        format: "Commander",
        loopDetection: { type: "On" },
      }),
    ).toEqual({ format: "Commander", loopDetection: { type: "Interactive" } });
  });

  it("leaves Off/Interactive loop-detection choices unchanged", () => {
    expect(
      migrateLegacyLoopDetectionOn({ format: "Commander", loopDetection: { type: "Off" } }),
    ).toEqual({ format: "Commander", loopDetection: { type: "Off" } });
    expect(
      migrateLegacyLoopDetectionOn({
        format: "Commander",
        loopDetection: { type: "Interactive" },
      }),
    ).toEqual({ format: "Commander", loopDetection: { type: "Interactive" } });
  });

  it("passes through a null lastHostConfig unchanged", () => {
    expect(migrateLegacyLoopDetectionOn(null)).toBeNull();
  });

  it("rebuilds legacy host configurations from current engine defaults", () => {
    const normalized = normalizeRememberedHostConfig({
      format: "Commander",
      formatConfig: {
        format: "Commander",
        starting_life: 25,
        deck_size: 100,
        commander_damage_threshold: 19,
        allow_debug_actions: true,
        uses_commander: false,
      },
      playerCount: 2,
      matchType: "Bo3",
      loopDetection: { type: "On" },
      isPublic: false,
      startWhenFull: false,
      ranked: true,
      aiSeats: [{ seatIndex: 1, difficulty: "Hard", deckName: "Deck" }],
    });

    expect(normalized).toEqual({
      format: "Commander",
      formatConfig: {
        ...FORMAT_DEFAULTS.Commander,
        starting_life: 25,
        commander_damage_threshold: 19,
        allow_debug_actions: true,
      },
      savedCustomFormatId: null,
      playerCount: 2,
      matchType: "Bo3",
      loopDetection: { type: "Interactive" },
      isPublic: false,
      startWhenFull: false,
      ranked: false,
      aiSeats: [{ seatIndex: 1, difficulty: "Hard", deckName: "Deck" }],
    });
  });

  it("drops unknown persisted format names instead of indexing inherited object keys", () => {
    expect(normalizeRememberedHostConfig({ format: "toString" })).toBeNull();
  });

  // ── Custom-format rehydration ────────────────────────────────────────
  //
  // Before the Custom branch existed, `isKnownFormat` was false for every
  // "Custom:<id>" string and this function returned null for the ENTIRE
  // remembered config — player count, AI seats, privacy, all of it — whenever
  // the player's last hosted game used a custom format. That is silent data
  // loss, and these tests are what fail if the branch is reverted.

  /** The `FormatConfig` the engine's resolver derives from `savedRules()`. */
  function customFormatConfigFixture() {
    return {
      format: "Custom:0",
      starting_life: 20,
      min_players: 2,
      max_players: 4,
      deck_size: { type: "Minimum", data: 60 },
      singleton: false,
      command_zone: false,
      commander_damage_threshold: null,
      range_of_influence: null,
      team_based: false,
      uses_commander: false,
      supplies_fixed_deck: false,
      sideboard_policy: { type: "Limited", data: 15 },
      default_deck_copy_limit: { type: "UpTo", data: 4 },
      allow_debug_actions: false,
      custom_rules: {
        id: 0,
        structural: {
          starting_life: 20,
          min_players: 2,
          max_players: 4,
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

  function seedSavedCustomFormat(id: string): void {
    localStorage.setItem(
      "phase-custom-formats",
      JSON.stringify([
        {
          id,
          name: "House Rules",
          savedAt: 1,
          def: {
            rules: customFormatConfigFixture().custom_rules,
            label: "House Rules",
            short_label: "HOU",
            description: "60-card minimum, 2–4 players, 20 life",
            reprint_policy: null,
            printing_fidelity: "NotApplicable",
          },
        },
      ]),
    );
  }

  function persistedCustomHostConfig(overrides: Record<string, unknown> = {}) {
    return {
      format: "Custom:0",
      formatConfig: customFormatConfigFixture(),
      savedCustomFormatId: "saved-1",
      playerCount: 3,
      matchType: "Bo1",
      loopDetection: { type: "Off" },
      isPublic: false,
      startWhenFull: false,
      ranked: false,
      aiSeats: [{ seatIndex: 1, difficulty: "Hard", deckName: null }],
      ...overrides,
    };
  }

  it("rehydrates a remembered custom-format config instead of discarding it", () => {
    seedSavedCustomFormat("saved-1");

    const normalized = normalizeRememberedHostConfig(persistedCustomHostConfig());

    // The assertion that flips if the Custom branch is removed: this was `null`.
    expect(normalized).not.toBeNull();
    expect(normalized?.format).toBe("Custom:0");
    expect(normalized?.savedCustomFormatId).toBe("saved-1");
    expect(normalized?.formatConfig).toEqual(customFormatConfigFixture());
    // ...and the format-independent tail ran, so nothing else was lost either.
    expect(normalized?.playerCount).toBe(3);
    expect(normalized?.isPublic).toBe(false);
    expect(normalized?.aiSeats).toEqual([
      { seatIndex: 1, difficulty: "Hard", deckName: null },
    ]);
  });

  it("clamps a remembered custom-format player count to the format's own seats", () => {
    seedSavedCustomFormat("saved-1");
    // The shared tail must clamp against the CUSTOM config's max_players (4),
    // not a registry default that does not exist for this format.
    expect(
      normalizeRememberedHostConfig(persistedCustomHostConfig({ playerCount: 9 }))?.playerCount,
    ).toBe(4);
  });

  it("drops a remembered custom format whose saved definition was deleted", () => {
    // Nothing seeded: the definition is gone (deleted, or another device).
    expect(normalizeRememberedHostConfig(persistedCustomHostConfig())).toBeNull();
  });

  it("drops a remembered custom format with no saved-definition id to resolve", () => {
    seedSavedCustomFormat("saved-1");
    expect(
      normalizeRememberedHostConfig(persistedCustomHostConfig({ savedCustomFormatId: null })),
    ).toBeNull();
  });

  it("drops a remembered custom format whose stored config fails the shape check", () => {
    seedSavedCustomFormat("saved-1");
    const { deck_size: _dropped, ...missingDeckSize } = customFormatConfigFixture();
    expect(
      normalizeRememberedHostConfig(
        persistedCustomHostConfig({ formatConfig: missingDeckSize }),
      ),
    ).toBeNull();
  });

  it("drops a remembered custom format whose config contradicts its own rules id", () => {
    seedSavedCustomFormat("saved-1");
    // `format: "Custom:7"` with `custom_rules.id: 0` is exactly what the
    // engine's `validate_custom_rules_consistency` rejects; the client mirror
    // must not accept it either.
    expect(
      normalizeRememberedHostConfig(
        persistedCustomHostConfig({
          format: "Custom:7",
          formatConfig: { ...customFormatConfigFixture(), format: "Custom:7" },
        }),
      ),
    ).toBeNull();
  });

  it("migrates v4 persisted settings before a stale format shape reaches hosting", () => {
    expect(
      migratePersistedMultiplayerState(
        {
          lastHostConfig: {
            format: "Commander",
            formatConfig: { deck_size: 100 },
            playerCount: 2,
            matchType: "Bo1",
            loopDetection: { type: "Off" },
            isPublic: true,
            startWhenFull: true,
            ranked: false,
            aiSeats: [],
          },
        },
        4,
      ),
    ).toEqual({
      lastHostConfig: {
        format: "Commander",
        formatConfig: FORMAT_DEFAULTS.Commander,
        savedCustomFormatId: null,
        playerCount: 2,
        matchType: "Bo1",
        loopDetection: { type: "Off" },
        isPublic: true,
        startWhenFull: true,
        ranked: false,
        aiSeats: [],
      },
    });
  });

  it("does not re-migrate a store already at v5", () => {
    const state = { lastHostConfig: { format: "Commander", loopDetection: { type: "On" } } };
    expect(migratePersistedMultiplayerState(state, 5)).toBe(state);
  });

  it("normalizes current-version persisted host settings during hydration", () => {
    localStorage.setItem(
      "phase-multiplayer",
      JSON.stringify({
        state: {
          lastHostConfig: {
            format: "Commander",
            formatConfig: { deck_size: 100 },
            playerCount: 2,
            matchType: "Bo1",
            loopDetection: { type: "Off" },
            isPublic: true,
            startWhenFull: true,
            ranked: false,
            aiSeats: [],
          },
        },
        version: 5,
      }),
    );

    act(() => useMultiplayerStore.persist.rehydrate());

    expect(useMultiplayerStore.getState().lastHostConfig?.formatConfig).toEqual(
      FORMAT_DEFAULTS.Commander,
    );
  });

  it("strips AI seats from team-based server host settings", async () => {
    useMultiplayerStore.getState().startHosting(
      hostingSettings({
        formatConfig: FORMAT_DEFAULTS.TwoHeadedGiant,
        aiSeats: [{ seatIndex: 1, difficulty: "Hard", deckName: null }],
      }),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: [],
      },
      HOST_URL,
    );

    await waitFor(() => expect(socketMocks.send).toHaveBeenCalled());
    const frame = JSON.parse(socketMocks.send.mock.calls[0][0] as string) as {
      data: { ai_seats: unknown[] };
    };
    expect(frame.data.ai_seats).toEqual([]);
  });

  it("passes AI seats through for non-team server host settings", async () => {
    const aiSeats = [{ seatIndex: 1, difficulty: "Hard", deckName: null }];
    useMultiplayerStore.getState().startHosting(
      hostingSettings({ aiSeats }),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: ["Goreclaw, Terror of Qal Sisma"],
      },
      HOST_URL,
    );

    await waitFor(() => expect(socketMocks.send).toHaveBeenCalled());
    const frame = JSON.parse(socketMocks.send.mock.calls[0][0] as string) as {
      data: { ai_seats: unknown[] };
    };
    expect(frame.data.ai_seats).toEqual(aiSeats);
  });

  it("saves server-host metadata with the reconnect token while waiting for players", async () => {
    useMultiplayerStore.getState().startHosting(
      hostingSettings(),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: ["Goreclaw, Terror of Qal Sisma"],
      },
      HOST_URL,
    );

    await waitFor(() => expect(socketMocks.send).toHaveBeenCalled());
    emitServerMessage("GameCreated", {
      game_code: "ABCDE",
      player_token: "host-token",
      full_key: { game_code: "ABCDE", generation: 1 },
    });

    expect(loadWsSession()).toMatchObject({
      gameCode: "ABCDE",
      playerToken: "host-token",
      fullKey: { game_code: "ABCDE", generation: 1 },
      serverUrl: "ws://localhost:8787",
      hostIsPublic: true,
      hostSession: {
        formatConfig: FORMAT_DEFAULTS.Commander,
        timerSeconds: null,
        matchType: "Bo1",
      },
    });
  });

  it("resumes a saved server-host room and receives joined-seat updates", async () => {
    saveWsSession({
      gameCode: "ABCDE",
      playerToken: "host-token",
      fullKey: { game_code: "ABCDE", generation: 1 },
      serverUrl: "ws://localhost:8787",
      timestamp: Date.now(),
      hostIsPublic: true,
      hostSession: {
        formatConfig: FORMAT_DEFAULTS.Commander,
        timerSeconds: null,
        matchType: "Bo1",
      },
    });

    expect(useMultiplayerStore.getState().resumeServerHosting()).toBe(true);

    await waitFor(() => expect(socketMocks.send).toHaveBeenCalled());
    expect(JSON.parse(socketMocks.send.mock.calls[0][0] as string)).toEqual({
      type: "Reconnect",
      data: {
        game_code: "ABCDE",
        player_token: "host-token",
        full_key: { game_code: "ABCDE", generation: 1 },
      },
    });

    const slots: PlayerSlot[] = [
      { playerId: 0, name: "Host", kind: { type: "HostHuman" } },
      { playerId: 1, name: "Guest", kind: { type: "JoinedHuman" } },
    ];
    emitServerMessage("GameCreated", {
      game_code: "ABCDE",
      player_token: "host-token",
      full_key: { game_code: "ABCDE", generation: 1 },
    });
    emitServerMessage("PlayerSlotsUpdate", { slots });

    await waitFor(() => {
      expect(useMultiplayerStore.getState()).toMatchObject({
        hostingStatus: "waiting",
        hostGameCode: "ABCDE",
        hostIsPublic: true,
        hostSession: {
          formatConfig: FORMAT_DEFAULTS.Commander,
          timerSeconds: null,
          matchType: "Bo1",
        },
        playerSlots: slots,
      });
    });
  });

  it("does not resume ordinary in-game websocket sessions as pregame hosts", async () => {
    saveWsSession({
      gameCode: "ABCDE",
      playerToken: "host-token",
      fullKey: { game_code: "ABCDE", generation: 1 },
      serverUrl: "ws://localhost:8787",
      timestamp: Date.now(),
    });

    expect(useMultiplayerStore.getState().resumeServerHosting()).toBe(false);
    expect(socketMocks.send).not.toHaveBeenCalled();
  });

  it("removes pregame host metadata once the server starts the game", async () => {
    useMultiplayerStore.getState().startHosting(
      hostingSettings(),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: ["Goreclaw, Terror of Qal Sisma"],
      },
      HOST_URL,
    );
    await waitFor(() => expect(socketMocks.send).toHaveBeenCalled());
    emitServerMessage("GameCreated", {
      game_code: "ABCDE",
      player_token: "host-token",
      full_key: { game_code: "ABCDE", generation: 1 },
    });

    emitServerMessage("GameStarted", {});

    expect(loadWsSession()).toMatchObject({
      gameCode: "ABCDE",
      playerToken: "host-token",
      fullKey: { game_code: "ABCDE", generation: 1 },
      serverUrl: "ws://localhost:8787",
    });
    expect(loadWsSession()?.hostSession).toBeUndefined();
  });

  // ── U15: the chosen host server ─────────────────────────────────────────

  // V-U15d
  it("dials the server startHosting was given, not the browsing anchor", async () => {
    const URL_A = "ws://anchor.example:8787";
    const URL_B = "ws://chosen.example:8787";
    useMultiplayerStore.setState({ hostingServer: URL_A });

    useMultiplayerStore.getState().startHosting(
      hostingSettings(),
      { main_deck: ["Forest"], sideboard: [], commander: [] },
      URL_B,
    );

    // Positive FIRST: without it the negative below would be satisfied by a
    // `startHosting` that opened nothing at all.
    await waitFor(() => expect(openPhaseSocket).toHaveBeenCalledWith(URL_B));
    expect(openPhaseSocket).not.toHaveBeenCalledWith(URL_A);
  });

  // V-U15e
  it("records the chosen server in the session and resumes on it", async () => {
    const URL_A = "ws://anchor.example:8787";
    const URL_B = "ws://chosen.example:8787";
    useMultiplayerStore.setState({ hostingServer: URL_A });

    useMultiplayerStore.getState().startHosting(
      hostingSettings(),
      { main_deck: ["Forest"], sideboard: [], commander: [] },
      URL_B,
    );
    await waitFor(() => expect(socketMocks.send).toHaveBeenCalled());
    // `full_key.game_code === game_code` is a PRECONDITION, not decoration:
    // without it `savePregameHostSession` early-returns and no session is
    // written for the resume to find.
    emitServerMessage("GameCreated", {
      game_code: "ABCDE",
      player_token: "host-token",
      full_key: { game_code: "ABCDE", generation: 1 },
    });

    // The two authorities disagree by construction: the anchor says A, the
    // session says B.
    const written = loadWsSession()!;
    expect(written.serverUrl).toBe(URL_B);

    // Put the store back in the state a fresh page load starts from. The
    // session is re-saved because `cancelHosting` clears storage, and it is the
    // session THIS run's production path wrote — not a hand-built one.
    useMultiplayerStore.getState().cancelHosting();
    saveWsSession(written);
    vi.mocked(openPhaseSocket).mockClear();
    expect(useMultiplayerStore.getState().resumeServerHosting()).toBe(true);
    await waitFor(() => expect(openPhaseSocket).toHaveBeenCalledWith(URL_B));
    expect(openPhaseSocket).not.toHaveBeenCalledWith(URL_A);

    // Paired negative: with the session cleared there is nothing to resume.
    useMultiplayerStore.getState().cancelHosting();
    clearWsSession();
    expect(useMultiplayerStore.getState().resumeServerHosting()).toBe(false);
  });

  // V-U15h — the LIVE mid-game reconnect, the third `openServerHostSocket`
  // call site. The only case in this file that needs fake timers, so they are
  // scoped to this block: installing them suite-wide would perturb two
  // thousand lines of real-timer tests.
  it("re-dials the latched server after a mid-game host-socket drop", async () => {
    const URL_A = "ws://anchor.example:8787";
    const URL_B = "ws://chosen.example:8787";
    const URL_C = "ws://moved.example:8787";
    vi.useFakeTimers();
    try {
      useMultiplayerStore.setState({ hostingServer: URL_A });
      useMultiplayerStore.getState().startHosting(
        hostingSettings(),
        { main_deck: ["Forest"], sideboard: [], commander: [] },
        URL_B,
      );

      // A microtask drain, NOT `waitFor`: RTL's fake-timer detector keys on a
      // `jest` global that vitest does not define, so `waitFor` would arm its
      // own timers on the frozen clock and spin to its 15 s cap. It must come
      // BEFORE the emit — `socket.ws.onmessage` is assigned only after the
      // awaited `openPhaseSocket` resolves.
      await vi.advanceTimersByTimeAsync(0);

      // Reach-guard: the INITIAL open was already B, so the assertion after
      // the drop cannot pass on a reconnect that never fired.
      expect(openPhaseSocket).toHaveBeenCalledWith(URL_B);
      const opensBefore = vi.mocked(openPhaseSocket).mock.calls.length;

      emitServerMessage("GameCreated", {
        game_code: "ABCDE",
        player_token: "host-token",
        full_key: { game_code: "ABCDE", generation: 1 },
      });
      expect(loadWsSession()?.serverUrl).toBe(URL_B);

      // Hostile multi-authority: the anchor MOVES between the open and the
      // re-dial. A reconnect that re-read it would land on C.
      useMultiplayerStore.getState().setHostingServer(URL_C);

      socketMocks.currentWs?.onclose?.();
      await vi.advanceTimersByTimeAsync(1000);

      // The call count rose, so this is not passing on a timer that never ran.
      expect(vi.mocked(openPhaseSocket).mock.calls.length).toBeGreaterThan(opensBefore);
      expect(openPhaseSocket).toHaveBeenLastCalledWith(URL_B);
      expect(openPhaseSocket).not.toHaveBeenCalledWith(URL_C);
    } finally {
      vi.useRealTimers();
    }
  });

  it("applies setup-time AI seats when starting a P2P host session", async () => {
    const ok = await useMultiplayerStore.getState().startP2PHostingSession(
      hostingSettings({
        aiSeats: [
          { seatIndex: 1, difficulty: "Hard", deckName: null },
          { seatIndex: 3, difficulty: "Easy", deckName: "My Deck" },
        ],
      }),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: ["Goreclaw, Terror of Qal Sisma"],
      },
      { useBroker: false },
    );

    expect(ok).toBe(true);
    expect(p2pMocks.applySeatMutation).toHaveBeenNthCalledWith(1, {
      type: "SetKind",
      data: {
        seatIndex: 1,
        kind: {
          type: "Ai",
          data: { difficulty: "Hard", deck: { type: "Random" } },
        },
      },
    });
    expect(p2pMocks.applySeatMutation).toHaveBeenNthCalledWith(2, {
      type: "SetKind",
      data: {
        seatIndex: 3,
        kind: {
          type: "Ai",
          data: { difficulty: "Easy", deck: { type: "Named", data: "My Deck" } },
        },
      },
    });
  });

  it("shows a retryable adapter-initialization failure while creating a P2P lobby", async () => {
    p2pMocks.initialize.mockRejectedValueOnce(
      new AdapterError(
        AdapterErrorCode.NOT_INITIALIZED,
        "Adapter initialization was canceled. Please try again.",
        true,
      ),
    );

    await expect(
      useMultiplayerStore.getState().startP2PHostingSession(
        hostingSettings(),
        {
          main_deck: ["Forest"],
          sideboard: [],
          commander: ["Goreclaw, Terror of Qal Sisma"],
        },
        { useBroker: false },
      ),
    ).resolves.toBe(false);

    expect(useMultiplayerStore.getState().toasts.get("generic")?.message).toBe(
      "Adapter initialization was canceled. Please try again.",
    );
  });

  it("does not apply setup-time AI seats when starting a team-based P2P host session", async () => {
    const ok = await useMultiplayerStore.getState().startP2PHostingSession(
      hostingSettings({
        formatConfig: FORMAT_DEFAULTS.TwoHeadedGiant,
        aiSeats: [{ seatIndex: 1, difficulty: "Hard", deckName: null }],
      }),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: [],
      },
      { useBroker: false },
    );

    expect(ok).toBe(true);
    expect(p2pMocks.applySeatMutation).not.toHaveBeenCalled();
  });

  it.each([false, true])(
    "uses the P2P host visibility setting when listing in the broker: %s",
    async (isPublic) => {
      const ok = await useMultiplayerStore.getState().startP2PHostingSession(
        hostingSettings({ public: isPublic }),
        {
          main_deck: ["Forest"],
          sideboard: [],
          commander: ["Goreclaw, Terror of Qal Sisma"],
        },
        { useBroker: true },
      );

      expect(ok).toBe(true);
      expect(useMultiplayerStore.getState().hostIsPublic).toBe(isPublic);
      expect(brokerMocks.registerHost).toHaveBeenCalledOnce();
      expect(brokerMocks.registerHost).toHaveBeenCalledWith(
        expect.objectContaining({ public: isPublic }),
      );
    },
  );

  it("removes open P2P seats in order before starting with current players", async () => {
    const ok = await useMultiplayerStore.getState().startP2PHostingSession(
      hostingSettings(),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: ["Goreclaw, Terror of Qal Sisma"],
      },
      { useBroker: false },
    );
    expect(ok).toBe(true);

    const slots: PlayerSlot[] = [
      { playerId: 0, name: "Host", kind: { type: "HostHuman" } },
      { playerId: 1, name: "", kind: { type: "WaitingHuman" } },
      { playerId: 2, name: "Guest", kind: { type: "JoinedHuman" } },
      { playerId: 3, name: "", kind: { type: "WaitingHuman" } },
    ];
    useMultiplayerStore.setState({ playerSlots: slots });

    await useMultiplayerStore.getState().startLobbyWithCurrentPlayers();

    expect(p2pMocks.applySeatMutation).toHaveBeenNthCalledWith(1, {
      type: "Remove",
      data: { seatIndex: 3 },
    });
    expect(p2pMocks.applySeatMutation).toHaveBeenNthCalledWith(2, {
      type: "Remove",
      data: { seatIndex: 1 },
    });
    expect(p2pMocks.startNow).toHaveBeenCalledOnce();
    expect(p2pMocks.startPregameGame).toHaveBeenCalledOnce();
  });

  it("transfers a started P2P host to the game route exactly once", async () => {
    useMultiplayerStore.setState({ activePlayerId: 2 });
    const ok = await useMultiplayerStore.getState().startP2PHostingSession(
      hostingSettings(),
      {
        main_deck: ["Forest"],
        sideboard: [],
        commander: ["Goreclaw, Terror of Qal Sisma"],
      },
      { useBroker: false },
    );
    expect(ok).toBe(true);

    await useMultiplayerStore.getState().startLobbyWithCurrentPlayers();
    const route = useMultiplayerStore.getState().pendingGameRoute;
    expect(route).toMatch(/^\/game\/[^?]+\?mode=p2p-host$/);
    expect(useMultiplayerStore.getState().activePlayerId).toBe(0);
    const gameId = route!.slice("/game/".length, -"?mode=p2p-host".length);

    // A different route cannot steal the active host; the correct route can
    // still claim it afterwards.
    expect(useMultiplayerStore.getState().takeActiveP2PHost("different-game")).toBeNull();
    const adapter = useMultiplayerStore.getState().takeActiveP2PHost(gameId);
    expect(adapter).not.toBeNull();
    expect(useMultiplayerStore.getState().takeActiveP2PHost(gameId)).toBeNull();

    // The game route owns the transferred adapter. Lobby cancellation cannot
    // dispose it before the route's own cleanup runs.
    useMultiplayerStore.getState().cancelHosting();
    expect(p2pMocks.dispose).not.toHaveBeenCalled();
  });

  it("does not assign the host seat until the P2P game has started", async () => {
    const ok = await useMultiplayerStore.getState().startP2PHostingSession(
      hostingSettings(),
      { main_deck: ["Forest"], sideboard: [], commander: [] },
      { useBroker: false },
    );
    expect(ok).toBe(true);
    useMultiplayerStore.setState({ activePlayerId: 2 });
    p2pMocks.startPregameGame.mockRejectedValueOnce(new Error("start failed"));

    await expect(useMultiplayerStore.getState().startLobbyWithCurrentPlayers()).rejects.toThrow(
      "start failed",
    );

    expect(useMultiplayerStore.getState().activePlayerId).toBe(2);
    expect(useMultiplayerStore.getState().pendingGameRoute).toBeNull();
  });

  it("reports a server host connection error instead of falling through to P2P", async () => {
    useMultiplayerStore.setState({
      hostingStatus: "waiting",
      hostGameCode: "ABCDE",
    });

    await expect(
      useMultiplayerStore.getState().seatMutateAsync({ type: "Start" }),
    ).rejects.toThrow("Host connection is not active.");
  });

  // ── Multi-source subscription channels ──────────────────────────────────

  it("keeps the healthy source's lobby when another source's handshake fails", async () => {
    const A = "wss://a.example/ws";
    const B = "wss://b.example/ws";
    useMultiplayerStore.setState({
      hostingServer: PRESET_URL,
      userLobbySources: [
        { url: A, name: "a.example", origin: "user" },
        { url: B, name: "b.example", origin: "user" },
      ],
      sourceStatus: new Map(),
      serverInfo: null,
    });
    driveReconnect();
    // Distinguishable identities per authority: A is opened AFTER the hosting
    // preset, so if the store took every source's handshake the last writer
    // would be the community source.
    vi.mocked(openPhaseSocket).mockImplementation(async (url: string) => {
      if (url === B) {
        throw new HandshakeError("ws_error", "connection refused");
      }
      return fakeSocket(url, {
        version: url === PRESET_URL ? "hosting-identity" : "community-identity",
      }) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>;
    });

    const seen: { url: string; codes: string[] }[] = [];
    const detach = await useMultiplayerStore
      .getState()
      .subscribeLobby((games, source) => {
        seen.push({ url: source.url, codes: games.map((g) => g.game_code) });
      });

    // A degraded source must not take the lobby down with it.
    expect(detach).not.toBeNull();
    brokerMocks.lobbyUpdaters.get(A)?.([lobbyGame({ game_code: "AAA11" })]);

    // Reach-guard: the healthy source's snapshot actually arrived, tagged
    // with the source that listed it — the assertion is not vacuous on an
    // empty fan-out.
    expect(seen).toContainEqual({ url: A, codes: ["AAA11"] });
    expect(useMultiplayerStore.getState().sourceStatus.get(B)?.state).toBe("offline");
    expect(useMultiplayerStore.getState().sourceStatus.get(A)?.state).toBe("open");
    // The global `serverInfo` is the HOSTING server's identity — it feeds
    // `HostControlTile`'s join-share URL and `GamePage`'s `useBroker`
    // inference. A browsed community source's handshake must not overwrite
    // it. Non-vacuous: it started `null` and now holds the hosting
    // handshake, so the field was written exactly once, by the right source.
    expect(useMultiplayerStore.getState().serverInfo?.version).toBe("hosting-identity");
    expect(useMultiplayerStore.getState().sourceStatus.get(A)?.serverInfo?.version).toBe(
      "community-identity",
    );
    detach?.();
  });

  it("reports the lobby offline only when every source fails", async () => {
    useMultiplayerStore.setState({
      userLobbySources: [{ url: "wss://a.example/ws", name: "a.example", origin: "user" }],
      sourceStatus: new Map(),
    });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(async () => {
      throw new HandshakeError("ws_error", "connection refused");
    });

    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});

    expect(detach).toBeNull();
  });

  it("only counts user entries against the source cap", () => {
    driveReconnect();
    const store = useMultiplayerStore.getState();
    for (let i = 0; i < MAX_USER_LOBBY_SOURCES; i++) {
      expect(store.addUserLobbySource(`wss://s${i}.example/ws`).ok).toBe(true);
    }
    // Reach-guard above: the cap is reached by user entries alone, whatever
    // `SERVER_PRESETS.length` happens to be on this build.
    expect(useMultiplayerStore.getState().userLobbySources).toHaveLength(
      MAX_USER_LOBBY_SOURCES,
    );
    expect(store.addUserLobbySource("wss://one-too-many.example/ws")).toEqual({
      ok: false,
      reason: "cap_reached",
    });
  });

  it("refuses a preset URL and a malformed URL as user sources", () => {
    const store = useMultiplayerStore.getState();

    expect(store.addUserLobbySource(PRESET_URL)).toEqual({
      ok: false,
      reason: "duplicate",
    });
    expect(store.addUserLobbySource("http://not-a-socket.example")).toEqual({
      ok: false,
      reason: "invalid_url",
    });
    expect(useMultiplayerStore.getState().userLobbySources).toEqual([]);
  });

  it("removes only the named user source and leaves the hosting choice alone", () => {
    const store = useMultiplayerStore.getState();
    store.addUserLobbySource("wss://a.example/ws");
    store.addUserLobbySource("wss://b.example/ws");
    const hosting = useMultiplayerStore.getState().hostingServer;
    // Guards the sibling below from passing for the wrong reason: the
    // fixture's hosting server is not already the fallback value.
    expect(hosting).not.toBe(DEFAULT_MULTIPLAYER_SERVER_URL);

    store.removeUserLobbySource("wss://a.example/ws");

    expect(useMultiplayerStore.getState().userLobbySources.map((s) => s.url)).toEqual([
      "wss://b.example/ws",
    ]);
    expect(useMultiplayerStore.getState().hostingServer).toBe(hosting);
  });

  it("falls back to the official server when the hosting source is removed", () => {
    const store = useMultiplayerStore.getState();
    expect(store.addUserLobbySource("wss://a.example/ws").ok).toBe(true);
    const [added] = useMultiplayerStore.getState().userLobbySources;
    store.setHostingServer(added.url);
    // Reach-guard: hosting really moved onto the hand-added source, so the
    // assertions below measure the removal and not the initial state.
    expect(useMultiplayerStore.getState().hostingServer).toBe(added.url);

    store.removeUserLobbySource(added.url);

    // Hosting on a URL that is no longer browsed hides the player's own game
    // from the merged list with no selection the picker can show or change.
    expect(useMultiplayerStore.getState().hostingServer).toBe(
      DEFAULT_MULTIPLAYER_SERVER_URL,
    );
    expect(useMultiplayerStore.getState().userLobbySources).toEqual([]);
  });

  it("finds a code in a non-hosting source's snapshot with its source", async () => {
    const A = "wss://a.example/ws";
    const B = "wss://b.example/ws";
    useMultiplayerStore.setState({
      hostingServer: A,
      userLobbySources: [
        { url: A, name: "a.example", origin: "user" },
        { url: B, name: "b.example", origin: "user" },
      ],
      sourceStatus: new Map(),
    });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(
      async (url: string) =>
        fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );

    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    brokerMocks.lobbyUpdaters.get(A)?.([lobbyGame({ game_code: "ONA11" })]);
    brokerMocks.lobbyUpdaters.get(B)?.([lobbyGame({ game_code: "ONB22" })]);

    expect(findLobbyGameByCode("onb22")?.source.url).toBe(B);
    expect(findLobbyGameByCode("ona11")?.source.url).toBe(A);
    expect(findLobbyGameByCode("NOPE9")).toBeUndefined();
    detach?.();
  });

  it("scopes a code lookup to one source's snapshot", async () => {
    const A = "wss://a.example/ws";
    const B = "wss://b.example/ws";
    useMultiplayerStore.setState({
      hostingServer: A,
      userLobbySources: [
        { url: A, name: "a.example", origin: "user" },
        { url: B, name: "b.example", origin: "user" },
      ],
      sourceStatus: new Map(),
    });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(
      async (url: string) =>
        fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );

    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    // "DUP11" is listed by BOTH authorities — codes are unique per server,
    // not across the merged list — while "ONB22" is listed only by B.
    brokerMocks.lobbyUpdaters.get(A)?.([
      lobbyGame({ game_code: "DUP11", host_name: "OnA" }),
    ]);
    brokerMocks.lobbyUpdaters.get(B)?.([
      lobbyGame({ game_code: "DUP11", host_name: "OnB" }),
      lobbyGame({ game_code: "ONB22" }),
    ]);

    // Paired unscoped positive on the same fixture: both codes ARE findable
    // unscoped (resolving to A, first in derived order, for the collision),
    // so the scoped misses below are the scope refusing them and not an
    // empty snapshot.
    expect(findLobbyGameByCode("DUP11")?.game.host_name).toBe("OnA");
    expect(findLobbyGameByCode("onb22")?.source.url).toBe(B);

    // Scoped: each source answers only for its own snapshot.
    expect(findLobbyGameByCode("DUP11", B)?.game.host_name).toBe("OnB");
    expect(findLobbyGameByCode("DUP11", B)?.source.url).toBe(B);
    expect(findLobbyGameByCode("DUP11", A)?.game.host_name).toBe("OnA");
    expect(findLobbyGameByCode("onb22", A)).toBeUndefined();
    // A URL that is no source at all resolves to nothing rather than falling
    // back to a global scan.
    expect(findLobbyGameByCode("onb22", "wss://nobody.example/ws")).toBeUndefined();
    detach?.();
  });

  /** Two browsed sources, each dialed through the flap-capable driver, with
   * every socket the store opened kept so a test can push frames at one
   * specific socket — including the pre-reconnect one. */
  function twoFlappableSources() {
    const A = "wss://a.example/ws";
    const B = "wss://b.example/ws";
    useMultiplayerStore.setState({
      hostingServer: A,
      userLobbySources: [
        { url: A, name: "a.example", origin: "user" },
        { url: B, name: "b.example", origin: "user" },
      ],
      sourceStatus: new Map(),
    });
    const controls = driveReconnectWithFlaps();
    const opened = new Map<string, ReturnType<typeof fakeSocket>>();
    vi.mocked(openPhaseSocket).mockImplementation(async (url: string) => {
      const socket = fakeSocket(url);
      opened.set(url, socket);
      return socket as unknown as Awaited<ReturnType<typeof openPhaseSocket>>;
    });
    return { A, B, controls, opened };
  }

  it("rebinds a source's ambient listener to the socket its reconnect produced", async () => {
    const { A, B, controls, opened } = twoFlappableSources();
    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});

    emitAmbient(opened.get(A)!, "PlayerCount", { count: 3 });
    emitAmbient(opened.get(B)!, "PlayerCount", { count: 5 });
    // Reach-guard: each source's pre-flap frame really did land, on its own
    // row, so the assertions below measure the flap and not an empty map.
    expect(useMultiplayerStore.getState().sourceStatus.get(A)?.playerCount).toBe(3);
    expect(useMultiplayerStore.getState().sourceStatus.get(B)?.playerCount).toBe(5);

    controls.get(B)!.drop();

    // Leaving "open" clears the count with the row: the socket that reported
    // it is gone, so nothing live is backing the number any more.
    expect(useMultiplayerStore.getState().sourceStatus.get(B)).toMatchObject({
      state: "reconnecting",
      playerCount: null,
    });

    const reopened = fakeSocket(B);
    controls.get(B)!.reopen(reopened);

    // Back open on a new socket that has reported nothing yet — the old
    // count must not return with the connection.
    expect(useMultiplayerStore.getState().sourceStatus.get(B)).toMatchObject({
      state: "open",
      playerCount: null,
    });

    emitAmbient(reopened, "PlayerCount", { count: 2 });

    // Heard on the POST-reconnect socket: the listener followed the socket
    // instead of staying bound to the dead one (which is the whole defect —
    // a source that flapped once would otherwise be deaf for the session).
    expect(useMultiplayerStore.getState().sourceStatus.get(B)?.playerCount).toBe(2);
    // The source that never flapped is untouched.
    expect(useMultiplayerStore.getState().sourceStatus.get(A)?.playerCount).toBe(3);
    detach?.();
  });

  it("fans a post-reconnect PasswordRequired frame out tagged with its source", async () => {
    const { B, controls, opened } = twoFlappableSources();
    const frames: { frame: AmbientLobbyFrame; url: string }[] = [];
    const detachAmbient = useMultiplayerStore
      .getState()
      .subscribeAmbientLobby((frame, source) => {
        frames.push({ frame, url: source.url });
      });
    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});

    emitAmbient(opened.get(B)!, "PasswordRequired", { game_code: "PRE111" });

    // Reach-guard: the fan-out reaches this subscriber before the flap...
    expect(frames).toEqual([
      { frame: { kind: "passwordRequired", gameCode: "PRE111" }, url: B },
    ]);

    controls.get(B)!.drop();
    const reopened = fakeSocket(B);
    controls.get(B)!.reopen(reopened);
    emitAmbient(reopened, "PasswordRequired", { game_code: "ABC123" });

    // ...and still after it, from the new socket, tagged with the authority
    // that asked for the password. A subscriber holds no socket, so this is
    // only true because the store re-attached its own listener.
    expect(frames).toHaveLength(2);
    expect(frames[frames.length - 1]).toEqual({
      frame: { kind: "passwordRequired", gameCode: "ABC123" },
      url: B,
    });
    detachAmbient();
    detach?.();
  });

  it("does not list games from a join-origin socket", async () => {
    const A = "wss://a.example/ws";
    const AD_HOC = "wss://adhoc.example/ws";
    useMultiplayerStore.setState({
      hostingServer: A,
      userLobbySources: [{ url: A, name: "a.example", origin: "user" }],
      sourceStatus: new Map(),
    });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(
      async (url: string) =>
        fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );
    brokerMocks.lookupJoinTargetOver.mockResolvedValue({
      ok: true,
      info: { is_p2p: false, format_config: null },
    });

    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    const origin = adHocLobbySource(AD_HOC) as LobbySource;
    await useMultiplayerStore.getState().lookupJoinTarget("ABC123", origin);

    // The ad-hoc channel never gets a lobby listener attached, so no frame on
    // it can reach a subscriber; the browsed source's channel does have one.
    // (Paired positive below — a bare `not.toHaveBeenCalled` here would pass
    // even if fan-out were broken everywhere.)
    expect(brokerMocks.lobbyUpdaters.has(AD_HOC)).toBe(false);
    expect(brokerMocks.lobbyUpdaters.has(A)).toBe(true);
    // The RPC's channel is closed once it settles: no lingering reconnect
    // loop and no status row for a server nobody browses.
    expect(useMultiplayerStore.getState().sourceStatus.has(AD_HOC)).toBe(false);
    detach?.();
  });

  it("leaves nothing behind when a join-origin dial fails", async () => {
    const A = "wss://a.example/ws";
    const AD_HOC = "wss://adhoc.example/ws";
    useMultiplayerStore.setState({
      hostingServer: A,
      userLobbySources: [{ url: A, name: "a.example", origin: "user" }],
      sourceStatus: new Map(),
    });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(async (url: string) => {
      if (url === AD_HOC) {
        throw new HandshakeError("ws_error", "connection refused");
      }
      return fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>;
    });

    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    const origin = adHocLobbySource(AD_HOC) as LobbySource;
    const result = await useMultiplayerStore
      .getState()
      .lookupJoinTarget("ABC123", origin);

    // Reach-guards: the dial reached the refusal branch, and the browsed
    // source DOES keep a status row — so the negative below is measuring the
    // failed ad-hoc channel's cleanup, not an empty map.
    expect(result).toMatchObject({ ok: false, reason: "connection_lost" });
    expect(useMultiplayerStore.getState().sourceStatus.has(A)).toBe(true);
    // A mistyped host is the likeliest way to reach this branch; each attempt
    // must not accumulate a dead reconnect handle and a phantom status row.
    expect(useMultiplayerStore.getState().sourceStatus.has(AD_HOC)).toBe(false);
    detach?.();
  });

  // ── Merged-list ordering ────────────────────────────────────────────────

  it("orders official rows first, then by score, then by wait time", () => {
    const official: LobbySource = { url: PRESET_URL, name: "official", origin: "official" };
    const scored: LobbySource = { url: "wss://s.example/ws", name: "s", origin: "user", score: 70 };
    const unscored: LobbySource = { url: "wss://u.example/ws", name: "u", origin: "user" };
    const zero: LobbySource = { url: "wss://z.example/ws", name: "z", origin: "user", score: 0 };

    const entry = (source: LobbySource, createdAt: number, code: string): LobbyGameEntry => ({
      game: lobbyGame({ game_code: code, created_at: createdAt }),
      source,
    });

    const ordered = [
      entry(unscored, 100, "UNSC1"),
      entry(scored, 300, "SCOR1"),
      entry(official, 400, "OFFI1"),
      entry(zero, 200, "ZERO1"),
    ]
      .sort(compareLobbyGameEntries)
      .map((e) => e.game.game_code);

    // Official first despite being newest; then score descending with an
    // absent score ranking below an explicit 0.
    expect(ordered).toEqual(["OFFI1", "SCOR1", "ZERO1", "UNSC1"]);
  });

  it("breaks ties on the longest-waiting table", () => {
    const source: LobbySource = { url: "wss://s.example/ws", name: "s", origin: "user" };
    const older: LobbyGameEntry = {
      game: lobbyGame({ game_code: "OLD11", created_at: 10 }),
      source,
    };
    const newer: LobbyGameEntry = {
      game: lobbyGame({ game_code: "NEW11", created_at: 20 }),
      source,
    };

    expect([newer, older].sort(compareLobbyGameEntries).map((e) => e.game.game_code)).toEqual([
      "OLD11",
      "NEW11",
    ]);
  });

  it("canonicalises a hand-added source's URL and host name", () => {
    expect(userLobbySource("  WSS://Play.Example.COM/ws  ")).toEqual({
      url: "wss://play.example.com/ws",
      name: "play.example.com",
      origin: "user",
    });
    expect(userLobbySource("wss:")).toBeNull();
  });

  // ── Directory sources ───────────────────────────────────────────────────

  function directoryRow(
    overrides: Partial<DirectoryRow> & { url: string },
  ): DirectoryRow {
    return {
      name: "listed",
      mode: "LobbyOnly",
      server_version: "0.71.0",
      protocol_version: PROTOCOL_VERSION,
      lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
      current_players: 0,
      first_seen_ms: 1_700_000_000_000,
      last_seen_ms: 1_700_000_060_000,
      score: null,
      ...overrides,
    };
  }

  /** Project fixtures through the PRODUCTION projection, so a test row carries
   * the same canonical URL and the same stored `rejection` a real listing
   * would. */
  function directoryEntries(
    ...rows: (Partial<DirectoryRow> & { url: string })[]
  ): DirectorySource[] {
    return projectDirectoryBody({
      directory_version: DIRECTORY_VERSION,
      servers: rows.map(directoryRow),
    })!;
  }

  const dialedUrls = () => lobbySources(useMultiplayerStore.getState()).map((s) => s.url);

  // V-U11i
  it("keeps a directory disable across the entry disappearing and returning", () => {
    const URL_A = "wss://a.example/ws";
    const URL_B = "wss://b.example/ws";
    useMultiplayerStore.setState({ directorySources: directoryEntries({ url: URL_A }) });
    expect(dialedUrls()).toContain(URL_A);

    useMultiplayerStore.getState().setDirectorySourceEnabled(URL_A, false);
    useMultiplayerStore.setState({ directorySources: [] });
    expect(useMultiplayerStore.getState().disabledDirectorySources).toEqual([URL_A]);

    useMultiplayerStore.setState({
      directorySources: directoryEntries({ url: URL_A }, { url: URL_B }),
    });
    expect(useMultiplayerStore.getState().disabledDirectorySources).toEqual([URL_A]);
    expect(dialedUrls()).not.toContain(URL_A);
    // Paired: a different URL seeded in the same step IS dialed, so the
    // absence above is the disable and not an empty projection.
    expect(dialedUrls()).toContain(URL_B);
  });

  // V-U11l
  it("closes a directory source's channel when it is switched off", async () => {
    const URL_A = "wss://a.example/ws";
    const URL_B = "wss://b.example/ws";
    useMultiplayerStore.setState({
      directorySources: directoryEntries({ url: URL_A }, { url: URL_B }),
    });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(
      async (url: string) =>
        fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );
    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    await waitFor(() => {
      expect(useMultiplayerStore.getState().sourceStatus.has(URL_A)).toBe(true);
    });

    useMultiplayerStore.getState().setDirectorySourceEnabled(URL_A, false);

    expect(useMultiplayerStore.getState().sourceStatus.has(URL_A)).toBe(false);
    // Paired: the enabled sibling keeps its row, so the teardown was scoped.
    expect(useMultiplayerStore.getState().sourceStatus.has(URL_B)).toBe(true);
    detach?.();
  });

  // V-U11m
  it("normalizes the persisted directory disable list", () => {
    expect(normalizeDisabledDirectorySources(undefined)).toEqual([]);
    expect(normalizeDisabledDirectorySources("nope")).toEqual([]);
    expect(normalizeDisabledDirectorySources([1, null, {}])).toEqual([]);
    expect(normalizeDisabledDirectorySources(["not a url"])).toEqual([]);
    // Deduped THROUGH the canonicaliser: two spellings of one authority are
    // one disable.
    expect(
      normalizeDisabledDirectorySources(["wss://a.example", "wss://a.example/"]),
    ).toEqual(["wss://a.example/"]);
    // The default is read from the initial state, never from a snapshot this
    // file's `beforeEach` wrote itself.
    expect(useMultiplayerStore.getInitialState().disabledDirectorySources).toEqual([]);
  });

  // V-U11n
  it("lets a currently-listed server be pinned as a hand-added source", () => {
    const URL_A = "wss://a.example/ws";
    useMultiplayerStore.setState({ directorySources: directoryEntries({ url: URL_A }) });
    expect(useMultiplayerStore.getState().addUserLobbySource(URL_A)).toMatchObject({
      ok: true,
    });

    const listed = lobbySources(useMultiplayerStore.getState()).filter(
      (s) => s.url === URL_A,
    );
    expect(listed).toHaveLength(1);
    expect(listed[0].origin).toBe("user");
    // Paired: a PRESET URL is still a duplicate — the rule was relaxed for
    // directory listings only.
    expect(useMultiplayerStore.getState().addUserLobbySource(PRESET_URL)).toEqual({
      ok: false,
      reason: "duplicate",
    });
  });

  // V-U11o
  it("keeps a removed user entry browsable while the directory still lists it", async () => {
    const LISTED = "wss://listed.example/ws";
    const LONE = "wss://lone.example/ws";
    useMultiplayerStore.setState({
      directorySources: directoryEntries({ url: LISTED }),
      userLobbySources: [userLobbySource(LISTED)!, userLobbySource(LONE)!],
    });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(
      async (url: string) =>
        fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );
    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    await waitFor(() => {
      expect(useMultiplayerStore.getState().sourceStatus.has(LONE)).toBe(true);
    });

    useMultiplayerStore.getState().removeUserLobbySource(LISTED);
    const stillListed = lobbySources(useMultiplayerStore.getState()).filter(
      (s) => s.url === LISTED,
    );
    expect(stillListed).toHaveLength(1);
    expect(stillListed[0].origin).toBe("directory");
    expect(useMultiplayerStore.getState().sourceStatus.has(LISTED)).toBe(true);

    // Paired: a user entry with no directory twin is removed AND torn down.
    useMultiplayerStore.getState().removeUserLobbySource(LONE);
    expect(dialedUrls()).not.toContain(LONE);
    expect(useMultiplayerStore.getState().sourceStatus.has(LONE)).toBe(false);
    detach?.();
  });

  // V-U15f — the inversion of phase 4's V-U11p. Hosting placement over a
  // directory listing is now a supported, disclosed choice, so the listing
  // RESOLVES instead of falling through — and the join-origin fallback that
  // consumes the result must still dial the same URL.
  it("resolves a directory listing as the hosting source without moving the dial target", async () => {
    // Pathful ON PURPOSE: a pathless announced URL projects to a DIFFERENT
    // `source.url`, so a pathless fixture whose two spellings drift would make
    // every `find` below return undefined and the row would pass whatever the
    // filter does.
    const URL_D = "wss://d.example/ws";
    useMultiplayerStore.setState({
      directorySources: directoryEntries({
        url: URL_D,
        score: {
          value: 88,
          samples: 50,
          success_rate: 1,
          completion_rate: 1,
          median_rtt_ms: 20,
        },
      }),
      // A leaked disable would void the fixture the same way a drifting URL
      // would.
      disabledDirectorySources: [],
      userLobbySources: [],
      hostingServer: URL_D,
      sourceStatus: new Map(),
    });
    // Reach-guard, BEFORE the call: without it the negative half passes on an
    // absent fixture.
    expect(
      lobbySources(useMultiplayerStore.getState()).some(
        (s) => s.origin === "directory" && s.url === URL_D,
      ),
    ).toBe(true);

    const resolved = hostingLobbySource(useMultiplayerStore.getState());
    // Was "user" / undefined before this phase: the listing was filtered out
    // and the fall-through synthesised an ad-hoc source at the same URL.
    expect(resolved?.origin).toBe("directory");
    expect(resolved?.score).toBe(88);

    // The inertness leg. `hostingLobbySource` feeds the join-origin fallback in
    // both of its production consumers, so what must NOT have changed is the
    // URL a join opens on — only the label and score attached to it.
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(
      async (url: string) =>
        fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );
    brokerMocks.lookupJoinTargetOver.mockResolvedValue({ ok: true, info: {} });
    await useMultiplayerStore
      .getState()
      .lookupJoinTarget("ABCDE", hostingLobbySource(useMultiplayerStore.getState())!);
    expect(openPhaseSocket).toHaveBeenCalledWith(URL_D, { surface: "lobby" });

    // Discriminating control. `adHocLobbySource` IS `userLobbySource`, so a
    // hand-added entry with no status row is deep-equal to the synthesis of
    // the same URL — a control built that way would pass even against a filter
    // that refused everything. Seeding a status row gives the FOUND source
    // `kind: "Full"`, which the ad-hoc synthesis cannot produce, and `kind` is
    // the only field that separates the two paths.
    useMultiplayerStore.setState({
      userLobbySources: [userLobbySource(URL_D)!],
      sourceStatus: new Map([
        [
          URL_D,
          {
            state: "open" as const,
            serverInfo: server("Full", PROTOCOL_VERSION, LOBBY_PROTOCOL_VERSION),
            playerCount: null,
          },
        ],
      ]),
    });
    expect(hostingLobbySource(useMultiplayerStore.getState())?.kind).toBe("Full");
  });

  // ── U12: connect-outcome reporting ──────────────────────────────────────

  /** Open every enabled source's channel through the mocked `withReconnect`,
   * with a fake socket per URL. */
  function stubSocketsPerUrl(): void {
    vi.mocked(openPhaseSocket).mockImplementation(
      async (url: string) =>
        fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );
  }

  // V-U12a
  it("reports one connect_ok per dialed directory source, with its rtt", async () => {
    const LISTED = "wss://listed.example/ws";
    const OFF = "wss://off.example/ws";
    useMultiplayerStore.setState({
      directorySources: directoryEntries({ url: LISTED }, { url: OFF }),
      // Paired negative: LISTED-BUT-DISABLED. It is in the directory and is not
      // dialed, so a reporter keyed on the LISTING rather than on the DIAL
      // would report it too.
      disabledDirectorySources: [OFF],
      hostingServer: LISTED,
    });
    driveReconnect();
    stubSocketsPerUrl();

    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    await waitFor(() => {
      expect(useMultiplayerStore.getState().sourceStatus.has(LISTED)).toBe(true);
    });

    const reported = metricsMocks.reportConnectOutcome.mock.calls;
    expect(reported).toHaveLength(1);
    expect(reported[0][0]).toBe(LISTED);
    expect(reported[0][1]).toBe("connect_ok");
    expect(typeof reported[0][2]).toBe("number");
    expect(reported.some(([url]) => url === OFF)).toBe(false);
    detach?.();
  });

  // V-U12b
  it("reports the announced key, and reports nothing for a shadowed listing", async () => {
    // Pathless ON PURPOSE: `wss://a.example` canonicalises to
    // `wss://a.example/`, so the announced and client spellings provably
    // DIFFER here. With a pathful fixture the two coincide and this row would
    // pass whichever one the code sent.
    const ANNOUNCED = "wss://a.example";
    const CLIENT = "wss://a.example/";
    useMultiplayerStore.setState({
      directorySources: directoryEntries({ url: ANNOUNCED }),
      userLobbySources: [],
      hostingServer: CLIENT,
    });
    driveReconnect();
    stubSocketsPerUrl();

    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    await waitFor(() => {
      expect(useMultiplayerStore.getState().sourceStatus.has(CLIENT)).toBe(true);
    });

    expect(metricsMocks.reportConnectOutcome).toHaveBeenCalledTimes(1);
    expect(metricsMocks.reportConnectOutcome.mock.calls[0][0]).toBe(ANNOUNCED);
    expect(metricsMocks.reportConnectOutcome.mock.calls[0][0]).not.toBe(CLIENT);
    detach?.();

    // Second half, the control: the SAME host, now also claimed by a
    // hand-added entry, so the listing is shadowed out of the join and the
    // client can no longer name it to the directory. No report at all.
    useMultiplayerStore.getState().closeSubscriptionSocket();
    metricsMocks.reportConnectOutcome.mockClear();
    useMultiplayerStore.setState({
      directorySources: directoryEntries({ url: ANNOUNCED }),
      userLobbySources: [userLobbySource(CLIENT)!],
    });
    driveReconnect();
    const detachTwo = await useMultiplayerStore.getState().subscribeLobby(() => {});
    await waitFor(() => {
      expect(useMultiplayerStore.getState().sourceStatus.has(CLIENT)).toBe(true);
    });
    expect(metricsMocks.reportConnectOutcome).not.toHaveBeenCalled();
    detachTwo?.();
  });

  // V-U12c
  it("reports connect_fail with no rtt, and still lets the error propagate", async () => {
    const LISTED = "wss://listed.example/ws";
    useMultiplayerStore.setState({
      directorySources: directoryEntries({ url: LISTED }),
      hostingServer: LISTED,
    });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockRejectedValue(
      new HandshakeError("ws_error", "could not reach the server"),
    );

    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    await waitFor(() => {
      // Reach-guard on the RETHROW: the store only reaches "offline" because
      // the factory's rejection propagated past the reporter.
      expect(useMultiplayerStore.getState().sourceStatus.get(LISTED)?.state).toBe("offline");
    });

    expect(metricsMocks.reportConnectOutcome).toHaveBeenCalledTimes(1);
    const [url, outcome, rtt] = metricsMocks.reportConnectOutcome.mock.calls[0];
    expect(url).toBe(LISTED);
    expect(outcome).toBe("connect_fail");
    expect(rtt).toBeUndefined();
    detach?.();
  });

  // V-U12d
  it("reports only the first attempt of a channel, not every re-dial", async () => {
    const A = "wss://a-listed.example/ws";
    const B = "wss://b-listed.example/ws";
    useMultiplayerStore.setState({
      directorySources: directoryEntries({ url: A }, { url: B }),
      hostingServer: A,
    });
    const controls = driveReconnectWithFlaps();
    stubSocketsPerUrl();

    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    await waitFor(() => {
      expect(controls.has(A)).toBe(true);
    });
    // Paired positive: a SECOND, different source opening does add a call, so
    // "exactly one for A" is not "the emitter never fires twice".
    expect(metricsMocks.reportConnectOutcome).toHaveBeenCalledTimes(2);

    const opensBefore = vi.mocked(openPhaseSocket).mock.calls.length;
    controls.get(A)!.drop();
    // The factory-faithful reopen: the real `withReconnect` re-invokes the
    // factory with a bumped attempt index. `reopen` would not, which is what
    // made this row vacuous against the harness as it stood.
    await controls.get(A)!.reopenViaFactory();

    // Reach-guard on the reopen: the factory really did run again.
    expect(vi.mocked(openPhaseSocket).mock.calls.length).toBe(opensBefore + 1);
    // Still two: the re-dial reported nothing.
    expect(metricsMocks.reportConnectOutcome).toHaveBeenCalledTimes(2);
    expect(
      metricsMocks.reportConnectOutcome.mock.calls.filter(([url]) => url === A),
    ).toHaveLength(1);

    // The egress check. Defence-in-depth, not the mitigation: the module mock
    // is what prevents a send, and this confirms the recording stub — not the
    // network — is what received every report above.
    expect(navigator.sendBeacon).not.toHaveBeenCalled();
    expect(globalThis.fetch).not.toHaveBeenCalled();
    detach?.();
  });

  // ── U18: the dial gate ──────────────────────────────────────────────────

  // V-U18a + V-U18b
  it("refuses to dial a directory source below the lobby protocol floor", async () => {
    const BAD_URL = "wss://old.example/ws";
    const GOOD_URL = "wss://new.example/ws";
    useMultiplayerStore.setState({
      directorySources: directoryEntries(
        {
          url: BAD_URL,
          lobby_protocol_version: MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL - 1,
        },
        { url: GOOD_URL },
      ),
      userLobbySources: [],
    });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(
      async (url: string) =>
        fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );

    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});

    // Reach-guards: the compatible sibling IS dialed, and the refused source
    // is present in the derived list — the negative is not passing because the
    // fixture was absent.
    expect(openPhaseSocket).toHaveBeenCalledWith(GOOD_URL, { surface: "lobby" });
    expect(dialedUrls()).toContain(BAD_URL);
    expect(openPhaseSocket).not.toHaveBeenCalledWith(BAD_URL, expect.anything());

    // V-U18b: the gate sits before `channelFor`, so a refused source writes no
    // status row and cannot read as "offline"/degraded.
    await waitFor(() => {
      expect(useMultiplayerStore.getState().sourceStatus.has(GOOD_URL)).toBe(true);
    });
    expect(useMultiplayerStore.getState().sourceStatus.has(BAD_URL)).toBe(false);
    detach?.();
  });

  // V-U18c
  it("gates the join path on the same verdict as the browse path", async () => {
    const BAD_URL = "wss://old.example/ws";
    const GOOD_URL = "wss://new.example/ws";
    const entries = directoryEntries(
      { url: BAD_URL, lobby_protocol_version: MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL - 1 },
      { url: GOOD_URL },
    );
    useMultiplayerStore.setState({ directorySources: entries, userLobbySources: [] });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(
      async (url: string) =>
        fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );
    brokerMocks.lookupJoinTargetOver.mockResolvedValue({
      ok: true,
      info: { is_p2p: false, format_config: null },
    });
    const bad = entries.find((e) => e.source.url === BAD_URL)!.source;
    const good = entries.find((e) => e.source.url === GOOD_URL)!.source;

    const refused = await useMultiplayerStore.getState().lookupJoinTarget("ABCDE", bad);
    expect(refused).toMatchObject({ ok: false, reason: "connection_lost" });
    expect(openPhaseSocket).not.toHaveBeenCalledWith(BAD_URL, expect.anything());

    // Paired: the compatible sibling reaches the RPC over a real socket.
    const allowed = await useMultiplayerStore.getState().lookupJoinTarget("ABCDE", good);
    expect(allowed).toMatchObject({ ok: true });
    expect(brokerMocks.lookupJoinTargetOver).toHaveBeenCalled();
  });

  // V-U18e
  it("takes the verdict on the lobby surface, not the full-game one", async () => {
    const URL_F = "wss://fullstale.example/ws";
    const entries = directoryEntries({
      url: URL_F,
      mode: "Full",
      protocol_version: PROTOCOL_VERSION - 1,
      lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
    });
    // The full-game surface refuses exactly this identity
    // (`isServerCompatible(server("Full", PROTOCOL_VERSION - 1, …)) === false`,
    // asserted above in this file). The lobby surface must not.
    expect(entries[0].rejection).toBeNull();

    useMultiplayerStore.setState({ directorySources: entries, userLobbySources: [] });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(
      async (url: string) =>
        fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );
    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    expect(openPhaseSocket).toHaveBeenCalledWith(URL_F, { surface: "lobby" });
    detach?.();
  });

  // V-U18f
  it("accepts a lobby version newer than this client's — no ceiling", async () => {
    const URL_N = "wss://newer.example/ws";
    const entries = directoryEntries({
      url: URL_N,
      lobby_protocol_version: LOBBY_PROTOCOL_VERSION + 5,
    });
    expect(entries[0].rejection).toBeNull();

    useMultiplayerStore.setState({ directorySources: entries, userLobbySources: [] });
    driveReconnect();
    vi.mocked(openPhaseSocket).mockImplementation(
      async (url: string) =>
        fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
    );
    const detach = await useMultiplayerStore.getState().subscribeLobby(() => {});
    expect(openPhaseSocket).toHaveBeenCalledWith(URL_N, { surface: "lobby" });
    detach?.();
  });

  // V-U18h — the gate is keyed through the SHADOWING predicate, so a preset or
  // hand-added URL the directory also lists is still judged at the handshake.
  it("still dials a listed-but-incompatible server the user pinned as their own", async () => {
    const URL_X = "wss://x.example/ws";
    const belowFloor = (url: string) =>
      directoryEntries({
        url,
        lobby_protocol_version: MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL - 1,
      });

    async function dial(claim: Partial<Parameters<typeof useMultiplayerStore.setState>[0]>) {
      useMultiplayerStore.getState().closeSubscriptionSocket();
      vi.mocked(openPhaseSocket).mockClear();
      useMultiplayerStore.setState({
        userLobbySources: [],
        sourceStatus: new Map(),
        disabledDirectorySources: [],
        ...claim,
      });
      driveReconnect();
      vi.mocked(openPhaseSocket).mockImplementation(
        async (url: string) =>
          fakeSocket(url) as unknown as Awaited<ReturnType<typeof openPhaseSocket>>,
      );
      return useMultiplayerStore.getState().subscribeLobby(() => {});
    }

    // (i) A PURE directory listing at URL_X: gated, no socket, no status row.
    let detach = await dial({ directorySources: belowFloor(URL_X) });
    expect(openPhaseSocket).not.toHaveBeenCalledWith(URL_X, expect.anything());
    expect(useMultiplayerStore.getState().sourceStatus.has(URL_X)).toBe(false);
    detach?.();

    // (ii) The SAME URL, the SAME below-floor row, now also hand-added. The
    // user's explicit claim wins and the server is judged at the handshake —
    // the only difference between the two halves is who claims the URL.
    detach = await dial({
      directorySources: belowFloor(URL_X),
      userLobbySources: [userLobbySource(URL_X)!],
    });
    expect(openPhaseSocket).toHaveBeenCalledWith(URL_X, { surface: "lobby" });
    await waitFor(() => {
      expect(useMultiplayerStore.getState().sourceStatus.has(URL_X)).toBe(true);
    });
    detach?.();

    // Third leg, same shape: a preset URL the directory lists as incompatible
    // is a preset, and presets are never gated.
    detach = await dial({ directorySources: belowFloor(PRESET_URL) });
    expect(openPhaseSocket).toHaveBeenCalledWith(PRESET_URL, { surface: "lobby" });
    detach?.();
  });
});

