import { act, cleanup, render, waitFor } from "@testing-library/react";
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
  clearGameStrict,
  loadGameStrict,
  ensureNativeEngine,
  fetchAvatarArtUrl,
  gameStoreState,
  getSharedAdapter,
  loadActiveGame,
  loadDraftRun,
  inspectActiveQuickDraftLifecycle,
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
    readonly nativeAiOptions:
      | { aiSeats: Array<{ difficulty: string }>; formatConfig?: { starting_life: number } }
      | undefined;
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
        nativeAi?: {
          aiSeats: Array<{ difficulty: string }>;
          formatConfig?: { starting_life: number };
        };
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
    initGame: vi.fn(async (gameId: string, adapter: { initialize: () => Promise<void> }, ..._payload: unknown[]) => {
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
    setActivePlayerId: vi.fn(),
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
    clearGameStrict: vi.fn(async () => undefined),
    loadGameStrict: vi.fn<(_gameId: string) => Promise<unknown>>(async () => null),
    ensureNativeEngine: vi.fn(),
    fetchAvatarArtUrl,
    gameStoreState,
    getSharedAdapter,
    loadActiveGame: vi.fn<() => Record<string, unknown> | null>(() => null),
    loadDraftRun: vi.fn<() => Promise<Record<string, unknown> | null>>(async () => null),
    inspectActiveQuickDraftLifecycle: vi.fn(async () => null as {
      id: string; setCode: string; phase?: string; currentGameId?: string;
    } | null),
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

vi.mock("../../services/gamePersistence", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../services/gamePersistence")>(),
  clearGameStrict,
  loadGameStrict,
}));

vi.mock("../../constants/storage", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../constants/storage")>(),
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
  loadDraftRun,
  inspectActiveQuickDraftLifecycle,
}));

vi.mock("../../services/serverDetection", () => ({
  detectServerUrl: vi.fn(async () => "ws://test-server"),
}));

import type { FormatConfig } from "../../adapter/types";
import { GameProvider } from "../GameProvider";
import { AdapterError, AdapterErrorCode } from "../../adapter/types";
import { clearPromptOverlayState } from "../../game/sessionCleanup";
import { createGameLoopController } from "../../game/controllers/gameLoopController";
import { loadGame } from "../../stores/gameStore";

const UNAVAILABLE = "This draft run is unavailable. End Run to draft again.";

function publishedPayload(playerDeck = ["Player"], opponentDeck = ["Opponent"], pool?: string[] | null) {
  return {
    player: { main_deck: playerDeck, sideboard: [], commander: [] },
    opponent: { main_deck: opponentDeck, sideboard: [], commander: [] },
    ai_decks: [],
    ...(pool === undefined ? {} : { booster_pack_pool: pool }),
  };
}

function seedSoloRun(draftId: string, gameId: string, options: {
  playerDeck?: string[]; opponentDeck?: string[]; pool?: string[] | null;
  setCode?: string; phase?: string; currentGameId?: string;
} = {}) {
  const playerDeck = options.playerDeck ?? ["Player"];
  const opponentDeck = options.opponentDeck ?? ["Opponent"];
  const run = {
    format: "run", results: [], usedBotSeats: [1], playerDeck, opponentDeck,
    ...(options.pool === undefined ? {} : { booster_pack_pool: options.pool }),
    activeMatch: { draftId, gameId, format: "run", resultCountAtLaunch: 0, botSeat: 1, opponentDeck },
  };
  inspectActiveQuickDraftLifecycle.mockResolvedValue({
    id: draftId, setCode: options.setCode ?? "TST",
    phase: options.phase ?? "playing", currentGameId: options.currentGameId ?? gameId,
  });
  loadDraftRun.mockResolvedValue(run);
  return run;
}

describe("GameProvider native AI routing", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    clearGameStrict.mockResolvedValue(undefined);
    loadGameStrict.mockReset();
    loadGameStrict.mockResolvedValue(null);
    vi.mocked(loadGame).mockReset();
    vi.mocked(loadGame).mockResolvedValue(null);
    sessionStorage.clear();
    loadDraftRun.mockReset();
    loadDraftRun.mockResolvedValue(null);
    inspectActiveQuickDraftLifecycle.mockResolvedValue(null);
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

  it.each([
    ["original", ["Cube A", "Cube A", "Undealt sentinel"]],
    ["upgraded legacy Cube", []],
    ["ordinary legacy run", undefined],
  ])("recovers the %s durable booster source after the handoff is gone", async (_label, pool) => {
    const playerDeck = ["Drafted player card"];
    const opponentDeck = ["Drafted opponent card"];
    loadDraftRun.mockResolvedValue({
      format: "run", results: [], usedBotSeats: [1], playerDeck, opponentDeck,
      booster_pack_pool: pool,
      activeMatch: {
        draftId: "cube-run", gameId: "recovered-cube", format: "run",
        resultCountAtLaunch: 0, botSeat: 1, opponentDeck,
      },
    });
    inspectActiveQuickDraftLifecycle.mockResolvedValue({ id: "cube-run", setCode: pool === undefined ? "TST" : "custom-cube" });
    render(<GameProvider gameId="recovered-cube" mode="ai" source="draft" draftId="cube-run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledOnce());
    expect(getSharedAdapter).toHaveBeenCalled();
    expect(gameStoreState.initGame.mock.calls[0][2]).toMatchObject({
      player: { main_deck: playerDeck }, opponent: { main_deck: opponentDeck },
      booster_pack_pool: pool,
    });
    expect(loadDraftRun).toHaveBeenCalledWith("cube-run");
    expect(ensureNativeEngine).not.toHaveBeenCalled();
  });

  it("retains exact solo handoff bytes and engine reason until a playable retry", async () => {
    const key = "phase:draft-deck:ante-game";
    seedSoloRun("ante-run", "ante-game", { playerDeck: ["Contract from Below"] });
    const raw = '{ "player": {"main_deck":["Contract from Below"],"sideboard":[],"commander":[]}, "opponent": {"main_deck":["Opponent"],"sideboard":[],"commander":[]}, "ai_decks":[] }';
    sessionStorage.setItem(key, raw);
    const reason = "Contract from Below is not legal without ante";
    gameStoreState.initGame.mockRejectedValueOnce(new Error(reason));
    const onNoDeck = vi.fn();
    const first = render(<GameProvider gameId="ante-game" mode="ai" source="draft" draftId="ante-run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(reason));
    expect(gameStoreState.initGame).toHaveBeenCalledOnce();
    expect(sessionStorage.getItem(key)).toBe(raw);
    first.unmount();

    render(<GameProvider gameId="ante-game" mode="ai" source="draft" draftId="ante-run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(sessionStorage.getItem(key)).toBeNull());
    const controllers = vi.mocked(createGameLoopController).mock.results;
    expect(controllers[controllers.length - 1]?.value.start).toHaveBeenCalled();
  });

  it("waits for initial durability before starting and consuming a raw draft handoff", async () => {
    const key = "phase:draft-deck:durable-raw";
    seedSoloRun("run", "durable-raw");
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem(key, raw);
    let finish!: () => void;
    gameStoreState.initGame.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    render(<GameProvider gameId="durable-raw" mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledOnce());
    expect(gameStoreState.initGame.mock.calls[0][7]).toBe("strict");
    expect(createGameLoopController).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(key)).toBe(raw);
    finish();
    await waitFor(() => expect(sessionStorage.getItem(key)).toBeNull());
    const controllers = vi.mocked(createGameLoopController).mock.results;
    expect(controllers[controllers.length - 1]?.value.start).toHaveBeenCalled();
  });

  it("reports initial write failure, keeps raw handoff, and retries the same game", async () => {
    const key = "phase:draft-deck:write-retry";
    seedSoloRun("run", "write-retry");
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem(key, raw);
    const reason = "IndexedDB initial write failed";
    gameStoreState.initGame.mockRejectedValueOnce(new Error(reason));
    const onNoDeck = vi.fn();
    const first = render(<GameProvider gameId="write-retry" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(reason));
    expect(gameStoreState.initGame.mock.calls[0][7]).toBe("strict");
    expect(createGameLoopController).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(key)).toBe(raw);
    first.unmount();
    render(<GameProvider gameId="write-retry" mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(sessionStorage.getItem(key)).toBeNull());
    expect(gameStoreState.initGame).toHaveBeenCalledTimes(2);
    expect(gameStoreState.initGame.mock.calls[1][7]).toBe("strict");
  });

  it("waits for strict durability in the run-only fallback and retries a failed write", async () => {
    seedSoloRun("run", "stage-write");
    let rejectWrite!: (error: Error) => void;
    gameStoreState.initGame.mockImplementationOnce(() => new Promise<void>((_resolve, reject) => { rejectWrite = reject; }));
    const onNoDeck = vi.fn();
    const first = render(<GameProvider gameId="stage-write" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledOnce());
    expect(gameStoreState.initGame.mock.calls[0][7]).toBe("strict");
    expect(createGameLoopController).not.toHaveBeenCalled();
    rejectWrite(new Error("IndexedDB stage write failed"));
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith("IndexedDB stage write failed"));
    expect(createGameLoopController).not.toHaveBeenCalled();
    first.unmount();
    render(<GameProvider gameId="stage-write" mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(createGameLoopController).toHaveBeenCalledOnce());
    expect(gameStoreState.initGame).toHaveBeenCalledTimes(2);
    expect(gameStoreState.initGame.mock.calls[1][7]).toBe("strict");
  });

  it("does not replay a retained raw handoff after End Run removes its authority", async () => {
    const key = "phase:draft-deck:ended-game";
    const raw = JSON.stringify(publishedPayload());
    seedSoloRun("ended-run", "ended-game");
    sessionStorage.setItem(key, raw);
    gameStoreState.initGame.mockRejectedValueOnce(new Error("exact engine error"));
    const firstError = vi.fn();
    const first = render(<GameProvider gameId="ended-game" mode="ai" source="draft" draftId="ended-run" onNoDeck={firstError}><div /></GameProvider>);
    await waitFor(() => expect(firstError).toHaveBeenCalledWith("exact engine error"));
    expect(gameStoreState.initGame).toHaveBeenCalledOnce();
    expect(sessionStorage.getItem(key)).toBe(raw);
    first.unmount();

    inspectActiveQuickDraftLifecycle.mockResolvedValue(null);
    loadDraftRun.mockResolvedValue(null);
    const afterEndRun = vi.fn();
    render(<GameProvider gameId="ended-game" mode="ai" source="draft" draftId="ended-run" onNoDeck={afterEndRun}><div /></GameProvider>);
    await waitFor(() => expect(afterEndRun).toHaveBeenCalledWith(UNAVAILABLE));
    expect(gameStoreState.initGame).toHaveBeenCalledOnce();
    expect(vi.mocked(createGameLoopController)).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(key)).toBe(raw);
  });

  it.each([false, true])("restores a progressed solo snapshot with raw handoff=%s", async (withRaw) => {
    const savedState = { players: [{}, {}], turn: 7 } as never;
    const key = "phase:draft-deck:progressed";
    seedSoloRun("run", "progressed", { phase: "launching", currentGameId: "old-game" });
    if (withRaw) sessionStorage.setItem(key, JSON.stringify(publishedPayload()));
    loadGameStrict.mockResolvedValue(savedState);
    const first = render(<GameProvider gameId="progressed" mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.resumeGame).toHaveBeenCalledWith("progressed", expect.anything(), savedState));
    const controllers = vi.mocked(createGameLoopController).mock.results;
    expect(controllers[controllers.length - 1]?.value.start).toHaveBeenCalled();
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(key)).toBeNull();
    first.unmount();
    render(<GameProvider gameId="progressed" mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.resumeGame).toHaveBeenCalledTimes(2));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
  });

  it.each([false, true])("requires a live run before solo saved restore with raw handoff=%s", async (withRaw) => {
    const savedState = { players: [{}, {}], turn: 7 } as never;
    const key = "phase:draft-deck:ended-saved";
    if (withRaw) sessionStorage.setItem(key, JSON.stringify(publishedPayload()));
    loadGameStrict.mockResolvedValueOnce(savedState);
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="ended-saved" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(UNAVAILABLE));
    expect(loadGameStrict).toHaveBeenCalledWith("ended-saved");
    expect(gameStoreState.resumeGame).not.toHaveBeenCalled();
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(clearGameStrict).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(key)).toBe(withRaw ? JSON.stringify(publishedPayload()) : null);
  });

  it.each([false, true].flatMap((withRaw) =>
    ["metadata ID", "draft ID", "game ID", "resolved stage", "Cube source"].map((change) => [withRaw, change] as const),
  ))("rejects mismatched solo saved authority with raw handoff=%s and %s", async (withRaw, change) => {
    const run = seedSoloRun("run", "saved-authority");
    if (change === "metadata ID") inspectActiveQuickDraftLifecycle.mockResolvedValue({ id: "other", setCode: "TST" });
    if (change === "draft ID") loadDraftRun.mockResolvedValue({ ...run, activeMatch: { ...run.activeMatch, draftId: "other" } });
    if (change === "game ID") loadDraftRun.mockResolvedValue({ ...run, activeMatch: { ...run.activeMatch, gameId: "different" } });
    if (change === "resolved stage") loadDraftRun.mockResolvedValue({ ...run, results: [{ gameId: "saved-authority", result: "win" }] });
    if (change === "Cube source") inspectActiveQuickDraftLifecycle.mockResolvedValue({ id: "run", setCode: "custom-cube" });
    const raw = JSON.stringify(publishedPayload());
    if (withRaw) sessionStorage.setItem("phase:draft-deck:saved-authority", raw);
    loadGameStrict.mockResolvedValueOnce({ players: [{}, {}] } as never);
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="saved-authority" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(UNAVAILABLE));
    expect(loadGameStrict).toHaveBeenCalledWith("saved-authority");
    expect(gameStoreState.resumeGame).not.toHaveBeenCalled();
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(clearGameStrict).not.toHaveBeenCalled();
    expect(sessionStorage.getItem("phase:draft-deck:saved-authority")).toBe(withRaw ? raw : null);
  });

  it('uses exact raw handoff and engine error when snapshot is missing', async () => {
    seedSoloRun("run", "raw-retry");
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem("phase:draft-deck:raw-retry", raw);
    const reason = "Exact engine ante error";
    gameStoreState.initGame.mockRejectedValueOnce(new Error(reason));
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="raw-retry" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(reason));
    expect(gameStoreState.initGame.mock.calls[0][2]).toEqual(publishedPayload());
    expect(sessionStorage.getItem("phase:draft-deck:raw-retry")).toBe(raw);
  });

  it.each([false, true])("stops solo startup on a strict read error with raw handoff=%s", async (withRaw) => {
    const gameId = withRaw ? "raw-read-error" : "staged-read-error";
    seedSoloRun("run", gameId);
    const raw = JSON.stringify(publishedPayload());
    if (withRaw) sessionStorage.setItem(`phase:draft-deck:${gameId}`, raw);
    const readError = new Error("saved game read failed");
    loadGameStrict.mockRejectedValueOnce(readError);
    const onNoDeck = vi.fn();
    const onResumeReset = vi.fn();
    const first = render(<GameProvider gameId={gameId} mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck} onResumeReset={onResumeReset}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(readError.message));
    expect(loadGameStrict).toHaveBeenCalledWith(gameId);
    expect(loadDraftRun).not.toHaveBeenCalled();
    expect(gameStoreState.resumeGame).not.toHaveBeenCalled();
    expect(clearGameStrict).not.toHaveBeenCalled();
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(createGameLoopController).not.toHaveBeenCalled();
    expect(onResumeReset).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(`phase:draft-deck:${gameId}`)).toBe(withRaw ? raw : null);
    first.unmount();

    const savedState = { players: [{}, {}], turn: 12 } as never;
    loadGameStrict.mockResolvedValueOnce(savedState);
    const restored = render(<GameProvider gameId={gameId} mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.resumeGame).toHaveBeenCalledWith(gameId, expect.anything(), savedState));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    restored.unmount();

    loadGameStrict.mockResolvedValueOnce(null);
    render(<GameProvider gameId={gameId} mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledOnce());
    expect(gameStoreState.initGame.mock.calls[0][2]).toMatchObject(publishedPayload());
  });

  it("does not report a deferred strict read rejection after unmount", async () => {
    seedSoloRun("run", "late-read-error");
    let rejectRead!: (error: Error) => void;
    loadGameStrict.mockReturnValueOnce(new Promise((_resolve, reject) => { rejectRead = reject; }));
    const onNoDeck = vi.fn();
    const mounted = render(<GameProvider gameId="late-read-error" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(loadGameStrict).toHaveBeenCalledWith("late-read-error"));
    mounted.unmount();
    await act(async () => { rejectRead(new Error("read failed after unmount")); });
    expect(onNoDeck).not.toHaveBeenCalled();
    expect(loadDraftRun).not.toHaveBeenCalled();
    expect(gameStoreState.resumeGame).not.toHaveBeenCalled();
    expect(clearGameStrict).not.toHaveBeenCalled();
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
  });

  it("does not restore after a deferred saved run authority read resolves post-unmount", async () => {
    const run = seedSoloRun("run", "authority-delayed");
    loadGameStrict.mockResolvedValueOnce({ players: [{}, {}] } as never);
    let resolveRun!: (value: typeof run) => void;
    loadDraftRun.mockReturnValueOnce(new Promise((resolve) => { resolveRun = resolve; }));
    const onNoDeck = vi.fn();
    const mounted = render(<GameProvider gameId="authority-delayed" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(loadDraftRun).toHaveBeenCalledWith("run"));
    mounted.unmount();
    await act(async () => { resolveRun(run); });
    expect(gameStoreState.resumeGame).not.toHaveBeenCalled();
    expect(createGameLoopController).not.toHaveBeenCalled();
    expect(onNoDeck).not.toHaveBeenCalled();
  });

  it("restores a saved snapshot again when raw handoff removal fails", async () => {
    seedSoloRun("run", "retry-removal");
    const key = "phase:draft-deck:retry-removal";
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem(key, raw);
    const savedState = { players: [{}, {}], turn: 11 } as never;
    loadGameStrict.mockResolvedValue(savedState);
    const originalRemove = sessionStorage.removeItem.bind(sessionStorage);
    const removal = vi.spyOn(sessionStorage, "removeItem").mockImplementation((item) => {
      if (item === key) throw new Error("storage removal failed");
      return originalRemove(item);
    });
    const onNoDeck = vi.fn();
    try {
      const first = render(<GameProvider gameId="retry-removal" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
      await waitFor(() => expect(gameStoreState.resumeGame).toHaveBeenCalledOnce());
      expect(sessionStorage.getItem(key)).toBe(raw);
      expect(onNoDeck).not.toHaveBeenCalled();
      first.unmount();
      removal.mockRestore();
      render(<GameProvider gameId="retry-removal" mode="ai" source="draft" draftId="run"><div /></GameProvider>);
      await waitFor(() => expect(gameStoreState.resumeGame).toHaveBeenCalledTimes(2));
      expect(gameStoreState.initGame).not.toHaveBeenCalled();
    } finally {
      removal.mockRestore();
    }
  });

  it("keeps replacement handoff bytes written while a saved solo game restores", async () => {
    seedSoloRun("run", "saved-replaced");
    const key = "phase:draft-deck:saved-replaced";
    const oldRaw = JSON.stringify(publishedPayload(["Old"]));
    const replacement = JSON.stringify(publishedPayload(["Replacement"]));
    sessionStorage.setItem(key, oldRaw);
    loadGameStrict.mockResolvedValueOnce({ players: [{}, {}] } as never);
    let finishRestore!: () => void;
    gameStoreState.resumeGame.mockImplementationOnce(() => new Promise<void>((resolve) => { finishRestore = resolve; }));
    render(<GameProvider gameId="saved-replaced" mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.resumeGame).toHaveBeenCalledOnce());
    expect(sessionStorage.getItem(key)).toBe(oldRaw);
    sessionStorage.setItem(key, replacement);
    finishRestore();
    await waitFor(() => expect(createGameLoopController).toHaveBeenCalledOnce());
    expect(sessionStorage.getItem(key)).toBe(replacement);
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
  });

  it("does not restore after a deferred solo saved load resolves post-unmount", async () => {
    seedSoloRun("run", "delayed");
    let resolveLoad!: (state: never) => void;
    loadGameStrict.mockReturnValueOnce(new Promise((resolve) => { resolveLoad = resolve; }));
    const onNoDeck = vi.fn();
    const mounted = render(<GameProvider gameId="delayed" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(loadGameStrict).toHaveBeenCalledWith("delayed"));
    mounted.unmount();
    await act(async () => { resolveLoad({ players: [{}, {}] } as never); });
    expect(loadDraftRun).not.toHaveBeenCalled();
    expect(gameStoreState.resumeGame).not.toHaveBeenCalled();
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(onNoDeck).not.toHaveBeenCalled();
  });

  it("does not initialize a staged fallback after its run read resolves post-unmount", async () => {
    const run = seedSoloRun("run", "staged-delayed");
    let resolveRun!: (value: typeof run) => void;
    loadDraftRun.mockReturnValueOnce(new Promise((resolve) => { resolveRun = resolve; }));
    const onNoDeck = vi.fn();
    const mounted = render(<GameProvider gameId="staged-delayed" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(loadDraftRun).toHaveBeenCalledWith("run"));
    mounted.unmount();
    await act(async () => { resolveRun(run); });
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(createGameLoopController).not.toHaveBeenCalled();
    expect(onNoDeck).not.toHaveBeenCalled();
  });

  it.each(["raw", "durable"])("uses the unresolved run despite stale metadata on the %s path", async (path) => {
    seedSoloRun("stale-run", "current-game", { phase: "launching", currentGameId: "prior-game" });
    if (path === "raw") sessionStorage.setItem("phase:draft-deck:current-game", JSON.stringify(publishedPayload()));
    render(<GameProvider gameId="current-game" mode="ai" source="draft" draftId="stale-run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledOnce());
    expect(gameStoreState.initGame.mock.calls[0][2]).toMatchObject(publishedPayload());
  });

  it("rejects a wrong active draft ID even when the run and payload match", async () => {
    seedSoloRun("run", "game");
    inspectActiveQuickDraftLifecycle.mockResolvedValue({ id: "other-run", setCode: "TST" });
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem("phase:draft-deck:game", raw);
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="game" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(UNAVAILABLE));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(sessionStorage.getItem("phase:draft-deck:game")).toBe(raw);
  });

  it.each(["draft ID", "game ID", "resolved stage"])("rejects raw handoff with wrong %s authority", async (change) => {
    const run = seedSoloRun("run", "game");
    if (change === "draft ID") loadDraftRun.mockResolvedValue({ ...run, activeMatch: { ...run.activeMatch, draftId: "other" } });
    if (change === "game ID") loadDraftRun.mockResolvedValue({ ...run, activeMatch: { ...run.activeMatch, gameId: "other" } });
    if (change === "resolved stage") loadDraftRun.mockResolvedValue({ ...run, results: [{ gameId: "game", result: "win" }] });
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem("phase:draft-deck:game", raw);
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="game" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(UNAVAILABLE));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(sessionStorage.getItem("phase:draft-deck:game")).toBe(raw);
  });

  it("requires a Cube source array for a raw handoff and preserves duplicate order", async () => {
    seedSoloRun("cube", "cube-game", { setCode: "custom-cube" });
    const key = "phase:draft-deck:cube-game";
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem(key, raw);
    const onNoDeck = vi.fn();
    const first = render(<GameProvider gameId="cube-game" mode="ai" source="draft" draftId="cube" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(UNAVAILABLE));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(key)).toBe(raw);
    first.unmount();

    const pool = ["Cube A", "Cube A", "Cube B"];
    seedSoloRun("cube", "cube-game", { setCode: "custom-cube", pool });
    sessionStorage.setItem(key, JSON.stringify(publishedPayload(["Player"], ["Opponent"], pool)));
    render(<GameProvider gameId="cube-game" mode="ai" source="draft" draftId="cube"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledOnce());
    expect(gameStoreState.initGame.mock.calls[0][2]).toMatchObject({ booster_pack_pool: pool });
  });

  it("rejects a raw Cube source whose duplicates or order differ from the run", async () => {
    const pool = ["Cube A", "Cube A", "Cube B"];
    seedSoloRun("cube", "cube-game", { setCode: "custom-cube", pool });
    const key = "phase:draft-deck:cube-game";
    const raw = JSON.stringify(publishedPayload(["Player"], ["Opponent"], ["Cube A", "Cube B", "Cube A"]));
    sessionStorage.setItem(key, raw);
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="cube-game" mode="ai" source="draft" draftId="cube" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(UNAVAILABLE));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(key)).toBe(raw);
  });

  it("accepts a published null booster source on an ordinary run", async () => {
    seedSoloRun("run", "game", { pool: null });
    sessionStorage.setItem("phase:draft-deck:game", JSON.stringify(publishedPayload(["Player"], ["Opponent"], null)));
    render(<GameProvider gameId="game" mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledOnce());
    expect(gameStoreState.initGame.mock.calls[0][2]).toMatchObject({ booster_pack_pool: null });
  });

  const invalidPayloads: Array<[string, (payload: ReturnType<typeof publishedPayload>) => unknown]> = [
    ["player main deck", (p) => ({ ...p, player: { ...p.player, main_deck: ["Other"] } })],
    ["opponent main deck", (p) => ({ ...p, opponent: { ...p.opponent, main_deck: ["Other"] } })],
    ["player sideboard", (p) => ({ ...p, player: { ...p.player, sideboard: ["Other"] } })],
    ["opponent sideboard", (p) => ({ ...p, opponent: { ...p.opponent, sideboard: ["Other"] } })],
    ["player commander", (p) => ({ ...p, player: { ...p.player, commander: ["Other"] } })],
    ["opponent commander", (p) => ({ ...p, opponent: { ...p.opponent, commander: ["Other"] } })],
    ["AI decks", (p) => ({ ...p, ai_decks: [{ main_deck: ["Other"] }] })],
    ["missing player", (p) => ({ opponent: p.opponent, ai_decks: [] })],
    ["non-array AI decks", (p) => ({ ...p, ai_decks: {} })],
    ["top-level engine field", (p) => ({ ...p, draft_set_codes: ["TST"] })],
    ["nested engine field", (p) => ({ ...p, player: { ...p.player, companion: ["Other"] } })],
    ["extra booster source", (p) => ({ ...p, booster_pack_pool: ["Other"] })],
  ];
  it.each(invalidPayloads)("rejects a raw handoff with mismatched %s", async (_label, mutate) => {
    seedSoloRun("run", "game");
    const raw = JSON.stringify(mutate(publishedPayload()));
    sessionStorage.setItem("phase:draft-deck:game", raw);
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="game" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(UNAVAILABLE));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(sessionStorage.getItem("phase:draft-deck:game")).toBe(raw);
  });

  it("retains raw bytes after a transient authority read failure and starts on retry", async () => {
    seedSoloRun("run", "game");
    const key = "phase:draft-deck:game";
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem(key, raw);
    loadDraftRun.mockRejectedValueOnce(new Error("IDB unavailable"));
    const onNoDeck = vi.fn();
    const first = render(<GameProvider gameId="game" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(UNAVAILABLE));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(key)).toBe(raw);
    first.unmount();

    render(<GameProvider gameId="game" mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledOnce());
    await waitFor(() => expect(sessionStorage.getItem(key)).toBeNull());
    const controllers = vi.mocked(createGameLoopController).mock.results;
    expect(controllers[controllers.length - 1]?.value.start).toHaveBeenCalled();
  });

  it("refuses malformed solo handoff without consuming it or loading an unrelated deck", async () => {
    const key = "phase:draft-deck:bad-json";
    sessionStorage.setItem(key, "{broken");
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="bad-json" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(expect.stringContaining("JSON")));
    expect(sessionStorage.getItem(key)).toBe("{broken");
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
  });

  it("reports a failed handoff read without starting an unrelated deck", async () => {
    const originalGetItem = sessionStorage.getItem.bind(sessionStorage);
    const read = vi.spyOn(sessionStorage, "getItem").mockImplementation((key) => {
      if (key === "phase:draft-deck:unreadable") throw new Error("session read failed");
      return originalGetItem(key);
    });
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="unreadable" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith("session read failed"));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    read.mockRestore();
  });

  it("restores a saved solo draft when reading its optional handoff throws", async () => {
    seedSoloRun("run", "saved-unreadable");
    const savedState = { players: [{}, {}], turn: 12 } as never;
    loadGameStrict.mockResolvedValueOnce(savedState);
    const key = "phase:draft-deck:saved-unreadable";
    const originalGetItem = sessionStorage.getItem.bind(sessionStorage);
    const read = vi.spyOn(sessionStorage, "getItem").mockImplementation((item) => {
      if (item === key) throw new Error("session read failed");
      return originalGetItem(item);
    });
    const onNoDeck = vi.fn();
    try {
      render(<GameProvider gameId="saved-unreadable" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
      await waitFor(() => expect(gameStoreState.resumeGame).toHaveBeenCalledWith("saved-unreadable", expect.anything(), savedState));
      await waitFor(() => expect(createGameLoopController).toHaveBeenCalledOnce());
      expect(vi.mocked(createGameLoopController).mock.results[0]?.value.start).toHaveBeenCalled();
      expect(loadGameStrict).toHaveBeenCalledWith("saved-unreadable");
      expect(read).toHaveBeenCalledWith(key);
      expect(gameStoreState.initGame).not.toHaveBeenCalled();
      expect(clearGameStrict).not.toHaveBeenCalled();
      expect(onNoDeck).not.toHaveBeenCalled();
    } finally {
      read.mockRestore();
    }
  });

  it("retains a handoff when controller start throws", async () => {
    const key = "phase:draft-deck:controller-failure";
    seedSoloRun("run", "controller-failure");
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem(key, raw);
    vi.mocked(createGameLoopController).mockImplementationOnce(() => ({
      start: vi.fn(() => { throw new Error("controller failed"); }),
      stop: vi.fn(), dispose: vi.fn(),
    } as unknown as ReturnType<typeof createGameLoopController>));
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="controller-failure" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith("controller failed"));
    expect(gameStoreState.initGame).toHaveBeenCalledOnce();
    expect(sessionStorage.getItem(key)).toBe(raw);
  });

  it.each(["consume read", "consume removal"])("keeps a playable draft started when the %s throws", async (failure) => {
    const key = `phase:draft-deck:${failure}`;
    seedSoloRun("run", failure);
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem(key, raw);
    const originalGetItem = sessionStorage.getItem.bind(sessionStorage);
    const read = vi.spyOn(sessionStorage, "getItem").mockImplementation((item) => {
      if (failure === "consume read" && item === key && gameStoreState.initGame.mock.calls.length > 0) {
        throw new Error("consume read failed");
      }
      return originalGetItem(item);
    });
    const removal = vi.spyOn(sessionStorage, "removeItem").mockImplementation(() => {
      if (failure === "consume removal") throw new Error("consume removal failed");
    });
    const onNoDeck = vi.fn();
    render(<GameProvider gameId={failure} mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => {
      const controllers = vi.mocked(createGameLoopController).mock.results;
      expect(controllers[controllers.length - 1]?.value.start).toHaveBeenCalled();
    });
    expect(gameStoreState.initGame).toHaveBeenCalledOnce();
    expect(onNoDeck).not.toHaveBeenCalled();
    expect(originalGetItem(key)).toBe(raw);
    read.mockRestore();
    removal.mockRestore();
  });

  it.each([
    { label: "ordinary local", source: undefined, mode: "local" as const },
    { label: "Commander", source: "multiplayer" as const, mode: "ai" as const },
  ])("restores a saved $label game when handoff storage cannot be read", async ({ source, mode }) => {
    const savedState = { players: [{}, {}] } as never;
    vi.mocked(loadGame).mockResolvedValueOnce(savedState);
    const read = vi.spyOn(sessionStorage, "getItem").mockImplementation(() => {
      throw new Error("session read failed");
    });
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="saved-local" mode={mode} source={source} onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.resumeGame).toHaveBeenCalledWith("saved-local", expect.anything(), savedState));
    const controllers = vi.mocked(createGameLoopController).mock.results;
    expect(controllers[controllers.length - 1]?.value.start).toHaveBeenCalled();
    expect(onNoDeck).not.toHaveBeenCalled();
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(read).not.toHaveBeenCalled();
    read.mockRestore();
  });

  it("keeps new handoff bytes written while the old initialization waits", async () => {
    const key = "phase:draft-deck:in-flight";
    seedSoloRun("run", "in-flight", { playerDeck: ["Old"] });
    const oldRaw = JSON.stringify(publishedPayload(["Old"]));
    const newRaw = JSON.stringify(publishedPayload(["New"]));
    sessionStorage.setItem(key, oldRaw);
    let finish!: () => void;
    const pending = new Promise<void>((resolve) => { finish = resolve; });
    gameStoreState.initGame.mockImplementationOnce(async () => { await pending; });
    render(<GameProvider gameId="in-flight" mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledOnce());
    sessionStorage.setItem(key, newRaw);
    finish();
    await waitFor(() => expect(vi.mocked(createGameLoopController)).toHaveBeenCalled());
    expect(sessionStorage.getItem(key)).toBe(newRaw);
  });

  it("keeps the solo handoff when its in-flight initialization is cancelled", async () => {
    const key = "phase:draft-deck:cancelled";
    seedSoloRun("run", "cancelled");
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem(key, raw);
    let finish!: () => void;
    const pending = new Promise<void>((resolve) => { finish = resolve; });
    gameStoreState.initGame.mockImplementationOnce(async () => { await pending; });
    const onNoDeck = vi.fn();
    const mounted = render(<GameProvider gameId="cancelled" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledOnce());
    mounted.unmount();
    finish();
    await Promise.resolve();
    expect(sessionStorage.getItem(key)).toBe(raw);
    expect(onNoDeck).not.toHaveBeenCalled();
  });

  it("does not initialize after an unresolved draft authority read is cancelled", async () => {
    const key = "phase:draft-deck:cancelled-read";
    const run = seedSoloRun("run", "cancelled-read");
    const authority = await import("../../stores/draftStore");
    const checkAuthority = vi.spyOn(authority, "isCoherentUnresolvedDraftStage");
    const raw = JSON.stringify(publishedPayload());
    sessionStorage.setItem(key, raw);
    let finish!: (value: typeof run) => void;
    const pending = new Promise<typeof run>((resolve) => { finish = resolve; });
    loadDraftRun.mockReturnValueOnce(pending);
    const onNoDeck = vi.fn();
    try {
      const mounted = render(<GameProvider gameId="cancelled-read" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
      await waitFor(() => expect(loadDraftRun).toHaveBeenCalledWith("run"));
      mounted.unmount();
      await act(async () => { finish(run); });
      await waitFor(() => expect(checkAuthority).toHaveBeenCalled());
      expect(gameStoreState.initGame).not.toHaveBeenCalled();
      expect(vi.mocked(createGameLoopController)).not.toHaveBeenCalled();
      expect(onNoDeck).not.toHaveBeenCalled();
      expect(sessionStorage.getItem(key)).toBe(raw);
    } finally {
      checkAuthority.mockRestore();
    }
  });

  it("refuses a wrong game ID in an otherwise durable solo run", async () => {
    inspectActiveQuickDraftLifecycle.mockResolvedValue({ id: "run", setCode: "TST" });
    loadDraftRun.mockResolvedValue({ format: "run", results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1],
      activeMatch: { draftId: "run", gameId: "different-game", format: "run",
        resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] } });
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="requested-game" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(UNAVAILABLE));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
  });

  it("refuses a resolved stage without starting an unrelated deck", async () => {
    inspectActiveQuickDraftLifecycle.mockResolvedValue({ id: "run", setCode: "TST" });
    loadDraftRun.mockResolvedValue({ format: "run", results: [{ gameId: "finished-game", result: "win" }],
      playerDeck: ["Player"], opponentDeck: ["Opponent"], usedBotSeats: [1],
      activeMatch: { draftId: "run", gameId: "finished-game", format: "run",
        resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] } });
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="finished-game" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(UNAVAILABLE));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
  });

  it.each([
    ["card database unavailable", false],
    ["card database unavailable", true],
    ["incompatible snapshot", false],
    ["incompatible snapshot", true],
  ])("preserves a progressed solo save after %s with raw handoff=%s and retries it", async (message, withRaw) => {
    const gameId = `restore-retry-${message.replace(/ /g, "-")}-${withRaw}`;
    const key = `phase:draft-deck:${gameId}`;
    const raw = JSON.stringify(publishedPayload());
    if (withRaw) sessionStorage.setItem(key, raw);
    seedSoloRun("run", gameId);
    const savedState = { players: [{}, {}], turn: 13 } as never;
    loadGameStrict.mockResolvedValue(savedState);
    gameStoreState.resumeGame.mockRejectedValueOnce(new Error(message));
    const onNoDeck = vi.fn();
    const onResumeReset = vi.fn();
    const first = render(<GameProvider gameId={gameId} mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck} onResumeReset={onResumeReset}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(message));
    expect(loadGameStrict).toHaveBeenCalledWith(gameId);
    expect(gameStoreState.resumeGame).toHaveBeenCalledOnce();
    expect(gameStoreState.resumeGame).toHaveBeenCalledWith(gameId, expect.anything(), savedState);
    expect(onResumeReset).not.toHaveBeenCalled();
    expect(clearGameStrict).not.toHaveBeenCalled();
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(createGameLoopController).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(key)).toBe(withRaw ? raw : null);
    first.unmount();

    const second = render(<GameProvider gameId={gameId} mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck} onResumeReset={onResumeReset}><div /></GameProvider>);
    await waitFor(() => expect(createGameLoopController).toHaveBeenCalledOnce());
    expect(loadGameStrict).toHaveBeenCalledTimes(2);
    expect(gameStoreState.resumeGame).toHaveBeenCalledTimes(2);
    expect(gameStoreState.resumeGame).toHaveBeenNthCalledWith(2, gameId, expect.anything(), savedState);
    expect(vi.mocked(createGameLoopController).mock.results[0]?.value.start).toHaveBeenCalledOnce();
    expect(onNoDeck).toHaveBeenCalledTimes(1);
    expect(onResumeReset).not.toHaveBeenCalled();
    expect(clearGameStrict).not.toHaveBeenCalled();
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    await waitFor(() => expect(sessionStorage.getItem(key)).toBeNull());
    second.unmount();
  });

  it("reports a saved solo restore failure even when its optional handoff cannot be read", async () => {
    const gameId = "unreadable-restore";
    seedSoloRun("run", gameId);
    const savedState = { players: [{}, {}], turn: 8 } as never;
    loadGameStrict.mockResolvedValueOnce(savedState);
    const error = new Error("card database unavailable");
    gameStoreState.resumeGame.mockRejectedValueOnce(error);
    const originalGet = sessionStorage.getItem.bind(sessionStorage);
    const read = vi.spyOn(sessionStorage, "getItem").mockImplementation((key) => {
      if (key === `phase:draft-deck:${gameId}`) throw new Error("handoff read failed");
      return originalGet(key);
    });
    const onNoDeck = vi.fn();
    try {
      render(<GameProvider gameId={gameId} mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
      await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(error.message));
      expect(gameStoreState.resumeGame).toHaveBeenCalledWith(gameId, expect.anything(), savedState);
      expect(clearGameStrict).not.toHaveBeenCalled();
      expect(gameStoreState.initGame).not.toHaveBeenCalled();
      expect(createGameLoopController).not.toHaveBeenCalled();
    } finally {
      read.mockRestore();
    }
  });

  it.each([false, true])("does not reset a solo saved game when restore rejects after unmount with raw handoff=%s", async (withRaw) => {
    seedSoloRun("run", "late-reject");
    const raw = JSON.stringify(publishedPayload());
    if (withRaw) sessionStorage.setItem("phase:draft-deck:late-reject", raw);
    loadGameStrict.mockResolvedValueOnce({ players: [{}, {}] } as never);
    let rejectResume!: (error: Error) => void;
    gameStoreState.resumeGame.mockReturnValueOnce(new Promise((_resolve, reject) => { rejectResume = reject; }));
    const onResumeReset = vi.fn();
    const onNoDeck = vi.fn();
    const mounted = render(<GameProvider gameId="late-reject" mode="ai" source="draft" draftId="run" onResumeReset={onResumeReset} onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.resumeGame).toHaveBeenCalledOnce());
    mounted.unmount();
    await act(async () => { rejectResume(new Error("late incompatibility")); });
    expect(onResumeReset).not.toHaveBeenCalled();
    expect(clearGameStrict).not.toHaveBeenCalled();
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(onNoDeck).not.toHaveBeenCalled();
    expect(sessionStorage.getItem("phase:draft-deck:late-reject")).toBe(withRaw ? raw : null);
  });

  it.each([false, true])("retains solo snapshot when restored controller fails with raw handoff=%s", async (withRaw) => {
    seedSoloRun("run", "controller-restore");
    const raw = JSON.stringify(publishedPayload());
    if (withRaw) sessionStorage.setItem("phase:draft-deck:controller-restore", raw);
    const savedState = { players: [{}, {}], turn: 9 } as never;
    loadGameStrict.mockResolvedValue(savedState);
    vi.mocked(createGameLoopController).mockReturnValueOnce({
      start: vi.fn(() => { throw new Error("controller startup failed"); }),
      stop: vi.fn(), dispose: vi.fn(),
    } as never);
    const onNoDeck = vi.fn();
    const first = render(<GameProvider gameId="controller-restore" mode="ai" source="draft" draftId="run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith("controller startup failed"));
    expect(clearGameStrict).not.toHaveBeenCalled();
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
    expect(sessionStorage.getItem("phase:draft-deck:controller-restore")).toBe(withRaw ? raw : null);
    first.unmount();
    render(<GameProvider gameId="controller-restore" mode="ai" source="draft" draftId="run"><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.resumeGame).toHaveBeenCalledTimes(2));
    expect(gameStoreState.resumeGame).toHaveBeenLastCalledWith("controller-restore", expect.anything(), savedState);
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
  });

  it("refuses a Cube fallback with no original booster source", async () => {
    inspectActiveQuickDraftLifecycle.mockResolvedValue({ id: "cube-run", setCode: "custom-cube" });
    loadDraftRun.mockResolvedValue({ format: "run", results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1],
      activeMatch: { draftId: "cube-run", gameId: "cube-game", format: "run",
        resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] } });
    const onNoDeck = vi.fn();
    render(<GameProvider gameId="cube-game" mode="ai" source="draft" draftId="cube-run" onNoDeck={onNoDeck}><div /></GameProvider>);
    await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(UNAVAILABLE));
    expect(gameStoreState.initGame).not.toHaveBeenCalled();
  });

  it.each([7, 8])("consumes all %i Commander seats and cube metadata on the desktop local route", async (playerCount) => {
    const deck = (seat: number) => ({ main_deck: [`Draft seat ${seat}`], sideboard: [], commander: [`Legend ${seat}`] });
    const payload = {
      player: deck(0), opponent: deck(1),
      ai_decks: Array.from({ length: playerCount - 2 }, (_, i) => deck(i + 2)),
      booster_pack_pool: ["Cube A", "Cube A", "Undealt sentinel"],
    };
    const key = `phase:draft-deck:commander-${playerCount}`;
    sessionStorage.setItem(key, JSON.stringify(payload));
    render(<GameProvider gameId={`commander-${playerCount}`} mode="ai" source="multiplayer" playerCount={playerCount}><div /></GameProvider>);
    await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalledOnce());
    expect(getSharedAdapter).toHaveBeenCalled();
    expect(gameStoreState.initGame.mock.calls[0][2]).toEqual(payload);
    expect(gameStoreState.initGame.mock.calls[0][4]).toBe(playerCount);
    expect(sessionStorage.getItem(key)).toBeNull();
    expect(nativeAdapterInitialize).not.toHaveBeenCalled();
    expect(ensureNativeEngine).not.toHaveBeenCalled();
    expect(loadDraftRun).not.toHaveBeenCalled();
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
      nativeSession: { gameCode: "GAME-XYZ", playerId: 0, playerToken: "tok-xyz", fullKey: { game_code: "GAME-XYZ", generation: 1 } },
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
      nativeSession: { gameCode: "GONE", playerId: 0, playerToken: "tok", fullKey: { game_code: "GONE", generation: 1 } },
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

  // The Tauri solo route writes no resume pointer, so `GameSetupPage` hands its
  // edited config over on the navigation and `GamePage` reads it back out of
  // router state. This pins the last hop of that hand-over: whatever config the
  // route resolved must reach the native adapter, or the sidecar starts the
  // game on the format's default life instead of the user's.
  it("passes the route's starting life to the native engine", async () => {
    // Spelled out rather than spread from `FORMAT_DEFAULTS`: the store is
    // mocked in this file, so the real registry is not reachable here.
    const formatConfig: FormatConfig = {
      format: "Commander",
      starting_life: 25,
      min_players: 2,
      max_players: 4,
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

    render(
      <GameProvider gameId="native-starting-life" mode="ai" formatConfig={formatConfig}>
        <div />
      </GameProvider>,
    );

    await waitFor(() => {
      expect(gameStoreState.setEngineMode).toHaveBeenCalledWith("native");
      expect(nativeAdapters).toHaveLength(1);
    });

    expect(nativeAdapters[0]!.nativeAiOptions?.formatConfig?.starting_life).toBe(25);
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

  // `activePlayerId` is written only from a wire and had no clear, so it
  // outlived the game that assigned it and the NEXT wire-assigned game read the
  // previous game's seat until its own assignment landed. `SeatSource` does not
  // cover this: it is keyed on mode, not on session.
  it("drops the wire-assigned seat when a draft match unmounts", () => {
    gameStoreState.gameId = "draft-match";
    gameStoreState.adapter = {} as never;
    gameStoreState.gameState = {} as never;

    const view = render(
      <GameProvider gameId="draft-match" mode="draft-match">
        <div />
      </GameProvider>,
    );

    multiplayerState.setActivePlayerId.mockClear();
    view.unmount();

    expect(multiplayerState.setActivePlayerId).toHaveBeenCalledWith(null);
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
