import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { detectAndParseDeck } from "../../services/deckParser";
import type { ArtChainEntry, CardArtOverride } from "../../stores/preferencesStore";
import type { PrintingEntry } from "../../services/scryfall";
import type * as Scryfall from "../../services/scryfall";
import type { FormatConfig } from "../../adapter/types";

const art = vi.hoisted(() => ({
  face: vi.fn(), printings: vi.fn(), bounds: vi.fn(), activeDeck: vi.fn(),
  hostPayloads: [] as unknown[], guestArguments: [] as unknown[][],
  joinRoom: vi.fn(), loadP2PSession: vi.fn(),
  preferences: { nativeEngineEnabled: false, artChain: [] as ArtChainEntry[], artOverrides: {} as Record<string, CardArtOverride> },
}));
vi.mock("../../services/scryfall", async (original) => ({
  ...await original<typeof Scryfall>(), getCardPrintings: art.printings,
}));

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
  clearGameStrict,
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
    setStateWithheld: vi.fn(),
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
    loadP2PTerminalResult: vi.fn<() => Promise<unknown>>(async () => null),
    loadFullTerminalCleanup: vi.fn<() => Promise<unknown>>(async () => null),
    clearFullTerminalCleanup: vi.fn(async () => undefined),
    clearGameStrict: vi.fn(async () => undefined),
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
  P2PGuestAdapter: class {
    constructor(...args: unknown[]) {
      art.guestArguments.push(args);
      return createAdapter();
    }
  },
  P2PHostAdapter: class {
    constructor(...args: unknown[]) {
      art.hostPayloads.push(args[0]);
      return createAdapter(args);
    }
  },
}));

vi.mock("../../adapter/wasm-adapter", () => ({
  WasmAdapter: class {},
  getSharedAdapter: () => ({ getCardFaceData: art.face, checkDeckPrintingsBounds: art.bounds }),
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
  clearGameStrict,
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
  loadActiveDeck: () => art.activeDeck(),
  loadSavedDeckBracket: () => null,
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
  joinRoom: art.joinRoom,
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
  loadP2PSession: art.loadP2PSession,
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
    getState: () => art.preferences,
  }),
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

import { GameProvider } from "../GameProvider";

const CATALOG: PrintingEntry[] = ["aaa", "bbb", "ccc"].map((set, index) => ({
  id: set, set, set_name: set, collector_number: `00${index + 1}`, released_at: `200${index}-01-01`,
  border_color: "black", frame_effects: [], full_art: false, faces: [],
}));
const retainedHost = { player: { main_deck: ["Island"], sideboard: [], printings: [
  { section: "main_deck", name: "Island", count: 1, printing: { set_code: "ccc", collector_number: "003" } },
] }, opponent: { main_deck: [], sideboard: [] }, ai_decks: [] };
describe("GameProvider P2P host lifecycle", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    art.preferences.artChain = [];
    art.preferences.artOverrides = {};
    art.hostPayloads.length = 0;
    art.guestArguments.length = 0;
    art.face.mockReset().mockImplementation(async (name: string) => ({ scryfall_oracle_id: name }));
    art.printings.mockReset().mockResolvedValue(CATALOG);
    art.bounds.mockReset().mockResolvedValue(undefined);
    art.activeDeck.mockReturnValue(detectAndParseDeck("1 Island (AAA) 001"));
    art.loadP2PSession.mockReset().mockResolvedValue(null);
    art.joinRoom.mockReset().mockResolvedValue({ conn: { peer: "host-peer" }, peer: { destroy: vi.fn() } });
    takeActiveP2PHost.mockReset().mockReturnValue(null);
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
      hostDeckData: retainedHost,
    });
    loadP2PTerminalResult.mockResolvedValue(null);
    loadFullTerminalCleanup.mockResolvedValue(null);
    clearFullTerminalCleanup.mockResolvedValue(undefined);
    clearGameStrict.mockResolvedValue(undefined);
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
    expect(art.hostPayloads).toEqual([retainedHost]);
    expect(art.face).not.toHaveBeenCalled();
  });

  it("maps the adapter's withheld-state notice to the banner and clears it on unmount", async () => {
    const adapter = createAdapter();
    takeActiveP2PHost.mockReturnValueOnce(adapter);
    const mount = render(
      <GameProvider gameId="p2p-game" mode="p2p-host">
        <div />
      </GameProvider>,
    );
    await waitFor(() => expect(adapter.onEvent).toHaveBeenCalled());
    const listener = vi.mocked(adapter.onEvent).mock.calls[0][0] as (event: unknown) => void;
    const notice = { revision: 4, reason: "State update exceeds the delivery bound" };

    listener({ type: "stateDelivery", withheld: notice });
    expect(multiplayerStore.setStateWithheld).toHaveBeenLastCalledWith(notice);
    listener({ type: "stateDelivery", withheld: null });
    expect(multiplayerStore.setStateWithheld).toHaveBeenLastCalledWith(null);
    listener({ type: "stateDelivery", withheld: notice });
    mount.unmount();
    expect(multiplayerStore.setStateWithheld).toHaveBeenLastCalledWith(null);
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

  it.each(["p2p-host", "p2p-join"] as const)("V4a/V4b %s binds a fresh import and fences cancelled preparation", async (mode) => {
    loadGame.mockResolvedValue(null);
    loadP2PHostSession.mockResolvedValue(null);
    for (const cancel of [false, true]) {
      let release!: (value: PrintingEntry[]) => void;
      const pending = new Promise<PrintingEntry[]>((resolve) => { release = resolve; });
      art.preferences.artOverrides = {};
      art.printings.mockClear().mockReturnValue(pending);
      gameStore.initGame.mockClear();
      art.hostPayloads.length = 0;
      art.guestArguments.length = 0;
      hostRoom.mockClear();
      const mounted = render(<GameProvider gameId="printing-peer" mode={mode} joinCode="ABCDE"><div /></GameProvider>);
      await waitFor(() => expect(art.printings).toHaveBeenCalledOnce());
      art.preferences.artOverrides = { Island: { scryfallId: "bbb", setCode: "bbb", collectorNumber: "002" } };
      if (cancel) mounted.unmount();
      await act(async () => { release(CATALOG); });
      if (cancel) {
        expect(gameStore.initGame).not.toHaveBeenCalled();
        expect(art.hostPayloads).toEqual([]);
        expect(art.guestArguments).toEqual([]);
        if (mode === "p2p-host") expect(hostRoom).not.toHaveBeenCalled();
        else {
          const resources = await art.joinRoom.mock.results[art.joinRoom.mock.results.length - 1].value;
          expect(resources.peer.destroy).toHaveBeenCalledOnce();
        }
      } else {
        await waitFor(() => expect(gameStore.initGame).toHaveBeenCalledOnce());
        const payload = mode === "p2p-host" ? art.hostPayloads[0] : art.guestArguments[0][0];
        expect(payload).toMatchObject({ player: { main_deck: ["Island"], printings: [
          { section: "main_deck", name: "Island", count: 1, printing: { set_code: "aaa", collector_number: "001" } },
        ] } });
        mounted.unmount();
      }
    }
  });

  it.each(["p2p-host", "p2p-join"] as const)("F3 %s keeps names, printing and bracket from one active deck", async (mode) => {
    loadGame.mockResolvedValue(null);
    loadP2PHostSession.mockResolvedValue(null);
    localStorage.setItem("phase-deck:A", JSON.stringify({ bracket: 2 }));
    localStorage.setItem("phase-deck:B", JSON.stringify({ bracket: 4 }));
    localStorage.setItem("phase-active-deck", "A");
    let release!: (value: PrintingEntry[]) => void;
    art.printings.mockReturnValueOnce(new Promise<PrintingEntry[]>((resolve) => { release = resolve; }));
    const first = render(<GameProvider gameId="bracket-A" mode={mode} joinCode="ABCDE"><div /></GameProvider>);
    await waitFor(() => expect(art.printings).toHaveBeenCalledOnce());
    art.activeDeck.mockReturnValue(detectAndParseDeck("2 Mountain (BBB) 002"));
    localStorage.setItem("phase-active-deck", "B");
    await act(async () => { release(CATALOG); });
    await waitFor(() => expect(gameStore.initGame).toHaveBeenCalledOnce());
    expect(mode === "p2p-host" ? art.hostPayloads[0] : art.guestArguments[0][0]).toMatchObject({ player: {
      main_deck: ["Island"], bracket_tier: "core",
      printings: [{ section: "main_deck", name: "Island", count: 1, printing: { set_code: "aaa", collector_number: "001" } }],
    } });
    first.unmount();
    render(<GameProvider gameId="bracket-B" mode={mode} joinCode="ABCDE"><div /></GameProvider>);
    await waitFor(() => expect(gameStore.initGame).toHaveBeenCalledTimes(2));
    expect(mode === "p2p-host" ? art.hostPayloads[1] : art.guestArguments[1][0]).toMatchObject({ player: {
      main_deck: ["Mountain", "Mountain"], bracket_tier: "optimized",
      printings: [{ section: "main_deck", name: "Mountain", count: 2, printing: { set_code: "bbb", collector_number: "002" } }],
    } });
    localStorage.removeItem("phase-active-deck");
    localStorage.removeItem("phase-deck:A");
    localStorage.removeItem("phase-deck:B");
  });

  it.each(["supplied", "reconnect"] as const)("V4a %s guest reaches setup without deck selection", async (route) => {
    const authority = { sessionKey: "retained-authority" };
    if (route === "reconnect") art.loadP2PSession.mockResolvedValue({ playerToken: "retained-token", authority });
    render(<GameProvider gameId="guest-no-art" mode="p2p-join" joinCode="ABCDE"
      formatConfig={route === "supplied" ? { format: "Dandan" } as FormatConfig : undefined}><div /></GameProvider>);
    await waitFor(() => expect(gameStore.initGame).toHaveBeenCalledOnce());
    expect(art.guestArguments[0][0]).toMatchObject({ player: { main_deck: [], sideboard: [] } });
    expect(art.guestArguments[0][4]).toBe(route === "reconnect" ? "retained-token" : undefined);
    if (route === "reconnect") expect(art.guestArguments[0][8]).toBe(authority);
    expect(art.face).not.toHaveBeenCalled();
    expect(art.joinRoom).toHaveBeenCalledWith("ABCDE", expect.any(AbortSignal));
  });

  it("V12 resumes retained C without selection while the next fresh host uses B", async () => {
    art.preferences.artOverrides = { Island: { scryfallId: "bbb", setCode: "bbb", collectorNumber: "002" } };
    art.activeDeck.mockReturnValue(null);
    const resumed = render(<GameProvider gameId="resumed-art" mode="p2p-host"><div /></GameProvider>);
    await waitFor(() => expect(gameStore.resumeP2PHost).toHaveBeenCalledOnce());
    expect(art.hostPayloads[0]).toBe(retainedHost);
    expect(art.face).not.toHaveBeenCalled();
    resumed.unmount();
    art.activeDeck.mockReturnValue(detectAndParseDeck("1 Island (AAA) 001"));
    loadGame.mockResolvedValue(null);
    loadP2PHostSession.mockResolvedValue(null);
    render(<GameProvider gameId="fresh-art" mode="p2p-host"><div /></GameProvider>);
    await waitFor(() => expect(gameStore.initGame).toHaveBeenCalledOnce());
    expect(art.hostPayloads[1]).toMatchObject({ player: { printings: [
      { section: "main_deck", name: "Island", count: 1, printing: { set_code: "bbb", collector_number: "002" } },
    ] } });
  });

  it("V4b host fences the final peer acquisition and releases a late peer", async () => {
    loadGame.mockResolvedValue(null);
    loadP2PHostSession.mockResolvedValue(null);
    for (const cancel of [false, true]) {
      gameStore.initGame.mockClear();
      art.hostPayloads.length = 0;
      hostRoom.mockClear();
      const resources = { peer: { id: "late-peer", destroy: vi.fn() }, roomCode: "ABCDE",
        onGuestConnected: vi.fn(() => () => {}), destroy: vi.fn() };
      let release!: () => void;
      hostRoom.mockImplementationOnce(() => new Promise((resolve) => { release = () => resolve(resources); }));
      const mounted = render(<GameProvider gameId="host-peer-fence" mode="p2p-host"><div /></GameProvider>);
      await waitFor(() => expect(hostRoom).toHaveBeenCalledOnce());
      expect(art.bounds).toHaveBeenCalled();
      if (cancel) mounted.unmount();
      await act(async () => { release(); });
      if (cancel) {
        expect(gameStore.initGame).not.toHaveBeenCalled();
        expect(art.hostPayloads).toEqual([]);
        expect(resources.destroy).toHaveBeenCalledOnce();
      } else {
        await waitFor(() => expect(gameStore.initGame).toHaveBeenCalledOnce());
        expect(resources.destroy).not.toHaveBeenCalled();
        mounted.unmount();
      }
    }
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
    // Each restart first finishes the resume-record deletion the owner was
    // written ahead of, then sends ACKs.
    expect(clearGameStrict).toHaveBeenCalledTimes(2);
    expect(clearGameStrict).toHaveBeenCalledWith("p2p-game");
    expect(clearGameStrict.mock.invocationCallOrder[0]).toBeLessThan(
      acknowledgeTerminalDelivery.mock.invocationCallOrder[0],
    );
  });

  it("keeps cleanup authority when finishing the resume-record deletion fails", async () => {
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
    nativeEngineKeyForCurrentOrigin.mockReturnValue({ release: { version: "test-release" } });
    clearGameStrict.mockRejectedValueOnce(new Error("resume record deletion failed"));
    const onP2PEvent = vi.fn();
    render(
      <GameProvider gameId="p2p-game" mode="p2p-host" onP2PEvent={onP2PEvent}>
        <div />
      </GameProvider>,
    );
    await waitFor(() => expect(onP2PEvent).toHaveBeenCalledWith({
      type: "terminalUnavailable",
      message: "resume record deletion failed",
    }));
    expect(onP2PEvent).toHaveBeenCalledWith({ type: "terminalResult", result: terminal });
    expect(clearGameStrict).toHaveBeenCalledWith("p2p-game");
    expect(readFullTerminalResult).not.toHaveBeenCalled();
    expect(acknowledgeTerminalDelivery).not.toHaveBeenCalled();
    expect(clearFullTerminalCleanup).not.toHaveBeenCalled();
    expect(adapters).toHaveLength(0);
    expect(hostRoom).not.toHaveBeenCalled();
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
