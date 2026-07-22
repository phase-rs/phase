import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type NativeAdapterEvent =
  | { type: "reconnectFailed" }
  | { type: "error"; message: string };

const {
  NativeEngineVersionMismatchError,
  WebSocketAdapter,
  WasmAdapter,
  clearActiveGame,
  ensureNativeEngine,
  gameStoreState,
  getSharedAdapter,
  nativeAdapterInitialize,
  nativeAdapters,
  multiplayerGetState,
  preferences,
  saveActiveGame,
  useGameStore,
  wasmAdapters,
} = vi.hoisted(() => {
  class NativeEngineVersionMismatchError extends Error {
    constructor() {
      super("Native engine version does not match this release");
      this.name = "NativeEngineVersionMismatchError";
    }
  }

  const nativeAdapterInitialize = vi.fn<() => Promise<void>>();
  const preferences = {
    aiArchetypeFilter: "Any",
    aiCoverageFloor: 0,
    aiSeats: [{ difficulty: "Medium", deckId: "Random" }],
    cedhMode: false,
    nativeEngineEnabled: true,
  };
  class WebSocketAdapter {
    private listener: ((event: NativeAdapterEvent) => void) | null = null;
    readonly nativeAiOptions: { aiSeats: Array<{ difficulty: string }> } | undefined;
    dispose = vi.fn();
    onEvent = vi.fn((listener: (event: NativeAdapterEvent) => void) => {
      this.listener = listener;
      return () => {
        this.listener = null;
      };
    });

    constructor(
      _serverUrl: string,
      _mode: string,
      _deck: unknown,
      _joinGameCode?: string,
      _joinPassword?: string,
      _reservationToken?: string,
      _displayName?: string,
      options?: { nativeAi?: { aiSeats: Array<{ difficulty: string }> } },
    ) {
      this.nativeAiOptions = options?.nativeAi;
      nativeAdapters.push(this);
    }

    initialize(): Promise<void> {
      return nativeAdapterInitialize();
    }

    emit(event: NativeAdapterEvent): void {
      this.listener?.(event);
    }
  }
  const nativeAdapters: WebSocketAdapter[] = [];

  class WasmAdapter {
    cardDbLoaded = true;
    initialize = vi.fn(async () => {});
    resetGameState = vi.fn();
  }
  const wasmAdapters: InstanceType<typeof WasmAdapter>[] = [];
  const getSharedAdapter = vi.fn(() => {
    const adapter = new WasmAdapter();
    wasmAdapters.push(adapter);
    return adapter;
  });

  const gameStoreState = {
    adapter: null as unknown,
    gameId: null as string | null,
    gameState: null,
    initGame: vi.fn(async (gameId: string, adapter: { initialize: () => Promise<void> }) => {
      gameStoreState.gameId = gameId;
      gameStoreState.adapter = adapter;
      await adapter.initialize();
    }),
    resumeGame: vi.fn(),
    resumeP2PHost: vi.fn(),
    reset: vi.fn(),
    setEngineMode: vi.fn(),
    setGameMode: vi.fn(),
  };
  const useGameStore = Object.assign(
    vi.fn((selector: (state: typeof gameStoreState) => unknown) => selector(gameStoreState)),
    {
      getState: () => gameStoreState,
      setState: (partial: Record<string, unknown>) => Object.assign(gameStoreState, partial),
      subscribe: vi.fn(() => () => {}),
    },
  );
  const multiplayerGetState = vi.fn();

  return {
    NativeEngineVersionMismatchError,
    WebSocketAdapter,
    WasmAdapter,
    clearActiveGame: vi.fn(),
    ensureNativeEngine: vi.fn(),
    gameStoreState,
    getSharedAdapter,
    nativeAdapterInitialize,
    nativeAdapters,
    multiplayerGetState,
    preferences,
    saveActiveGame: vi.fn(),
    useGameStore,
    wasmAdapters,
  };
});

vi.mock("../../adapter/ws-adapter", () => ({
  NativeEngineVersionMismatchError,
  WebSocketAdapter,
}));

vi.mock("../../adapter/wasm-adapter", () => ({
  WasmAdapter,
  getSharedAdapter,
}));

vi.mock("../../services/nativeEngine", () => ({
  canAttemptNativeEngine: () => true,
  ensureNativeEngine,
  nativeEngineKeyForCurrentOrigin: () => ({ release: { version: "0.0.0-test" } }),
}));

vi.mock("../../services/nativeEngineSocket", () => ({
  NativeEngineSocket: class {},
}));

vi.mock("../../stores/gameStore", () => ({
  clearActiveGame,
  clearGame: vi.fn(),
  clearP2PHostSession: vi.fn(),
  loadActiveGame: vi.fn(() => null),
  loadGame: vi.fn(async () => null),
  loadP2PHostSession: vi.fn(),
  nextGameSessionGeneration: vi.fn(() => 1),
  saveActiveGame,
  useGameStore,
}));

vi.mock("../../constants/storage", () => ({
  ACTIVE_DECK_KEY: "active-deck",
  isRandomDeckSelection: () => false,
  loadActiveDeck: () => ({ main: ["Island"], sideboard: [] }),
  loadSavedDeckBracket: () => null,
}));

vi.mock("../../services/aiDeckCatalog", () => ({
  buildLegalAiDeckCatalog: vi.fn(async () => ({
    candidates: [{ id: "ai-deck", deck: { main: ["Mountain"], sideboard: [] }, bracket: null }],
  })),
}));

vi.mock("../../services/randomDeckSelection", () => ({
  pickRandomDeckCandidate: (candidates: unknown[]) => candidates[0],
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

vi.mock("../../data/formatRegistry", () => ({
  formatSuppliesDeck: () => false,
}));

vi.mock("../../stores/preferencesStore", () => {
  return {
    AI_DECK_RANDOM: "Random",
    usePreferencesStore: Object.assign(vi.fn(), { getState: () => preferences }),
  };
});

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

vi.mock("../../game/sessionCleanup", () => ({
  clearPromptOverlayState: vi.fn(),
}));

vi.mock("../../hooks/useGameplayPreferencesSync", () => ({
  useGameplayPreferencesSync: vi.fn(),
}));

vi.mock("../../audio/AudioManager", () => ({
  audioManager: { setContext: vi.fn() },
}));

vi.mock("../../stores/multiplayerStore", () => ({
  useMultiplayerStore: Object.assign(vi.fn(), { getState: multiplayerGetState, setState: vi.fn() }),
}));

vi.mock("../../stores/multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: { getState: vi.fn() },
}));

vi.mock("../../services/playerAvatars", () => ({
  assignRandomAvatars: vi.fn(),
  avatarCardNameForName: vi.fn(),
  fetchAvatarArtUrl: vi.fn(),
}));

vi.mock("../../services/multiplayerSession", () => ({
  clearWsSession: vi.fn(),
  loadWsSession: vi.fn(() => null),
  saveWsSession: vi.fn(),
}));

vi.mock("../../pwa/updateMarker", () => ({
  consumeRecentAutoUpdateMarker: vi.fn(),
}));

vi.mock("../../services/quickDraftPersistence", () => ({
  loadDraftRun: vi.fn(),
}));

import { GameProvider } from "../GameProvider";

describe("GameProvider native AI routing", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    clearActiveGame.mockReset();
    ensureNativeEngine.mockReset();
    nativeAdapterInitialize.mockReset();
    saveActiveGame.mockReset();
    nativeAdapters.splice(0);
    wasmAdapters.splice(0);
    multiplayerGetState.mockReset();
    preferences.aiSeats = [{ difficulty: "Medium", deckId: "Random" }];
    preferences.cedhMode = false;
    gameStoreState.adapter = null;
    gameStoreState.gameId = null;
    gameStoreState.gameState = null;
    ensureNativeEngine.mockResolvedValue({ port: 9375 });
    nativeAdapterInitialize.mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
  });

  it("falls back to WASM when release parity rejects the native engine", async () => {
    nativeAdapterInitialize.mockRejectedValue(new NativeEngineVersionMismatchError());

    render(
      <GameProvider gameId="native-parity" mode="ai">
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(gameStoreState.setEngineMode).toHaveBeenCalledWith(
        "wasm",
        "server_version_mismatch",
      );
    });
    expect(ensureNativeEngine).toHaveBeenCalledWith({ release: { version: "0.0.0-test" } });
    expect(saveActiveGame).toHaveBeenCalledWith(
      expect.objectContaining({ id: "native-parity", mode: "ai" }),
    );
    expect(wasmAdapters).toHaveLength(1);
  });

  it("does not write a resume pointer for a native game and concedes on exit", async () => {
    const view = render(
      <GameProvider gameId="native-no-resume" mode="ai">
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(gameStoreState.setEngineMode).toHaveBeenCalledWith("native");
    });
    const nativeEngineModeCall = gameStoreState.setEngineMode.mock.calls.findIndex(
      ([mode]) => mode === "native",
    );
    expect(nativeEngineModeCall).toBeGreaterThanOrEqual(0);
    expect(gameStoreState.setEngineMode.mock.invocationCallOrder[nativeEngineModeCall]).toBeLessThan(
      gameStoreState.initGame.mock.invocationCallOrder[0],
    );
    expect(clearActiveGame).toHaveBeenCalledOnce();
    expect(saveActiveGame).not.toHaveBeenCalled();
    expect(multiplayerGetState).not.toHaveBeenCalled();

    view.unmount();
    expect(nativeAdapters).toHaveLength(1);
    expect(nativeAdapters[0].dispose).toHaveBeenCalledWith({ concede: true });
  });

  it("preserves every exact server AI difficulty label from buildLocalAiDeckList", async () => {
    preferences.aiSeats = [
      { difficulty: "VeryEasy", deckId: "Random" },
      { difficulty: "Easy", deckId: "Random" },
      { difficulty: "Medium", deckId: "Random" },
      { difficulty: "Hard", deckId: "Random" },
      { difficulty: "VeryHard", deckId: "Random" },
      { difficulty: "CEDH", deckId: "Random" },
    ];

    render(
      <GameProvider gameId="native-difficulties" mode="ai" playerCount={7}>
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(gameStoreState.setEngineMode).toHaveBeenCalledWith("native");
      expect(nativeAdapters).toHaveLength(1);
    });

    expect(nativeAdapters[0]!.nativeAiOptions?.aiSeats.map((seat) => seat.difficulty)).toEqual([
      "VeryEasy",
      "Easy",
      "Medium",
      "Hard",
      "VeryHard",
      "CEDH",
    ]);
  });

  async function expectNativeTerminalEvent(event: NativeAdapterEvent) {
    const onWsEvent = vi.fn();
    render(
      <GameProvider gameId="native-terminal" mode="ai" onWsEvent={onWsEvent}>
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(gameStoreState.setEngineMode).toHaveBeenCalledWith("native");
      expect(nativeAdapters).toHaveLength(1);
    });

    const nativeAdapter = nativeAdapters[0]!;
    nativeAdapter.emit(event);

    expect(nativeAdapter.dispose).toHaveBeenCalledOnce();
    expect(gameStoreState.adapter).toBeNull();
    expect(onWsEvent).toHaveBeenCalledWith(event);
  }

  it("disposes a native game and surfaces reconnect failure as terminal", async () => {
    await expectNativeTerminalEvent({ type: "reconnectFailed" });
  });

  it("disposes a native game and surfaces bridge errors as terminal", async () => {
    await expectNativeTerminalEvent({ type: "error", message: "WebSocket connection failed" });
  });
});
