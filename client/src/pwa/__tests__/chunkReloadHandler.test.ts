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

function firePayloadlessError(): void {
  window.dispatchEvent(new Event("vite:preloadError", { cancelable: true }));
}

function guardEntry(message: string): { count: number; firstAt: number } | null {
  const raw = window.sessionStorage.getItem(`chunk-reload:${message}`);
  return raw ? (JSON.parse(raw) as { count: number; firstAt: number }) : null;
}

// NOTE: the handler latches loop-abort reporting per guard key at module
// level (survives across tests in this file), so every test that reaches the
// breach path must use its own unique chunk message.
function expectAbortEvent(chunk: string | undefined): void {
  expect(mocks.trackEvent).toHaveBeenCalledWith(
    "chunk_reload",
    expect.objectContaining({ reason: "loop-abort", deferred: false, chunk }),
  );
}

async function expectProbeEvent(fields: Record<string, unknown>): Promise<void> {
  await vi.waitFor(() => {
    expect(mocks.trackEvent).toHaveBeenCalledWith(
      "chunk_reload",
      expect.objectContaining({ reason: "loop-abort-probe", deferred: false, ...fields }),
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

  it("aborts the loop on the third error: no reload, error surfaced, marker then probe", async () => {
    const url = "https://phase-rs.dev/assets/GamePage-abort.js";
    const message = `Failed to fetch dynamically imported module: ${url}`;

    firePreloadError(message);
    firePreloadError(message);
    firePreloadError(message);

    expect(reloadSpy).toHaveBeenCalledTimes(2);
    expect(mocks.setUpdateError).toHaveBeenCalledTimes(1);
    // The abort marker is emitted synchronously — it must not depend on the
    // async probe resolving (the user may refresh/close during the probe).
    expectAbortEvent(message);
    expect(mocks.flushNow).toHaveBeenCalled();
    await expectProbeEvent({
      probe_status: 200,
      probe_cache: "HIT",
      probe_ray: "ray-1",
      probe_sw: 0,
    });
    expect(fetchMock).toHaveBeenCalledWith(url, expect.objectContaining({ cache: "no-store" }));
    // The abort path replaces (not accompanies) the preload-error event.
    const reasons = mocks.trackEvent.mock.calls.map(([, fields]) => fields.reason);
    expect(reasons.filter((r) => r === "preload-error")).toHaveLength(2);
  });

  it("reports probe_status 0 when the probe fetch itself fails", async () => {
    const message =
      "Failed to fetch dynamically imported module: https://phase-rs.dev/assets/GamePage-dead.js";
    fetchMock.mockRejectedValue(new TypeError("Failed to fetch"));
    window.sessionStorage.setItem(
      `chunk-reload:${message}`,
      JSON.stringify({ count: 2, firstAt: Date.now() }),
    );

    firePreloadError(message);

    expect(reloadSpy).not.toHaveBeenCalled();
    expectAbortEvent(message);
    await expectProbeEvent({ probe_status: 0, probe_sw: 0 });
  });

  it("reports the abort exactly once per chunk despite repeated failures", async () => {
    const message =
      "Failed to fetch dynamically imported module: https://phase-rs.dev/assets/GamePage-latch.js";
    window.sessionStorage.setItem(
      `chunk-reload:${message}`,
      JSON.stringify({ count: 2, firstAt: Date.now() }),
    );

    firePreloadError(message);
    firePreloadError(message);
    firePreloadError(message);

    expect(reloadSpy).not.toHaveBeenCalled();
    expect(mocks.setUpdateError).toHaveBeenCalledTimes(1);
    await expectProbeEvent({ probe_status: 200 });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const abortCount = mocks.trackEvent.mock.calls.filter(
      ([, fields]) => fields.reason === "loop-abort",
    ).length;
    expect(abortCount).toBe(1);
  });

  it("skips the probe fetch when the message carries no URL", async () => {
    const message = "Unable to preload CSS";
    window.sessionStorage.setItem(
      `chunk-reload:${message}`,
      JSON.stringify({ count: 2, firstAt: Date.now() }),
    );

    firePreloadError(message);

    expect(reloadSpy).not.toHaveBeenCalled();
    expectAbortEvent(message);
    await expectProbeEvent({ probe_sw: 0 });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("guards payload-less events under the shared unknown key, failing conservative", () => {
    firePayloadlessError();
    firePayloadlessError();
    expect(reloadSpy).toHaveBeenCalledTimes(2);

    firePayloadlessError();

    expect(reloadSpy).toHaveBeenCalledTimes(2);
    expect(mocks.setUpdateError).toHaveBeenCalledTimes(1);
    expectAbortEvent(undefined);
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

  it("leaves an already-queued deferred reload intact when a later error breaches", () => {
    mocks.isMultiplayerGameLive.mockReturnValue(true);
    const message =
      "Failed to fetch dynamically imported module: https://phase-rs.dev/assets/GamePage-mp.js";

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
    expectAbortEvent(message);

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
