import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Deck = { main_deck: string[] };
type DeckList = { player: Deck; opponent: Deck; ai_decks: Deck[] };

const {
  deckSupplyForFormat,
  gameStoreState,
  native,
  recorded,
  useGameStore,
  multiplayerState,
} = vi.hoisted(() => {
  const recorded = {
    p2pHost: [] as DeckList[],
    p2pGuest: [] as DeckList[],
    ws: [] as { mode: string; deck: Deck; opts?: { nativeAi?: { aiSeats: { deck: Deck }[] } } }[],
  };
  const gameStoreState = {
    adapter: null as unknown,
    gameId: null as string | null,
    gameState: null,
    initGame: vi.fn(async (..._args: unknown[]) => {}),
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
      subscribe: vi.fn(() => () => {}),
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
    takeActiveP2PHost: vi.fn(() => null),
    openBroker: vi.fn(),
    closeBroker: vi.fn(),
  };
  return {
    deckSupplyForFormat: vi.fn(async (format: string) =>
      format === "Dandan" ? "HostPile" : format === "Momir" ? "EngineFixed" : "PlayerBuilt"),
    gameStoreState,
    native: { enabled: false },
    recorded,
    useGameStore,
    multiplayerState,
  };
});

vi.mock("../../services/engineRuntime", () => ({ deckSupplyForFormat }));

vi.mock("../../adapter/p2p-adapter", () => {
  const adapter = () => ({ dispose: vi.fn(), onEvent: vi.fn(() => () => {}) });
  return {
    P2PHostAdapter: class {
      constructor(deckList: DeckList) {
        recorded.p2pHost.push(deckList);
        return adapter();
      }
    },
    P2PGuestAdapter: class {
      constructor(deckList: DeckList) {
        recorded.p2pGuest.push(deckList);
        return adapter();
      }
    },
  };
});

vi.mock("../../adapter/wasm-adapter", () => ({
  WasmAdapter: class {},
  getSharedAdapter: () => ({ cardDbLoaded: true }),
}));

vi.mock("../../adapter/ws-adapter", () => ({
  NativeEngineVersionMismatchError: class extends Error {},
  WebSocketAdapter: class {
    dispose = vi.fn();
    onEvent = vi.fn(() => () => {});
    initialize = vi.fn(async () => {});
    tryReconnect = vi.fn(() => true);
    constructor(_url: string, mode: string, deck: Deck, ...rest: unknown[]) {
      recorded.ws.push({ mode, deck, opts: rest[4] as never });
    }
  },
  bootstrapFullTerminalDelivery: vi.fn(async () => null),
  readFullTerminalResult: vi.fn(async () => null),
  acknowledgeFullTerminalDelivery: vi.fn(async () => undefined),
}));

vi.mock("../../services/fullTerminalResult", () => ({
  loadFullTerminalDelivery: vi.fn(async () => null),
  commitFullTerminalDelivery: vi.fn(async () => true),
  replaceFullTerminalDelivery: vi.fn(async () => true),
}));

vi.mock("../../services/nativeEngine", () => ({
  canAttemptNativeEngine: () => native.enabled,
  ensureNativeEngine: vi.fn(async () => {}),
  nativeEngineKeyForCurrentOrigin: () => (native.enabled ? { dev: true } : null),
}));

vi.mock("../../services/nativeEngineSocket", () => ({ NativeEngineSocket: class {} }));

vi.mock("../../stores/gameStore", () => ({
  clearActiveGame: vi.fn(),
  clearGame: vi.fn(),
  clearP2PHostSession: vi.fn(),
  loadActiveGame: vi.fn(() => null),
  loadGame: vi.fn(async () => null),
  loadP2PHostSession: vi.fn(async () => null),
  nextGameSessionGeneration: vi.fn(() => 1),
  saveActiveGame: vi.fn(),
  useGameStore,
}));

vi.mock("../../network/connection", () => ({
  hostRoom: vi.fn(async () => ({
    peer: { id: "host-peer", destroy: vi.fn() },
    roomCode: "ABCDE",
    onGuestConnected: vi.fn(() => () => {}),
    destroy: vi.fn(),
  })),
  joinRoom: vi.fn(async () => ({ conn: { peer: "host-peer" }, peer: { destroy: vi.fn() } })),
}));

vi.mock("../../services/p2pSession", () => ({ loadP2PSession: vi.fn(async () => null) }));
vi.mock("../../services/p2pTerminalResult", () => ({ loadP2PTerminalResult: vi.fn(async () => null) }));
vi.mock("../../services/aiDeckCatalog", () => ({ buildLegalAiDeckCatalog: vi.fn(async () => ({ candidates: [] })) }));
vi.mock("../../stores/preferencesStore", () => ({
  AI_DECK_RANDOM: "Random",
  usePreferencesStore: Object.assign(vi.fn(), {
    getState: () => ({ aiSeats: [], cedhMode: false, nativeEngineEnabled: true }),
  }),
}));
vi.mock("../../services/cedhLock", () => ({ effectiveAiDifficulty: (difficulty: string) => difficulty }));
vi.mock("../../game/controllers/gameLoopController", () => ({
  createGameLoopController: vi.fn(() => ({ start: vi.fn(), dispose: vi.fn(), stop: vi.fn() })),
}));
vi.mock("../../game/dispatch", () => ({ dispatchAction: vi.fn(), processRemoteUpdate: vi.fn() }));
vi.mock("../../game/sessionCleanup", () => ({ clearPromptOverlayState: vi.fn() }));
vi.mock("../../hooks/useGameplayPreferencesSync", () => ({ useGameplayPreferencesSync: vi.fn() }));
vi.mock("../../audio/AudioManager", () => ({ audioManager: { setContext: vi.fn() } }));
vi.mock("../../stores/multiplayerStore", () => ({
  useMultiplayerStore: Object.assign(vi.fn(), { getState: () => multiplayerState, setState: vi.fn() }),
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
  loadWsSession: vi.fn(() => null),
  saveWsSession: vi.fn(),
}));
vi.mock("../../pwa/updateMarker", () => ({ consumeRecentAutoUpdateMarker: vi.fn() }));
vi.mock("../../services/quickDraftPersistence", () => ({
  loadDraftRun: vi.fn(),
  inspectActiveQuickDraftLifecycle: vi.fn(),
}));
vi.mock("../../services/serverDetection", () => ({ detectServerUrl: vi.fn(async () => "ws://test-server") }));

import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../../constants/storage";
import { formatMetadata } from "../../data/formatRegistry";
import type { GameFormat } from "../../adapter/types";
import { GameProvider } from "../GameProvider";

const PILE = Array<string>(80).fill("Island");
const ACTIVE = Array<string>(60).fill("Plains");
const NOT_LOADED = "Could not load deck. Try re-importing it.";

function config(format: GameFormat) {
  return formatMetadata(format)!.default_config;
}

function renderRoute(props: Partial<Parameters<typeof GameProvider>[0]> & { mode: Parameters<typeof GameProvider>[0]["mode"] }) {
  const onNoDeck = vi.fn();
  render(
    <GameProvider gameId="g1" onNoDeck={onNoDeck} {...props}>
      <div />
    </GameProvider>,
  );
  return onNoDeck;
}

async function localAiDeckList(): Promise<DeckList> {
  await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalled());
  return gameStoreState.initGame.mock.calls[0][2] as DeckList;
}

describe("GameProvider host-supplied pile routes", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    recorded.p2pHost.splice(0);
    recorded.p2pGuest.splice(0);
    recorded.ws.splice(0);
    native.enabled = false;
    gameStoreState.adapter = null;
    gameStoreState.gameId = null;
    localStorage.setItem(`${STORAGE_KEY_PREFIX}Pile A`, JSON.stringify({ main: [{ name: "Island", count: 80 }], sideboard: [] }));
    localStorage.setItem(`${STORAGE_KEY_PREFIX}Active`, JSON.stringify({ main: [{ name: "Plains", count: 60 }], sideboard: [] }));
    localStorage.setItem(ACTIVE_DECK_KEY, "Active");
  });
  afterEach(cleanup);

  describe("supplied format", () => {
    it("local AI puts the named pile on the player seat and nothing elsewhere", async () => {
      renderRoute({ mode: "ai", formatConfig: config("Dandan"), pile: "Pile A" });
      const deckList = await localAiDeckList();
      expect(deckList.player.main_deck).toEqual(PILE);
      expect(deckList.opponent.main_deck).toEqual([]);
    });

    it("local AI submits empty for the default pile", async () => {
      renderRoute({ mode: "ai", formatConfig: config("Dandan") });
      expect((await localAiDeckList()).player.main_deck).toEqual([]);
    });

    it("local AI submits empty for an engine-fixed format with a named pile", async () => {
      renderRoute({ mode: "ai", formatConfig: config("Momir"), pile: "Pile A" });
      expect((await localAiDeckList()).player.main_deck).toEqual([]);
    });

    it("native AI carries the named pile on the player seat and empty AI seats", async () => {
      native.enabled = true;
      renderRoute({ mode: "ai", formatConfig: config("Dandan"), pile: "Pile A" });
      await waitFor(() => expect(recorded.ws).toHaveLength(1));
      expect(recorded.ws[0].deck.main_deck).toEqual(PILE);
      expect(recorded.ws[0].opts?.nativeAi?.aiSeats.map(({ deck }) => deck.main_deck)).toEqual([[]]);
    });

    it("a vanished named pile reports the deck could not load", async () => {
      const onNoDeck = renderRoute({ mode: "ai", formatConfig: config("Dandan"), pile: "Gone" });
      await waitFor(() => expect(onNoDeck).toHaveBeenCalledWith(NOT_LOADED));
      expect(gameStoreState.initGame).not.toHaveBeenCalled();
    });

    it("a fresh P2P host submits the pile, not the active deck", async () => {
      renderRoute({ mode: "p2p-host", formatConfig: config("Dandan"), pile: "Pile A" });
      await waitFor(() => expect(recorded.p2pHost).toHaveLength(1));
      expect(recorded.p2pHost[0].player.main_deck).toEqual(PILE);
    });

    it("a P2P guest submits empty although an active deck exists", async () => {
      renderRoute({ mode: "p2p-join", joinCode: "ABCDE", formatConfig: config("Dandan") });
      await waitFor(() => expect(recorded.p2pGuest).toHaveLength(1));
      expect(recorded.p2pGuest[0].player.main_deck).toEqual([]);
    });

    it("an online guest submits empty although an active deck exists", async () => {
      renderRoute({ mode: "online", joinCode: "ABC123", formatConfig: config("Dandan") });
      await waitFor(() => expect(recorded.ws).toHaveLength(1));
      expect(recorded.ws[0].mode).toBe("join");
      expect(recorded.ws[0].deck.main_deck).toEqual([]);
    });
  });

  it("a deckless P2P guest with no format dials with an empty deck", async () => {
    localStorage.removeItem(ACTIVE_DECK_KEY);
    const onNoDeck = renderRoute({ mode: "p2p-join", joinCode: "ABCDE" });
    await waitFor(() => expect(recorded.p2pGuest).toHaveLength(1));
    expect(recorded.p2pGuest[0].player.main_deck).toEqual([]);
    expect(onNoDeck).not.toHaveBeenCalled();
  });

  describe("player-built format", () => {
    it("a P2P guest submits its active deck", async () => {
      renderRoute({ mode: "p2p-join", joinCode: "ABCDE", formatConfig: config("Standard") });
      await waitFor(() => expect(recorded.p2pGuest).toHaveLength(1));
      expect(recorded.p2pGuest[0].player.main_deck).toEqual(ACTIVE);
    });

    it("an online guest submits its active deck", async () => {
      renderRoute({ mode: "online", joinCode: "ABC123", formatConfig: config("Standard") });
      await waitFor(() => expect(recorded.ws).toHaveLength(1));
      expect(recorded.ws[0].deck.main_deck).toEqual(ACTIVE);
    });

    it("a fresh P2P host submits its active deck", async () => {
      renderRoute({ mode: "p2p-host", formatConfig: config("Standard"), pile: "Pile A" });
      await waitFor(() => expect(recorded.p2pHost).toHaveLength(1));
      expect(recorded.p2pHost[0].player.main_deck).toEqual(ACTIVE);
    });

    it("a P2P host with no active deck is told it has none", async () => {
      localStorage.removeItem(ACTIVE_DECK_KEY);
      const onNoDeck = renderRoute({ mode: "p2p-host", formatConfig: config("Standard") });
      await waitFor(() => expect(onNoDeck).toHaveBeenCalled());
      expect(recorded.p2pHost).toHaveLength(0);
    });
  });
});
