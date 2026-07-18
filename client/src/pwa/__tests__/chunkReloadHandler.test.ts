import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { installChunkReloadHandler } from "../chunkReloadHandler";

const mocks = vi.hoisted(() => ({
  isMultiplayerGameLive: vi.fn<() => boolean>(() => false),
  whenMultiplayerGameEnds: vi.fn<(cb: () => void) => () => void>(),
  trackEvent: vi.fn(),
  flushNow: vi.fn(),
  pushUpdateDebug: vi.fn(),
  setUpdateError: vi.fn(),
  setUpdateStatus: vi.fn(),
}));

vi.mock("../multiplayerGuard", () => ({
  isMultiplayerGameLive: mocks.isMultiplayerGameLive,
  whenMultiplayerGameEnds: mocks.whenMultiplayerGameEnds,
}));
vi.mock("../updateStatus", () => ({
  pushUpdateDebug: mocks.pushUpdateDebug,
  setUpdateError: mocks.setUpdateError,
  setUpdateStatus: mocks.setUpdateStatus,
}));
vi.mock("../../services/telemetry", () => ({
  trackEvent: mocks.trackEvent,
  flushNow: mocks.flushNow,
}));

const CHUNK_URL = "https://phase-rs.dev/assets/GamePage-abc.js";
const RELOAD_GUARD_WINDOW_MS = 10 * 60 * 1000;

function firePreloadError(message: string): void {
  const event = new Event("vite:preloadError", { cancelable: true }) as Event & {
    payload?: Error;
  };
  event.payload = new Error(message);
  window.dispatchEvent(event);
}

function guardEntry(message: string): { count: number; firstAt: number } | null {
  const raw = window.sessionStorage.getItem(`chunk-reload:${message}`);
  return raw ? (JSON.parse(raw) as { count: number; firstAt: number }) : null;
}

async function expectLoopAbortEvent(fields: Record<string, unknown>): Promise<void> {
  await vi.waitFor(() => {
    expect(mocks.trackEvent).toHaveBeenCalledWith(
      "chunk_reload",
      expect.objectContaining({ reason: "loop-abort", deferred: false, ...fields }),
    );
  });
}

describe("chunkReloadHandler loop breaker", () => {
  let reloadSpy: ReturnType<typeof vi.fn>;
  let gameEndCallbacks: Array<() => void>;
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeAll(() => {
    installChunkReloadHandler();
  });

  beforeEach(() => {
    vi.clearAllMocks();
    window.sessionStorage.clear();
    gameEndCallbacks = [];
    mocks.isMultiplayerGameLive.mockReturnValue(false);
    mocks.whenMultiplayerGameEnds.mockImplementation((cb: () => void) => {
      gameEndCallbacks.push(cb);
      return () => {};
    });
    reloadSpy = vi.fn();
    Object.defineProperty(window.location, "reload", {
      value: reloadSpy,
      configurable: true,
    });
    fetchMock = vi.fn(async () => ({
      status: 200,
      headers: {
        get: (name: string) =>
          ({ "cf-cache-status": "HIT", "cf-ray": "ray-1" })[name.toLowerCase()] ?? null,
      },
    }));
    vi.stubGlobal("fetch", fetchMock);
  });

  it("reloads on the first two preload errors and counts each executed reload", () => {
    const message = `Failed to fetch dynamically imported module: ${CHUNK_URL}`;

    firePreloadError(message);
    firePreloadError(message);

    expect(reloadSpy).toHaveBeenCalledTimes(2);
    expect(guardEntry(message)?.count).toBe(2);
    expect(mocks.setUpdateError).not.toHaveBeenCalled();
    expect(mocks.trackEvent).toHaveBeenCalledTimes(2);
    expect(mocks.trackEvent).toHaveBeenCalledWith("chunk_reload", {
      reason: "preload-error",
      deferred: false,
      chunk: message,
    });
  });

  it("aborts the loop on the third error: no reload, error surfaced, probe reported", async () => {
    const message = `Failed to fetch dynamically imported module: ${CHUNK_URL}`;

    firePreloadError(message);
    firePreloadError(message);
    firePreloadError(message);

    expect(reloadSpy).toHaveBeenCalledTimes(2);
    expect(mocks.setUpdateError).toHaveBeenCalledTimes(1);
    await expectLoopAbortEvent({
      probe_status: 200,
      probe_cache: "HIT",
      probe_ray: "ray-1",
      probe_sw: 0,
    });
    expect(fetchMock).toHaveBeenCalledWith(
      CHUNK_URL,
      expect.objectContaining({ cache: "no-store" }),
    );
    // The abort path replaces (not accompanies) the preload-error event.
    const reasons = mocks.trackEvent.mock.calls.map(([, fields]) => fields.reason);
    expect(reasons.filter((r) => r === "preload-error")).toHaveLength(2);
  });

  it("skips the probe fetch when the message carries no URL", async () => {
    const message = "Unable to preload CSS";
    window.sessionStorage.setItem(
      `chunk-reload:${message}`,
      JSON.stringify({ count: 2, firstAt: Date.now() }),
    );

    firePreloadError(message);

    expect(reloadSpy).not.toHaveBeenCalled();
    await expectLoopAbortEvent({ probe_sw: 0 });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("tracks each failing chunk independently", () => {
    const messageA = `Failed to fetch dynamically imported module: ${CHUNK_URL}`;
    const messageB = "Failed to fetch dynamically imported module: https://phase-rs.dev/assets/DeckPage-def.js";
    window.sessionStorage.setItem(
      `chunk-reload:${messageA}`,
      JSON.stringify({ count: 2, firstAt: Date.now() }),
    );

    firePreloadError(messageB);

    expect(reloadSpy).toHaveBeenCalledTimes(1);
    expect(guardEntry(messageB)?.count).toBe(1);
  });

  it("resets the counter once the guard window has expired", () => {
    const message = `Failed to fetch dynamically imported module: ${CHUNK_URL}`;
    window.sessionStorage.setItem(
      `chunk-reload:${message}`,
      JSON.stringify({ count: 2, firstAt: Date.now() - RELOAD_GUARD_WINDOW_MS - 1000 }),
    );

    firePreloadError(message);

    expect(reloadSpy).toHaveBeenCalledTimes(1);
    expect(guardEntry(message)?.count).toBe(1);
  });

  it("never trips during a multiplayer game: queuing is not counting", () => {
    mocks.isMultiplayerGameLive.mockReturnValue(true);
    const message = `Failed to fetch dynamically imported module: ${CHUNK_URL}`;

    firePreloadError(message);
    firePreloadError(message);
    firePreloadError(message);

    // First-failure-wins: one queued reload, no executed reloads, no breach.
    expect(reloadSpy).not.toHaveBeenCalled();
    expect(mocks.whenMultiplayerGameEnds).toHaveBeenCalledTimes(1);
    expect(mocks.setUpdateStatus).toHaveBeenCalledWith("deferred");
    expect(mocks.setUpdateError).not.toHaveBeenCalled();
    expect(guardEntry(message)).toBeNull();

    // The deferred reload counts when it fires, not when queued.
    for (const cb of gameEndCallbacks) cb();
    expect(reloadSpy).toHaveBeenCalledTimes(1);
    expect(guardEntry(message)?.count).toBe(1);
  });

  it("leaves an already-queued deferred reload intact when a later error breaches", async () => {
    mocks.isMultiplayerGameLive.mockReturnValue(true);
    const message = `Failed to fetch dynamically imported module: ${CHUNK_URL}`;

    firePreloadError(message);
    expect(mocks.whenMultiplayerGameEnds).toHaveBeenCalledTimes(1);

    // A breach mid-game (counter pre-filled from before the game started)
    // must not queue more work, but must not cancel the queued reload either.
    window.sessionStorage.setItem(
      `chunk-reload:${message}`,
      JSON.stringify({ count: 2, firstAt: Date.now() }),
    );
    firePreloadError(message);

    expect(mocks.whenMultiplayerGameEnds).toHaveBeenCalledTimes(1);
    expect(mocks.setUpdateError).toHaveBeenCalledTimes(1);
    await expectLoopAbortEvent({});

    for (const cb of gameEndCallbacks) cb();
    expect(reloadSpy).toHaveBeenCalledTimes(1);
  });

  it("degrades to always-reload when sessionStorage is unavailable", () => {
    const getItem = vi
      .spyOn(window.sessionStorage, "getItem")
      .mockImplementation(() => {
        throw new Error("blocked");
      });
    const setItem = vi
      .spyOn(window.sessionStorage, "setItem")
      .mockImplementation(() => {
        throw new Error("blocked");
      });
    const message = `Failed to fetch dynamically imported module: ${CHUNK_URL}`;

    firePreloadError(message);
    firePreloadError(message);
    firePreloadError(message);

    expect(reloadSpy).toHaveBeenCalledTimes(3);
    expect(mocks.setUpdateError).not.toHaveBeenCalled();

    getItem.mockRestore();
    setItem.mockRestore();
  });
});
