import { afterEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => {
  let releasePlatform!: () => void;
  const platformLatch = new Promise<void>((resolve) => { releasePlatform = resolve; });
  let releaseMigration!: () => void;
  const migrationLatch = new Promise<void>((resolve) => { releaseMigration = resolve; });
  let releaseConnectivity!: () => void;
  const connectivityLatch = new Promise<void>((resolve) => { releaseConnectivity = resolve; });
  const calls: string[] = [];
  return {
    calls,
    releasePlatform,
    releaseMigration,
    releaseConnectivity,
    initializePlatform: vi.fn(async () => {
      calls.push("platform-start");
      await platformLatch;
      calls.push("platform-settled");
    }),
    initializeConnectivity: vi.fn(async () => {
      calls.push("connectivity-start");
      await connectivityLatch;
      calls.push("connectivity-settled");
    }),
    importLegacyStorage: vi.fn(async () => {
      calls.push("migration-start");
      await migrationLatch;
      calls.push("migration-settled");
    }),
  };
});

vi.mock("../services/platform", () => ({ initializeHostPlatform: mocks.initializePlatform }));
vi.mock("../stores/connectivityStore", () => ({ initializeConnectivity: mocks.initializeConnectivity }));
vi.mock("../App", () => ({ App: () => null }));
vi.mock("../polyfills/cryptoRandomUUID", () => ({}));
vi.mock("../i18n", () => ({}));
vi.mock("react-dom/client", () => ({
  createRoot: () => ({ render: () => mocks.calls.push("render") }),
}));
vi.mock("../services/legacyMigration", () => ({
  importLegacyStorage: mocks.importLegacyStorage,
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

it("settles connectivity after migration before rendering or registrations", async () => {
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
    mocks.calls.push("animation-frame");
    callback(0);
    return 1;
  });

  await import("../main");
  expect(mocks.initializePlatform).toHaveBeenCalledOnce();
  expect(mocks.calls).toEqual(["platform-start"]);

  mocks.releasePlatform();
  await vi.waitFor(() => expect(mocks.calls).toContain("migration-start"));
  expect(mocks.calls).toEqual([
    "platform-start",
    "platform-settled",
    "migration-start",
  ]);
  expect(mocks.initializeConnectivity).not.toHaveBeenCalled();

  mocks.releaseMigration();
  await vi.waitFor(() => expect(mocks.calls).toContain("connectivity-start"));
  expect(mocks.calls).toEqual([
    "platform-start",
    "platform-settled",
    "migration-start",
    "migration-settled",
    "connectivity-start",
  ]);
  expect(mocks.calls).not.toContain("render");
  expect(mocks.calls).not.toContain("service-worker");

  mocks.releaseConnectivity();
  await vi.waitFor(() => expect(mocks.calls).toContain("marked"));
  expect(mocks.calls).toEqual([
    "platform-start",
    "platform-settled",
    "migration-start",
    "migration-settled",
    "connectivity-start",
    "connectivity-settled",
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
