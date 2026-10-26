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
  P2PGuestAdapter: class {
    constructor(...args: unknown[]) {
      art.guestArguments.push(args);
      return createAdapter();
    }
  },
  P2PHostAdapter: class {
    constructor(payload: unknown) {
      art.hostPayloads.push(payload);
      return createAdapter();
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
  loadP2PSession: art.loadP2PSession,
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
