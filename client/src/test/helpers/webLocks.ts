import "fake-indexeddb/auto";
import { createStore, get, set } from "idb-keyval";
import { vi } from "vitest";

import { setSavedDeckTxnLockWaitForTests, type SavedDeckTxn } from "../../services/savedDeckTransaction";

/** A cast to `SavedDeckTxn` for calling a token-gated primitive directly in test code. */
export const testSavedDeckTxn = {} as SavedDeckTxn;

interface HeldLock {
  name: string;
  mode: "exclusive";
}

interface QueuedRequest {
  name: string;
  resolve: () => void;
  reject: (error: Error) => void;
  signal?: AbortSignal;
  onAbort?: () => void;
}

/**
 * An in-memory, single-realm LockManager: two simulated tabs in one test share it, exactly as
 * same-origin tabs share one lock manager.
 */
class FifoWebLocks {
  private held = new Map<string, HeldLock>();
  private queues = new Map<string, QueuedRequest[]>();

  request<T>(
    name: string,
    options: { mode?: "exclusive" | "shared"; signal?: AbortSignal; ifAvailable?: boolean; steal?: boolean },
    callback: () => Promise<T>,
  ): Promise<T> {
    if (options.mode === "shared" || options.ifAvailable || options.steal) {
      throw new TypeError("FifoWebLocks models exclusive, non-ifAvailable, non-steal requests only");
    }
    const signal = options.signal;
    if (signal?.aborted) {
      return Promise.reject(signal.reason ?? new DOMException("aborted", "AbortError"));
    }
    return new Promise<T>((resolvePromise, rejectPromise) => {
      const grant = () => {
        this.held.set(name, { name, mode: "exclusive" });
        queueMicrotask(() => {
          if (signal?.aborted) {
            this.held.delete(name);
            this.advance(name);
            rejectPromise(signal.reason ?? new DOMException("aborted", "AbortError"));
            return;
          }
          callback().then(
            (value) => {
              this.held.delete(name);
              this.advance(name);
              resolvePromise(value);
            },
            (error) => {
              this.held.delete(name);
              this.advance(name);
              rejectPromise(error);
            },
          );
        });
      };

      if (!this.held.has(name)) {
        grant();
        return;
      }

      const entry: QueuedRequest = {
        name,
        resolve: grant,
        reject: rejectPromise,
      };
      if (signal) {
        entry.onAbort = () => {
          const queue = this.queues.get(name);
          if (!queue) return;
          const index = queue.indexOf(entry);
          if (index !== -1) queue.splice(index, 1);
          entry.reject(signal.reason ?? new DOMException("aborted", "AbortError"));
        };
        signal.addEventListener("abort", entry.onAbort);
      }
      const queue = this.queues.get(name) ?? [];
      queue.push(entry);
      this.queues.set(name, queue);
    });
  }

  private advance(name: string): void {
    const queue = this.queues.get(name);
    if (!queue || queue.length === 0) return;
    const next = queue.shift()!;
    if (next.signal && next.onAbort) next.signal.removeEventListener("abort", next.onAbort);
    if (next.signal?.aborted) {
      next.reject(next.signal.reason ?? new DOMException("aborted", "AbortError"));
      this.advance(name);
      return;
    }
    next.resolve();
  }

  query(): Promise<{ held: { name: string; mode: string }[]; pending: { name: string; mode: string }[] }> {
    return Promise.resolve({
      held: [...this.held.values()].map(({ name, mode }) => ({ name, mode })),
      pending: [...this.queues.values()].flatMap((queue) => queue.map((r) => ({ name: r.name, mode: "exclusive" }))),
    });
  }
}

/** A double whose `request` rejects before granting, modeling a refused lock request. */
export function refusingWebLocks(error: DOMException): { request: () => Promise<never>; query: () => Promise<{ held: never[]; pending: never[] }> } {
  return {
    request: () => Promise.reject(error),
    query: () => Promise.resolve({ held: [], pending: [] }),
  };
}

export function installFifoWebLocks(): void {
  Object.defineProperty(globalThis.navigator, "locks", {
    configurable: true,
    value: new FifoWebLocks(),
  });
  setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
}

export function installRefusingWebLocks(error: DOMException): void {
  Object.defineProperty(globalThis.navigator, "locks", {
    configurable: true,
    value: refusingWebLocks(error),
  });
}

export function uninstallWebLocks(): void {
  Reflect.deleteProperty(globalThis.navigator, "locks");
  setSavedDeckTxnLockWaitForTests(null);
}

const GENERATION_DB = "phase-saved-deck-library";
const GENERATION_STORE = "generation";

/** Clear localStorage and the saved-deck-library IDB generation store. Every suite that installs
 *  the lock double must call this in `beforeEach`, or a stale IDB generation makes the barrier
 *  wait and the autosave skip. */
export async function resetSavedDeckLibraryForTests(): Promise<void> {
  localStorage.clear();
  const store = createStore(GENERATION_DB, GENERATION_STORE);
  await store("readwrite", (objectStore) => {
    objectStore.clear();
  });
}

export function readIdbGenerationForTests(): Promise<number | undefined> {
  return get<number>("generation", createStore(GENERATION_DB, GENERATION_STORE));
}

/** Test-only seam to make the IDB and localStorage generations diverge without running a
 *  transaction. Production code never calls this. */
export async function seedGenerationForTests(idb: number, local: number): Promise<void> {
  await set("generation", idb, createStore(GENERATION_DB, GENERATION_STORE));
  localStorage.setItem("phase-saved-deck-library-generation", String(local));
}

/** Wait until the saved-deck library lock has no held and no pending request. */
export async function awaitSavedDeckLibraryIdle(): Promise<void> {
  const locks = globalThis.navigator?.locks;
  if (!locks) return;
  await vi.waitFor(async () => {
    const { held, pending } = await locks.query();
    if ((held?.length ?? 0) > 0 || (pending?.length ?? 0) > 0) throw new Error("saved-deck library lock still busy");
  });
}
