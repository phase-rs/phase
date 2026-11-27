/**
 * One cross-tab transaction protocol for a resource every same-origin tab
 * shares: an exclusive Web Lock, then a write-ahead generation barrier, then
 * the body, then this tab's local publication of the generation.
 *
 * The lock orders the bodies of every tab that takes it. It does not make
 * another tab's writes visible here: a write another tab made under a lock it
 * has released reaches this tab's storage in its own time. The barrier closes
 * that: a body runs only once this tab's local generation, which each tab
 * publishes after its body's writes, has reached the one committed in
 * IndexedDB, so the body sees every earlier body's writes.
 *
 * With no lock manager nothing is serialized: a caller either runs its body
 * unguarded or skips it. A tab running a build that takes no lock is not
 * serialized either.
 */
import { get, set, type UseStore } from "idb-keyval";

/** What one transaction resource is: its lock and its two generation counters. */
export interface CrossTabResource {
  readonly lockName: string;
  /** The `localStorage` key holding the last generation this tab published. */
  readonly localGenerationKey: string;
  /** The IndexedDB store holding the write-ahead generation. */
  readonly generationStore: () => UseStore;
  /** The write-ahead generation's key in that store. */
  readonly generationKey: string;
}

export type CrossTabTxnFailure =
  | "lock-refused"
  | "lock-timeout"
  | "generation-unreadable"
  | "view-stale"
  | "generation-unpublished";

export type CrossTabTxnSkipReason = "lock-unavailable" | CrossTabTxnFailure;

export type CrossTabTxnResult<T, R extends CrossTabTxnSkipReason = CrossTabTxnSkipReason> =
  | { status: "committed"; value: T }
  | { status: "skipped"; reason: R };

export type NoLockManagerPolicy = "run-unguarded" | "skip";

export const LOCK_WAIT_TIMEOUT_MS = 6000;
export const GENERATION_IO_TIMEOUT_MS = 1000;
export const VIEW_TIMEOUT_MS = 2000;

type Bounded<T> = { settled: true; value: T } | { settled: false };

/** Race `start()` against `ms`; a synchronous throw from `start` becomes a rejection. */
function within<T>(start: () => Promise<T>, ms: number): Promise<Bounded<T>> {
  return new Promise<Bounded<T>>((resolve) => {
    let done = false;
    const timer = setTimeout(() => {
      if (done) return;
      done = true;
      resolve({ settled: false });
    }, ms);
    Promise.resolve()
      .then(start)
      .then(
        (value) => {
          if (done) return;
          done = true;
          clearTimeout(timer);
          resolve({ settled: true, value });
        },
        () => {
          if (done) return;
          done = true;
          clearTimeout(timer);
          resolve({ settled: false });
        },
      );
  });
}

let lockWaitForTests: number | null = null;

/** Narrow test seam: production builds always use LOCK_WAIT_TIMEOUT_MS. */
export function setCrossTabTxnLockWaitForTests(ms: number | null): void {
  lockWaitForTests = ms;
}

function lockWaitMs(): number {
  return import.meta.env.MODE === "test" && lockWaitForTests !== null ? lockWaitForTests : LOCK_WAIT_TIMEOUT_MS;
}

function localGeneration(resource: CrossTabResource): number {
  return Number(localStorage.getItem(resource.localGenerationKey) ?? 0);
}

/**
 * Wait until this tab's view of the resource reflects every committed
 * transaction, or give up after VIEW_TIMEOUT_MS.
 */
function awaitView(resource: CrossTabResource, committed: number): Promise<boolean> {
  if (localGeneration(resource) >= committed) return Promise.resolve(true);
  return new Promise<boolean>((resolve) => {
    let done = false;
    const finish = (result: boolean) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      clearInterval(backstop);
      window.removeEventListener("storage", onStorage);
      resolve(result);
    };
    const check = () => {
      if (localGeneration(resource) >= committed) finish(true);
    };
    const onStorage = (event: StorageEvent) => {
      if (event.key === resource.localGenerationKey || event.key === null) check();
    };
    window.addEventListener("storage", onStorage);
    const backstop = setInterval(check, 25);
    const timer = setTimeout(() => finish(false), VIEW_TIMEOUT_MS);
  });
}

type BarrierOutcome =
  | { proceed: true; publish: number }
  | { proceed: false; reason: CrossTabTxnFailure; publish: number | null };

async function runBarrier(resource: CrossTabResource): Promise<BarrierOutcome> {
  const read = await within(
    () => get<number>(resource.generationKey, resource.generationStore()),
    GENERATION_IO_TIMEOUT_MS,
  );
  if (!read.settled) {
    return { proceed: false, reason: "generation-unreadable", publish: null };
  }
  // Treating an empty store as unconfirmed would make every transaction skip on a fresh
  // resource, so only a read that fails to settle (I/O failure or timeout) is genuinely unreadable.
  const committed = typeof read.value === "number" ? read.value : 0;
  const caughtUp = await awaitView(resource, committed);
  if (!caughtUp) {
    // Without this, a generation whose local publish never arrives would fail every later
    // transaction here too.
    return { proceed: false, reason: "view-stale", publish: committed };
  }
  const next = Math.max(localGeneration(resource), committed) + 1;
  const published = await within(
    () => set(resource.generationKey, next, resource.generationStore()),
    GENERATION_IO_TIMEOUT_MS,
  );
  if (!published.settled) {
    return { proceed: false, reason: "generation-unpublished", publish: next };
  }
  return { proceed: true, publish: next };
}

function publishLocalGeneration(resource: CrossTabResource, next: number): void {
  try {
    localStorage.setItem(resource.localGenerationKey, String(next));
  } catch (error) {
    console.warn(`[crossTabTransaction] failed to publish local generation of ${resource.lockName}:`, error);
  }
}

async function runLocked<T>(
  resource: CrossTabResource,
  locks: LockManager,
  body: () => T | Promise<T>,
): Promise<CrossTabTxnResult<T, CrossTabTxnFailure>> {
  const controller = new AbortController();
  let granted = false;
  let timedOut = false;
  const waitMs = lockWaitMs();
  const timer = Number.isFinite(waitMs)
    ? setTimeout(() => {
        timedOut = true;
        controller.abort();
      }, waitMs)
    : null;

  try {
    return await locks.request(resource.lockName, { mode: "exclusive", signal: controller.signal }, async () => {
      granted = true;
      if (timer) clearTimeout(timer);
      const outcome = await runBarrier(resource);
      try {
        if (!outcome.proceed) {
          return { status: "skipped", reason: outcome.reason } as CrossTabTxnResult<T, CrossTabTxnFailure>;
        }
        const value = await body();
        return { status: "committed", value } as CrossTabTxnResult<T, CrossTabTxnFailure>;
      } finally {
        if (outcome.publish !== null) publishLocalGeneration(resource, outcome.publish);
      }
    });
  } catch (error) {
    if (timer) clearTimeout(timer);
    if (granted) throw error;
    return { status: "skipped", reason: timedOut ? "lock-timeout" : "lock-refused" };
  }
}

/**
 * Run `body` under `resource`'s lock and its write-ahead generation barrier, or skip it
 * without running it if either cannot be confirmed within its bound. A throw from `body`
 * rejects. With no lock manager, `noLockManager` decides whether `body` runs unguarded, at
 * the call, or is skipped: "run-unguarded" never yields "lock-unavailable" (that reason only
 * arises from "skip"), so its overload narrows the skip reason to `CrossTabTxnFailure`.
 */
export async function runCrossTabTransaction<T>(
  resource: CrossTabResource,
  body: () => T | Promise<T>,
  noLockManager: "run-unguarded",
): Promise<CrossTabTxnResult<T, CrossTabTxnFailure>>;
export async function runCrossTabTransaction<T>(
  resource: CrossTabResource,
  body: () => T | Promise<T>,
  noLockManager: "skip",
): Promise<CrossTabTxnResult<T>>;
export async function runCrossTabTransaction<T>(
  resource: CrossTabResource,
  body: () => T | Promise<T>,
  noLockManager: NoLockManagerPolicy,
): Promise<CrossTabTxnResult<T>>;
export async function runCrossTabTransaction<T>(
  resource: CrossTabResource,
  body: () => T | Promise<T>,
  noLockManager: NoLockManagerPolicy,
): Promise<CrossTabTxnResult<T>> {
  const locks = globalThis.navigator?.locks ?? null;
  if (!locks) {
    if (noLockManager === "skip") return { status: "skipped", reason: "lock-unavailable" };
    return { status: "committed", value: await body() };
  }
  return runLocked(resource, locks, body);
}
