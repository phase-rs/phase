import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, useLocation, useNavigate } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useAppNotificationStore } from "../../stores/appToastStore";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn<(command: string) => Promise<string | null>>(),
  listen: vi.fn(),
  unlisten: vi.fn(),
  isDesktopTauri: vi.fn(() => true),
  isMultiplayerGameLive: vi.fn(() => false),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));
vi.mock("../../services/platform", () => ({ isDesktopTauri: mocks.isDesktopTauri }));
vi.mock("../../pwa/multiplayerGuard", () => ({ isMultiplayerGameLive: mocks.isMultiplayerGameLive }));

import { useDesktopDeepLinks } from "../useDesktopDeepLinks";

const ARRIVAL = "/multiplayer?join=AB12CD%40wss%3A%2F%2Flobby.phase-rs.dev%2Fws";
const OTHER_ORIGIN = `https://preview.phase-rs.dev${ARRIVAL}`;
const originalLocation = window.location;
const assign = vi.fn();
let pendingHandler: (() => void) | null = null;

function sameOrigin(path: string): string {
  return `${originalLocation.origin}${path}`;
}

function Harness() {
  useDesktopDeepLinks();
  const location = useLocation();
  const navigate = useNavigate();
  return (
    <>
      <p data-testid="location">{location.pathname + location.search}</p>
      <button type="button" onClick={() => navigate("/elsewhere")}>
        elsewhere
      </button>
    </>
  );
}

function renderHarness() {
  return render(
    <MemoryRouter initialEntries={["/"]}>
      <Harness />
    </MemoryRouter>,
  );
}

/** Resolve the subscription and every take it chains. */
async function settle() {
  await act(async () => {
    for (let i = 0; i < 5; i += 1) await Promise.resolve();
  });
}

function probe(): string | null {
  return screen.getByTestId("location").textContent;
}

async function firePending() {
  expect(pendingHandler).not.toBeNull();
  await act(async () => {
    pendingHandler?.();
  });
  await settle();
}

describe("useDesktopDeepLinks", () => {
  beforeEach(() => {
    pendingHandler = null;
    mocks.invoke.mockReset().mockResolvedValue(null);
    mocks.listen.mockReset().mockImplementation(async (_event: string, handler: () => void) => {
      pendingHandler = handler;
      return mocks.unlisten;
    });
    mocks.unlisten.mockReset();
    mocks.isDesktopTauri.mockReset().mockReturnValue(true);
    mocks.isMultiplayerGameLive.mockReset().mockReturnValue(false);
    assign.mockReset();
    Object.defineProperty(window, "location", {
      configurable: true,
      writable: true,
      value: { ...originalLocation, origin: originalLocation.origin, assign },
    });
    useAppNotificationStore.getState().clearNotification();
  });

  afterEach(() => {
    cleanup();
    Object.defineProperty(window, "location", {
      configurable: true,
      writable: true,
      value: originalLocation,
    });
    useAppNotificationStore.getState().clearNotification();
  });

  it("(a) takes on mount and navigates to a same-origin link", async () => {
    mocks.invoke.mockResolvedValueOnce(sameOrigin(ARRIVAL));
    renderHarness();

    await waitFor(() => expect(probe()).toBe(ARRIVAL));
    expect(mocks.invoke).toHaveBeenCalledWith("take_pending_deep_link");
    expect(mocks.listen).toHaveBeenCalledWith("deep-link-pending", expect.any(Function));
    expect(assign).not.toHaveBeenCalled();
  });

  it("(b) takes again on each pending event", async () => {
    renderHarness();
    await settle();
    expect(mocks.invoke).toHaveBeenCalledTimes(1);
    expect(probe()).toBe("/");

    mocks.invoke.mockResolvedValueOnce(sameOrigin(ARRIVAL));
    await firePending();

    expect(mocks.invoke).toHaveBeenCalledTimes(2);
    expect(probe()).toBe(ARRIVAL);
  });

  it("(c) assigns an other-origin link and never navigates", async () => {
    mocks.invoke.mockResolvedValueOnce(OTHER_ORIGIN);
    renderHarness();

    await waitFor(() => expect(assign).toHaveBeenCalledWith(OTHER_ORIGIN));
    expect(probe()).toBe("/");
  });

  it("(d) while a game is live, takes and discards the link with an app-wide notice", async () => {
    mocks.isMultiplayerGameLive.mockReturnValue(true);
    mocks.invoke.mockResolvedValueOnce(sameOrigin(ARRIVAL));
    renderHarness();
    await settle();

    expect(mocks.invoke).toHaveBeenCalledTimes(1);
    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Game in progress",
      description: "Finish or leave your current game, then open the link again.",
    });
    expect(probe()).toBe("/");
    expect(assign).not.toHaveBeenCalled();
  });

  it("(d') while a game is live, an other-origin link is not assigned either", async () => {
    mocks.isMultiplayerGameLive.mockReturnValue(true);
    mocks.invoke.mockResolvedValueOnce(OTHER_ORIGIN);
    renderHarness();
    await settle();

    expect(mocks.invoke).toHaveBeenCalledTimes(1);
    expect(useAppNotificationStore.getState().notification?.title).toBe("Game in progress");
    expect(assign).not.toHaveBeenCalled();
  });

  it("(e) re-delivery after the game ends navigates", async () => {
    mocks.isMultiplayerGameLive.mockReturnValue(true);
    mocks.invoke.mockResolvedValueOnce(sameOrigin(ARRIVAL));
    renderHarness();
    await settle();
    expect(probe()).toBe("/");

    mocks.isMultiplayerGameLive.mockReturnValue(false);
    mocks.invoke.mockResolvedValueOnce(sameOrigin(ARRIVAL));
    await firePending();

    expect(probe()).toBe(ARRIVAL);
  });

  it("(f) subscribes before the mount take", async () => {
    let resolveListen!: (stop: () => void) => void;
    mocks.listen.mockImplementation((_event: string, handler: () => void) => {
      pendingHandler = handler;
      return new Promise((resolve) => {
        resolveListen = resolve;
      });
    });
    mocks.invoke.mockResolvedValueOnce(sameOrigin(ARRIVAL));
    renderHarness();
    await settle();

    expect(mocks.listen).toHaveBeenCalledTimes(1);
    expect(mocks.invoke).not.toHaveBeenCalled();

    await act(async () => {
      resolveListen(mocks.unlisten);
    });
    await settle();
    expect(mocks.invoke).toHaveBeenCalledTimes(1);
    expect(probe()).toBe(ARRIVAL);
  });

  it("(g) a route change neither re-subscribes nor re-takes", async () => {
    renderHarness();
    await settle();
    expect(mocks.listen).toHaveBeenCalledTimes(1);
    expect(mocks.invoke).toHaveBeenCalledTimes(1);

    await act(async () => {
      screen.getByRole("button", { name: "elsewhere" }).click();
    });
    await settle();

    expect(probe()).toBe("/elsewhere");
    expect(mocks.listen).toHaveBeenCalledTimes(1);
    expect(mocks.invoke).toHaveBeenCalledTimes(1);
    expect(mocks.unlisten).not.toHaveBeenCalled();
  });

  it("unsubscribes on unmount", async () => {
    const view = renderHarness();
    await settle();
    view.unmount();
    await settle();
    expect(mocks.unlisten).toHaveBeenCalledTimes(1);
  });

  it("a null take is a no-op", async () => {
    renderHarness();
    await settle();

    expect(mocks.invoke).toHaveBeenCalledTimes(1);
    expect(probe()).toBe("/");
    expect(assign).not.toHaveBeenCalled();
    expect(useAppNotificationStore.getState().notification).toBeNull();
  });

  it("a rejected take (older shell) is a no-op", async () => {
    mocks.invoke.mockRejectedValueOnce(new Error("command take_pending_deep_link not found"));
    renderHarness();
    await settle();

    expect(mocks.invoke).toHaveBeenCalledTimes(1);
    expect(probe()).toBe("/");
    expect(assign).not.toHaveBeenCalled();
    expect(useAppNotificationStore.getState().notification).toBeNull();
  });

  it("does nothing outside the desktop shell", async () => {
    mocks.isDesktopTauri.mockReturnValue(false);
    mocks.invoke.mockResolvedValue(sameOrigin(ARRIVAL));
    renderHarness();
    await settle();

    expect(mocks.listen).not.toHaveBeenCalled();
    expect(mocks.invoke).not.toHaveBeenCalled();
    expect(probe()).toBe("/");
  });
});
