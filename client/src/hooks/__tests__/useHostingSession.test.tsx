import { act, cleanup, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useConnectivityStore } from "../../stores/connectivityStore";

const mocks = vi.hoisted(() => ({
  clearPendingGameRoute: vi.fn(),
  resumeServerHosting: vi.fn(),
  cancelHosting: vi.fn(),
  closeBroker: vi.fn(),
  closeSubscriptionSocket: vi.fn(),
  pendingGameRoute: null as string | null,
}));

vi.mock("../../stores/multiplayerStore", () => ({
  useMultiplayerStore: (selector: (state: typeof mocks) => unknown) => selector(mocks),
}));

import { useHostingSession } from "../useHostingSession";

function RouterWrapper({ children }: { children: ReactNode }) {
  return <MemoryRouter>{children}</MemoryRouter>;
}

function expectNoHostingTeardown() {
  expect(mocks.clearPendingGameRoute).not.toHaveBeenCalled();
  expect(mocks.cancelHosting).not.toHaveBeenCalled();
  expect(mocks.closeBroker).not.toHaveBeenCalled();
  expect(mocks.closeSubscriptionSocket).not.toHaveBeenCalled();
}

describe("useHostingSession", () => {
  beforeEach(() => {
    mocks.pendingGameRoute = null;
    vi.clearAllMocks();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  afterEach(() => {
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  it("does not recover a persisted host session during a forced-offline cold start", () => {
    useConnectivityStore.setState({ forcedOffline: true });
    renderHook(() => useHostingSession(), { wrapper: RouterWrapper });

    expect(mocks.resumeServerHosting).not.toHaveBeenCalled();
    expectNoHostingTeardown();
  });

  it("does not recover a persisted host session during a browser-offline cold start", () => {
    useConnectivityStore.setState({ browserOnline: false });
    renderHook(() => useHostingSession(), { wrapper: RouterWrapper });

    expect(mocks.resumeServerHosting).not.toHaveBeenCalled();
    expectNoHostingTeardown();
  });

  it("recovers exactly once when forced offline mode ends", () => {
    useConnectivityStore.setState({ forcedOffline: true });
    renderHook(() => useHostingSession(), { wrapper: RouterWrapper });

    act(() => useConnectivityStore.setState({ forcedOffline: false }));
    expect(mocks.resumeServerHosting).toHaveBeenCalledTimes(1);

    act(() => useConnectivityStore.setState({ forcedOffline: false }));
    expect(mocks.resumeServerHosting).toHaveBeenCalledTimes(1);
  });

  it("recovers an online cold start once and does not tear down an active host when the browser goes offline", () => {
    renderHook(() => useHostingSession(), { wrapper: RouterWrapper });
    expect(mocks.resumeServerHosting).toHaveBeenCalledTimes(1);

    act(() => useConnectivityStore.setState({ browserOnline: false }));
    expect(mocks.resumeServerHosting).toHaveBeenCalledTimes(1);
    expectNoHostingTeardown();
  });
});
