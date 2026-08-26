import { afterEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => {
  let release!: () => void;
  const latch = new Promise<void>((resolve) => { release = resolve; });
  const calls: string[] = [];
  return { calls, latch, release, initialize: vi.fn(() => latch) };
});

vi.mock("../services/platform", () => ({ initializeHostPlatform: mocks.initialize }));
vi.mock("../App", () => ({ App: () => null }));
vi.mock("../polyfills/cryptoRandomUUID", () => ({}));
vi.mock("../i18n", () => ({}));
vi.mock("react-dom/client", () => ({
  createRoot: () => ({ render: () => mocks.calls.push("render") }),
}));
vi.mock("../services/legacyMigration", () => ({
  importLegacyStorage: vi.fn(async () => { mocks.calls.push("migration"); }),
  markRemoteLoadOk: vi.fn(() => { mocks.calls.push("marked"); }),
}));
vi.mock("../pwa/registerServiceWorker", () => ({
  registerServiceWorker: () => mocks.calls.push("service-worker"),
}));
vi.mock("../pwa/tauriUpdater", () => ({
  registerTauriUpdater: () => mocks.calls.push("updater"),
}));
vi.mock("../pwa/chunkReloadHandler", () => ({
  installChunkReloadHandler: () => mocks.calls.push("chunk-handler"),
}));
vi.mock("../services/externalLinks", () => ({
  installTauriExternalLinkHandler: () => mocks.calls.push("external-links"),
}));
vi.mock("../services/telemetryEvents", () => ({
  installTelemetry: () => mocks.calls.push("telemetry"),
}));

afterEach(() => {
  vi.restoreAllMocks();
});

it("settles the platform latch before every bootstrap side effect", async () => {
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
    mocks.calls.push("animation-frame");
    callback(0);
    return 1;
  });

  await import("../main");
  expect(mocks.initialize).toHaveBeenCalledOnce();
  expect(mocks.calls).toEqual([]);

  mocks.release();
  await vi.waitFor(() => expect(mocks.calls).toContain("marked"));
  expect(mocks.calls).toEqual([
    "migration",
    "render",
    "service-worker",
    "updater",
    "chunk-handler",
    "external-links",
    "telemetry",
    "animation-frame",
    "marked",
  ]);
});
