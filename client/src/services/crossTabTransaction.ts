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
 * A resource whose view is one recorded value has each step record that value
 * with its generation, at its write-ahead and after its body. When this tab's
 * view has not reached the committed generation within the bound, such a step
 * writes the recorded value into its own view and proceeds on it. A resource
 * without a record skips and records the committed generation locally. The
 * timeout alone never advances a recording resource's local generation.
 *
 * With no lock manager nothing is serialized: a caller either runs its body
 * unguarded or skips it. A tab running a build that takes no lock is not
 * serialized either.
 */
import { get, getMany, set, setMany, type UseStore } from "idb-keyval";
import { assertNever } from "../utils/assertNever";

/** What one transaction resource is: its lock, its two generation counters and its stale-view rule. */
export interface CrossTabResource {
  readonly lockName: string;
  /** The `localStorage` key holding the last generation this tab published. */
  readonly localGenerationKey: string;
  /** The IndexedDB store holding the write-ahead generation. */
  readonly generationStore: () => UseStore;
  /** The write-ahead generation's key in that store. */
  readonly generationKey: string;
  /** What a step does when this tab's view has not reached the committed generation within the bound. */
  readonly staleView: StaleViewRule;
}

/** A resource's view, when it is one value a step can record and restore. */
export interface RecordedView {
  /** The record's key, in the resource's generation store. */
  readonly recordKey: string;
  /** This tab's view of the value; throws as its storage read does. */
  read(): string | null;
  /** Makes this tab's view hold `value`; throws as the write does. */
  write(value: string | null): void;
}

export type StaleViewRule =
  /** Skips the body and records the committed generation locally. */
  | { readonly kind: "adopt-committed" }
  /** Writes the last recorded value into this tab's view and runs the body on it. */
  | { readonly kind: "restore-recorded"; readonly view: RecordedView };

/** The value a recording resource's step left, with the generation it was left at. */
interface ViewRecord {
  generation: number;
  value: string | null;
}

function isViewRecord(value: unknown): value is ViewRecord {
  if (typeof value !== "object" || value === null) return false;
  const { generation, value: recorded } = value as Record<string, unknown>;
  return Number.isInteger(generation) && (recorded === null || typeof recorded === "string");
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

function runBarrier(resource: CrossTabResource): Promise<BarrierOutcome> {
  const rule = resource.staleView;
  switch (rule.kind) {
    case "adopt-committed":
      return runAdoptingBarrier(resource);
    case "restore-recorded":
      return runRecordingBarrier(resource, rule.view);
    default:
      return assertNever(rule);
  }
}

async function runAdoptingBarrier(resource: CrossTabResource): Promise<BarrierOutcome> {
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

async function runRecordingBarrier(resource: CrossTabResource, view: RecordedView): Promise<BarrierOutcome> {
  const read = await within(
    () => getMany<unknown>([resource.generationKey, view.recordKey], resource.generationStore()),
    GENERATION_IO_TIMEOUT_MS,
  );
  if (!read.settled) {
    return { proceed: false, reason: "generation-unreadable", publish: null };
  }
  const [storedGeneration, storedRecord] = read.value;
  // As for a resource without a record, an empty store is generation 0, not unreadable.
  const committed = typeof storedGeneration === "number" ? storedGeneration : 0;
  const caughtUp = await awaitView(resource, committed);
  if (!caughtUp) {
    // The timeout alone is no evidence this tab's view holds what the committed step left, so
    // the committed generation is never taken as seen here. Instead the step makes its view hold
    // the last value any step recorded, inside this lock. When that record carries the committed
    // generation, the last step completed and this rolls forward to its result; when it carries
    // an earlier one, the step that wrote the committed generation ahead never recorded its
    // result, and this rolls back to the value from before it. The restore is a plain write: it
    // removes nothing the value it replaces led to. Rolling forward, the step that left the
    // recorded value already removed what its own write displaced; rolling back, what the
    // unrecorded step's value led to is left unreachable, as when a tab closes before publishing.
    if (!isViewRecord(storedRecord)) {
      return { proceed: false, reason: "view-stale", publish: null };
    }
    try {
      view.write(storedRecord.value);
    } catch {
      return { proceed: false, reason: "view-stale", publish: null };
    }
  }
  let current: string | null;
  try {
    current = view.read();
  } catch {
    return { proceed: false, reason: "view-stale", publish: null };
  }
  const next = Math.max(localGeneration(resource), committed) + 1;
  // One transaction, so a step that writes its generation ahead and then never records its
  // result still leaves the record of the value it started from.
  const published = await within(
    () =>
      setMany(
        [
          [resource.generationKey, next],
          [view.recordKey, { generation: committed, value: current } satisfies ViewRecord],
        ],
        resource.generationStore(),
      ),
    GENERATION_IO_TIMEOUT_MS,
  );
  if (!published.settled) {
    // Not even this step's own generation is taken as seen: its write-ahead may never land, and
    // another context could then commit that same generation. A write-ahead that lands late is
    // an unrecorded step, which the next step waits for and rolls back.
    return { proceed: false, reason: "generation-unpublished", publish: null };
  }
  return { proceed: true, publish: next };
}

/**
 * Record the value this tab's view holds after a step's body, with the generation that step
 * committed. A record whose write outlasts its bound still lands before any later holder's
 * write: IndexedDB starts a read-write transaction only after every earlier-created one over the
 * same store has finished, and each record write is created inside the lock.
 */
async function recordView(resource: CrossTabResource, view: RecordedView, generation: number): Promise<void> {
  let value: string | null;
  try {
    value = view.read();
  } catch {
    return;
  }
  await within(
    () => set(view.recordKey, { generation, value } satisfies ViewRecord, resource.generationStore()),
    GENERATION_IO_TIMEOUT_MS,
  );
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
        const rule = resource.staleView;
        if (rule.kind !== "restore-recorded") {
          const value = await body();
          return { status: "committed", value } as CrossTabTxnResult<T, CrossTabTxnFailure>;
        }
        try {
          const value = await body();
          return { status: "committed", value } as CrossTabTxnResult<T, CrossTabTxnFailure>;
        } finally {
          await recordView(resource, rule.view, outcome.publish);
        }
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
 * without running it if either cannot be confirmed within its bound. A recording resource's
 * step that cannot read or restore its record, or whose write-ahead does not settle, is skipped
 * and publishes no generation. A throw
 * from `body` rejects. With no lock manager, `noLockManager` decides whether `body` runs unguarded, at
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
