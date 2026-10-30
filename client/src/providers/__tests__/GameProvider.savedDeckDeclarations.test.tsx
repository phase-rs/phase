import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ComboDeclaration, CommanderBracketTier } from "../../types/bracketEstimate";

type Deck = {
  main_deck: string[];
  bracket_tier?: CommanderBracketTier | null;
  combo_declaration?: ComboDeclaration;
};
type DeckList = { player: Deck; opponent: Deck; ai_decks: Deck[] };

const {
  deckSupplyForFormat,
  gameStoreState,
  native,
  preferences,
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
    preferences: {
      aiSeats: [] as { difficulty: string; deckId: string }[],
      aiBracketFilter: [] as number[],
      cedhMode: false,
      nativeEngineEnabled: false,
    },
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

// A P2P host reads its pending Full terminal cleanup before building a deck;
// jsdom has no IndexedDB, so these launches start with none pending.
vi.mock("../../services/gamePersistence", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../services/gamePersistence")>(),
  loadFullTerminalCleanupStrict: vi.fn(async () => null),
}));

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
vi.mock("../../services/deckCompatibility", () => ({
  evaluateDeckCompatibility: vi.fn(async () => ({
    selected_format_compatible: true,
    coverage: { total_unique: 1, supported_unique: 1, unsupported_cards: [] },
  })),
}));
// Stands in for `estimate_bracket_for_deck` with no combo table loaded, where the
// floor comes only from `combo_declaration_floor`: the engine's own
// `declared_early_combo_raises_tier_above_the_card_derived_floor` pins that half.
vi.mock("../../services/bracketEstimate", () => ({
  estimateDeckBracket: vi.fn(async ({ deck }: { deck: { combo_declaration: ComboDeclaration } }) => {
    const declaration = deck.combo_declaration;
    const tier = declaration.kind !== "intended"
      ? "core"
      : declaration.window === "early_game" ? "optimized" : "upgraded";
    return { kind: "estimate", estimate: { tier } };
  }),
}));
vi.mock("../../hooks/useDecks", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../hooks/useDecks")>(),
  loadPreconDeckMap: vi.fn(async () => null),
}));
vi.mock("../../stores/preferencesStore", () => ({
  AI_DECK_RANDOM: "Random",
  usePreferencesStore: Object.assign(vi.fn(), {
    getState: () => ({
      ...preferences,
      aiArchetypeFilter: "Any",
      aiCoverageFloor: 0,
      humanPoolSelection: { excludedGroups: [], excludedDecks: [], includedDecks: [] },
      aiPoolSelection: { excludedGroups: [], excludedDecks: [], includedDecks: [] },
    }),
  }),
}));
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

import { ACTIVE_DECK_KEY, RANDOM_DECK_SELECTION, STORAGE_KEY_PREFIX } from "../../constants/storage";
import { formatMetadata } from "../../data/formatRegistry";
import { GameProvider } from "../GameProvider";

const EARLY: ComboDeclaration = { kind: "intended", window: "early_game" };
const LATE: ComboDeclaration = { kind: "intended", window: "late_game" };
const NONE_INTENDED: ComboDeclaration = { kind: "none_intended" };
const UNDECLARED: ComboDeclaration = { kind: "undeclared" };

const OPTIMIZED_BRACKET = 4;
const CORE_BRACKET = 2;

/** Saves an untagged Commander deck, the case the engine plays at its estimate. */
function saveDeck(name: string, declaration: ComboDeclaration): void {
  localStorage.setItem(`${STORAGE_KEY_PREFIX}${name}`, JSON.stringify({
    main: [{ name: `${name} Card`, count: 1 }],
    sideboard: [],
    commander: [`${name} Commander`],
    format: "Commander",
    ...(declaration.kind === "undeclared" ? {} : { combo_declaration: declaration }),
  }));
}

function seat(deckId: string) {
  return { difficulty: "Medium", deckId };
}

async function startedDeckList(playerCount: number): Promise<DeckList> {
  render(
    <GameProvider
      gameId="saved-declarations"
      mode="ai"
      playerCount={playerCount}
      formatConfig={formatMetadata("Commander")!.default_config}
    >
      <div />
    </GameProvider>,
  );
  await waitFor(() => expect(gameStoreState.initGame).toHaveBeenCalled());
  return gameStoreState.initGame.mock.calls[0][2] as DeckList;
}

describe("a saved deck's combo declaration reaches its own seat", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    native.enabled = false;
    gameStoreState.adapter = null;
    gameStoreState.gameId = null;
    preferences.aiBracketFilter = [];
    preferences.aiSeats = [];
  });
  afterEach(cleanup);

  it("carries each picked deck's answer to the player and every AI seat", async () => {
    saveDeck("Human Early", EARLY);
    saveDeck("Opponent Late", LATE);
    saveDeck("Extra Plain", UNDECLARED);
    saveDeck("Extra Refused", NONE_INTENDED);
    localStorage.setItem(ACTIVE_DECK_KEY, RANDOM_DECK_SELECTION);
    // Only the early deck estimates to bracket 4, so the human Random draw is forced.
    preferences.aiBracketFilter = [OPTIMIZED_BRACKET];
    preferences.aiSeats = [
      seat("saved:Opponent Late"),
      seat("saved:Extra Plain"),
      seat("saved:Extra Refused"),
    ];

    const deckList = await startedDeckList(4);

    expect(deckList.player.main_deck).toEqual(["Human Early Card"]);
    expect(deckList.player.combo_declaration).toEqual(EARLY);
    expect(deckList.opponent.main_deck).toEqual(["Opponent Late Card"]);
    expect(deckList.opponent.combo_declaration).toEqual(LATE);
    expect(deckList.ai_decks.map((deck) => deck.main_deck)).toEqual([
      ["Extra Plain Card"],
      ["Extra Refused Card"],
    ]);
    expect(deckList.ai_decks.map((deck) => deck.combo_declaration)).toEqual([
      UNDECLARED,
      NONE_INTENDED,
    ]);
    // Untagged decks stay undeclared on the wire, so the engine plays each estimate.
    for (const deck of [deckList.player, deckList.opponent, ...deckList.ai_decks]) {
      expect(deck.bracket_tier).toBeNull();
    }
  });

  it("files an early-combo deck in bracket 4, so a bracket 2 pool never draws it", async () => {
    saveDeck("Early Combo", EARLY);
    saveDeck("Fair Deck", UNDECLARED);
    saveDeck("Mine", UNDECLARED);
    localStorage.setItem(ACTIVE_DECK_KEY, "Mine");
    preferences.aiBracketFilter = [CORE_BRACKET];
    preferences.aiSeats = [seat("Random"), seat("Random")];

    const deckList = await startedDeckList(3);

    const seated = [deckList.opponent, ...deckList.ai_decks].map((deck) => deck.main_deck);
    expect(seated).toContainEqual(["Fair Deck Card"]);
    expect(seated).toContainEqual(["Mine Card"]);
    expect(seated).not.toContainEqual(["Early Combo Card"]);
  });

  it("seats the early-combo deck in a bracket 4 pool with the answer that put it there", async () => {
    saveDeck("Early Combo", EARLY);
    saveDeck("Fair Deck", UNDECLARED);
    saveDeck("Mine", NONE_INTENDED);
    localStorage.setItem(ACTIVE_DECK_KEY, "Mine");
    preferences.aiBracketFilter = [OPTIMIZED_BRACKET];
    preferences.aiSeats = [seat("Random")];

    const deckList = await startedDeckList(2);

    expect(deckList.opponent.main_deck).toEqual(["Early Combo Card"]);
    expect(deckList.opponent.combo_declaration).toEqual(EARLY);
    // A fixed human deck keeps its own sidecar, whatever the AI drew.
    expect(deckList.player.main_deck).toEqual(["Mine Card"]);
    expect(deckList.player.combo_declaration).toEqual(NONE_INTENDED);
  });
});
