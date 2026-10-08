import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { BUNDLED_CEDH_DECKS } from "../../data/cedhDecks.ts";
import type { DeckCatalogCandidate } from "../deckCatalog.ts";
import { VisualPackBackendError } from "../visualPacks/backend.ts";

const dependencies = vi.hoisted(() => ({
  ensureCardDatabase: vi.fn(),
  loadScryfallData: vi.fn(),
  loadPreconDeckMap: vi.fn(),
  buildDeckCatalog: vi.fn(),
  prepareDeckLibraryForOffline: vi.fn(),
  useRealLoaders: false,
  actual: {
    engineRuntime: null as typeof import("../engineRuntime.ts") | null,
    scryfall: null as typeof import("../scryfall.ts") | null,
    decks: null as typeof import("../../hooks/useDecks.ts") | null,
    catalog: null as typeof import("../deckCatalog.ts") | null,
  },
}));
const wasm = vi.hoisted(() => ({
  initialize: vi.fn(),
  loadCardDatabase: vi.fn(),
  searchCards: vi.fn(),
  cardDatabasePayload: null as string | null,
  cardDatabaseLoaded: false,
}));

vi.mock("@wasm/engine", () => ({
  default: wasm.initialize,
  load_card_database: wasm.loadCardDatabase,
  search_cards_js: wasm.searchCards,
}));
vi.mock("../engineRuntime.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../engineRuntime.ts")>();
  dependencies.actual.engineRuntime = actual;
  return {
    ...actual,
    ensureCardDatabase: () => dependencies.useRealLoaders
      ? actual.ensureCardDatabase()
      : dependencies.ensureCardDatabase(),
  };
});
vi.mock("../scryfall.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../scryfall.ts")>();
  dependencies.actual.scryfall = actual;
  return {
    ...actual,
    loadScryfallData: () => dependencies.useRealLoaders
      ? actual.loadScryfallData()
      : dependencies.loadScryfallData(),
  };
});
vi.mock("../../hooks/useDecks.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../hooks/useDecks.ts")>();
  dependencies.actual.decks = actual;
  return {
    ...actual,
    loadPreconDeckMap: () => dependencies.useRealLoaders
      ? actual.loadPreconDeckMap()
      : dependencies.loadPreconDeckMap(),
  };
});
vi.mock("../deckCatalog.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../deckCatalog.ts")>();
  dependencies.actual.catalog = actual;
  return {
    ...actual,
    buildDeckCatalog: () => dependencies.useRealLoaders
      ? actual.buildDeckCatalog()
      : dependencies.buildDeckCatalog(),
  };
});
vi.mock("../visualPacks/deckLibraryAutoSync.ts", async (importOriginal) => ({
  ...await importOriginal<typeof import("../visualPacks/deckLibraryAutoSync.ts")>(),
  prepareDeckLibraryForOffline: dependencies.prepareDeckLibraryForOffline,
}));

const GENERATED_PRECON_ID = "generated-precon";

function bundledDeckId(): string {
  const deckId = Object.keys(BUNDLED_CEDH_DECKS)[0];
  if (!deckId) throw new Error("Expected a bundled AI deck fixture");
  return deckId;
}

function preconCandidate(deckId: string): DeckCatalogCandidate {
  return {
    id: `precon:${deckId}`,
    name: deckId,
    source: { type: "precon", deckId, code: "TST" },
    deck: { main: [], sideboard: [] },
  };
}

function readyCatalog(): DeckCatalogCandidate[] {
  return [preconCandidate(GENERATED_PRECON_ID), preconCandidate(bundledDeckId())];
}

function readyReadiness(deckLibrary: "ready" | "not-installed" = "ready") {
  return {
    status: "ready" as const,
    capabilities: {
      engine: { status: "ready" as const, cardCount: 3 },
      scryfallSearch: { status: "ready" as const },
      preconCatalog: { status: "ready" as const },
      bundledAiCatalog: { status: "ready" as const },
      deckLibrary: deckLibrary === "ready"
        ? { status: "ready" as const }
        : { status: "not-installed" as const },
    },
  };
}

function readinessWithNotReady(
  capability: "engine" | "scryfallSearch" | "preconCatalog" | "bundledAiCatalog",
) {
  const readiness = readyReadiness();
  return {
    status: "not-ready" as const,
    capabilities: {
      ...readiness.capabilities,
      [capability]: { status: "not-ready" as const },
    },
  };
}

function readinessWithDeckLibraryError(error: string) {
  const readiness = readyReadiness();
  return {
    status: "not-ready" as const,
    capabilities: {
      ...readiness.capabilities,
      deckLibrary: { status: "not-ready" as const, error },
    },
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function configureReadyDependencies(): void {
  dependencies.useRealLoaders = false;
  dependencies.ensureCardDatabase.mockResolvedValue(3);
  dependencies.loadScryfallData.mockResolvedValue({ lightning_bolt: {} });
  dependencies.loadPreconDeckMap.mockResolvedValue({ [GENERATED_PRECON_ID]: {} });
  dependencies.buildDeckCatalog.mockResolvedValue(readyCatalog());
  dependencies.prepareDeckLibraryForOffline.mockResolvedValue("ready");
  wasm.initialize.mockResolvedValue(undefined);
  wasm.loadCardDatabase.mockResolvedValue(1);
  wasm.searchCards.mockReturnValue({ results: [], total: 0 });
  wasm.cardDatabasePayload = null;
  wasm.cardDatabaseLoaded = false;
}

async function loadOfflineAssets() {
  return import("../offlineAssets.ts");
}

beforeEach(() => {
  vi.clearAllMocks();
  configureReadyDependencies();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("prepareOfflineAssets", () => {
  it.each([
    ["selected", "ready"],
    ["not selected", "not-installed"],
  ] as const)("reports ready assets when the Deck Catalog is %s", async (_label, deckLibraryResult) => {
    dependencies.prepareDeckLibraryForOffline.mockResolvedValue(deckLibraryResult);
    const { prepareOfflineAssets } = await loadOfflineAssets();

    await expect(prepareOfflineAssets()).resolves.toEqual(readyReadiness(deckLibraryResult));
    expect(dependencies.buildDeckCatalog).toHaveBeenCalledOnce();
  });

  it("inspects the Deck Catalog only after every independent warmer settles", async () => {
    const engine = deferred<number>();
    const scryfall = deferred<Record<string, unknown> | null>();
    const precons = deferred<Record<string, unknown> | null>();
    const deckLibrary = deferred<"ready" | "not-installed">();
    dependencies.ensureCardDatabase.mockReturnValue(engine.promise);
    dependencies.loadScryfallData.mockReturnValue(scryfall.promise);
    dependencies.loadPreconDeckMap.mockReturnValue(precons.promise);
    dependencies.prepareDeckLibraryForOffline.mockReturnValue(deckLibrary.promise);
    const { prepareOfflineAssets } = await loadOfflineAssets();

    const preparation = prepareOfflineAssets();
    await Promise.resolve();
    expect(dependencies.buildDeckCatalog).not.toHaveBeenCalled();

    engine.resolve(3);
    scryfall.resolve({ lightning_bolt: {} });
    precons.resolve({ [GENERATED_PRECON_ID]: {} });
    await Promise.resolve();
    expect(dependencies.buildDeckCatalog).not.toHaveBeenCalled();

    deckLibrary.resolve("ready");
    await vi.waitFor(() => expect(dependencies.buildDeckCatalog).toHaveBeenCalledOnce());
    await expect(preparation).resolves.toEqual(readyReadiness());
  });

  it("reports every settled failure without hiding the typed Deck Catalog error", async () => {
    dependencies.ensureCardDatabase.mockRejectedValue(new Error("engine unavailable"));
    dependencies.loadScryfallData.mockResolvedValue(null);
    dependencies.loadPreconDeckMap.mockResolvedValue(null);
    dependencies.prepareDeckLibraryForOffline.mockRejectedValue(new VisualPackBackendError("network"));
    dependencies.buildDeckCatalog.mockResolvedValue([preconCandidate(bundledDeckId())]);
    const { prepareOfflineAssets } = await loadOfflineAssets();

    await expect(prepareOfflineAssets()).resolves.toEqual({
      status: "not-ready",
      capabilities: {
        engine: { status: "not-ready" },
        scryfallSearch: { status: "not-ready" },
        preconCatalog: { status: "not-ready" },
        bundledAiCatalog: { status: "ready" },
        deckLibrary: { status: "not-ready", error: "network" },
      },
    });
    expect(dependencies.ensureCardDatabase).toHaveBeenCalledOnce();
    expect(dependencies.loadScryfallData).toHaveBeenCalledOnce();
    expect(dependencies.loadPreconDeckMap).toHaveBeenCalledOnce();
    expect(dependencies.prepareDeckLibraryForOffline).toHaveBeenCalledOnce();
    expect(dependencies.buildDeckCatalog).toHaveBeenCalledOnce();
  });

  it("reports an ordinary engine rejection independently", async () => {
    dependencies.ensureCardDatabase.mockRejectedValue(new Error("engine unavailable"));
    const { prepareOfflineAssets } = await loadOfflineAssets();

    await expect(prepareOfflineAssets()).resolves.toEqual(readinessWithNotReady("engine"));
  });

  it("reports reload-required alongside independent failures", async () => {
    const { EngineModuleReloadRequiredError } = await import("../engineRuntime.ts");
    dependencies.ensureCardDatabase.mockRejectedValue(new EngineModuleReloadRequiredError(new Error("import failed")));
    dependencies.loadScryfallData.mockResolvedValue(null);
    const { prepareOfflineAssets } = await loadOfflineAssets();

    await expect(prepareOfflineAssets()).resolves.toEqual({
      status: "reload-required",
      capabilities: {
        engine: { status: "reload-required" },
        scryfallSearch: { status: "not-ready" },
        preconCatalog: { status: "ready" },
        bundledAiCatalog: { status: "ready" },
        deckLibrary: { status: "ready" },
      },
    });
    expect(dependencies.loadScryfallData).toHaveBeenCalledOnce();
    expect(dependencies.loadPreconDeckMap).toHaveBeenCalledOnce();
    expect(dependencies.prepareDeckLibraryForOffline).toHaveBeenCalledOnce();
    expect(dependencies.buildDeckCatalog).toHaveBeenCalledOnce();
  });

  it.each([0, -1])("treats a non-positive engine card count (%i) as not ready", async (cardCount) => {
    dependencies.ensureCardDatabase.mockResolvedValue(cardCount);
    const { prepareOfflineAssets } = await loadOfflineAssets();

    await expect(prepareOfflineAssets()).resolves.toEqual(readinessWithNotReady("engine"));
  });

  it.each([
    ["null Scryfall data", () => dependencies.loadScryfallData.mockResolvedValue(null), "scryfallSearch"],
    ["empty Scryfall data", () => dependencies.loadScryfallData.mockResolvedValue({}), "scryfallSearch"],
    ["null precon data", () => dependencies.loadPreconDeckMap.mockResolvedValue(null), "preconCatalog"],
    ["empty precon data", () => dependencies.loadPreconDeckMap.mockResolvedValue({}), "preconCatalog"],
    [
      "catalog missing the generated precon",
      () => dependencies.buildDeckCatalog.mockResolvedValue([preconCandidate(bundledDeckId())]),
      "preconCatalog",
    ],
    [
      "catalog missing the bundled deck",
      () => dependencies.buildDeckCatalog.mockResolvedValue([preconCandidate(GENERATED_PRECON_ID)]),
      "bundledAiCatalog",
    ],
  ] as const)("marks %s as not ready without collapsing independent capabilities", async (_label, configure, capability) => {
    configure();
    const { prepareOfflineAssets } = await loadOfflineAssets();

    await expect(prepareOfflineAssets()).resolves.toEqual(readinessWithNotReady(capability));
  });

  it("reports both shared-catalog capabilities when catalog inspection rejects", async () => {
    dependencies.buildDeckCatalog.mockRejectedValue(new Error("catalog unavailable"));
    const { prepareOfflineAssets } = await loadOfflineAssets();

    await expect(prepareOfflineAssets()).resolves.toEqual({
      status: "not-ready",
      capabilities: {
        engine: { status: "ready", cardCount: 3 },
        scryfallSearch: { status: "ready" },
        preconCatalog: { status: "not-ready" },
        bundledAiCatalog: { status: "not-ready" },
        deckLibrary: { status: "ready" },
      },
    });
  });

  it("retains the initial precon failure after catalog inspection retries it", async () => {
    dependencies.loadPreconDeckMap
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({ [GENERATED_PRECON_ID]: {} });
    dependencies.buildDeckCatalog.mockImplementation(async () => {
      await dependencies.loadPreconDeckMap();
      return readyCatalog();
    });
    const { prepareOfflineAssets } = await loadOfflineAssets();

    await expect(prepareOfflineAssets()).resolves.toEqual({
      status: "not-ready",
      capabilities: {
        engine: { status: "ready", cardCount: 3 },
        scryfallSearch: { status: "ready" },
        preconCatalog: { status: "not-ready" },
        bundledAiCatalog: { status: "ready" },
        deckLibrary: { status: "ready" },
      },
    });
    expect(dependencies.loadPreconDeckMap).toHaveBeenCalledTimes(2);
  });

  it.each([
    "unsupported_shell",
    "unauthorized",
    "unavailable",
    "invalid_input",
    "conflict",
    "cancelled",
    "network",
    "storage",
    "insufficient_storage",
    "trust",
    "emit",
    "internal",
  ] as const)("preserves the Deck Catalog backend error kind %s", async (error) => {
    dependencies.prepareDeckLibraryForOffline.mockRejectedValue(new VisualPackBackendError(error));
    const { prepareOfflineAssets } = await loadOfflineAssets();

    await expect(prepareOfflineAssets()).resolves.toEqual(readinessWithDeckLibraryError(error));
  });

  it("maps an unexpected Deck Catalog rejection to unavailable", async () => {
    dependencies.prepareDeckLibraryForOffline.mockRejectedValue(new Error("unknown backend failure"));
    const { prepareOfflineAssets } = await loadOfflineAssets();

    await expect(prepareOfflineAssets()).resolves.toEqual(readinessWithDeckLibraryError("unavailable"));
  });

  it("retains real successful browser-data caches after a later fetch failure", async () => {
    const minimalCardDataExport = JSON.stringify({
      "lightning bolt": {
        name: "Lightning Bolt",
        mana_cost: { type: "Cost", generic: 0, shards: ["Red"] },
        card_type: { supertypes: [], core_types: ["Instant"], subtypes: [] },
        oracle_text: "Lightning Bolt deals 3 damage to any target.",
        keywords: [],
        abilities: [],
        triggers: [],
        static_abilities: [],
        replacements: [],
        color_identity: ["Red"],
        legalities: {},
      },
    });
    const scryfallData = {
      "lightning bolt": {
        oracle_id: "lightning-bolt",
        name: "Lightning Bolt",
        face_names: ["lightning bolt"],
        mana_cost: "{R}",
        cmc: 1,
        type_line: "Instant",
        colors: ["R"],
        color_identity: ["R"],
        keywords: [],
        faces: [{}],
      },
    };
    const precons = {
      [GENERATED_PRECON_ID]: {
        code: "TST",
        name: "Generated Commander",
        type: "Commander Deck",
        coveragePct: 100,
        mainBoard: [{ name: "Lightning Bolt", count: 1 }],
      },
    };
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/card-data.json") return Promise.resolve(new Response(minimalCardDataExport, { status: 200 }));
      if (url === "/scryfall-data.json") return Promise.resolve(new Response(JSON.stringify(scryfallData), { status: 200 }));
      if (url === "/decks.json") return Promise.resolve(new Response(JSON.stringify(precons), { status: 200 }));
      return Promise.reject(new Error(`Unexpected fetch: ${url}`));
    });
    vi.stubGlobal("fetch", fetchMock);
    wasm.loadCardDatabase.mockImplementation(async (payload: string) => {
      wasm.cardDatabasePayload = payload;
      const cardData = JSON.parse(payload) as Record<string, unknown>;
      const lightningBolt = cardData["lightning bolt"];
      wasm.cardDatabaseLoaded = payload === minimalCardDataExport
        && Object.keys(cardData).length > 0
        && typeof lightningBolt === "object"
        && lightningBolt !== null
        && (lightningBolt as { name?: unknown }).name === "Lightning Bolt";
      return wasm.cardDatabaseLoaded ? 1 : 0;
    });
    wasm.searchCards.mockImplementation(() => {
      if (!wasm.cardDatabaseLoaded || wasm.cardDatabasePayload !== minimalCardDataExport) {
        throw new Error("Card database was not successfully loaded");
      }
      return {
        results: [{
          name: "Lightning Bolt",
          oracle_id: "lightning-bolt",
          mana_value: 1,
          color_identity: ["R"],
          legalities: {},
        }],
        total: 1,
      };
    });
    dependencies.useRealLoaders = true;
    dependencies.prepareDeckLibraryForOffline.mockResolvedValue("not-installed");
    const { prepareOfflineAssets } = await loadOfflineAssets();

    await expect(prepareOfflineAssets()).resolves.toEqual({
      status: "ready",
      capabilities: {
        engine: { status: "ready", cardCount: 1 },
        scryfallSearch: { status: "ready" },
        preconCatalog: { status: "ready" },
        bundledAiCatalog: { status: "ready" },
        deckLibrary: { status: "not-installed" },
      },
    });
    expect(wasm.loadCardDatabase).toHaveBeenCalledWith(minimalCardDataExport);
    expect(wasm.cardDatabaseLoaded).toBe(true);

    fetchMock.mockClear();
    fetchMock.mockRejectedValue(new Error("offline"));
    const engineRuntime = dependencies.actual.engineRuntime;
    const decks = dependencies.actual.decks;
    const catalogService = dependencies.actual.catalog;
    if (!engineRuntime || !decks || !catalogService) throw new Error("Expected real loader modules");

    await expect(engineRuntime.searchCards({ text: "Lightning Bolt" })).resolves.toMatchObject({
      cards: [{ name: "Lightning Bolt" }],
      total: 1,
    });
    await expect(decks.loadPreconDeckMap()).resolves.toEqual(precons);
    const catalog = await catalogService.buildDeckCatalog();

    expect(catalog.some((candidate) => candidate.source.type === "precon"
      && candidate.source.deckId === GENERATED_PRECON_ID)).toBe(true);
    expect(catalog.some((candidate) => candidate.source.type === "precon"
      && candidate.source.deckId === bundledDeckId())).toBe(true);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
