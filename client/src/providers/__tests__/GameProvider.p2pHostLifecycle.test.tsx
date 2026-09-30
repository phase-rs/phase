import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type TestP2PHostAdapter = {
  constructorArgs: unknown[];
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
  loadP2PTerminalResult,
  loadFullTerminalCleanup,
  clearFullTerminalCleanup,
  readFullTerminalResult,
  acknowledgeTerminalDelivery,
  nativeEngineKeyForCurrentOrigin,
  ensureNativeEngine,
  multiplayerStore,
  takeActiveP2PHost,
  useGameStore,
} = vi.hoisted(() => {
  const adapters: TestP2PHostAdapter[] = [];
  const createAdapter = (constructorArgs: unknown[] = []): TestP2PHostAdapter => {
    let disposed = false;
    const adapter: TestP2PHostAdapter = {
      constructorArgs,
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
  };

  return {
    adapters,
    createAdapter,
    gameStore,
    hostRoom: vi.fn(async () => ({
      peer: { id: "fresh-peer", destroy: vi.fn() },
      roomCode: "ABCDE",
      onGuestConnected: vi.fn(() => () => {}),
    })),
    loadGame: vi.fn(),
    loadP2PHostSession: vi.fn(),
    loadP2PTerminalResult: vi.fn<() => Promise<unknown>>(async () => null),
    loadFullTerminalCleanup: vi.fn<() => Promise<unknown>>(async () => null),
    clearFullTerminalCleanup: vi.fn(async () => undefined),
    readFullTerminalResult: vi.fn<() => Promise<unknown>>(async () => null),
    acknowledgeTerminalDelivery: vi.fn(async () => true),
    nativeEngineKeyForCurrentOrigin: vi.fn<() => { release: { version: string } } | null>(() => null),
    ensureNativeEngine: vi.fn(async () => ({ port: 0 })),
    multiplayerStore,
    takeActiveP2PHost,
    useGameStore,
  };
});

vi.mock("../../adapter/p2p-adapter", () => ({
  P2PGuestAdapter: class {},
  P2PHostAdapter: class {
    constructor(...args: unknown[]) {
      return createAdapter(args);
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
  acknowledgeFullTerminalDelivery: acknowledgeTerminalDelivery,
  bootstrapFullTerminalDelivery: vi.fn(),
  readFullTerminalResult,
}));

vi.mock("../../services/gamePersistence", () => ({
  clearFullTerminalCleanupStrict: clearFullTerminalCleanup,
  loadFullTerminalCleanupStrict: loadFullTerminalCleanup,
  loadGameStrict: vi.fn(async () => null),
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
  ensureNativeEngine,
  nativeEngineKeyForCurrentOrigin,
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
  loadP2PTerminalResult,
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
    loadP2PTerminalResult.mockResolvedValue(null);
    loadFullTerminalCleanup.mockResolvedValue(null);
    clearFullTerminalCleanup.mockResolvedValue(undefined);
    readFullTerminalResult.mockResolvedValue(null);
    acknowledgeTerminalDelivery.mockResolvedValue(true);
    nativeEngineKeyForCurrentOrigin.mockReturnValue(null);
    ensureNativeEngine.mockResolvedValue({ port: 0 });
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

  it("resumes a native P2P host from its durable terminal result", async () => {
    const terminal = {
      key: "session-key",
      lease: { sessionKey: "session-key", hostIncarnation: "previous-host" },
      recipient: 0,
      revision: 7,
      terminalId: "durable-terminal",
      finalStateCommitment: "sha256:retained",
      display: { winner: 0, reason: "Full terminal" },
    };
    const session = {
      gameStarted: true,
      roomCode: "ABCDE",
      sessionKey: "session-key",
      nativeSession: {
        gameCode: "native-game",
        fullKey: { game_code: "native-game", generation: 1 },
        playerTokens: { 0: "host-token", 1: "guest-token" },
      },
    };
    loadP2PHostSession.mockResolvedValue(session);
    loadP2PTerminalResult.mockResolvedValue(terminal);
    nativeEngineKeyForCurrentOrigin.mockReturnValue({ release: { version: "test-version" } });
    const onP2PEvent = vi.fn();

    render(
      <GameProvider gameId="p2p-game" mode="p2p-host" onP2PEvent={onP2PEvent}>
        <div />
      </GameProvider>,
    );

    await waitFor(() => expect(gameStore.resumeP2PHost).toHaveBeenCalledOnce());
    expect(ensureNativeEngine).toHaveBeenCalledWith({ release: { version: "test-version" } });
    expect(hostRoom).toHaveBeenCalledWith(expect.any(AbortSignal), { preferredRoomCode: "ABCDE" });
    expect(adapters).toHaveLength(1);
    const persistence = adapters[0].constructorArgs[10] as {
      resumeData?: { session: unknown; terminalResult?: unknown };
    };
    expect(persistence.resumeData).toEqual({ session, terminalResult: terminal });
    expect(onP2PEvent).not.toHaveBeenCalledWith({ type: "terminalResult", result: terminal });
  });

  it.each(["missing native key", "native setup failure"])(
    "shows a retained result despite %s",
    async (failure) => {
      const terminal = retainedTerminal();
      loadP2PHostSession.mockResolvedValue({
        gameStarted: true,
        roomCode: "ABCDE",
        sessionKey: "session-key",
        nativeSession: {
          gameCode: "native-game",
          fullKey: { game_code: "native-game", generation: 1 },
          playerTokens: { 0: "host-token", 1: "guest-token" },
        },
      });
      loadP2PTerminalResult.mockResolvedValue(terminal);
      if (failure === "native setup failure") {
        nativeEngineKeyForCurrentOrigin.mockReturnValue({ release: { version: "test-version" } });
        ensureNativeEngine.mockRejectedValueOnce(new Error("native setup rejected"));
      }
      const onP2PEvent = vi.fn();

      render(
        <GameProvider gameId="p2p-game" mode="p2p-host" onP2PEvent={onP2PEvent}>
          <div />
        </GameProvider>,
      );

      await waitFor(() => expect(onP2PEvent).toHaveBeenCalledWith({
        type: "terminalResult",
        result: terminal,
      }));
      expect(adapters).toHaveLength(0);
      expect(hostRoom).not.toHaveBeenCalled();
      expect(onP2PEvent).not.toHaveBeenCalledWith(expect.objectContaining({ type: "error" }));
    },
  );

  it("retries partial terminal ACKs after restart from metadata without a final view", async () => {
    const terminal = retainedTerminal();
    const cleanupOwner = {
      gameId: "p2p-game",
      p2pSessionKey: terminal.key,
      p2pTerminalId: terminal.terminalId,
      p2pResult: terminal,
      fullKey: { game_code: "native-game", generation: 1 },
      terminalRevision: terminal.revision,
      deliveries: [
        { recipient: 0, deliveryId: "host-delivery", credential: "host-credential" },
        { recipient: 1, deliveryId: "guest-delivery", credential: "guest-credential" },
      ],
    };
    loadFullTerminalCleanup.mockResolvedValue(cleanupOwner);
    const nativeKey = { release: { version: "test-release" } };
    nativeEngineKeyForCurrentOrigin.mockReturnValue(nativeKey);
    const readMetadataOnly = (deliveryId: string, credential: string) => ({
      key: cleanupOwner.fullKey,
      terminalRevision: terminal.revision,
      deliveryId,
      credential,
    });
    readFullTerminalResult
      .mockResolvedValueOnce(readMetadataOnly("host-delivery", "host-credential"))
      .mockResolvedValueOnce(readMetadataOnly("guest-delivery", "guest-credential"))
      .mockResolvedValueOnce(readMetadataOnly("host-delivery", "host-credential"))
      .mockResolvedValueOnce(readMetadataOnly("guest-delivery", "guest-credential"));
    acknowledgeTerminalDelivery
      .mockResolvedValueOnce(true)
      .mockResolvedValueOnce(false);
    const onP2PEvent = vi.fn();

    const firstMount = render(
      <GameProvider gameId="p2p-game" mode="p2p-host" onP2PEvent={onP2PEvent}>
        <div />
      </GameProvider>,
    );

    await waitFor(() => expect(acknowledgeTerminalDelivery).toHaveBeenCalledTimes(2));
    expect(clearFullTerminalCleanup).not.toHaveBeenCalled();
    expect(onP2PEvent).toHaveBeenCalledWith({ type: "terminalResult", result: terminal });
    firstMount.unmount();

    const resumedEvent = vi.fn();
    render(
      <GameProvider gameId="p2p-game" mode="p2p-host" onP2PEvent={resumedEvent}>
        <div />
      </GameProvider>,
    );
    await waitFor(() => expect(clearFullTerminalCleanup).toHaveBeenCalledWith("p2p-game"));
    expect(resumedEvent).toHaveBeenCalledWith({ type: "terminalResult", result: terminal });
    expect(readFullTerminalResult).toHaveBeenCalledTimes(4);
    expect(acknowledgeTerminalDelivery).toHaveBeenCalledTimes(4);
    expect(acknowledgeTerminalDelivery).toHaveBeenNthCalledWith(
      1,
      "native-engine://phase-server",
      "host-delivery",
      "host-credential",
      expect.any(Function),
    );
    expect(acknowledgeTerminalDelivery).toHaveBeenNthCalledWith(
      2,
      "native-engine://phase-server",
      "guest-delivery",
      "guest-credential",
      expect.any(Function),
    );
    expect(loadGame).not.toHaveBeenCalled();
    expect(loadP2PHostSession).not.toHaveBeenCalled();
    expect(adapters).toHaveLength(0);
    expect(hostRoom).not.toHaveBeenCalled();
    expect(ensureNativeEngine).toHaveBeenCalledTimes(2);
    expect(ensureNativeEngine).toHaveBeenCalledWith(nativeKey);
    expect(ensureNativeEngine.mock.invocationCallOrder[0]).toBeLessThan(
      readFullTerminalResult.mock.invocationCallOrder[0],
    );
  });

  it.each(["missing key", "startup failure"] as const)(
    "keeps cleanup authority and the saved result when native cleanup has %s",
    async (failure) => {
      const terminal = retainedTerminal();
      loadFullTerminalCleanup.mockResolvedValue({
        gameId: "p2p-game",
        p2pSessionKey: terminal.key,
        p2pTerminalId: terminal.terminalId,
        p2pResult: terminal,
        fullKey: { game_code: "native-game", generation: 1 },
        terminalRevision: terminal.revision,
        deliveries: [{ recipient: 0, deliveryId: "host-delivery", credential: "host-credential" }],
      });
      if (failure === "startup failure") {
        nativeEngineKeyForCurrentOrigin.mockReturnValue({ release: { version: "test-release" } });
        ensureNativeEngine.mockRejectedValueOnce(new Error("native startup failed"));
      }
      const onP2PEvent = vi.fn();
      render(
        <GameProvider gameId="p2p-game" mode="p2p-host" onP2PEvent={onP2PEvent}>
          <div />
        </GameProvider>,
      );
      await waitFor(() => expect(onP2PEvent).toHaveBeenCalledWith(
        expect.objectContaining({ type: "terminalUnavailable" }),
      ));
      expect(onP2PEvent).toHaveBeenCalledWith({ type: "terminalResult", result: terminal });
      expect(readFullTerminalResult).not.toHaveBeenCalled();
      expect(acknowledgeTerminalDelivery).not.toHaveBeenCalled();
      expect(clearFullTerminalCleanup).not.toHaveBeenCalled();
      expect(loadGame).not.toHaveBeenCalled();
      expect(loadP2PHostSession).not.toHaveBeenCalled();
      expect(adapters).toHaveLength(0);
      expect(hostRoom).not.toHaveBeenCalled();
      if (failure === "startup failure") expect(ensureNativeEngine).toHaveBeenCalledOnce();
      else expect(ensureNativeEngine).not.toHaveBeenCalled();
    },
  );

  it("keeps the terminal short-circuit when no native host session can resume", async () => {
    const terminal = retainedTerminal();
    loadP2PTerminalResult.mockResolvedValue(terminal);
    const onP2PEvent = vi.fn();

    render(
      <GameProvider gameId="p2p-game" mode="p2p-host" onP2PEvent={onP2PEvent}>
        <div />
      </GameProvider>,
    );

    await waitFor(() => expect(onP2PEvent).toHaveBeenCalledWith({ type: "terminalResult", result: terminal }));
    expect(adapters).toHaveLength(0);
    expect(hostRoom).not.toHaveBeenCalled();
    expect(ensureNativeEngine).not.toHaveBeenCalled();
  });
});

function retainedTerminal() {
  return {
    key: "session-key",
    lease: { sessionKey: "session-key", hostIncarnation: "previous-host" },
    recipient: 0,
    revision: 7,
    terminalId: "durable-terminal",
    finalStateCommitment: "sha256:retained",
    display: { winner: 0, reason: "Full terminal" },
  };
}
