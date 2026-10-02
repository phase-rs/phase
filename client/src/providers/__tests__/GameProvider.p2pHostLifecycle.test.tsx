import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type TestP2PHostAdapter = {
  dispose: () => void;
  initialize: () => Promise<void>;
  onEvent: (listener: unknown) => () => void;
};

const {
  adapters,
  createAdapter,
  gameStore,
  hostRoom,
  loadGame,
  loadP2PHostSession,
  multiplayerStore,
  takeActiveP2PHost,
  useGameStore,
} = vi.hoisted(() => {
  const adapters: TestP2PHostAdapter[] = [];
  const createAdapter = (): TestP2PHostAdapter => {
    let disposed = false;
    const adapter: TestP2PHostAdapter = {
      dispose: vi.fn(() => {
        disposed = true;
      }),
      initialize: vi.fn(async () => {
        if (disposed) throw new Error("P2P host adapter has been disposed");
      }),
      onEvent: vi.fn(() => vi.fn()),
    };
    adapters.push(adapter);
    return adapter;
  };

  const gameStore = {
    adapter: null,
    gameId: null,
    gameState: null,
    initGame: vi.fn(),
    reset: vi.fn(),
    resumeGame: vi.fn(),
    resumeNativeSolo: vi.fn(),
    resumeP2PHost: vi.fn(async (_gameId: string, adapter: TestP2PHostAdapter) => {
      await adapter.initialize();
    }),
    setEngineMode: vi.fn(),
    setGameMode: vi.fn(),
  };
  const useGameStore = Object.assign(vi.fn(), {
    getState: () => gameStore,
    setState: vi.fn((partial: Record<string, unknown>) => Object.assign(gameStore, partial)),
    subscribe: vi.fn(() => () => {}),
  });

  const takeActiveP2PHost = vi.fn<() => TestP2PHostAdapter | null>();
  const multiplayerStore = {
    displayName: "Host",
    setActivePlayerId: vi.fn(),
    takeActiveP2PHost,
    openBroker: vi.fn(),
    closeBroker: vi.fn(),
  };

  return {
    adapters,
    createAdapter,
    gameStore,
    hostRoom: vi.fn(async () => ({
      peer: { id: "fresh-peer", destroy: vi.fn() },
      roomCode: "ABCDE",
      onGuestConnected: vi.fn(() => () => {}),
      destroy: vi.fn(),
    })),
    loadGame: vi.fn(),
    loadP2PHostSession: vi.fn(),
    multiplayerStore,
    takeActiveP2PHost,
    useGameStore,
  };
});

vi.mock("../../adapter/p2p-adapter", () => ({
  P2PGuestAdapter: class {},
  P2PHostAdapter: class {
    constructor() {
      return createAdapter();
    }
  },
}));

vi.mock("../../adapter/wasm-adapter", () => ({
  WasmAdapter: class {},
  getSharedAdapter: vi.fn(),
}));

vi.mock("../../adapter/ws-adapter", () => ({
  NativeEngineVersionMismatchError: class extends Error {},
  WebSocketAdapter: class {},
  acknowledgeFullTerminalDelivery: vi.fn(),
  bootstrapFullTerminalDelivery: vi.fn(),
  readFullTerminalResult: vi.fn(),
}));

vi.mock("../../audio/AudioManager", () => ({
  audioManager: { setContext: vi.fn() },
}));

vi.mock("../../constants/storage", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../constants/storage")>(),
  ACTIVE_DECK_KEY: "active-deck",
  isRandomDeckSelection: () => false,
  loadActiveDeck: () => ({ main: ["Island"], sideboard: [] }),
  loadSavedDeckBracket: () => null,
}));

vi.mock("../../data/formatRegistry", () => ({
  formatSuppliesDeck: () => false,
}));

vi.mock("../../game/controllers/gameLoopController", () => ({
  createGameLoopController: vi.fn(() => ({ dispose: vi.fn(), start: vi.fn() })),
}));

vi.mock("../../game/dispatch", () => ({
  dispatchAction: vi.fn(),
  processRemoteUpdate: vi.fn(),
}));

vi.mock("../../game/sessionCleanup", () => ({
  clearPromptOverlayState: vi.fn(),
}));

vi.mock("../../hooks/useGameplayPreferencesSync", () => ({
  useGameplayPreferencesSync: vi.fn(),
}));

vi.mock("../../network/connection", () => ({
  hostRoom,
  joinRoom: vi.fn(),
}));

vi.mock("../../pwa/updateMarker", () => ({
  consumeRecentAutoUpdateMarker: vi.fn(),
}));

vi.mock("../../services/aiDeckCatalog", () => ({
  buildLegalAiDeckCatalog: vi.fn(),
}));

vi.mock("../../services/cedhLock", () => ({
  effectiveAiDifficulty: (difficulty: string) => difficulty,
}));

vi.mock("../../services/nativeEngine", () => ({
  canAttemptNativeEngine: () => false,
  ensureNativeEngine: vi.fn(),
  nativeEngineKeyForCurrentOrigin: () => null,
}));

vi.mock("../../services/nativeEngineSocket", () => ({
  NativeEngineSocket: class {},
}));

vi.mock("../../services/playerAvatars", () => ({
  assignRandomAvatars: vi.fn(() => [
    { name: "Host", cardName: "Island" },
    { name: "Guest", cardName: "Mountain" },
  ]),
  avatarCardNameForName: vi.fn(),
  fetchAvatarArtUrl: vi.fn(async () => null),
}));

vi.mock("../../services/p2pSession", () => ({
  loadP2PSession: vi.fn(),
}));

vi.mock("../../services/p2pTerminalResult", () => ({
  loadP2PTerminalResult: vi.fn(async () => null),
}));

vi.mock("../../services/quickDraftPersistence", () => ({
  loadDraftRun: vi.fn(),
}));

vi.mock("../../services/randomDeckSelection", () => ({
  pickRandomDeckCandidate: vi.fn(),
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

vi.mock("../../services/multiplayerSession", () => ({
  clearWsSession: vi.fn(),
  loadWsSession: () => null,
  saveWsSession: vi.fn(),
}));

vi.mock("../../stores/gameStore", () => ({
  clearActiveGame: vi.fn(),
  clearGame: vi.fn(),
  clearP2PHostSession: vi.fn(),
  loadActiveGame: vi.fn(),
  loadGame,
  loadP2PHostSession,
  nextGameSessionGeneration: vi.fn(),
  saveActiveGame: vi.fn(),
  useGameStore,
}));

vi.mock("../../stores/multiplayerStore", () => ({
  useMultiplayerStore: Object.assign(vi.fn(), {
    getState: () => multiplayerStore,
    setState: vi.fn(),
  }),
}));

vi.mock("../../stores/multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: { getState: () => ({ matchPairing: null }) },
}));

vi.mock("../../stores/preferencesStore", () => ({
  AI_DECK_RANDOM: "Random",
  usePreferencesStore: Object.assign(vi.fn(), {
    getState: () => ({ nativeEngineEnabled: false }),
  }),
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

import { GameProvider } from "../GameProvider";

describe("GameProvider P2P host lifecycle", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    adapters.splice(0);
    gameStore.adapter = null;
    gameStore.gameId = null;
    gameStore.gameState = null;
    loadGame.mockResolvedValue({ state: {} });
    loadP2PHostSession.mockResolvedValue({
      gameStarted: true,
      roomCode: "ABCDE",
      sessionKey: "session-key",
      nativeSession: undefined,
    });
  });

  afterEach(cleanup);

  it("claims a pre-game host once, then resumes with a fresh host after remount", async () => {
    const firstAdapter = createAdapter();
    takeActiveP2PHost
      .mockReturnValueOnce(firstAdapter)
      .mockReturnValueOnce(null);

    const firstMount = render(
      <GameProvider gameId="p2p-game" mode="p2p-host">
        <div />
      </GameProvider>,
    );

    await waitFor(() => expect(gameStore.resumeP2PHost).toHaveBeenCalledWith("p2p-game", firstAdapter));
    expect(firstAdapter.initialize).toHaveBeenCalledOnce();

    firstMount.unmount();
    expect(firstAdapter.dispose).toHaveBeenCalledOnce();

    render(
      <GameProvider gameId="p2p-game" mode="p2p-host">
        <div />
      </GameProvider>,
    );

    await waitFor(() => expect(adapters).toHaveLength(2));
    const resumedAdapter = adapters[1];
    await waitFor(() => expect(gameStore.resumeP2PHost).toHaveBeenLastCalledWith("p2p-game", resumedAdapter));

    expect(takeActiveP2PHost).toHaveBeenCalledTimes(2);
    expect(loadGame).toHaveBeenCalledWith("p2p-game");
    expect(loadP2PHostSession).toHaveBeenCalledWith("p2p-game");
    expect(hostRoom).toHaveBeenCalledWith(expect.any(AbortSignal), {
      preferredRoomCode: "ABCDE",
    });
    expect(resumedAdapter).not.toBe(firstAdapter);
    expect(resumedAdapter.initialize).toHaveBeenCalledOnce();
    expect(firstAdapter.initialize).toHaveBeenCalledOnce();
  });

  it("claims seat zero before a pre-game host can replay an identity event", async () => {
    let observedSeat: number | null = 2;
    multiplayerStore.setActivePlayerId.mockImplementation((playerId: number) => {
      observedSeat = playerId;
    });
    takeActiveP2PHost.mockImplementationOnce(() => {
      expect(observedSeat).toBe(0);
      return createAdapter();
    });

    render(
      <GameProvider gameId="p2p-game" mode="p2p-host">
        <div />
      </GameProvider>,
    );

    await waitFor(() => expect(gameStore.resumeP2PHost).toHaveBeenCalled());
    expect(multiplayerStore.setActivePlayerId).toHaveBeenCalledWith(0);
  });

  describe("broker bring-up abort", () => {
    function fakeBroker() {
      return { unregister: vi.fn(async () => undefined), close: vi.fn() };
    }
    const flush = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };

    beforeEach(() => {
      takeActiveP2PHost.mockReturnValue(null);
      loadGame.mockResolvedValue(null);
      loadP2PHostSession.mockResolvedValue(null);
    });

    it("hands openBroker the mount's signal, aborted by unmount", async () => {
      let release!: (v: unknown) => void;
      multiplayerStore.openBroker.mockImplementationOnce(() => new Promise((r) => { release = r; }));
      const m = render(<GameProvider gameId="g" mode="p2p-host" useBroker><div /></GameProvider>);
      await waitFor(() => expect(multiplayerStore.openBroker).toHaveBeenCalledOnce());
      m.unmount();
      const signal = multiplayerStore.openBroker.mock.calls[0][1] as AbortSignal | undefined;
      release(null);
      await flush();
      expect(signal?.aborted).toBe(true);
    });

    it("releases a broker openBroker hands back after the unmount", async () => {
      const broker = fakeBroker();
      let release!: (v: unknown) => void;
      multiplayerStore.openBroker.mockImplementationOnce(() => new Promise((r) => { release = r; }));
      const m = render(<GameProvider gameId="g" mode="p2p-host" useBroker><div /></GameProvider>);
      await waitFor(() => expect(multiplayerStore.openBroker).toHaveBeenCalledOnce());
      m.unmount();
      release({ broker, gameCode: "ROOM1" });
      await flush();
      expect(multiplayerStore.closeBroker).toHaveBeenCalledWith(broker);
    });

    it("unregisters and closes a broker assigned before the unmount", async () => {
      const broker = fakeBroker();
      multiplayerStore.openBroker.mockResolvedValueOnce({ broker, gameCode: "ROOM1" });
      let releaseInit!: () => void;
      gameStore.initGame.mockImplementationOnce(() => new Promise<void>((r) => { releaseInit = r; }));
      const m = render(<GameProvider gameId="g" mode="p2p-host" useBroker><div /></GameProvider>);
      await waitFor(() => expect(gameStore.initGame).toHaveBeenCalledOnce());
      m.unmount();
      releaseInit();
      await flush();
      expect(broker.unregister).toHaveBeenCalledWith("ROOM1");
      expect(multiplayerStore.closeBroker).toHaveBeenCalledWith(broker);
    });
  });
});
