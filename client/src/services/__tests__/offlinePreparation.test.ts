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
    const visual = {
      catalogStatus: vi.fn(async () => {
        calls.push("catalog");
        return { status: "ready", summary: { installedPacks: [{}] } };
      }),
      verify: vi.fn(async () => {
        calls.push("verify");
        return { issues: [] };
      }),
    };
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
    mocks.loadVisual.mockResolvedValue({
      catalogStatus: vi.fn(async () => ({ status: "ready", summary: { installedPacks: [{}] } })),
      verify: vi.fn(async () => ({ issues: [{ kind: "missing_object" }] })),
    });

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
    ["empty catalog", { catalogStatus: vi.fn(async () => ({ status: "empty" })) }, { status: "not-installed" }],
    ["invalid catalog", { catalogStatus: vi.fn(async () => ({ status: "invalid" })) }, { status: "warning", issueKinds: ["invalid-catalog"] }],
    ["no installed packs", { catalogStatus: vi.fn(async () => ({ status: "ready", summary: { installedPacks: [] } })) }, { status: "not-installed" }],
  ] as const)("reports optional visual %s without verification", async (_label, backend, visualPacks) => {
    mocks.loadVisual.mockResolvedValueOnce(backend);

    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "ready",
      visualPacks,
    });
  });

  it("reports healthy installed visual packs and catches visual backend failures as optional warnings", async () => {
    const healthy = {
      catalogStatus: vi.fn(async () => ({ status: "ready" as const, summary: { installedPacks: [{}] } })),
      verify: vi.fn(async () => ({ issues: [] })),
    };
    mocks.loadVisual.mockResolvedValueOnce(healthy);
    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "ready",
      visualPacks: { status: "ready" },
    });
    expect(healthy.verify).toHaveBeenCalledWith("full");

    mocks.loadVisual.mockRejectedValueOnce(new Error("adapter unavailable"));
    await expect(prepareForOffline({ nativeEngineEnabled: false })).resolves.toMatchObject({
      status: "ready",
      visualPacks: { status: "warning", issueKinds: ["unavailable"] },
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
