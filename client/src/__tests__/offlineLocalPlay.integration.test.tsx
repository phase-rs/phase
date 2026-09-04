import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import userEvent from "@testing-library/user-event";
import "fake-indexeddb/auto";
import { Outlet } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { disposeConnectivity, initializeConnectivity, useConnectivityStore } from "../stores/connectivityStore";
import { useCardImage } from "../hooks/useCardImage";
import { useCloudSyncStore } from "../stores/cloudSyncStore";
import { clearActiveGame, loadActiveGame, useGameStore } from "../stores/gameStore";
import { buildGameState } from "../test/factories/gameStateFactory";

/*
 * This is intentionally one integration fixture rather than a fourth copy of
 * the unit matrices owned by the feed, updater, and visual-pack modules. The
 * outer App and the two registration entry points are real; only remote/OS
 * boundaries and heavyweight shell presentation are substituted.
 */
const test = vi.hoisted(() => {
  const platform = { bundled: false, desktop: false };
  const aiCandidates = [{
    id: "saved:Offline Saved",
    name: "Offline Saved",
    source: { type: "saved" },
    deck: { main: [{ name: "Lightning Bolt", count: 60 }], sideboard: [] },
    coveragePct: 100,
    archetype: null,
    bracket: null,
    knownFormat: "Commander",
  }];
  const ledger = {
    cloudProvider: 0,
    cloudResume: 0,
    cloudRestore: 0,
    cloudReconcile: 0,
    cloudUnsubscribe: 0,
    catalogReconcile: 0,
    catalogRefresh: 0,
    catalogStart: 0,
    catalogBackgroundPause: [] as boolean[],
    serviceWorkerRegister: 0,
    serviceWorkerUpdate: 0,
    tauriCheck: 0,
    unexpectedFetches: [] as string[],
    adapterInitialize: 0,
    adapterInitializeGame: 0,
    adapterSnapshot: 0,
    adapterDisposed: 0,
    visualResolutions: 0,
    remoteImageContinuation: 0,
    nativeConstructor: 0,
    webSocketConstructor: 0,
    p2pConstructor: 0,
    multiplayerTransport: 0,
    activeQuickDraftLoads: 0,
    draftRunLoads: 0,
    deckJsonRequests: 0,
    feedRequests: 0,
  };
  const localSnapshot = { value: null as unknown };
  const localAdapter = {
    cardDbLoaded: true,
    warmCardDatabase: vi.fn(async () => undefined),
    initialize: vi.fn(async () => { ledger.adapterInitialize += 1; }),
    initializeGame: vi.fn(async () => {
      ledger.adapterInitializeGame += 1;
      return { events: [], log_entries: [] };
    }),
    getSnapshot: vi.fn(async () => {
      ledger.adapterSnapshot += 1;
      return localSnapshot.value;
    }),
    dispose: vi.fn(() => { ledger.adapterDisposed += 1; }),
    resetGameState: vi.fn(),
  };
  const registration = {
    update: vi.fn(async () => {
      ledger.serviceWorkerUpdate += 1;
    }),
    addEventListener: vi.fn(),
    installing: null,
    waiting: null,
    active: { state: "activated" },
  } as unknown as ServiceWorkerRegistration;
  const registerSW = vi.fn((options: { onRegisteredSW?: (url: string, registration?: ServiceWorkerRegistration) => void }) => {
    ledger.serviceWorkerRegister += 1;
    options.onRegisteredSW?.("/sw.js", registration);
  });
  const cloudProvider = {
    id: "supabase" as const,
    isConfigured: () => true,
    resume: async () => { ledger.cloudResume += 1; },
    pause: async () => undefined,
    restoreSession: async () => {
      ledger.cloudRestore += 1;
      return { userId: "offline-test-user", label: "Offline Test" };
    },
    signIn: async () => undefined,
    signOut: async () => undefined,
    identity: () => ({ userId: "offline-test-user", label: "Offline Test" }),
    pullMeta: async () => {
      ledger.cloudReconcile += 1;
      return { revision: 1, updatedAt: "2026-01-01T00:00:00.000Z" };
    },
    pull: async () => null,
    push: async () => ({ revision: 1, updatedAt: "2026-01-01T00:00:00.000Z" }),
    subscribe: () => async () => { ledger.cloudUnsubscribe += 1; },
  };

  return {
    ledger,
    platform,
    registration,
    aiCandidates,
    registerSW,
    cloudProvider,
    localAdapter,
    localSnapshot,
    reset() {
      Object.assign(ledger, {
        cloudProvider: 0,
        cloudResume: 0,
        cloudRestore: 0,
        cloudReconcile: 0,
        cloudUnsubscribe: 0,
        catalogReconcile: 0,
        catalogRefresh: 0,
        catalogStart: 0,
        catalogBackgroundPause: [],
        serviceWorkerRegister: 0,
        serviceWorkerUpdate: 0,
        tauriCheck: 0,
        unexpectedFetches: [],
        adapterInitialize: 0,
        adapterInitializeGame: 0,
        adapterSnapshot: 0,
        adapterDisposed: 0,
        visualResolutions: 0,
        remoteImageContinuation: 0,
        nativeConstructor: 0,
        webSocketConstructor: 0,
        p2pConstructor: 0,
        multiplayerTransport: 0,
        activeQuickDraftLoads: 0,
        draftRunLoads: 0,
        deckJsonRequests: 0,
        feedRequests: 0,
      });
      localSnapshot.value = null;
      platform.bundled = false;
      platform.desktop = false;
      registerSW.mockClear();
      vi.mocked(registration.update).mockClear();
      localAdapter.initialize.mockClear();
      localAdapter.initializeGame.mockClear();
      localAdapter.getSnapshot.mockClear();
      localAdapter.dispose.mockClear();
    },
  };
});

vi.mock("virtual:pwa-register", () => ({ registerSW: test.registerSW }));
vi.mock("@tauri-apps/plugin-updater", () => ({
  check: vi.fn(async () => {
    test.ledger.tauriCheck += 1;
    return null;
  }),
}));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: vi.fn() }));
vi.mock("../services/cloudSync", () => ({
  SyncConflictError: class SyncConflictError extends Error {},
  isCloudSyncConfigured: () => true,
  getCloudSyncProvider: () => {
    test.ledger.cloudProvider += 1;
    return test.cloudProvider;
  },
  pauseCloudSyncProvider: () => test.cloudProvider.pause(),
}));
vi.mock("../services/platform", () => ({
  isTauri: () => false,
  isBundledTauriOrigin: () => test.platform.bundled,
  isDesktopTauri: () => test.platform.desktop,
  loadVisualPackBackend: async () => ({
    catalogStatus: vi.fn(),
    curatedSelector: vi.fn(),
    curatedDrift: vi.fn(),
    deckLibrarySelector: vi.fn(),
    deckLibraryDrift: vi.fn(),
    reconcileDeckLibrary: vi.fn(async () => { test.ledger.catalogReconcile += 1; }),
    refreshCatalog: vi.fn(async () => {
      test.ledger.catalogRefresh += 1;
      throw new Error("Deck Catalog refresh must stay unreachable");
    }),
    catalogSummary: vi.fn(),
    estimateInstall: vi.fn(),
    start: vi.fn(async () => {
      test.ledger.catalogStart += 1;
      throw new Error("Deck Catalog start must stay unreachable");
    }),
    cancel: vi.fn(),
    operationStatus: vi.fn(),
    remove: vi.fn(),
    verify: vi.fn(),
    resolve: async (keys: Array<{ kind: string; key: string }>) => {
      test.ledger.visualResolutions += 1;
      return {
        revision: "0",
        entries: keys.map((key, ordinal) => ({
          ordinal,
          key,
          matches: [{
            packId: "deck_library",
            assetKey: "asset:lightning-bolt",
            catalogRoot: "a".repeat(64),
            url: "blob:installed-lightning-bolt",
            media: "image/jpeg",
          }],
        })),
      };
    },
    subscribeProgress: vi.fn(async () => () => undefined),
    subscribeRevision: vi.fn(async () => () => undefined),
    setDeckLibraryBackgroundPaused: async (paused: boolean) => {
      test.ledger.catalogBackgroundPause.push(paused);
    },
    prepareDeckLibraryForOffline: async () => "ready" as const,
  }),
}));
vi.mock("../audio/useAudioContext", () => ({ useAudioContext: () => undefined }));
vi.mock("../adapter/wasm-adapter", () => ({
  WasmAdapter: class WasmAdapter {},
  getSharedAdapter: () => test.localAdapter,
}));
vi.mock("../game/controllers/gameLoopController", () => ({
  createGameLoopController: () => ({ start: vi.fn(), dispose: vi.fn() }),
}));
vi.mock("../services/deckCompatibility", () => ({
  evaluateDeckCompatibility: vi.fn(async () => ({
    selected_format_compatible: true,
    standard: { compatible: true },
    commander: { compatible: true },
    bo3_ready: true,
    unknown_cards: [],
    coverage: { total_unique: 1, supported_unique: 1, unsupported_cards: [] },
    color_identity: [],
    color_distribution: [],
  })),
  evaluateDeckCompatibilityBatch: vi.fn(async () => ({})),
}));
vi.mock("../services/engineRuntime", () => ({
  companionCandidates: vi.fn(async () => []),
  commanderPartnerCandidates: vi.fn(async () => []),
  isCardCommanderEligibleForFormat: vi.fn(async () => false),
  maxDeckCopies: vi.fn(async () => ({ type: "UpTo", data: 4 })),
  sideboardPolicyForFormat: vi.fn(async () => ({ type: "Limited", data: 15 })),
  signatureSpellSelectionPolicy: vi.fn(async () => ({ type: "NotApplicable" })),
}));
vi.mock("../services/aiDeckCatalog", () => ({
  buildLegalAiDeckCatalog: vi.fn(async () => ({
    candidates: test.aiCandidates,
  })),
  useAiDeckCatalog: () => ({
    candidates: test.aiCandidates,
    loading: false,
    error: null,
  }),
}));
vi.mock("../services/scryfall", () => {
  const remote = () => {
    test.ledger.remoteImageContinuation += 1;
    return Promise.reject(new Error("remote image continuation is forbidden"));
  };
  return {
    CARD_BACK_URL: "data:,card-back",
    IMAGE_SIZE_WIDTHS: { small: 146, normal: 488 },
    deriveImageUrl: (src: string) => src,
    fetchCardData: vi.fn(async (name: string) => ({
      name,
      mana_cost: "",
      cmc: 0,
      type_line: "Instant",
      colors: [],
      color_identity: [],
      keywords: [],
    })),
    fetchCardImageAsset: remote,
    fetchCardImageAssetByOracleId: remote,
    fetchTokenImageAssetByRef: remote,
    fetchTokenImageUrl: remote,
    findPrintingById: vi.fn(),
    getCardPrintingsByName: vi.fn(async () => []),
    getCardPrintings: vi.fn(async () => []),
    hasAlternatePrintingsSync: vi.fn(() => false),
    imageUrlSize: vi.fn(() => null),
    isCardDataResident: vi.fn(() => false),
    isCardImageFlipLayoutSync: vi.fn(() => false),
    isCardImageRotatedSync: vi.fn(() => false),
    isLocaleArtReady: vi.fn(() => true),
    loadLocaleArt: vi.fn(async () => new Map()),
    loadPrintingsData: vi.fn(async () => null),
    loadScryfallData: vi.fn(async () => null),
    manaSymbolSourceUrl: vi.fn(() => "data:,mana"),
    normalizeCardName: (name: string) => name.toLowerCase(),
    pickOldestPrinting: vi.fn(),
    resolveAlternateCardFaceSync: vi.fn(() => null),
    resolveFaceIndexSync: vi.fn(() => null),
    resolveOracleIdSync: vi.fn((name: string) => name === "Lightning Bolt" ? "oracle-bolt" : null),
    resolvePrintingImageUrl: vi.fn(),
    scryfallLegalityKey: vi.fn(() => undefined),
  };
});
vi.mock("../services/deckMigrations", () => ({ migrateSavedDecks: vi.fn() }));
vi.mock("../hooks/useHostingSession", () => ({ useHostingSession: vi.fn() }));
vi.mock("../hooks/useDeckCardData", () => ({
  useDeckCardData: () => ({ cardDataCache: new Map(), cacheCards: vi.fn() }),
}));
vi.mock("../hooks/useSetSymbols", () => ({
  useSetSymbol: () => ({ src: null, isLoading: false }),
}));
vi.mock("../startup/preloadAssets", () => ({ ensurePreload: vi.fn(), subscribePreload: () => () => undefined }));
vi.mock("../components/chrome/AppShell", async () => {
  return { AppShell: () => <Outlet /> };
});
vi.mock("../components/chrome/AppToast", () => ({ AppToast: () => null }));
vi.mock("../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../components/chrome/NativeEngineProgressOverlay", () => ({ NativeEngineProgressOverlay: () => null }));
vi.mock("../components/chrome/RouteTelemetry", () => ({ RouteTelemetry: () => null }));
vi.mock("../components/chrome/HostControlTile", () => ({ HostControlTile: () => null }));
vi.mock("../components/ErrorBoundary", () => ({ ErrorBoundary: ({ children }: { children: ReactNode }) => children }));
vi.mock("../components/modal/EngineLostModal", () => ({ EngineLostModal: () => null }));
vi.mock("../components/modal/NonFatalPanicToast", () => ({ NonFatalPanicToast: () => null }));
vi.mock("../components/modal/StuckDecisionToast", () => ({ StuckDecisionToast: () => null }));
vi.mock("../components/splash/SplashScreen", () => ({ SplashScreen: () => null }));
vi.mock("../components/chrome/PreviewBadge", () => ({ PreviewBadge: () => null }));
vi.mock("../components/chrome/GameMenu", () => ({ GameMenu: () => null }));
vi.mock("../components/menu/home/HomeDashboard", () => ({ HomeDashboard: () => <div>Home dashboard</div> }));
vi.mock("../adapter/p2p-adapter", () => ({
  P2PGuestAdapter: class P2PGuestAdapter { constructor() { test.ledger.p2pConstructor += 1; throw new Error("P2P must stay unreachable"); } },
  P2PHostAdapter: class P2PHostAdapter { constructor() { test.ledger.p2pConstructor += 1; throw new Error("P2P must stay unreachable"); } },
}));
vi.mock("../adapter/ws-adapter", () => ({
  NativeEngineVersionMismatchError: class NativeEngineVersionMismatchError extends Error {},
  WebSocketAdapter: class WebSocketAdapter { constructor() { test.ledger.webSocketConstructor += 1; throw new Error("WebSocket must stay unreachable"); } },
  acknowledgeFullTerminalDelivery: vi.fn(),
  bootstrapFullTerminalDelivery: vi.fn(),
  readFullTerminalResult: vi.fn(),
}));
vi.mock("../audio/AudioManager", () => ({ audioManager: { setContext: vi.fn() } }));
vi.mock("../game/dispatch", () => ({ dispatchAction: vi.fn(), processRemoteUpdate: vi.fn() }));
vi.mock("../game/staleStateWatchdog", () => ({ resyncFromAdapterSafely: vi.fn() }));
vi.mock("../game/sessionCleanup", () => ({ clearPromptOverlayState: vi.fn() }));
vi.mock("../hooks/useGameplayPreferencesSync", () => ({ useGameplayPreferencesSync: vi.fn() }));
vi.mock("../network/connection", () => ({
  hostRoom: vi.fn(() => { test.ledger.multiplayerTransport += 1; throw new Error("multiplayer must stay unreachable"); }),
  joinRoom: vi.fn(() => { test.ledger.multiplayerTransport += 1; throw new Error("multiplayer must stay unreachable"); }),
}));
vi.mock("../services/p2pSession", () => ({ loadP2PSession: vi.fn(() => { test.ledger.p2pConstructor += 1; throw new Error("P2P resume must stay unreachable"); }) }));
vi.mock("../services/p2pTerminalResult", () => ({ loadP2PTerminalResult: vi.fn(() => { test.ledger.p2pConstructor += 1; throw new Error("P2P terminal must stay unreachable"); }) }));
vi.mock("../services/quickDraftPersistence", () => ({
  loadActiveQuickDraft: vi.fn(() => {
    test.ledger.activeQuickDraftLoads += 1;
    throw new Error("quick-draft metadata must stay unreachable");
  }),
  // GameProvider imports this separate persistence path for draft-source
  // local games, even though this AI journey must not reach it.
  loadDraftRun: vi.fn(() => {
    test.ledger.draftRunLoads += 1;
    throw new Error("draft run must stay unreachable");
  }),
}));
vi.mock("../services/fullTerminalResult", () => ({
  commitFullTerminalDelivery: vi.fn(),
  loadFullTerminalDelivery: vi.fn(),
  replaceFullTerminalDelivery: vi.fn(),
}));
vi.mock("../services/serverDetection", () => ({ detectServerUrl: vi.fn() }));
vi.mock("../services/nativeEngine", () => ({
  canAttemptNativeEngine: () => false,
  ensureNativeEngine: vi.fn(() => { test.ledger.nativeConstructor += 1; throw new Error("native engine must stay unreachable"); }),
  nativeEngineKeyForCurrentOrigin: () => null,
}));
vi.mock("../services/nativeEngineSocket", () => ({ NativeEngineSocket: class NativeEngineSocket { constructor() { test.ledger.nativeConstructor += 1; throw new Error("native socket must stay unreachable"); } } }));

import { App } from "../App";

function setOffline(value: boolean): void {
  act(() => useConnectivityStore.getState().setForcedOffline(value));
}

function navigate(path: string): void {
  window.history.pushState({}, "", path);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

function InstalledImageProbe() {
  const image = useCardImage("Lightning Bolt", {
    sourcePrinting: { setCode: "LEA", collectorNumber: "161" },
  });
  return (
    <>
      <output data-testid="installed-image">{image.src ?? "none"}</output>
      <button type="button" onClick={() => image.advanceFailedSource?.(image.src ?? "")}>fail installed image</button>
    </>
  );
}

async function settleReconnect(): Promise<void> {
  await settleEffects();
  expect(test.ledger.cloudReconcile).toBe(1);
}

async function settleEffects(): Promise<void> {
  await act(async () => {
    for (let index = 0; index < 64; index += 1) await Promise.resolve();
  });
}

async function settleCatalogActivation(): Promise<void> {
  for (let attempt = 0; attempt < 50; attempt += 1) {
    await vi.advanceTimersByTimeAsync(1);
    await settleEffects();
    if (test.ledger.catalogBackgroundPause.includes(false)) return;
  }
  throw new Error("deck catalog never unpaused after the feed-ready barrier");
}

function ledgerSnapshot() {
  return structuredClone(test.ledger);
}

let originalServiceWorkerDescriptor: PropertyDescriptor | undefined;
let pwaServiceWorkerProfileActive = false;

beforeEach(async () => {
  vi.useFakeTimers();
  vi.stubEnv("DEV", false);
  test.reset();
  localStorage.clear();
  sessionStorage.clear();
  window.history.replaceState({}, "", "/");
  disposeConnectivity();
  useConnectivityStore.setState({ forcedOffline: true, browserOnline: true });
  await initializeConnectivity();
  vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL) => {
    const url = String(input);
    if (url === "/decks.json") {
      test.ledger.deckJsonRequests += 1;
      return Promise.resolve(new Response(JSON.stringify({
      Secrets_of_Strixhaven_SOS: {
        code: "SOS",
        name: "Secrets of Strixhaven",
        type: "Commander Deck",
        coveragePct: 100,
          commander: [{ name: "Dean of Theory", count: 1 }],
          mainBoard: [{ name: "Island", count: 99 }],
        },
      }), { status: 200 }));
    }
    if (url.startsWith("/feeds/")) {
      test.ledger.feedRequests += 1;
      return Promise.resolve(new Response(JSON.stringify({
        id: "offline-fixture",
        name: "Offline fixture",
        version: 1,
        updated: "2026-01-01T00:00:00.000Z",
        decks: [],
      }), { status: 200 }));
    }
    if (url === "/sw.js") return Promise.resolve(new Response("", { status: 200 }));
    test.ledger.unexpectedFetches.push(url);
    return Promise.reject(new Error(`unexpected remote fetch: ${url}`));
  }));
});

afterEach(async () => {
  const [{ disposeServiceWorkerUpdater }, { disposeTauriUpdater }, { disposeCloudSyncModuleForTest }] = await Promise.all([
    import("../pwa/registerServiceWorker"),
    import("../pwa/tauriUpdater"),
    import("../stores/cloudSyncStore"),
  ]);
  disposeServiceWorkerUpdater();
  disposeTauriUpdater();
  cleanup();
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
  useGameStore.getState().reset();
  clearActiveGame();
  disposeCloudSyncModuleForTest({});
  useCloudSyncStore.setState({
    available: false,
    paused: false,
    identity: null,
    sessionResolved: false,
    status: "idle",
    error: null,
    dirty: false,
    lastSyncedRevision: null,
    lastSyncedAt: null,
    conflict: null,
    conflictDiff: null,
  });
  disposeConnectivity();
  localStorage.clear();
  sessionStorage.clear();
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  if (pwaServiceWorkerProfileActive) {
    if (originalServiceWorkerDescriptor) {
      Object.defineProperty(navigator, "serviceWorker", originalServiceWorkerDescriptor);
    } else {
      Reflect.deleteProperty(navigator, "serviceWorker");
    }
    originalServiceWorkerDescriptor = undefined;
    pwaServiceWorkerProfileActive = false;
  }
});

describe("offline local play integration", () => {
  it("completes the prepared offline journey without remote continuation", async () => {
    vi.useRealTimers();
    const { STORAGE_KEY_PREFIX } = await import("../constants/storage");
    const { ACTIVE_DECK_KEY } = await import("../constants/storage");
    const { buildDeckCatalog } = await import("../services/deckCatalog");
    const { PreconDeckModal } = await import("../components/menu/PreconDeckModal");
    await import("../pages/GameSetupPage");
    await import("../pages/DeckBuilderPage");
    await import("../pages/GamePage");

    localStorage.setItem(`${STORAGE_KEY_PREFIX}Offline Saved`, JSON.stringify({
      main: [{ name: "Lightning Bolt", count: 60, sourcePrinting: { setCode: "LEA", collectorNumber: "161" } }],
      sideboard: [],
      format: "Commander",
    }));
    localStorage.setItem(ACTIVE_DECK_KEY, "Offline Saved");
    test.localSnapshot.value = {
      state: buildGameState(),
      legalResult: { actions: [], autoPassRecommended: false },
      seq: Number.MAX_SAFE_INTEGER,
    };

    const catalog = await buildDeckCatalog();
    expect(catalog).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: "saved:Offline Saved", source: { type: "saved" } }),
      expect.objectContaining({ id: "precon:Secrets_of_Strixhaven_SOS", name: "Secrets of Strixhaven (SOS)" }),
      expect.objectContaining({ id: "precon:BundledCedh_HeliodBallista_Demo", name: "cEDH Demo - Heliod Combo (BCDH)" }),
    ]));
    render(<PreconDeckModal open onClose={() => undefined} onImported={() => undefined} />);
    expect(await screen.findByText("Secrets of Strixhaven")).toBeInTheDocument();
    expect(test.ledger.deckJsonRequests).toBe(1);
    cleanup();

    const app = render(<App />);
    expect(await screen.findByText("Home dashboard")).toBeInTheDocument();

    // Match bootstrap ordering after React has mounted. With DEV disabled and
    // the effective-offline policy forced, neither lifecycle may begin remote
    // registration or update work.
    originalServiceWorkerDescriptor = Object.getOwnPropertyDescriptor(navigator, "serviceWorker");
    pwaServiceWorkerProfileActive = true;
    Object.defineProperty(navigator, "serviceWorker", {
      configurable: true,
      value: { controller: (test.registration as ServiceWorkerRegistration).active },
    });
    const { registerServiceWorker } = await import("../pwa/registerServiceWorker");
    const { registerTauriUpdater } = await import("../pwa/tauriUpdater");
    registerServiceWorker();
    registerTauriUpdater();
    await settleEffects();
    expect(test.ledger.serviceWorkerRegister).toBe(0);
    expect(test.ledger.serviceWorkerUpdate).toBe(0);
    expect(test.ledger.tauriCheck).toBe(0);

    navigate("/setup");
    expect((await screen.findAllByText("Offline Saved")).length).toBeGreaterThan(0);

    navigate("/deck-builder?deck=Offline%20Saved&returnTo=%2Fsetup");
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /remove one lightning bolt/i }));
    await user.click(screen.getByRole("button", { name: /^save$/i }));
    await waitFor(() => {
      const saved = JSON.parse(localStorage.getItem(`${STORAGE_KEY_PREFIX}Offline Saved`) ?? "{}");
      expect(saved.main).toEqual([expect.objectContaining({ name: "Lightning Bolt", count: 59 })]);
    });

    navigate("/setup");
    expect((await screen.findAllByText("Offline Saved")).length).toBeGreaterThan(0);
    await user.click(screen.getByRole("button", { name: /start match/i }));
    await waitFor(() => expect(useGameStore.getState().gameState).not.toBeNull());
    expect(test.ledger.adapterInitialize).toBe(1);
    expect(test.ledger.adapterInitializeGame).toBe(1);
    expect(test.ledger.adapterSnapshot).toBe(1);
    expect(window.location.pathname).toMatch(/^\/game\//);
    expect(window.location.search).toContain("mode=ai");
    expect(loadActiveGame()).toEqual(expect.objectContaining({
      id: window.location.pathname.slice("/game/".length),
      mode: "ai",
    }));

    render(<InstalledImageProbe />);
    expect(await screen.findByTestId("installed-image")).toHaveTextContent("blob:installed-lightning-bolt");
    expect(test.ledger.visualResolutions).toBeGreaterThan(0);
    await user.click(screen.getByRole("button", { name: "fail installed image" }));
    expect(screen.getByTestId("installed-image")).toHaveTextContent("none");
    expect(test.ledger.remoteImageContinuation).toBe(0);
    expect(test.ledger.unexpectedFetches).toEqual([]);
    expect(test.ledger.cloudProvider).toBe(0);
    expect(test.ledger.cloudResume).toBe(0);
    expect(test.ledger.cloudRestore).toBe(0);
    expect(test.ledger.cloudReconcile).toBe(0);
    expect(test.ledger.feedRequests).toBe(0);
    expect(test.ledger.catalogReconcile).toBe(0);
    expect(test.ledger.catalogRefresh).toBe(0);
    expect(test.ledger.catalogStart).toBe(0);
    expect(test.ledger.serviceWorkerRegister).toBe(0);
    expect(test.ledger.serviceWorkerUpdate).toBe(0);
    expect(test.ledger.tauriCheck).toBe(0);
    expect(test.ledger.nativeConstructor).toBe(0);
    expect(test.ledger.webSocketConstructor).toBe(0);
    expect(test.ledger.p2pConstructor).toBe(0);
    expect(test.ledger.multiplayerTransport).toBe(0);
    expect(test.ledger.activeQuickDraftLoads).toBe(0);
    expect(test.ledger.draftRunLoads).toBe(0);
    app.unmount();
    await waitFor(() => expect(useGameStore.getState().gameState).toBeNull());
    expect(test.ledger.adapterDisposed).toBe(1);
  }, 30_000);

  it("runs only the PWA lifecycle after one offline-to-online reconnect", async () => {
    test.platform.bundled = false;
    test.platform.desktop = false;
    originalServiceWorkerDescriptor = Object.getOwnPropertyDescriptor(navigator, "serviceWorker");
    pwaServiceWorkerProfileActive = true;
    Object.defineProperty(navigator, "serviceWorker", {
      configurable: true,
      value: { controller: (test.registration as ServiceWorkerRegistration).active },
    });
    const { registerServiceWorker } = await import("../pwa/registerServiceWorker");
    const { registerTauriUpdater } = await import("../pwa/tauriUpdater");
    const app = render(<App />);
    registerServiceWorker();
    registerTauriUpdater();
    await settleEffects();

    // Offline policy reaches the real cloud store and catalog scheduler, but
    // neither is permitted to resolve its provider/backend or refresh feeds.
    expect(test.ledger.cloudProvider).toBe(0);
    expect(test.ledger.cloudResume).toBe(0);
    expect(test.ledger.cloudRestore).toBe(0);
    expect(test.ledger.cloudReconcile).toBe(0);
    expect(test.ledger.feedRequests).toBe(0);
    expect(test.ledger.catalogReconcile).toBe(0);
    expect(test.ledger.catalogBackgroundPause).toContain(true);
    expect(test.ledger.serviceWorkerRegister).toBe(0);
    expect(test.ledger.tauriCheck).toBe(0);

    setOffline(false);
    await settleReconnect();
    await settleCatalogActivation();
    expect(test.ledger.cloudProvider).toBe(1);
    expect(test.ledger.cloudResume).toBe(1);
    expect(test.ledger.cloudRestore).toBe(1);
    expect(test.ledger.cloudReconcile).toBe(1);
    expect(test.ledger.feedRequests).toBeGreaterThan(0);
    expect(test.ledger.serviceWorkerRegister).toBe(1);
    expect(test.ledger.catalogBackgroundPause).toContain(false);
    expect(test.ledger.catalogReconcile).toBe(0);
    await vi.advanceTimersByTimeAsync(499);
    await settleEffects();
    expect(test.ledger.catalogReconcile).toBe(0);
    await vi.advanceTimersByTimeAsync(1);
    await settleEffects();
    // The first exact debounce releases the fresh preference/feed barrier;
    // its post-hydration signal owns one final 500 ms reconciliation window.
    expect(test.ledger.catalogReconcile).toBe(0);
    await vi.advanceTimersByTimeAsync(500);
    await settleEffects();
    expect(test.ledger.catalogReconcile).toBe(1);
    expect(test.ledger.serviceWorkerUpdate).toBe(1);
    expect(test.ledger.tauriCheck).toBe(0);

    const afterFirstReconnect = ledgerSnapshot();
    setOffline(false);
    await vi.advanceTimersByTimeAsync(500);
    await act(async () => { for (let index = 0; index < 8; index += 1) await Promise.resolve(); });
    expect(test.ledger).toEqual(afterFirstReconnect);
    app.unmount();
    await settleEffects();
    expect(test.ledger.cloudUnsubscribe).toBe(1);
  });

  it("runs only the bundled-Tauri updater after one offline-to-online reconnect", async () => {
    test.platform.bundled = true;
    test.platform.desktop = true;
    const { registerServiceWorker } = await import("../pwa/registerServiceWorker");
    const { registerTauriUpdater } = await import("../pwa/tauriUpdater");
    const app = render(<App />);
    registerServiceWorker();
    registerTauriUpdater();
    await settleEffects();

    expect(test.ledger.cloudProvider).toBe(0);
    expect(test.ledger.cloudResume).toBe(0);
    expect(test.ledger.cloudRestore).toBe(0);
    expect(test.ledger.cloudReconcile).toBe(0);
    expect(test.ledger.feedRequests).toBe(0);
    expect(test.ledger.catalogReconcile).toBe(0);
    expect(test.ledger.catalogBackgroundPause).toContain(true);
    expect(test.ledger.serviceWorkerRegister).toBe(0);
    expect(test.ledger.tauriCheck).toBe(0);
    setOffline(false);
    await settleReconnect();
    await settleCatalogActivation();
    expect(test.ledger.catalogBackgroundPause).toContain(false);
    expect(test.ledger.catalogReconcile).toBe(0);
    await vi.advanceTimersByTimeAsync(499);
    await settleEffects();
    expect(test.ledger.catalogReconcile).toBe(0);
    await vi.advanceTimersByTimeAsync(1);
    await settleEffects();
    expect(test.ledger.catalogReconcile).toBe(0);
    await vi.advanceTimersByTimeAsync(500);
    await settleEffects();
    expect(test.ledger.tauriCheck).toBe(1);
    expect(test.ledger.serviceWorkerRegister).toBe(0);
    expect(test.ledger.cloudProvider).toBe(1);
    expect(test.ledger.cloudResume).toBe(1);
    expect(test.ledger.cloudRestore).toBe(1);
    expect(test.ledger.cloudReconcile).toBe(1);
    expect(test.ledger.feedRequests).toBeGreaterThan(0);
    expect(test.ledger.catalogReconcile).toBe(1);

    const afterFirstReconnect = ledgerSnapshot();
    setOffline(false);
    await vi.advanceTimersByTimeAsync(500);
    await act(async () => { for (let index = 0; index < 8; index += 1) await Promise.resolve(); });
    expect(test.ledger).toEqual(afterFirstReconnect);
    app.unmount();
    await settleEffects();
    expect(test.ledger.cloudUnsubscribe).toBe(1);
  });
});
