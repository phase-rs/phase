import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  offline: false,
  shell: vi.fn(),
  assets: vi.fn(),
  loadVisual: vi.fn(),
  canNative: vi.fn(),
  nativeKey: vi.fn(),
  prepareNative: vi.fn(),
}));

vi.mock("../../stores/connectivityStore.ts", () => ({ getEffectiveOffline: () => mocks.offline }));
vi.mock("../../pwa/registerServiceWorker.ts", () => ({ checkAppShellReadiness: mocks.shell }));
vi.mock("../offlineAssets.ts", () => ({ prepareOfflineAssets: mocks.assets }));
vi.mock("../platform.ts", () => ({ loadVisualPackBackend: mocks.loadVisual }));
vi.mock("../nativeEngine.ts", () => ({
  canAttemptNativeEngine: mocks.canNative,
  nativeEngineKeyForCurrentOrigin: mocks.nativeKey,
  prepareNativeEngineForOffline: mocks.prepareNative,
}));

import { prepareForOffline } from "../offlinePreparation.ts";
import { MANA_SYMBOL_SHARDS } from "../scryfall.ts";

const assetsReady = {
  status: "ready",
  capabilities: {
    engine: { status: "ready", cardCount: 1 },
    scryfallSearch: { status: "ready" },
    preconCatalog: { status: "ready" },
    bundledAiCatalog: { status: "ready" },
    deckLibrary: { status: "not-installed" },
  },
} as const;

/**
 * A visual-pack backend whose core install succeeds, so that a test about ART
 * reporting is not also a test about the core pack.
 *
 * `start` answers `healthy` — the backend's own "already installed, nothing to
 * do" reply — which is the shortest path through `installCorePack` and needs no
 * progress events. Tests that care about the install itself override it.
 */
function visualBackend(overrides: Record<string, unknown> = {}) {
  return {
    catalogStatus: vi.fn(async () => ({ status: "ready", summary: { installedPacks: [{ packId: "core" }] } })),
    verify: vi.fn(async () => ({ issues: [] })),
    subscribeProgress: vi.fn(async () => () => undefined),
    start: vi.fn(async () => ({ status: "healthy" })),
    operationStatus: vi.fn(async () => ({ state: "completed" })),
    remove: vi.fn(async () => ({ removed: [], revision: "1", cleanupIssues: [] })),
    resolve: resolvesCore(true),
    ...overrides,
  };
}

/**
 * A `resolve` answering that every requested candidate either is or is not
 * backed by cached bytes in `core`.
 *
 * The real `resolve` admits an object only when `cache.match(path)` succeeds,
 * so `matches: []` is exactly how an evicted or corrupt cache presents itself
 * while the pack receipt is still on disk.
 */
function resolvesCore(present: boolean) {
  return vi.fn(async (keys: { kind: string; key: string }[]) => ({
    revision: "1",
    entries: keys.map((key, ordinal) => ({
      ordinal,
      key,
      matches: present ? [{ packId: "core", assetKey: `asset:${key.key}` }] : [],
    })),
  }));
}

/** `catalogStatus` reporting exactly these packs as installed. */
function installed(...packIds: string[]) {
  return vi.fn(async () => ({ status: "ready", summary: { installedPacks: packIds.map((packId) => ({ packId })) } }));
}

describe("prepareForOffline", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.offline = false;
    mocks.shell.mockResolvedValue({ status: "ready" });
    mocks.assets.mockResolvedValue(assetsReady);
    mocks.loadVisual.mockResolvedValue(null);
    mocks.canNative.mockReturnValue(false);
    mocks.nativeKey.mockReturnValue(null);
    mocks.prepareNative.mockResolvedValue({ port: 4312 });
  });

  it("does no preparation work while already effectively offline", async () => {
    mocks.offline = true;

    await expect(prepareForOffline({ nativeEngineEnabled: true })).resolves.toMatchObject({
      status: "reconnect-required",
    });

    expect(mocks.shell).not.toHaveBeenCalled();
    expect(mocks.assets).not.toHaveBeenCalled();
    expect(mocks.loadVisual).not.toHaveBeenCalled();
    expect(mocks.prepareNative).not.toHaveBeenCalled();
  });

  it("runs shell, assets, visual verification, native preparation, then shell again", async () => {
    const calls: string[] = [];
    mocks.shell.mockImplementation(async () => {
      calls.push("shell");
      return { status: "ready" };
    });
    mocks.assets.mockImplementation(async () => {
      calls.push("assets");
      return assetsReady;
    });
    // Core resolves cleanly, so this stays a test of ORDER rather than of the
    // install. The catalog is read once, by the art inventory: core is gated on
    // `resolve`, and only consults the catalog when something is missing.
    const visual = visualBackend({
      catalogStatus: vi.fn(async () => {
        calls.push("catalog");
        return { status: "ready", summary: { installedPacks: [{ packId: "core" }, { packId: "curated" }] } };
      }),
      verify: vi.fn(async () => {
        calls.push("verify");
        return { issues: [] };
      }),
    });
    mocks.loadVisual.mockImplementation(async () => {
      calls.push("visual");
      return visual;
    });
    mocks.canNative.mockReturnValue(true);
    mocks.nativeKey.mockReturnValue({ release: { version: "1.0.0" } });
    mocks.prepareNative.mockImplementation(async () => {
      calls.push("native");
      return { port: 4312 };
    });

    await expect(prepareForOffline({ nativeEngineEnabled: true })).resolves.toMatchObject({ status: "ready" });

    expect(calls).toEqual(["shell", "assets", "visual", "catalog", "verify", "native", "shell"]);
    expect(mocks.prepareNative).toHaveBeenCalledWith({ release: { version: "1.0.0" } });
  });

  it("short-circuits every initial app-shell failure before assets or native preparation", async () => {
    mocks.shell.mockResolvedValueOnce({ status: "not-ready", reason: "insecure-context" });

    await expect(prepareForOffline({ nativeEngineEnabled: true })).resolves.toMatchObject({
      status: "failed",
      requiredGaps: ["appShell"],
    });

    expect(mocks.assets).not.toHaveBeenCalled();
    expect(mocks.loadVisual).not.toHaveBeenCalled();
    expect(mocks.prepareNative).not.toHaveBeenCalled();
  });

  it("keeps optional visual verification issues visible without blocking ready local play", async () => {
    mocks.loadVisual.mockResolvedValue(visualBackend({
      catalogStatus: installed("core", "curated"),
      verify: vi.fn(async () => ({ issues: [{ kind: "missing_object" }] })),
    }));

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "ready",
      visualPacks: { status: "warning", issueKinds: ["missing_object"] },
    });
  });

  it.each([
    ["deferred reload", { status: "reload-required", reason: "deferred-reload" }, "reload-or-relaunch-required"],
    ["controller mismatch", { status: "reload-required", reason: "controller-mismatch" }, "reload-or-relaunch-required"],
    ["update in progress", { status: "not-ready", reason: "update-in-progress" }, "reload-or-relaunch-required"],
    ["insecure context", { status: "not-ready", reason: "insecure-context" }, "failed"],
    ["unsupported service worker", { status: "not-ready", reason: "service-worker-unsupported" }, "failed"],
    ["unavailable lifecycle", { status: "not-ready", reason: "lifecycle-unavailable" }, "failed"],
    ["unavailable active worker", { status: "not-ready", reason: "active-worker-unavailable" }, "failed"],
    ["unavailable controller", { status: "not-ready", reason: "controller-unavailable" }, "failed"],
    ["unavailable shell cache", { status: "not-ready", reason: "shell-cache-unavailable" }, "failed"],
    ["unavailable remote marker", { status: "not-ready", reason: "remote-load-marker-unavailable" }, "failed"],
    ["changed lifecycle", { status: "not-ready", reason: "lifecycle-changed" }, "failed"],
  ] as const)("short-circuits initial shell %s", async (_label, shell, status) => {
    mocks.shell.mockResolvedValueOnce(shell);

    await expect(prepareForOffline({ nativeEngineEnabled: true })).resolves.toMatchObject({
      status,
      requiredGaps: ["appShell"],
    });

    expect(mocks.assets).not.toHaveBeenCalled();
    expect(mocks.loadVisual).not.toHaveBeenCalled();
    expect(mocks.nativeKey).not.toHaveBeenCalled();
    expect(mocks.prepareNative).not.toHaveBeenCalled();
  });

  it.each([
    ["no backend", null, { status: "not-installed" }],
    ["empty catalog", visualBackend({ catalogStatus: vi.fn(async () => ({ status: "empty" })) }), { status: "not-installed" }],
    ["invalid catalog", visualBackend({ catalogStatus: vi.fn(async () => ({ status: "invalid" })) }), { status: "warning", issueKinds: ["invalid-catalog"] }],
    ["no installed packs", visualBackend({ catalogStatus: installed() }), { status: "not-installed" }],
    ["only the core pack", visualBackend({ catalogStatus: installed("core") }), { status: "not-installed" }],
  ] as const)("reports optional visual %s without verification", async (_label, backend, visualPacks) => {
    mocks.loadVisual.mockResolvedValue(backend);

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "ready",
      visualPacks,
    });
  });

  it("reports healthy installed visual packs and catches visual backend failures as optional warnings", async () => {
    const healthy = visualBackend({ catalogStatus: installed("core", "curated") });
    mocks.loadVisual.mockResolvedValue(healthy);
    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "ready",
      visualPacks: { status: "ready", installedPacks: ["curated"] },
    });
    expect(healthy.verify).toHaveBeenCalledWith("full");

    // A backend that fails to LOAD leaves art as an optional warning, but core
    // — the card back and mana symbols — is genuinely absent and cannot be
    // installed, so overall readiness is not "ready". That distinction is the
    // whole point of core being a required capability rather than an advisory.
    mocks.loadVisual.mockRejectedValueOnce(new Error("adapter unavailable"));
    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "failed",
      requiredGaps: ["coreVisuals"],
      visualPacks: { status: "warning", issueKinds: ["unavailable"] },
    });
  });

  it("installs the core pack when it is missing, rather than only reporting it", async () => {
    // Nothing resolves until the install has run, which is what makes this a
    // test of the install rather than of the report.
    const resolve = vi.fn()
      .mockImplementationOnce(resolvesCore(false))
      .mockImplementation(resolvesCore(true));
    const backend = visualBackend({ catalogStatus: installed(), resolve });
    mocks.loadVisual.mockResolvedValue(backend);

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "ready",
      capabilities: { coreVisuals: { status: "ready" } },
    });
    expect(backend.start).toHaveBeenCalledWith({
      kind: "install",
      selector: { kind: "core" },
      // Derived, not a literal: a hardcoded count would silently go stale the
      // day a mana shard is added to the catalog.
      objectEstimate: 1 + MANA_SYMBOL_SHARDS.length,
    });
  });

  it("does not reinstall the core pack when its assets are already drawable", async () => {
    const backend = visualBackend({ catalogStatus: installed("core") });
    mocks.loadVisual.mockResolvedValue(backend);

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      capabilities: { coreVisuals: { status: "ready" } },
    });
    expect(backend.start).not.toHaveBeenCalled();
  });

  /**
   * The receipt outliving the bytes is the case a catalog-only check cannot
   * see: Cache Storage was evicted, so `core` is installed and undrawable.
   *
   * The recovery must DROP the receipt first. Both `start()` paths key on that
   * receipt standing at the current root — an install short-circuits to
   * `healthy` and a repair filters its own selector out — so an install issued
   * while it stands would report success and download nothing. That is
   * recorded as MEASURED in `scryfallBackend.ts`, and asserting the order here
   * is what stops the recovery from silently regressing into a no-op.
   */
  it("drops the stale receipt before reinstalling core when its cached bytes are gone", async () => {
    const resolve = vi.fn()
      .mockImplementationOnce(resolvesCore(false))
      .mockImplementation(resolvesCore(true));
    const order: string[] = [];
    const backend = visualBackend({
      catalogStatus: installed("core"),
      resolve,
      remove: vi.fn(async () => { order.push("remove"); return { removed: [], revision: "1", cleanupIssues: [] }; }),
      start: vi.fn(async () => { order.push("start"); return { status: "healthy" }; }),
    });
    mocks.loadVisual.mockResolvedValue(backend);

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      capabilities: { coreVisuals: { status: "ready" } },
    });
    expect(backend.remove).toHaveBeenCalledWith({ kind: "packs", packIds: ["core"] }, "reject_dependents");
    expect(order).toEqual(["remove", "start"]);
  });

  it("does not drop a receipt that does not exist when first installing core", async () => {
    const resolve = vi.fn()
      .mockImplementationOnce(resolvesCore(false))
      .mockImplementation(resolvesCore(true));
    const backend = visualBackend({ catalogStatus: installed(), resolve, remove: vi.fn() });
    mocks.loadVisual.mockResolvedValue(backend);

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      capabilities: { coreVisuals: { status: "ready" } },
    });
    expect(backend.remove).not.toHaveBeenCalled();
  });

  it("stays not-ready when core assets are still missing after the operation", async () => {
    const backend = visualBackend({ catalogStatus: installed("core"), resolve: resolvesCore(false) });
    mocks.loadVisual.mockResolvedValue(backend);

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "failed",
      requiredGaps: ["coreVisuals"],
      capabilities: { coreVisuals: { status: "not-ready" } },
    });
  });


  it("holds readiness back when the core install fails", async () => {
    const backend = visualBackend({
      catalogStatus: installed(),
      resolve: resolvesCore(false),
      start: vi.fn(async () => { throw new Error("network"); }),
    });
    mocks.loadVisual.mockResolvedValue(backend);

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "failed",
      requiredGaps: ["coreVisuals"],
      capabilities: { coreVisuals: { status: "not-ready" } },
    });
  });

  it("waits for a started core install to reach a terminal progress event", async () => {
    let emit: ((event: unknown) => void) | null = null;
    const backend = visualBackend({
      catalogStatus: installed(),
      // Missing before the install, present after — otherwise the install path
      // is never entered and there is no progress event to wait on.
      resolve: vi.fn().mockImplementationOnce(resolvesCore(false)).mockImplementation(resolvesCore(true)),
      subscribeProgress: vi.fn(async (listener: (event: unknown) => void) => {
        emit = listener;
        return () => undefined;
      }),
      start: vi.fn(async () => ({ status: "started", operationId: "op-7" })),
      // Still downloading at the post-start read, so only the event can settle it.
      operationStatus: vi.fn(async () => ({ state: "downloading" })),
    });
    mocks.loadVisual.mockResolvedValue(backend);

    const preparation = prepareForOffline({ nativeEngineEnabled: false });
    await vi.waitFor(() => { expect(emit).not.toBeNull(); });
    // An event for a DIFFERENT operation must not settle this wait.
    emit!({ phase: "completed", operation: { operationId: "op-other" } });
    emit!({ phase: "completed", operation: { operationId: "op-7" } });

    await expect(preparation).resolves.toMatchObject({
      capabilities: { coreVisuals: { status: "ready" } },
    });
  });

  it.each([
    ["disabled", false, true, { release: { version: "1.0.0" } }],
    ["unsupported", true, false, { release: { version: "1.0.0" } }],
    ["without a current key", true, true, null],
  ] as const)("marks native preparation %s as not applicable", async (_label, nativeEngineEnabled, canNative, key) => {
    mocks.canNative.mockReturnValue(canNative);
    mocks.nativeKey.mockReturnValue(key);

    await expect(prepareForOffline({ nativeEngineEnabled })).resolves.toMatchObject({
      status: "ready",
      capabilities: { nativeEngine: { status: "not-applicable" } },
    });
    expect(mocks.prepareNative).not.toHaveBeenCalled();
  });

  it.each([
    ["release", { release: { version: "1.0.0" } }],
    ["preview", { preview: { fingerprint: "preview-123" } }],
  ] as const)("prepares the exact current %s native key", async (_label, key) => {
    mocks.canNative.mockReturnValue(true);
    mocks.nativeKey.mockReturnValue(key);

    await expect(prepareForOffline({ nativeEngineEnabled: true })).resolves.toMatchObject({
      status: "ready",
      capabilities: { nativeEngine: { status: "ready" } },
    });
    expect(mocks.prepareNative).toHaveBeenCalledWith(key);
  });

  it("makes a required native preparation failure a named gap", async () => {
    mocks.canNative.mockReturnValue(true);
    mocks.nativeKey.mockReturnValue({ release: { version: "1.0.0" } });
    mocks.prepareNative.mockRejectedValueOnce(new Error("native unavailable"));

    await expect(prepareForOffline({ nativeEngineEnabled: true })).resolves.toMatchObject({
      status: "failed",
      capabilities: { nativeEngine: { status: "not-ready" } },
      requiredGaps: ["nativeEngine"],
    });
  });

  it("preserves named asset gaps and turns browser engine module skew into reload required", async () => {
    mocks.assets.mockResolvedValueOnce({
      ...assetsReady,
      status: "reload-required",
      capabilities: {
        ...assetsReady.capabilities,
        engine: { status: "reload-required" },
        scryfallSearch: { status: "not-ready" },
        deckLibrary: { status: "not-ready", error: "unavailable" },
      },
    });

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "reload-or-relaunch-required",
      requiredGaps: ["browserEngine", "scryfallSearch", "deckLibrary"],
    });
  });

  it.each([
    ["browser engine", "engine", "browserEngine"],
    ["Scryfall search", "scryfallSearch", "scryfallSearch"],
    ["preconstructed catalog", "preconCatalog", "preconCatalog"],
    ["bundled AI catalog", "bundledAiCatalog", "bundledAiCatalog"],
    ["Deck Library", "deckLibrary", "deckLibrary"],
  ] as const)("maps a missing %s asset to its named required gap", async (_label, asset, capability) => {
    const capabilities = { ...assetsReady.capabilities } as Record<string, { status: string }>;
    capabilities[asset] = { status: "not-ready" };
    mocks.assets.mockResolvedValueOnce({ status: "not-ready", capabilities });

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "failed",
      requiredGaps: [capability],
    });
  });

  it.each([
    ["reload required", { status: "reload-required", reason: "controller-mismatch" }, "reload-or-relaunch-required"],
    ["update in progress", { status: "not-ready", reason: "update-in-progress" }, "reload-or-relaunch-required"],
    ["lifecycle changed", { status: "not-ready", reason: "lifecycle-changed" }, "reload-or-relaunch-required"],
    ["ordinary failure", { status: "not-ready", reason: "insecure-context" }, "failed"],
  ] as const)("gives final shell %s precedence while retaining merged gaps", async (_label, finalShell, status) => {
    mocks.assets.mockResolvedValueOnce({
      ...assetsReady,
      status: "not-ready",
      capabilities: { ...assetsReady.capabilities, scryfallSearch: { status: "not-ready" } },
    });
    mocks.canNative.mockReturnValue(true);
    mocks.nativeKey.mockReturnValue({ release: { version: "1.0.0" } });
    mocks.prepareNative.mockRejectedValueOnce(new Error("missing"));
    mocks.shell.mockResolvedValueOnce({ status: "ready" }).mockResolvedValueOnce(finalShell);

    await expect(prepareForOffline({ nativeEngineEnabled: true })).resolves.toMatchObject({
      status,
      requiredGaps: ["appShell", "scryfallSearch", "nativeEngine"],
    });
  });

  it.each([
    ["ready", { status: "ready" }, "reload-or-relaunch-required", "ready", ["browserEngine"]],
    ["reload required", { status: "reload-required", reason: "deferred-reload" }, "reload-or-relaunch-required", "reload-required", ["appShell", "browserEngine"]],
    ["update in progress", { status: "not-ready", reason: "update-in-progress" }, "reload-or-relaunch-required", "not-ready", ["appShell", "browserEngine"]],
    ["lifecycle changed", { status: "not-ready", reason: "lifecycle-changed" }, "reload-or-relaunch-required", "not-ready", ["appShell", "browserEngine"]],
    ["ordinary not ready", { status: "not-ready", reason: "insecure-context" }, "failed", "not-ready", ["appShell", "browserEngine"]],
  ] as const)("orders final shell %s ahead of browser-engine reload required", async (_label, finalShell, status, appShellStatus, requiredGaps) => {
    mocks.assets.mockResolvedValueOnce({
      ...assetsReady,
      status: "reload-required",
      capabilities: { ...assetsReady.capabilities, engine: { status: "reload-required" } },
    });
    mocks.shell.mockResolvedValueOnce({ status: "ready" }).mockResolvedValueOnce(finalShell);

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status,
      capabilities: {
        appShell: { status: appShellStatus },
        browserEngine: { status: "reload-required" },
      },
      requiredGaps,
    });
  });

  it("runs fresh authorities again for every retry", async () => {
    await prepareForOffline({ nativeEngineEnabled: false });
    await prepareForOffline({ nativeEngineEnabled: false });

    expect(mocks.shell).toHaveBeenCalledTimes(4);
    expect(mocks.assets).toHaveBeenCalledTimes(2);
    expect(mocks.loadVisual).toHaveBeenCalledTimes(2);
  });
});
