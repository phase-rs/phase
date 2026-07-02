import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { EngineWorkerClient } from "../engine-worker-client";

const notifyEngineSlow = vi.hoisted(() => vi.fn());
vi.mock("../../game/engineRecovery", () => ({
  notifyEngineSlow,
}));

/**
 * Controllable stand-in for the engine Web Worker. Captures posted messages
 * and lets a test decide whether (and when) to reply, so we can exercise the
 * watchdog timeout that surfaces a slow-operation dialog while preserving the
 * in-flight request for a late worker response.
 */
class MockWorker {
  /** The most recently constructed instance, so a test can drive its replies. */
  static last: MockWorker | undefined;

  onmessage: ((e: MessageEvent) => void) | null = null;
  onerror: ((e: ErrorEvent) => void) | null = null;
  readonly posted: Array<Record<string, unknown>> = [];

  constructor() {
    MockWorker.last = this;
  }

  postMessage(msg: Record<string, unknown>): void {
    this.posted.push(msg);
  }

  terminate(): void {}

  /** Simulate a `result` reply for a previously-posted request id. */
  replyResult(id: number, data: unknown): void {
    this.onmessage?.({ data: { type: "result", id, data } } as MessageEvent);
  }
}

function currentWorker(): MockWorker {
  if (!MockWorker.last) throw new Error("no MockWorker constructed yet");
  return MockWorker.last;
}

beforeEach(() => {
  vi.stubGlobal("Worker", MockWorker);
});

afterEach(() => {
  vi.useRealTimers();
  notifyEngineSlow.mockClear();
  vi.unstubAllGlobals();
});

describe("EngineWorkerClient request timeout", () => {
  it("notifies on a slow gameplay round-trip but keeps the request alive", async () => {
    vi.useFakeTimers();
    const client = new EngineWorkerClient();

    const promise = client.getState();
    let settled = false;
    promise.then(
      () => {
        settled = true;
      },
      () => {
        settled = true;
      },
    );
    const worker = currentWorker();
    const reqId = worker.posted[0].id as number;

    await vi.advanceTimersByTimeAsync(60_000);

    expect(notifyEngineSlow).toHaveBeenCalledWith("getState-timeout");
    expect(settled).toBe(false);

    worker.replyResult(reqId, { stack: [] });
    await expect(promise).resolves.toEqual({ stack: [] });
  });

  it("does not false-reject when the worker replies before the timeout, and clears the timer", async () => {
    vi.useFakeTimers();
    const client = new EngineWorkerClient();

    const promise = client.getState();
    const worker = currentWorker();
    const reqId = worker.posted[0].id as number;

    // Slow-but-completing reply at 30s — well within the 60s watchdog.
    await vi.advanceTimersByTimeAsync(30_000);
    worker.replyResult(reqId, { stack: [] });

    await expect(promise).resolves.toEqual({ stack: [] });

    // Pushing past the original deadline must not re-settle or throw: the
    // settle path cleared the watchdog timer. A still-pending timer would
    // fire here and reject an already-resolved promise (an unhandled
    // rejection that fails the run).
    await vi.advanceTimersByTimeAsync(60_000);
    await expect(promise).resolves.toEqual({ stack: [] });
  });
});
