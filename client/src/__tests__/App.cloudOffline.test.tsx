import { act, render } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  calls: [] as string[],
  offline: { value: true },
  cleanup: vi.fn(),
  feedInit: vi.fn(),
  feedInitializationReady: true,
  deckLibraryAutoSync: vi.fn(),
  init: vi.fn(),
  pause: vi.fn(),
  migrate: vi.fn(),
}));

vi.mock("../stores/connectivityStore", () => ({
  useEffectiveOffline: () => mocks.offline.value,
}));
vi.mock("../stores/cloudSyncStore", () => ({
  useCloudSyncStore: {
    getState: () => ({ init: mocks.init, pause: mocks.pause }),
  },
}));
vi.mock("../services/deckMigrations", () => ({ migrateSavedDecks: mocks.migrate }));
vi.mock("../hooks/useFeedInitialization", () => ({
  useFeedInitialization: (...args: unknown[]) => {
    mocks.feedInit(...args);
    return mocks.feedInitializationReady;
  },
}));
vi.mock("../hooks/useHostingSession", () => ({ useHostingSession: vi.fn() }));
vi.mock("../services/visualPacks/deckLibraryAutoSync", () => ({ useDeckLibraryAutoSync: mocks.deckLibraryAutoSync }));
vi.mock("../startup/preloadAssets", () => ({ ensurePreload: vi.fn(), subscribePreload: () => () => {} }));
vi.mock("../components/chrome/AppShell", () => ({ AppShell: () => null }));
vi.mock("../components/chrome/AppToast", () => ({ AppToast: () => null }));
vi.mock("../components/chrome/NativeEngineProgressOverlay", () => ({ NativeEngineProgressOverlay: () => null }));
vi.mock("../components/chrome/RouteTelemetry", () => ({ RouteTelemetry: () => null }));
vi.mock("../components/chrome/HostControlTile", () => ({ HostControlTile: () => null }));
vi.mock("../components/ErrorBoundary", () => ({ ErrorBoundary: ({ children }: { children: ReactNode }) => children }));
vi.mock("../components/modal/EngineLostModal", () => ({ EngineLostModal: () => null }));
vi.mock("../components/modal/NonFatalPanicToast", () => ({ NonFatalPanicToast: () => null }));
vi.mock("../components/modal/StuckDecisionToast", () => ({ StuckDecisionToast: () => null }));
vi.mock("../components/splash/SplashScreen", () => ({ SplashScreen: () => null }));
vi.mock("../pages/MenuPage", () => ({ MenuPage: () => null }));

import { App } from "../App";

beforeEach(() => {
  vi.clearAllMocks();
  mocks.calls.length = 0;
  mocks.offline.value = true;
  mocks.feedInitializationReady = true;
  mocks.migrate.mockImplementation(() => mocks.calls.push("migrate"));
  mocks.pause.mockImplementation(() => mocks.calls.push("pause"));
  mocks.init.mockImplementation(() => {
    mocks.calls.push("init");
    return mocks.cleanup;
  });
});

afterEach(() => vi.restoreAllMocks());

describe("App cloud lifecycle", () => {
  it("passes the cloud lifecycle's effective-offline value to feed initialization", () => {
    const view = render(<App />);
    expect(mocks.feedInit).toHaveBeenLastCalledWith(true);

    mocks.offline.value = false;
    act(() => view.rerender(<App />));
    expect(mocks.feedInit).toHaveBeenLastCalledWith(false);
  });

  it("passes the sole offline policy and current feed readiness to deck-library scheduling", () => {
    const view = render(<App />);
    expect(mocks.deckLibraryAutoSync).toHaveBeenLastCalledWith(true, true);

    mocks.offline.value = false;
    mocks.feedInitializationReady = false;
    act(() => view.rerender(<App />));
    expect(mocks.deckLibraryAutoSync).toHaveBeenLastCalledWith(false, false);
  });

  it("migrates before pausing on an offline mount", () => {
    render(<App />);

    expect(mocks.calls).toEqual(["migrate", "pause"]);
    expect(mocks.init).not.toHaveBeenCalled();
  });

  it("starts an online generation and cleans it before a later offline pause", () => {
    mocks.offline.value = false;
    const view = render(<App />);
    expect(mocks.calls).toEqual(["migrate", "init"]);

    mocks.offline.value = true;
    act(() => view.rerender(<App />));

    expect(mocks.cleanup).toHaveBeenCalledTimes(1);
    expect(mocks.calls).toEqual(["migrate", "init", "pause"]);
  });

  it("does not retain an offline effect cleanup when returning online", () => {
    const view = render(<App />);
    mocks.offline.value = false;
    act(() => view.rerender(<App />));

    expect(mocks.cleanup).not.toHaveBeenCalled();
    expect(mocks.calls).toEqual(["migrate", "pause", "init"]);
  });

  it("orders online cleanup before the next pause and successor init", () => {
    mocks.cleanup.mockImplementation(() => mocks.calls.push("cleanup"));
    mocks.offline.value = false;
    const view = render(<App />);

    mocks.offline.value = true;
    act(() => view.rerender(<App />));
    mocks.offline.value = false;
    act(() => view.rerender(<App />));

    expect(mocks.calls).toEqual(["migrate", "init", "cleanup", "pause", "init"]);
  });
});
