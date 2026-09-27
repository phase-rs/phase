import { createStore, get, set } from "idb-keyval";

const SAVED_DECK_LIBRARY_LOCK = "phase-saved-deck-library";
/** localStorage key holding the last-published saved-deck library generation. */
const SAVED_DECK_GENERATION_KEY = "phase-saved-deck-library-generation";

export const LOCK_WAIT_TIMEOUT_MS = 6000;
export const GENERATION_IO_TIMEOUT_MS = 1000;
export const LIBRARY_VIEW_TIMEOUT_MS = 2000;

declare const savedDeckTxnBrand: unique symbol;
/** Proof that the holder runs inside a saved-deck library transaction. */
export type SavedDeckTxn = { readonly [savedDeckTxnBrand]: true };
const TXN = {} as SavedDeckTxn; // module-private; the brand exists only at compile time

export type SavedDeckTxnFailure =
  | "lock-refused"
  | "lock-timeout"
  | "generation-unreadable"
  | "library-view-stale"
  | "generation-unpublished";

export type SavedDeckTxnSkipReason = "lock-unavailable" | SavedDeckTxnFailure;

export type SavedDeckTxnResult<T, R extends SavedDeckTxnSkipReason = SavedDeckTxnSkipReason> =
  | { status: "committed"; value: T }
  | { status: "skipped"; reason: R };

export type NoLockManagerPolicy = "run-unguarded" | "skip";

/** A user-initiated saved-deck library write was refused before its body ran. */
export class SavedDeckLibraryBusyError extends Error {
  readonly reason: SavedDeckTxnFailure;
  constructor(reason: SavedDeckTxnFailure) {
    super(`Saved-deck library unavailable: ${reason}`);
    this.name = "SavedDeckLibraryBusyError";
    this.reason = reason;
  }
}

/** A user-initiated write was refused because the deck it targeted changed before it ran. */
export class SavedDeckChangedError extends Error {
  readonly deckName: string;
  constructor(deckName: string) {
    super(`Saved deck changed before the write ran: ${deckName}`);
    this.name = "SavedDeckChangedError";
    this.deckName = deckName;
  }
}

let _generationStore: ReturnType<typeof createStore> | null = null;
function generationStore(): ReturnType<typeof createStore> {
  if (!_generationStore) {
    _generationStore = createStore("phase-saved-deck-library", "generation");
  }
  return _generationStore;
}

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

export type SavedDeckTxnGatePhase = "draft-autosave-after-owner-selection" | "builder-save-after-data-write";
let gateForTests: ((phase: SavedDeckTxnGatePhase) => Promise<void> | void) | null = null;
let lockWaitForTests: number | null = null;

/** Narrow test seam: production builds never invoke a transaction gate. */
export function setSavedDeckTxnGateForTests(gate: typeof gateForTests): void {
  gateForTests = gate;
}
/** Narrow test seam: production builds always use LOCK_WAIT_TIMEOUT_MS. */
export function setSavedDeckTxnLockWaitForTests(ms: number | null): void {
  lockWaitForTests = ms;
}
export function savedDeckTxnGate(phase: SavedDeckTxnGatePhase): Promise<void> | void {
  if (import.meta.env.MODE === "test") return gateForTests?.(phase);
}

function lockWaitMs(): number {
  return import.meta.env.MODE === "test" && lockWaitForTests !== null ? lockWaitForTests : LOCK_WAIT_TIMEOUT_MS;
}

/**
 * Wait until this tab's copy of the library reflects every committed transaction, or give up
 * after LIBRARY_VIEW_TIMEOUT_MS.
 */
function awaitLibraryView(committed: number): Promise<boolean> {
  if (Number(localStorage.getItem(SAVED_DECK_GENERATION_KEY) ?? 0) >= committed) return Promise.resolve(true);
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
      if (Number(localStorage.getItem(SAVED_DECK_GENERATION_KEY) ?? 0) >= committed) finish(true);
    };
    const onStorage = (event: StorageEvent) => {
      if (event.key === SAVED_DECK_GENERATION_KEY || event.key === null) check();
    };
    window.addEventListener("storage", onStorage);
    const backstop = setInterval(check, 25);
    const timer = setTimeout(() => finish(false), LIBRARY_VIEW_TIMEOUT_MS);
  });
}

type BarrierOutcome =
  | { proceed: true; publish: number }
  | { proceed: false; reason: SavedDeckTxnFailure; publish: number | null };

async function runLibraryBarrier(): Promise<BarrierOutcome> {
  const read = await within(() => get<number>("generation", generationStore()), GENERATION_IO_TIMEOUT_MS);
  if (!read.settled) {
    return { proceed: false, reason: "generation-unreadable", publish: null };
  }
  // Treating an empty store as unconfirmed would make every autosave skip on a fresh library, so
  // only a read that fails to settle (I/O failure or timeout) is genuinely unreadable.
  const committed = typeof read.value === "number" ? read.value : 0;
  const caughtUp = await awaitLibraryView(committed);
  if (!caughtUp) {
    // Without this, a generation whose local publish never arrives would fail every later
    // transaction here too.
    return { proceed: false, reason: "library-view-stale", publish: committed };
  }
  const next = Math.max(Number(localStorage.getItem(SAVED_DECK_GENERATION_KEY) ?? 0), committed) + 1;
  const published = await within(() => set("generation", next, generationStore()), GENERATION_IO_TIMEOUT_MS);
  if (!published.settled) {
    return { proceed: false, reason: "generation-unpublished", publish: next };
  }
  return { proceed: true, publish: next };
}

function publishLocalGeneration(next: number): void {
  try {
    localStorage.setItem(SAVED_DECK_GENERATION_KEY, String(next));
  } catch (error) {
    console.warn("[savedDeckTransaction] failed to publish local generation:", error);
  }
}

async function runLocked<T>(
  locks: LockManager,
  body: (txn: SavedDeckTxn) => T | Promise<T>,
): Promise<SavedDeckTxnResult<T, SavedDeckTxnFailure>> {
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
    return await locks.request(SAVED_DECK_LIBRARY_LOCK, { mode: "exclusive", signal: controller.signal }, async () => {
      granted = true;
      if (timer) clearTimeout(timer);
      const outcome = await runLibraryBarrier();
      try {
        if (!outcome.proceed) {
          return { status: "skipped", reason: outcome.reason } as SavedDeckTxnResult<T, SavedDeckTxnFailure>;
        }
        const value = await body(TXN);
        return { status: "committed", value } as SavedDeckTxnResult<T, SavedDeckTxnFailure>;
      } finally {
        if (outcome.publish !== null) publishLocalGeneration(outcome.publish);
      }
    });
  } catch (error) {
    if (timer) clearTimeout(timer);
    if (granted) throw error;
    return { status: "skipped", reason: timedOut ? "lock-timeout" : "lock-refused" };
  }
}

/**
 * Run a background write under the saved-deck library lock and the write-ahead generation
 * barrier, or skip it without running `body` if either cannot be confirmed within its bound. With
 * no lock manager, `noLockManager` decides whether `body` runs unguarded or is skipped:
 * "run-unguarded" never yields "lock-unavailable" (that reason only arises from "skip"), so its
 * overload narrows the skip reason to `SavedDeckTxnFailure`.
 */
export async function withSavedDeckLibraryOrSkip<T>(
  body: (txn: SavedDeckTxn) => T | Promise<T>,
  noLockManager: "run-unguarded",
): Promise<SavedDeckTxnResult<T, SavedDeckTxnFailure>>;
export async function withSavedDeckLibraryOrSkip<T>(
  body: (txn: SavedDeckTxn) => T | Promise<T>,
  noLockManager: "skip",
): Promise<SavedDeckTxnResult<T>>;
export async function withSavedDeckLibraryOrSkip<T>(
  body: (txn: SavedDeckTxn) => T | Promise<T>,
  noLockManager: NoLockManagerPolicy,
): Promise<SavedDeckTxnResult<T>> {
  const locks = globalThis.navigator?.locks ?? null;
  if (!locks) {
    if (noLockManager === "skip") return { status: "skipped", reason: "lock-unavailable" };
    return { status: "committed", value: await body(TXN) };
  }
  return runLocked(locks, body);
}

/**
 * Run a user-initiated write under the saved-deck library lock and the write-ahead generation
 * barrier. If either cannot be confirmed within its bound, reject with SavedDeckLibraryBusyError
 * without running `body`. With no lock manager, run `body` unguarded.
 */
export async function withSavedDeckLibrary<T>(body: (txn: SavedDeckTxn) => T | Promise<T>): Promise<T> {
  const locks = globalThis.navigator?.locks ?? null;
  if (!locks) return body(TXN);

  const result = await runLocked(locks, body);
  if (result.status === "skipped") throw new SavedDeckLibraryBusyError(result.reason);
  return result.value;
}
