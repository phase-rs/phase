import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useMultiplayerStore } from "../../stores/multiplayerStore";

type NativeAdapterEvent =
  | { type: "reconnectFailed" }
  | { type: "error"; message: string }
  // Non-terminal: the server refused a fire-and-forget request. Listed here
  // alongside the two terminal events precisely because `handleNativeEvent`
  // must treat it differently from both.
  | { type: "requestRejected"; reason: string };

const {
  NativeEngineVersionMismatchError,
  WebSocketAdapter,
  WasmAdapter,
  clearActiveGame,
  ensureNativeEngine,
  fetchAvatarArtUrl,
  gameStoreState,
  getSharedAdapter,
  loadActiveGame,
  nativeAdapterInitialize,
  nativeAdapters,
  multiplayerDraftGetState,
  multiplayerGetState,
  multiplayerState,
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
  const fetchAvatarArtUrl = vi.fn<() => Promise<string | null>>();
  const preferences = {
    aiArchetypeFilter: "Any",
    aiCoverageFloor: 0,
    aiSeats: [{ difficulty: "Medium", deckId: "Random" }],
    cedhMode: false,
    nativeEngineEnabled: true,
  };
  type NativePregameReconnect = {
    kind: string;
    gameCode: string;
    playerId: number;
    playerToken: string;
  };
  class WebSocketAdapter {
    private listener: ((event: NativeAdapterEvent) => void) | null = null;
    readonly nativeAiOptions: { aiSeats: Array<{ difficulty: string }> } | undefined;
    readonly nativePregameOptions: NativePregameReconnect | undefined;
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
      options?: {
        nativeAi?: { aiSeats: Array<{ difficulty: string }> };
        nativePregame?: NativePregameReconnect;
      },
    ) {
      this.nativeAiOptions = options?.nativeAi;
      this.nativePregameOptions = options?.nativePregame;
      nativeAdapters.push(this);
    }

    initialize(): Promise<void> {
      return nativeAdapterInitialize();
    }

    // Reconnect adapters echo their supplied creds; a fresh game gets a stable
    // server-issued session so the resume pointer can be persisted.
    get nativeSession(): { gameCode: string; playerId: number; playerToken: string } {
      return this.nativePregameOptions
        ? {
            gameCode: this.nativePregameOptions.gameCode,
            playerId: this.nativePregameOptions.playerId,
            playerToken: this.nativePregameOptions.playerToken,
          }
        : { gameCode: "NATIVE-SESSION", playerId: 0, playerToken: "native-token" };
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
    resumeNativeSolo: vi.fn(async (gameId: string, adapter: { initialize: () => Promise<void> }) => {
      gameStoreState.gameId = gameId;
      gameStoreState.adapter = adapter;
      await adapter.initialize();
    }),
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
    setConnectionStatus: vi.fn(),
    setIsSpectator: vi.fn(),
    setLatency: vi.fn(),
    setSpectators: vi.fn(),
    showToast: vi.fn(),
  };
  const multiplayerGetState = vi.fn(() => multiplayerState);
  const multiplayerDraftGetState = vi.fn(() => ({ matchPairing: null }));

  return {
    NativeEngineVersionMismatchError,
    WebSocketAdapter,
    WasmAdapter,
    clearActiveGame: vi.fn(),
    ensureNativeEngine: vi.fn(),
    fetchAvatarArtUrl,
    gameStoreState,
    getSharedAdapter,
    loadActiveGame: vi.fn<() => Record<string, unknown> | null>(() => null),
    nativeAdapterInitialize,
    nativeAdapters,
    multiplayerDraftGetState,
    multiplayerGetState,
    multiplayerState,
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
  loadActiveGame,
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
  useMultiplayerDraftStore: { getState: multiplayerDraftGetState },
}));

vi.mock("../../services/playerAvatars", () => ({
  assignRandomAvatars: vi.fn(() => [
    { name: "Jace", cardName: "Jace, the Mind Sculptor" },
    { name: "Liliana", cardName: "Liliana of the Veil" },
  ]),
  avatarCardNameForName: vi.fn(),
  fetchAvatarArtUrl,
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

vi.mock("../../services/serverDetection", () => ({
  detectServerUrl: vi.fn(async () => "ws://test-server"),
}));

import { GameProvider } from "../GameProvider";
import { AdapterError, AdapterErrorCode } from "../../adapter/types";
import { clearPromptOverlayState } from "../../game/sessionCleanup";

describe("GameProvider native AI routing", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useGameStore.subscribe.mockReset();
    useGameStore.subscribe.mockImplementation(() => () => {});
    clearActiveGame.mockReset();
    ensureNativeEngine.mockReset();
    fetchAvatarArtUrl.mockReset();
    nativeAdapterInitialize.mockReset();
    saveActiveGame.mockReset();
    loadActiveGame.mockReset();
    loadActiveGame.mockReturnValue(null);
    gameStoreState.resumeNativeSolo.mockClear();
    gameStoreState.initGame.mockClear();
    nativeAdapters.splice(0);
    wasmAdapters.splice(0);
    multiplayerDraftGetState.mockReset();
    multiplayerDraftGetState.mockReturnValue({ matchPairing: null });
    multiplayerGetState.mockReset();
    multiplayerGetState.mockReturnValue(multiplayerState);
    preferences.aiSeats = [{ difficulty: "Medium", deckId: "Random" }];
    preferences.cedhMode = false;
    gameStoreState.adapter = null;
    gameStoreState.gameId = null;
    gameStoreState.gameState = null;
    ensureNativeEngine.mockResolvedValue({ port: 9375 });
    fetchAvatarArtUrl.mockResolvedValue(null);
    nativeAdapterInitialize.mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
    gameStoreState.adapter = null;
    gameStoreState.gameId = null;
    gameStoreState.gameState = null;
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
    // The fallback is otherwise silent, and a version mismatch is a different
    // user problem than an engine that could not start at all.
    expect(multiplayerState.showToast).toHaveBeenCalledWith(
      "Native engine version mismatch — this game is running in-browser.",
    );
  });

  it("writes a native resume pointer and suspends (no concede) on exit", async () => {
    const view = render(
      <GameProvider gameId="native-resume-write" mode="ai">
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

    // A live native game persists a server-authoritative resume pointer carrying
    // the reconnect credentials — the old no-resume contract cleared it instead.
    await waitFor(() => {
      expect(saveActiveGame).toHaveBeenCalledWith(
        expect.objectContaining({
          id: "native-resume-write",
          mode: "ai",
          nativeSession: expect.objectContaining({
            gameCode: "NATIVE-SESSION",
            playerToken: "native-token",
          }),
        }),
      );
    });
    expect(clearActiveGame).not.toHaveBeenCalled();

    view.unmount();
    expect(nativeAdapters).toHaveLength(1);
    // Suspend, not concede: leaving keeps the server session resumable. Only an
    // explicit Concede (useConcedeHandler) ends the game.
    expect(nativeAdapters[0].dispose).toHaveBeenCalledWith();
  });

  it("reconnects to a suspended native game via its resume pointer", async () => {
    loadActiveGame.mockReturnValue({
      id: "native-resume-read",
      mode: "ai",
      difficulty: "Medium",
      nativeSession: { gameCode: "GAME-XYZ", playerId: 0, playerToken: "tok-xyz" },
    });

    render(
      <GameProvider gameId="native-resume-read" mode="ai">
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(gameStoreState.resumeNativeSolo).toHaveBeenCalled();
    });

    // The adapter is built with a reconnect pregame frame carrying the persisted
    // credentials, and the fresh-game `initGame` path is never taken.
    expect(nativeAdapters).toHaveLength(1);
    expect(nativeAdapters[0].nativePregameOptions).toEqual(
      expect.objectContaining({
        kind: "reconnect",
        gameCode: "GAME-XYZ",
        playerId: 0,
        playerToken: "tok-xyz",
      }),
    );
    expect(gameStoreState.resumeNativeSolo).toHaveBeenCalledWith(
      "native-resume-read",
      nativeAdapters[0],
    );
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
  });

  it("surfaces a terminal error when a native resume fails, without a WASM fallback", async () => {
    loadActiveGame.mockReturnValue({
      id: "native-resume-fail",
      mode: "ai",
      difficulty: "Medium",
      nativeSession: { gameCode: "GONE", playerId: 0, playerToken: "tok" },
    });
    // The reconnect handshake fails (e.g. the server no longer holds the game).
    nativeAdapterInitialize.mockRejectedValue(new Error("Reconnect grace period expired"));
    const onWsEvent = vi.fn();

    render(
      <GameProvider gameId="native-resume-fail" mode="ai" onWsEvent={onWsEvent}>
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(onWsEvent).toHaveBeenCalledWith(
        expect.objectContaining({ type: "error", message: "Reconnect grace period expired" }),
      );
    });
    // A resume has no local snapshot, so it must NOT silently fall back to a
    // fresh WASM game (which would look like the suspended game vanished).
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(wasmAdapters).toHaveLength(0);
    // The pointer is kept so the player can retry once the engine is back.
    expect(clearActiveGame).not.toHaveBeenCalled();
  });

  it("uses each commander's name for native AI opponents", async () => {
    gameStoreState.gameId = "native-commander-names";
    gameStoreState.gameState = {
      command_zone: [1, 2],
      objects: {
        1: { name: "Aesi, Tyrant of Gyre Strait", owner: 0, is_commander: true },
        2: { name: "Muldrotha, the Gravetide", owner: 1, is_commander: true },
      },
    } as never;

    render(
      <GameProvider gameId="native-commander-names" mode="ai">
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(useMultiplayerStore.setState).toHaveBeenCalledWith(
        expect.objectContaining({
          playerNames: new Map([[0, "Aesi"], [1, "Muldrotha"]]),
        }),
      );
    });
  });

  it("waits for the new AI game state before assigning commander names", async () => {
    let gameStateListener: ((state: typeof gameStoreState) => void) | undefined;
    useGameStore.subscribe.mockImplementation((listener) => {
      gameStateListener = listener;
      return () => {};
    });
    gameStoreState.gameId = "previous-ai-game";
    gameStoreState.gameState = {
      command_zone: [1, 2],
      objects: {
        1: { name: "Aesi, Tyrant of Gyre Strait", owner: 0, is_commander: true },
        2: { name: "Muldrotha, the Gravetide", owner: 1, is_commander: true },
      },
    } as never;

    render(
      <GameProvider gameId="next-ai-game" mode="ai">
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(gameStoreState.initGame.mock.calls.some(([id]) => id === "next-ai-game")).toBe(true);
    });
    expect(useMultiplayerStore.setState).not.toHaveBeenCalled();

    gameStoreState.gameId = "next-ai-game";
    gameStoreState.gameState = {
      command_zone: [3, 4],
      objects: {
        3: { name: "Tatyova, Benthic Druid", owner: 0, is_commander: true },
        4: { name: "Krenko, Mob Boss", owner: 1, is_commander: true },
      },
    } as never;
    expect(gameStateListener).toBeDefined();
    gameStateListener!(gameStoreState);

    await waitFor(() => {
      expect(useMultiplayerStore.setState).toHaveBeenCalledWith(
        expect.objectContaining({
          playerNames: new Map([[0, "Tatyova"], [1, "Krenko"]]),
        }),
      );
    });
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

    // `native-ai` is set immediately before the game loop starts, so it is the
    // signal that the session is live — the point from which a terminal socket
    // event is a real lost connection rather than a setup failure.
    await waitFor(() => {
      expect(gameStoreState.setGameMode).toHaveBeenCalledWith("native-ai");
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

  // `requestRejected` needs its OWN forwarding branch, not a place in an
  // existing group: `stateChanged` and `gameOver` are handled inline and are
  // never forwarded, so before this change the only `onWsEvent` call in
  // `handleNativeEvent` was the terminal one. Adding the event to that branch
  // would have forwarded it AND destroyed the session — the opposite of the
  // point.
  it("forwards a refused request without disposing the native session", async () => {
    const onWsEvent = vi.fn();
    render(
      <GameProvider gameId="native-request-rejected" mode="ai" onWsEvent={onWsEvent}>
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(gameStoreState.setGameMode).toHaveBeenCalledWith("native-ai");
      expect(nativeAdapters).toHaveLength(1);
    });

    const nativeAdapter = nativeAdapters[0]!;
    const event = {
      type: "requestRejected" as const,
      reason: "There is no previous action of yours to take back",
    };
    nativeAdapter.emit(event);

    // Forwarded, so GamePage can toast it. This is the assertion that fails
    // if the new branch is omitted, and it is what the GamePage-level test
    // (which mocks GameProvider) cannot cover.
    expect(onWsEvent).toHaveBeenCalledWith(event);
    // …and the session survives. `expectNativeTerminalEvent` above asserts
    // the exact opposite of these two for `error`/`reconnectFailed` against
    // the same fixture, which is what makes them a real discrimination
    // rather than a property the harness has anyway.
    expect(nativeAdapter.dispose).not.toHaveBeenCalled();
    expect(gameStoreState.adapter).not.toBeNull();
  });

  it("disposes a native game and surfaces bridge errors as terminal", async () => {
    await expectNativeTerminalEvent({ type: "error", message: "WebSocket connection failed" });
  });

  it("keeps a native setup failure off the terminal connection surface", async () => {
    // The socket dying during the native handshake emits a terminal event and
    // then rejects initialization. The rejection is handled by falling back to
    // WASM, so forwarding the event would leave GamePage's connection-lost
    // banner pinned over the local game that took over.
    const onWsEvent = vi.fn();
    nativeAdapterInitialize.mockImplementation(async () => {
      nativeAdapters[0]!.emit({ type: "reconnectFailed" });
      throw new Error("Connection closed before game started");
    });

    render(
      <GameProvider gameId="native-setup-failure" mode="ai" onWsEvent={onWsEvent}>
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(gameStoreState.setEngineMode).toHaveBeenCalledWith("wasm", expect.anything());
    });
    expect(wasmAdapters).toHaveLength(1);
    expect(onWsEvent).not.toHaveBeenCalled();
    // Silent to the banner, but not silent to the player.
    expect(multiplayerState.showToast).toHaveBeenCalledWith(
      "Native engine unavailable — this game is running in-browser.",
    );
  });

  it("clears prompt overlays when a draft match unmounts", () => {
    gameStoreState.gameId = "draft-match";
    gameStoreState.adapter = {} as never;
    gameStoreState.gameState = {} as never;

    const view = render(
      <GameProvider gameId="draft-match" mode="draft-match">
        <div />
      </GameProvider>,
    );

    vi.mocked(clearPromptOverlayState).mockClear();
    view.unmount();

    expect(clearPromptOverlayState).toHaveBeenCalledOnce();
  });
});

describe("GameProvider online deck rejection", () => {
  it("surfaces only typed deck rejections from online initialization", async () => {
    const onWsEvent = vi.fn();
    nativeAdapterInitialize.mockRejectedValue(
      new AdapterError(AdapterErrorCode.DECK_REJECTED, "Invalid deck contents", false),
    );

    render(
      <GameProvider gameId="online-deck-rejected" mode="online" onWsEvent={onWsEvent}>
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(onWsEvent).toHaveBeenCalledWith({
        type: "deckRejected",
        reason: "Invalid deck contents",
      });
    });

    cleanup();
    onWsEvent.mockClear();
    const connectionStatusCallCount = multiplayerState.setConnectionStatus.mock.calls.length;
    nativeAdapterInitialize.mockRejectedValue(
      new AdapterError(
        AdapterErrorCode.ACTION_REJECTED,
        "Deck not legal for this format",
        true,
      ),
    );

    render(
      <GameProvider gameId="online-action-rejected" mode="online" onWsEvent={onWsEvent}>
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(
        multiplayerState.setConnectionStatus.mock.calls.slice(connectionStatusCallCount),
      ).toContainEqual(["disconnected"]);
    });
    expect(onWsEvent).not.toHaveBeenCalledWith(expect.objectContaining({ type: "deckRejected" }));
  });
});
