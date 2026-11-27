import { createStore } from "idb-keyval";

import { assertNever } from "../utils/assertNever";
import {
  runCrossTabTransaction,
  setCrossTabTxnLockWaitForTests,
  VIEW_TIMEOUT_MS,
  type CrossTabResource,
  type CrossTabTxnFailure,
  type NoLockManagerPolicy,
} from "./crossTabTransaction";

const SAVED_DECK_LIBRARY_LOCK = "phase-saved-deck-library";
/** localStorage key holding the last-published saved-deck library generation. */
const SAVED_DECK_GENERATION_KEY = "phase-saved-deck-library-generation";

export { GENERATION_IO_TIMEOUT_MS, LOCK_WAIT_TIMEOUT_MS } from "./crossTabTransaction";
export const LIBRARY_VIEW_TIMEOUT_MS = VIEW_TIMEOUT_MS;

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

export type { NoLockManagerPolicy } from "./crossTabTransaction";

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
  /** The bytes `deckName` held at the moment this was thrown (`null` meaning none), when the
   *  thrower captured them under its own lock — a caller that wants to act on exactly what the
   *  refusal saw reads this instead of re-reading storage itself, which could race a later write. */
  readonly stored?: string | null;
  constructor(deckName: string, stored?: string | null) {
    super(`Saved deck changed before the write ran: ${deckName}`);
    this.name = "SavedDeckChangedError";
    this.deckName = deckName;
    this.stored = stored;
  }
}

export type SavedDeckTxnGatePhase = "draft-autosave-after-owner-selection" | "builder-save-after-data-write";
let gateForTests: ((phase: SavedDeckTxnGatePhase) => Promise<void> | void) | null = null;

/** Narrow test seam: production builds never invoke a transaction gate. */
export function setSavedDeckTxnGateForTests(gate: typeof gateForTests): void {
  gateForTests = gate;
}
/** Narrow test seam: production builds always use LOCK_WAIT_TIMEOUT_MS. It sets the one lock wait
 *  every cross-tab transaction shares. */
export function setSavedDeckTxnLockWaitForTests(ms: number | null): void {
  setCrossTabTxnLockWaitForTests(ms);
}
export function savedDeckTxnGate(phase: SavedDeckTxnGatePhase): Promise<void> | void {
  if (import.meta.env.MODE === "test") return gateForTests?.(phase);
}

let _generationStore: ReturnType<typeof createStore> | null = null;
function generationStore(): ReturnType<typeof createStore> {
  if (!_generationStore) {
    _generationStore = createStore("phase-saved-deck-library", "generation");
  }
  return _generationStore;
}

const SAVED_DECK_RESOURCE: CrossTabResource = {
  lockName: SAVED_DECK_LIBRARY_LOCK,
  localGenerationKey: SAVED_DECK_GENERATION_KEY,
  generationStore,
  generationKey: "generation",
};

/** The library's name for each of the runner's failures. */
function savedDeckFailure(reason: CrossTabTxnFailure): SavedDeckTxnFailure {
  switch (reason) {
    case "lock-refused":
    case "lock-timeout":
    case "generation-unreadable":
    case "generation-unpublished":
      return reason;
    case "view-stale":
      return "library-view-stale";
    default:
      return assertNever(reason);
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
  const result = await runCrossTabTransaction(SAVED_DECK_RESOURCE, () => body(TXN), noLockManager);
  if (result.status === "committed") return result;
  return {
    status: "skipped",
    reason: result.reason === "lock-unavailable" ? "lock-unavailable" : savedDeckFailure(result.reason),
  };
}

/**
 * Run a user-initiated write under the saved-deck library lock and the write-ahead generation
 * barrier. If either cannot be confirmed within its bound, reject with SavedDeckLibraryBusyError
 * without running `body`. With no lock manager, run `body` unguarded.
 */
export async function withSavedDeckLibrary<T>(body: (txn: SavedDeckTxn) => T | Promise<T>): Promise<T> {
  const result = await runCrossTabTransaction(SAVED_DECK_RESOURCE, () => body(TXN), "run-unguarded");
  if (result.status === "skipped") throw new SavedDeckLibraryBusyError(savedDeckFailure(result.reason));
  return result.value;
}
