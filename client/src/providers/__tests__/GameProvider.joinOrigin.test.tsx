import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The join/spectate origin is carried by the route and handed to
 * `GameProvider` as `serverUrl`. These cases pin the precedence of the four
 * rungs the socket-open site consults: the build override, the server a
 * resumable session was recorded on, the route's origin, and finally this
 * client's hosting server via `detectServerUrl()`.
 */
const {
  adapters,
  bootstrapFullTerminalDelivery,
  detectServerUrl,
  gameStoreState,
  loadWsSession,
  loadFullTerminalDelivery,
  multiplayerState,
  tryReconnect,
  useGameStore,
  WebSocketAdapter,
} = vi.hoisted(() => {
  const adapters: { serverUrl: string }[] = [];
  const tryReconnect = vi.fn(() => true);
  class WebSocketAdapter {
    dispose = vi.fn();
    onEvent = vi.fn(() => () => {});
    initialize = vi.fn(async () => {});
    tryReconnect = tryReconnect;

    constructor(serverUrl: string) {
      adapters.push({ serverUrl });
    }
  }
  const gameStoreState = {
    adapter: null as unknown,
    gameId: null as string | null,
    gameState: null,
    initGame: vi.fn(async () => {}),
    resumeGame: vi.fn(),
    resumeP2PHost: vi.fn(),
    resumeNativeSolo: vi.fn(),
    reset: vi.fn(),
    setEngineMode: vi.fn(),
    setGameMode: vi.fn(),
  };
  const useGameStore = Object.assign(
    vi.fn((selector: (state: typeof gameStoreState) => unknown) => selector(gameStoreState)),
    {
      getState: () => gameStoreState,
      setState: (partial: Record<string, unknown>) => Object.assign(gameStoreState, partial),
      subscribe: vi.fn<(listener: (state: typeof gameStoreState) => void) => () => void>(
        () => () => {},
      ),
    },
  );
  const multiplayerState = {
    displayName: "Player",
    setActionPending: vi.fn(),
    setActivePlayerId: vi.fn(),
    setConnectionStatus: vi.fn(),
    setIsSpectator: vi.fn(),
    setLatency: vi.fn(),
    setOpponentDisplayName: vi.fn(),
    setSpectators: vi.fn(),
    showToast: vi.fn(),
  };
  return {
    adapters,
    bootstrapFullTerminalDelivery: vi.fn<
      () => Promise<{ delivery_id: string; credential: string } | null>
    >(async () => null),
    detectServerUrl: vi.fn(async () => "ws://test-server"),
    gameStoreState,
    loadWsSession: vi.fn<() => Record<string, unknown> | null>(() => null),
    loadFullTerminalDelivery: vi.fn(async () => null),
    multiplayerState,
    tryReconnect,
    useGameStore,
    WebSocketAdapter,
  };
});

vi.mock("../../adapter/ws-adapter", () => ({
  NativeEngineVersionMismatchError: class extends Error {},
  WebSocketAdapter,
  // Without these the reconnect case throws inside the terminal-delivery
  // probe and returns before any socket is opened.
  bootstrapFullTerminalDelivery,
  readFullTerminalResult: vi.fn(async () => null),
  acknowledgeFullTerminalDelivery: vi.fn(async () => undefined),
}));

vi.mock("../../services/fullTerminalResult", () => ({
  loadFullTerminalDelivery,
  commitFullTerminalDelivery: vi.fn(async () => true),
  replaceFullTerminalDelivery: vi.fn(async () => true),
}));

vi.mock("../../adapter/wasm-adapter", () => ({
  WasmAdapter: class {
    cardDbLoaded = true;
    initialize = vi.fn(async () => {});
    resetGameState = vi.fn();
  },
  getSharedAdapter: vi.fn(),
}));

vi.mock("../../services/nativeEngine", () => ({
  canAttemptNativeEngine: () => false,
  ensureNativeEngine: vi.fn(),
  nativeEngineKeyForCurrentOrigin: () => null,
}));

vi.mock("../../services/nativeEngineSocket", () => ({ NativeEngineSocket: class {} }));

vi.mock("../../stores/gameStore", () => ({
  clearActiveGame: vi.fn(),
  clearGame: vi.fn(),
  clearP2PHostSession: vi.fn(),
  loadActiveGame: vi.fn(() => null),
  loadGame: vi.fn(async () => null),
  loadP2PHostSession: vi.fn(),
  nextGameSessionGeneration: vi.fn(() => 1),
  saveActiveGame: vi.fn(),
  useGameStore,
}));

vi.mock("../../constants/storage", () => ({
  ACTIVE_DECK_KEY: "active-deck",
  isRandomDeckSelection: () => false,
  loadActiveDeck: () => ({ main: ["Island"], sideboard: [] }),
  loadSavedDeckBracket: () => null,
}));

vi.mock("../../services/deckParser", () => ({
  expandParsedDeck: (deck: { main: string[]; sideboard: string[] }) => ({
    main_deck: deck.main,
    sideboard: deck.sideboard,
    commander: [],
    planar_deck: [],
    scheme_deck: [],
    signature_spell: [],
    companion: [],
    sticker_sheets: [],
  }),
}));

// Partially mocked: `deckCatalog` reads real registry tables at module scope.
vi.mock("../../data/formatRegistry", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../data/formatRegistry")>()),
  formatSuppliesDeck: () => false,
}));

vi.mock("../../services/aiDeckCatalog", () => ({
  buildLegalAiDeckCatalog: vi.fn(async () => ({ candidates: [] })),
}));

vi.mock("../../services/randomDeckSelection", () => ({
  pickRandomDeckCandidate: (candidates: unknown[]) => candidates[0],
}));

vi.mock("../../stores/preferencesStore", () => ({
  AI_DECK_RANDOM: "Random",
  usePreferencesStore: Object.assign(vi.fn(), {
    getState: () => ({ aiSeats: [], cedhMode: false, nativeEngineEnabled: false }),
  }),
}));

vi.mock("../../services/cedhLock", () => ({
  effectiveAiDifficulty: (difficulty: string) => difficulty,
}));

vi.mock("../../game/controllers/gameLoopController", () => ({
  createGameLoopController: vi.fn(() => ({ start: vi.fn(), dispose: vi.fn(), stop: vi.fn() })),
}));

vi.mock("../../game/dispatch", () => ({
  dispatchAction: vi.fn(),
  processRemoteUpdate: vi.fn(),
}));

vi.mock("../../game/sessionCleanup", () => ({ clearPromptOverlayState: vi.fn() }));

vi.mock("../../hooks/useGameplayPreferencesSync", () => ({
  useGameplayPreferencesSync: vi.fn(),
}));

vi.mock("../../audio/AudioManager", () => ({ audioManager: { setContext: vi.fn() } }));

vi.mock("../../stores/multiplayerStore", () => ({
  useMultiplayerStore: Object.assign(vi.fn(), {
    getState: () => multiplayerState,
    setState: vi.fn(),
  }),
}));

vi.mock("../../stores/multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: { getState: () => ({ matchPairing: null }) },
}));

vi.mock("../../services/playerAvatars", () => ({
  assignRandomAvatars: vi.fn(() => []),
  avatarCardNameForName: vi.fn(),
  fetchAvatarArtUrl: vi.fn(async () => null),
}));

vi.mock("../../services/multiplayerSession", () => ({
  clearWsSession: vi.fn(),
  loadWsSession,
  saveWsSession: vi.fn(),
}));

vi.mock("../../pwa/updateMarker", () => ({ consumeRecentAutoUpdateMarker: vi.fn() }));

vi.mock("../../services/quickDraftPersistence", () => ({ loadDraftRun: vi.fn() }));

vi.mock("../../services/serverDetection", () => ({ detectServerUrl }));

import { GameProvider } from "../GameProvider";

function renderJoin(props: { serverUrl?: string; joinCode?: string }) {
  return render(
    <GameProvider gameId="g1" mode="online" joinCode={props.joinCode} serverUrl={props.serverUrl}>
      <div />
    </GameProvider>,
  );
}

async function openedServerUrl(): Promise<string> {
  await waitFor(() => {
    expect(adapters).toHaveLength(1);
  });
  return adapters[0].serverUrl;
}

describe("GameProvider join origin", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    adapters.splice(0);
    loadWsSession.mockReturnValue(null);
    loadFullTerminalDelivery.mockResolvedValue(null);
    bootstrapFullTerminalDelivery.mockResolvedValue(null);
    detectServerUrl.mockResolvedValue("ws://test-server");
    gameStoreState.adapter = null;
    gameStoreState.gameId = null;
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllEnvs();
  });

  it("opens the join socket on the route's origin", async () => {
    renderJoin({ joinCode: "ABC123", serverUrl: "wss://origin.example/ws" });

    await expect(openedServerUrl()).resolves.toBe("wss://origin.example/ws");
    expect(detectServerUrl).not.toHaveBeenCalled();
  });

  it("keeps the VITE_WS_URL override ahead of the route's origin", async () => {
    vi.stubEnv("VITE_WS_URL", "ws://forced");

    renderJoin({ joinCode: "ABC123", serverUrl: "wss://origin.example/ws" });

    await expect(openedServerUrl()).resolves.toBe("ws://forced");
  });

  it("falls back to the hosting server when the route carried no origin", async () => {
    renderJoin({ joinCode: "ABC123" });

    await expect(openedServerUrl()).resolves.toBe("ws://test-server");
  });

  it("reconnects on the server the session was recorded on", async () => {
    loadWsSession.mockReturnValue({
      gameCode: "ABC123",
      playerToken: "tok",
      fullKey: { game_code: "ABC123", generation: 1 },
      serverUrl: "wss://session.example/ws",
      timestamp: Date.now(),
    });

    // No `joinCode` -> the reconnect branch. It reaches the socket-open site
    // only because both terminal-delivery probes resolve `null`.
    renderJoin({ serverUrl: "wss://origin.example/ws" });

    await expect(openedServerUrl()).resolves.toBe("wss://session.example/ws");
    await waitFor(() => {
      expect(tryReconnect).toHaveBeenCalled();
    });
  });

  it("does not open a socket when a terminal delivery is waiting", async () => {
    loadWsSession.mockReturnValue({
      gameCode: "ABC123",
      playerToken: "tok",
      fullKey: { game_code: "ABC123", generation: 1 },
      serverUrl: "wss://session.example/ws",
      timestamp: Date.now(),
    });
    bootstrapFullTerminalDelivery.mockResolvedValue({
      delivery_id: "d1",
      credential: "c1",
    });
    const onWsEvent = vi.fn();

    render(
      <GameProvider gameId="g1" mode="online" serverUrl="wss://origin.example/ws" onWsEvent={onWsEvent}>
        <div />
      </GameProvider>,
    );

    // Negative control for the recipe above: the reconnect case's reach
    // depends on the `null` terminal path, not on the mock merely existing.
    await waitFor(() => {
      expect(onWsEvent).toHaveBeenCalledWith(
        expect.objectContaining({ type: "terminalDelivery" }),
      );
    });
    expect(adapters).toHaveLength(0);
  });
});
