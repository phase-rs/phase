import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  GENERATION_IO_TIMEOUT_MS,
  LIBRARY_VIEW_TIMEOUT_MS,
  LOCK_WAIT_TIMEOUT_MS,
  setSavedDeckTxnLockWaitForTests,
  withSavedDeckLibrary,
  withSavedDeckLibraryOrSkip,
} from "../savedDeckTransaction";
import {
  installFifoWebLocks,
  installRefusingWebLocks,
  readIdbGenerationForTests,
  resetSavedDeckLibraryForTests,
  seedGenerationForTests,
  uninstallWebLocks,
} from "../../test/helpers/webLocks";

const GENERATION_KEY = "phase-saved-deck-library-generation";

beforeEach(async () => {
  installFifoWebLocks();
  await resetSavedDeckLibraryForTests();
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  uninstallWebLocks();
});

describe("withSavedDeckLibrary / withSavedDeckLibraryOrSkip", () => {
  it("runs the body while the lock is held", async () => {
    let observedHeld: unknown[] = [];
    await withSavedDeckLibrary(async () => {
      observedHeld = (await navigator.locks.query()).held ?? [];
    });
    expect(observedHeld).toEqual([{ name: "phase-saved-deck-library", mode: "exclusive" }]);
  });

  it("with no lock manager: a user write and a run-unguarded background write run synchronously; a skip background write is skipped", async () => {
    uninstallWebLocks();
    let ran = false;
    const promise = withSavedDeckLibrary(() => {
      ran = true;
    });
    // Synchronous visibility: the body already ran before the returned promise was awaited.
    expect(ran).toBe(true);
    await promise;

    let ranUnguarded = false;
    const unguardedPromise = withSavedDeckLibraryOrSkip(() => {
      ranUnguarded = true;
    }, "run-unguarded");
    expect(ranUnguarded).toBe(true);
    await expect(unguardedPromise).resolves.toEqual({ status: "committed", value: undefined });

    const skipSpy = vi.fn();
    const result = await withSavedDeckLibraryOrSkip(skipSpy, "skip");
    expect(result).toEqual({ status: "skipped", reason: "lock-unavailable" });
    expect(skipSpy).not.toHaveBeenCalled();

    // Paired positive: with the double installed, the same call commits.
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests();
    const committed = await withSavedDeckLibraryOrSkip(() => "value", "skip");
    expect(committed).toEqual({ status: "committed", value: "value" });
  });

  it("a refused lock request rejects a user write with lock-refused and skips a background write, running neither body", async () => {
    installRefusingWebLocks(new DOMException("nope", "InvalidStateError"));
    const bodySpy = vi.fn(() => "value");
    await expect(withSavedDeckLibrary(bodySpy)).rejects.toMatchObject({ reason: "lock-refused" });
    expect(bodySpy).not.toHaveBeenCalled();

    const bodySpy2 = vi.fn(() => "value");
    const result = await withSavedDeckLibraryOrSkip(bodySpy2, "run-unguarded");
    expect(result).toEqual({ status: "skipped", reason: "lock-refused" });
    expect(bodySpy2).not.toHaveBeenCalled();
  });

  it("rethrows a body error without retrying the body", async () => {
    const bodySpy = vi.fn(() => {
      throw new Error("boom");
    });
    await expect(withSavedDeckLibrary(bodySpy)).rejects.toThrow("boom");
    expect(bodySpy).toHaveBeenCalledTimes(1);
  });

  it("serializes two transactions FIFO with no overlap", async () => {
    const order: string[] = [];
    let releaseFirst!: () => void;
    const first = withSavedDeckLibrary(() => {
      order.push("first-start");
      return new Promise<void>((resolve) => {
        releaseFirst = resolve;
      });
    });
    const second = withSavedDeckLibrary(() => {
      order.push("second-start");
    });
    await vi.waitFor(() => expect(order).toEqual(["first-start"]));
    releaseFirst();
    await Promise.all([first, second]);
    expect(order).toEqual(["first-start", "second-start"]);
  });

  it("publishes the next generation to both IDB and localStorage after a committed transaction, and after a throw", async () => {
    await withSavedDeckLibrary(() => undefined);
    const idbGen1 = await readIdbGenerationForTests();
    const localGen1 = Number(localStorage.getItem(GENERATION_KEY));
    expect(idbGen1).toBe(1);
    expect(localGen1).toBe(1);

    await expect(
      withSavedDeckLibrary(() => {
        throw new Error("boom");
      }),
    ).rejects.toThrow("boom");
    const idbGen2 = await readIdbGenerationForTests();
    const localGen2 = Number(localStorage.getItem(GENERATION_KEY));
    expect(idbGen2).toBe(2);
    expect(localGen2).toBe(2);
  });

  it("IDB unavailable: both policies fail before running the body", async () => {
    vi.spyOn(IDBDatabase.prototype, "transaction").mockImplementation(() => {
      throw new Error("IDB unavailable");
    });
    const bodySpy = vi.fn();
    const skipped = await withSavedDeckLibraryOrSkip(bodySpy, "run-unguarded");
    expect(skipped).toEqual({ status: "skipped", reason: "generation-unreadable" });
    expect(bodySpy).not.toHaveBeenCalled();

    const bodySpy2 = vi.fn();
    await expect(withSavedDeckLibrary(bodySpy2)).rejects.toMatchObject({ reason: "generation-unreadable" });
    expect(bodySpy2).not.toHaveBeenCalled();
  });

  it("on a lock-wait timeout, both policies give up without running the body while the holder keeps the lock", async () => {
    setSavedDeckTxnLockWaitForTests(null);
    vi.useFakeTimers();
    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    const bodySpy = vi.fn(() => "value");
    const waiter = withSavedDeckLibrary(bodySpy);
    waiter.catch(() => {}); // avoid an unhandled-rejection window before the assertion below attaches
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    await vi.advanceTimersByTimeAsync(LOCK_WAIT_TIMEOUT_MS);
    await expect(waiter).rejects.toMatchObject({ reason: "lock-timeout" });
    expect(bodySpy).not.toHaveBeenCalled();
    expect((await navigator.locks.query()).held).toHaveLength(1); // holder still holds it
    expect((await navigator.locks.query()).pending).toHaveLength(0); // the aborted request left the queue

    const skipSpy = vi.fn();
    const skipWaiter = withSavedDeckLibraryOrSkip(skipSpy, "run-unguarded");
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });
    await vi.advanceTimersByTimeAsync(LOCK_WAIT_TIMEOUT_MS);
    await expect(skipWaiter).resolves.toEqual({ status: "skipped", reason: "lock-timeout" });
    expect(skipSpy).not.toHaveBeenCalled();

    releaseHolder();
    await holder;
    vi.useRealTimers();
    setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
  });

  it("under the production lock wait, a queued request is not rejected just before LOCK_WAIT_TIMEOUT_MS and rejects with lock-timeout at it", async () => {
    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    expect(LOCK_WAIT_TIMEOUT_MS).toBe(6000);
    setSavedDeckTxnLockWaitForTests(null); // use the real LOCK_WAIT_TIMEOUT_MS, not a test knob
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
    let rejected = false;
    const waiter = withSavedDeckLibrary(() => "value");
    waiter.catch(() => {
      rejected = true;
    });
    // Synchronous: `locks.request` queues this request before its first await, so the
    // FIFO double's `pending` list already reflects it without waiting for a tick.
    expect((await navigator.locks.query()).pending).toHaveLength(1);

    // Hardcoded, not `LOCK_WAIT_TIMEOUT_MS - 1`/`LOCK_WAIT_TIMEOUT_MS`: importing the constant on
    // both sides would make this pass no matter what the constant is set to.
    await vi.advanceTimersByTimeAsync(5999);
    expect(rejected).toBe(false);

    await vi.advanceTimersByTimeAsync(1);
    expect(rejected).toBe(true);
    await expect(waiter).rejects.toMatchObject({ reason: "lock-timeout" });

    vi.useRealTimers();
    releaseHolder();
    await holder;
    setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
  });

  it("on a view-catch-up timeout, both policies fail and record the committed generation locally", async () => {
    await seedGenerationForTests(3, 2);
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
    const skipSpy = vi.fn();
    const skipResult = withSavedDeckLibraryOrSkip(skipSpy, "run-unguarded");
    await vi.advanceTimersByTimeAsync(LIBRARY_VIEW_TIMEOUT_MS);
    await expect(skipResult).resolves.toEqual({ status: "skipped", reason: "library-view-stale" });
    expect(skipSpy).not.toHaveBeenCalled();
    expect(localStorage.getItem(GENERATION_KEY)).toBe("3");

    await seedGenerationForTests(4, 3);
    const bodySpy = vi.fn();
    const userResult = withSavedDeckLibrary(bodySpy);
    userResult.catch(() => {}); // avoid an unhandled-rejection window before the assertion below attaches
    await vi.advanceTimersByTimeAsync(LIBRARY_VIEW_TIMEOUT_MS);
    await expect(userResult).rejects.toMatchObject({ reason: "library-view-stale" });
    expect(bodySpy).not.toHaveBeenCalled();
    expect(localStorage.getItem(GENERATION_KEY)).toBe("4");
    vi.useRealTimers();
  });

  it("an IDB get that hangs: both policies fail after one I/O bound", async () => {
    // A hung transaction whose store's get/put never settle, on every mode.
    vi.spyOn(IDBDatabase.prototype, "transaction").mockImplementation(function (this: IDBDatabase) {
      return {
        objectStore: () => ({ get: () => ({}) as IDBRequest, put: () => ({}) as IDBRequest }),
      } as unknown as IDBTransaction;
    });
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
    const skipSpy = vi.fn();
    const skipResult = withSavedDeckLibraryOrSkip(skipSpy, "run-unguarded");
    await vi.advanceTimersByTimeAsync(GENERATION_IO_TIMEOUT_MS);
    await expect(skipResult).resolves.toEqual({ status: "skipped", reason: "generation-unreadable" });
    expect(skipSpy).not.toHaveBeenCalled();

    const bodySpy = vi.fn();
    const userResult = withSavedDeckLibrary(bodySpy);
    userResult.catch(() => {}); // avoid an unhandled-rejection window before the assertion below attaches
    await vi.advanceTimersByTimeAsync(GENERATION_IO_TIMEOUT_MS);
    await expect(userResult).rejects.toMatchObject({ reason: "generation-unreadable" });
    expect(bodySpy).not.toHaveBeenCalled();
    vi.useRealTimers();
  });

  it("an IDB set that hangs: both policies fail without running the body and still publish locally", async () => {
    // Readonly (get) resolves fast via a synthetic request (never touching real IDB open
    // machinery, which can outlast a single fake-timer advance); readwrite (set) hangs.
    vi.spyOn(IDBDatabase.prototype, "transaction").mockImplementation(function (this: IDBDatabase, _storeNames: unknown, mode?: string) {
      if (mode === "readwrite") {
        return {
          objectStore: () => ({ get: () => ({}) as IDBRequest, put: () => ({}) as IDBRequest }),
        } as unknown as IDBTransaction;
      }
      const request = {} as IDBRequest & { result?: number };
      request.result = 0;
      queueMicrotask(() => request.onsuccess?.({} as Event));
      return { objectStore: () => ({ get: () => request }) } as unknown as IDBTransaction;
    });
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
    const skipSpy = vi.fn();
    const skipResult = withSavedDeckLibraryOrSkip(skipSpy, "run-unguarded");
    await vi.advanceTimersByTimeAsync(GENERATION_IO_TIMEOUT_MS);
    await expect(skipResult).resolves.toEqual({ status: "skipped", reason: "generation-unpublished" });
    expect(skipSpy).not.toHaveBeenCalled();
    expect(Number(localStorage.getItem(GENERATION_KEY))).toBe(1);

    vi.restoreAllMocks();
    vi.spyOn(IDBDatabase.prototype, "transaction").mockImplementation(function (this: IDBDatabase, _storeNames: unknown, mode?: string) {
      if (mode === "readwrite") {
        return {
          objectStore: () => ({ get: () => ({}) as IDBRequest, put: () => ({}) as IDBRequest }),
        } as unknown as IDBTransaction;
      }
      const request = {} as IDBRequest & { result?: number };
      request.result = 1;
      queueMicrotask(() => request.onsuccess?.({} as Event));
      return { objectStore: () => ({ get: () => request }) } as unknown as IDBTransaction;
    });
    const bodySpy = vi.fn();
    const userResult = withSavedDeckLibrary(bodySpy);
    userResult.catch(() => {}); // avoid an unhandled-rejection window before the assertion below attaches
    await vi.advanceTimersByTimeAsync(GENERATION_IO_TIMEOUT_MS);
    await expect(userResult).rejects.toMatchObject({ reason: "generation-unpublished" });
    expect(bodySpy).not.toHaveBeenCalled();
    expect(Number(localStorage.getItem(GENERATION_KEY))).toBe(2);
    vi.useRealTimers();
  });

  it("write-ahead: on an IDB set rejection neither policy runs its body", async () => {
    vi.spyOn(IDBObjectStore.prototype, "put").mockImplementation(() => {
      throw new DOMException("", "QuotaExceededError");
    });
    let sentinelSkip = false;
    const skipResult = await withSavedDeckLibraryOrSkip(() => {
      sentinelSkip = true;
    }, "run-unguarded");
    expect(skipResult).toEqual({ status: "skipped", reason: "generation-unpublished" });
    expect(sentinelSkip).toBe(false);

    let sentinelUser = false;
    await expect(
      withSavedDeckLibrary(() => {
        sentinelUser = true;
      }),
    ).rejects.toMatchObject({ reason: "generation-unpublished" });
    expect(sentinelUser).toBe(false);
  });

  it("a body sees its own generation already committed to IDB, one ahead of the localStorage view", async () => {
    await withSavedDeckLibrary(() => undefined); // previous generation = 1
    let idbSeen: number | undefined;
    let localSeen: string | null = null;
    await withSavedDeckLibrary(async () => {
      idbSeen = await readIdbGenerationForTests();
      localSeen = localStorage.getItem(GENERATION_KEY);
    });
    expect(idbSeen).toBe(2);
    expect(Number(localSeen ?? 0)).toBe(1);
  });

  it("the first transaction on a fresh library commits, and a later tab that lags behind the committed IDB generation waits for its local view before committing", async () => {
    expect(await readIdbGenerationForTests()).toBeUndefined();
    await withSavedDeckLibrary(() => undefined);
    expect(await readIdbGenerationForTests()).toBe(1);

    // Simulate tab B lagging: its localStorage mirror never received the first tab's publish
    // (removed here to stand in for propagation lag), even though IDB already holds generation 1.
    localStorage.removeItem(GENERATION_KEY);
    let bodyRan = false;
    const second = withSavedDeckLibraryOrSkip(() => {
      bodyRan = true;
    }, "skip");
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(bodyRan).toBe(false);

    localStorage.setItem(GENERATION_KEY, "1");
    window.dispatchEvent(new StorageEvent("storage", { key: GENERATION_KEY }));
    await expect(second).resolves.toEqual({ status: "committed", value: undefined });
    expect(bodyRan).toBe(true);
  });

  it("after a view timeout, the next transaction commits", async () => {
    await seedGenerationForTests(3, 2);
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
    const first = withSavedDeckLibraryOrSkip(() => "first", "skip");
    await vi.advanceTimersByTimeAsync(LIBRARY_VIEW_TIMEOUT_MS);
    await expect(first).resolves.toEqual({ status: "skipped", reason: "library-view-stale" });

    vi.useRealTimers();
    const second = await withSavedDeckLibraryOrSkip(() => "second", "skip");
    expect(second).toEqual({ status: "committed", value: "second" });
    expect(await readIdbGenerationForTests()).toBe(4);
  });
});
