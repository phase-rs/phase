import { openDB, type DBSchema, type IDBPDatabase } from "idb";

import {
  VisualPackBackendError,
  VisualPackStorageRefusalError,
  type DeckLibraryBackgroundLifecycle,
  type DeckLibraryPreparationResult,
  type VisualPackBackend,
} from "../backend.ts";
import { isCardDataResident } from "../../scryfall.ts";
import { curatedDescriptors, planCuratedPack } from "../curatedPack.ts";
import { invalidateDeckLibraryPack, planDeckLibraryPack } from "../deckLibraryPack.ts";
import {
  estimatedImageBytes,
  installedRevision,
  minimumImageBytes,
  operationId,
  packId,
  type AssetKey,
  type CandidateKey,
  type CatalogRoot,
  type CatalogStatus,
  type CatalogSummary,
  type CatalogScanProgress,
  type CuratedDrift,
  type CuratedInstallSelector,
  type DeckLibraryDrift,
  type DeckLibraryInstallSelector,
  type InstallEstimate,
  type InstallSelector,
  type OperationId,
  type OperationStatus,
  type PackId,
  type ProgressEvent,
  type RemovalMode,
  type RemovalResponse,
  type RemovalSelector,
  type ResolutionKey,
  type ResolutionResponse,
  type RevisionEvent,
  type StartRequest,
  type StartResponse,
  type StorageHeadroom,
  type StorageOutlook,
  type StoragePersistence,
  type VerificationMode,
  type VerificationResponse,
  type VisualPackErrorKind,
  type VisualPackMedia,
} from "../types.ts";
import { CARD_CANDIDATE_PROJECTION_VERSION } from "../candidateKeys.ts";
import { syntheticCachePath } from "./records.ts";
import {
  countScryfallAssets,
  forEachScryfallAsset,
  loadScryfallBulkSource,
  ScryfallBulkError,
  type ScryfallAssetDescriptor,
  type ScryfallBulkSource,
} from "./scryfallBulk.ts";

const DATABASE = "phase-visual-packs-scryfall-v1";
const DATABASE_VERSION = 2;
const CACHE = "phase-visual-pack-scryfall-images-v1";
const STATE = "state";
const DOWNLOAD_CONCURRENCY = 4;
const PROGRESS_INTERVAL_MS = 250;
let pendingDatabaseOpen: Promise<IDBPDatabase<ScryfallVisualPackSchema>> | null = null;

type CatalogRecord = Readonly<ScryfallBulkSource>;

type StateRecord = Readonly<{
  id: typeof STATE;
  revision: string;
  catalog: CatalogRecord | null;
}>;

type PackRecord = Readonly<{
  id: string;
  packId: PackId;
  root: CatalogRoot;
  dependencies: readonly PackId[];
  operationId: OperationId;
  /** Stable opt-in identity, retained across normal deck-library promotions. */
  optInGeneration?: OperationId;
  /** Descriptor candidate projection committed atomically with the receipt revision. */
  candidateProjectionVersion?: number;
}>;

type ObjectRecord = Readonly<{
  id: string;
  root: CatalogRoot;
  packId: PackId;
  assetKey: AssetKey;
  candidateKeys: readonly CandidateKey[];
  /**
   * The URL these bytes were downloaded from — half of the identity by which a
   * NEW root may reuse them instead of downloading them again.
   *
   * Optional because it is additive. A row written before this field existed
   * carries `undefined`, which no descriptor's `sourceUrl` can equal, so such a
   * row is never reused. That is the safe default in the direction that
   * matters: re-downloading only costs time, whereas reusing bytes whose
   * provenance is unrecorded would serve the wrong art. It is also why the
   * store needs no version bump and no migration.
   */
  sourceUrl?: string;
  object: CatalogRoot;
  byteLength: number;
  media: VisualPackMedia;
  path: string;
}>;

/** The content half of an `ObjectRecord`: the bytes a row points at, with no
 *  claim about which pack or root points at them. */
type ObjectContent = Readonly<{
  object: CatalogRoot;
  byteLength: number;
  path: string;
}>;

type MembershipDiff = Readonly<{
  installedDigest: CatalogRoot | null;
  add: number;
  remove: number;
  refresh: number;
}>;

type ReconciliationTaskOwnership = {
  operationId: OperationId | null;
  background: boolean;
};

type TransactionGatePhase =
  | "complete-pack-before-write"
  | "complete-pack-before-commit"
  | "finish-before-write"
  | "finish-before-commit"
  | "selector-after-operations-read"
  | "selector-before-commit";

let transactionGateForTests: ((phase: TransactionGatePhase) => void) | null = null;

/** Narrow test seam: production builds never invoke a transaction gate. */
export function setScryfallTransactionGateForTests(gate: ((phase: TransactionGatePhase) => void) | null): void {
  transactionGateForTests = gate;
}

function transactionGate(phase: TransactionGatePhase): void {
  if (import.meta.env.MODE === "test") transactionGateForTests?.(phase);
}

function abortTransaction(transaction: { abort(): void; done: Promise<unknown> }): void {
  transaction.abort();
  void transaction.done.catch(() => undefined);
}

function bindTransactionAbort(
  transaction: { abort(): void; done: Promise<unknown> },
  signal: AbortSignal | undefined,
): void {
  if (!signal) return;
  const abort = () => abortTransaction(transaction);
  if (signal.aborted) abort();
  else signal.addEventListener("abort", abort, { once: true });
  void transaction.done.catch(() => undefined).finally(() => signal.removeEventListener("abort", abort));
}

type OperationObjectRecord = Readonly<{
  id: string;
  operationId: OperationId;
  objectId: string;
  complete: boolean;
}>;

type ScryfallOperationRecord = Readonly<{
  id: OperationId;
  kind: "install" | "repair";
  state: "downloading" | "cancel_requested" | "finalizing" | "completed" | "cancelled";
  catalog: CatalogRecord;
  selectors: readonly InstallSelector[];
  packIds: readonly PackId[];
  packTotal: number;
  packsPromoted: number;
  objectTotal: number;
  objectEstimate?: number;
  objectsPromoted: number;
  completedRevision: string | null;
  /** Receipt-root descriptors captured for deck-library repair. */
  repairDescriptors?: readonly ScryfallAssetDescriptor[];
  /** Expected durable deck-library opt-in identity for guarded background work. */
  deckLibraryGeneration?: OperationId;
  /** This record was created by installed-only reconciliation, never by the UI. */
  background?: boolean;
  /** Current Deck Catalog descriptor projection this operation may commit. */
  candidateProjectionVersion?: number;
}>;

const DECK_LIBRARY_LOCK = "phase-visual-packs:deck-library";

interface ScryfallVisualPackSchema extends DBSchema {
  state: { key: string; value: StateRecord };
  packs: { key: string; value: PackRecord; indexes: { "by-root": CatalogRoot } };
  objects: {
    key: string;
    value: ObjectRecord;
    indexes: { "by-pack": string; "by-candidate-key": CandidateKey };
  };
  operations: { key: OperationId; value: ScryfallOperationRecord };
  operationObjects: { key: string; value: OperationObjectRecord; indexes: { "by-operation": OperationId } };
}

function objectId(root: CatalogRoot, selectedPack: PackId, selectedAsset: AssetKey): string {
  return `${root}:${selectedPack}:${selectedAsset}`;
}

function operationObjectId(selectedOperation: OperationId, selectedObject: string): string {
  return `${selectedOperation}:${selectedObject}`;
}

/**
 * The identity under which an already-downloaded image may be reused by a
 * DIFFERENT catalog root. `objectId` cannot serve: it is keyed on the root, so
 * a new root matches nothing and every image is fetched again.
 *
 * Both halves are load-bearing. An asset key alone names a slot, not bytes: a
 * `canonical_card:` key carries no printing identity, so what stands behind it
 * is whatever `scryfall-data.json` currently supplies, and a regeneration of
 * that file moves the URL while the key stays put. Reusing on the key alone
 * would pin the superseded art with no way to notice — which is the same
 * reason the membership digest covers source URLs as well as keys.
 */
function contentId(selectedAsset: AssetKey, sourceUrl: string): string {
  return `${selectedAsset}\t${sourceUrl}`;
}

/** The reuse snapshot for a selector that does not participate in content
 *  reuse. Shared and empty, so a non-curated install never scans `objects` and
 *  `installObject` keeps ONE code path rather than growing a second one that
 *  could drift from it. */
const NO_DONORS = (): Promise<Map<string, ObjectContent>> => Promise.resolve(new Map());

function operationStatus(record: ScryfallOperationRecord): OperationStatus {
  return {
    operationId: record.id,
    catalogRoot: record.catalog.root,
    kind: record.kind ?? "install",
    state: record.state,
    packTotal: record.packTotal,
    packsPromoted: record.packsPromoted,
    objectTotal: record.objectTotal,
    objectEstimate: record.objectEstimate ?? null,
    objectsPromoted: record.objectsPromoted,
    completedRevision: record.completedRevision === null ? null : installedRevision(record.completedRevision),
  };
}

function initialState(): StateRecord {
  return { id: STATE, revision: "0", catalog: null };
}

function operationToken(): OperationId {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return operationId(Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join(""));
}

function errorKind(error: unknown): VisualPackErrorKind {
  if (error instanceof VisualPackBackendError) return error.kind;
  if (error instanceof ScryfallBulkError) {
    if (error.kind === "unsupported") return "unavailable";
    return error.kind;
  }
  if (error instanceof DOMException && error.name === "AbortError") return "cancelled";
  if (error instanceof DOMException && error.name === "QuotaExceededError") return "storage";
  return "storage";
}

/**
 * Whether re-running an operation record could ever reach a different outcome.
 *
 * `conflict` is the one kind that says the request no longer describes
 * reality. A curated selector stores only a membership digest, and a digest is
 * not invertible, so once the stored preferences move there is no way to
 * reconstruct the membership the record named — replanning would install a
 * DIFFERENT membership under the old root, and adopting the new digest would
 * orphan every row already written under the old one. Network and storage
 * failures carry no such claim: they are transient, and an operation that hits
 * one MUST stay resumable, which is what the panel's Resume control is for.
 *
 * `internal` is the second, for a different reason and with the same
 * consequence: it is what a defect in our own deterministic planning code is
 * classified as, and re-running deterministic code reaches the same defect.
 * Leaving such an operation resumable wedges the panel across restarts, since
 * `create()` re-runs every record still `downloading` on every launch.
 *
 * Everything else is retryable by default, which `insufficient_storage`
 * inherits CORRECTLY: freeing disk changes the outcome, so a record left
 * resumable can still succeed. It is also unreachable from here today —
 * `reserveStorage` throws out of `start()` before any record is persisted, and
 * this function is consulted only from `run()`'s catch — so the default is a
 * statement about a future caller rather than about the refusal path.
 */
function retryable(kind: VisualPackErrorKind): boolean {
  return kind !== "conflict" && kind !== "internal" && kind !== "cancelled";
}

function backendError(error: unknown): VisualPackBackendError {
  if (error instanceof VisualPackBackendError) return error;
  return new VisualPackBackendError(errorKind(error), error instanceof Error ? error.message : undefined);
}

function selectorPack(selector: InstallSelector): PackId {
  switch (selector.kind) {
    case "core": return packId("core");
    case "printing": return packId(`printing:${selector.set}`);
    case "locale": return packId(`locale:${selector.language}:${selector.set}`);
    case "complete": return packId("complete");
    case "curated": return packId("curated");
    case "deck_library": return packId("deck_library");
  }
}

/**
 * The catalog root a selector's packs and objects are stored at.
 *
 * Curated membership is identified by its own digest; every other selector is
 * identified by the bulk catalog it was built from. The root is a property of
 * a SELECTOR, not of an operation — an operation installs and repairs a list
 * of packs — so it is derived here rather than stored on the record.
 */
function selectorRoot(selector: InstallSelector, catalog: CatalogRecord): CatalogRoot {
  return selector.kind === "curated" || selector.kind === "deck_library" ? selector.membershipDigest : catalog.root;
}

function selectorForPack(selectedPack: PackId, root: CatalogRoot): InstallSelector {
  if (selectedPack === packId("core")) return { kind: "core" };
  if (selectedPack === packId("complete")) return { kind: "complete", rootSha256: root };
  if (selectedPack === packId("curated")) return { kind: "curated", membershipDigest: root };
  if (selectedPack === packId("deck_library")) return { kind: "deck_library", membershipDigest: root };
  const printing = /^printing:([a-z0-9]{3,6})$/.exec(selectedPack);
  if (printing) return { kind: "printing", set: printing[1] };
  const locale = /^locale:(de|es|fr|it|pt):([a-z0-9]{3,6})$/.exec(selectedPack);
  if (locale) return { kind: "locale", language: locale[1], set: locale[2] };
  throw new VisualPackBackendError("invalid_input");
}

function localPack(selectedPack: PackId): boolean {
  return selectedPack === packId("curated") || selectedPack === packId("deck_library");
}

function deckLibraryGeneration(receipt: PackRecord): OperationId {
  return receipt.optInGeneration ?? receipt.operationId;
}

function deckLibraryOperation(operation: ScryfallOperationRecord): boolean {
  return operation.packIds.includes(packId("deck_library"));
}

function hasCurrentCandidateProjectionIntent(operation: ScryfallOperationRecord): boolean {
  return operation.kind !== "repair"
    && deckLibraryOperation(operation)
    && operation.candidateProjectionVersion === CARD_CANDIDATE_PROJECTION_VERSION;
}

/** A non-repair Deck Catalog operation made before the candidate projection
 *  upgrade must never resume against a new descriptor vocabulary. Repairs use
 *  their captured receipt descriptors and deliberately remain resumable. */
function hasStaleCandidateProjectionIntent(operation: ScryfallOperationRecord): boolean {
  return operation.kind !== "repair"
    && deckLibraryOperation(operation)
    && !hasCurrentCandidateProjectionIntent(operation);
}

function webLocks(): LockManager | undefined {
  return globalThis.navigator?.locks;
}

function descriptorFromObject(row: ObjectRecord): ScryfallAssetDescriptor {
  if (!row.sourceUrl) throw new VisualPackBackendError("invalid_input");
  return {
    packId: row.packId,
    assetKey: row.assetKey,
    candidateKeys: row.candidateKeys,
    sourceUrl: row.sourceUrl,
    media: row.media,
  };
}

function sameResolutionKey(record: ObjectRecord, key: ResolutionKey): boolean {
  return key.kind === "asset" ? record.assetKey === key.key : record.candidateKeys.includes(key.key);
}

/**
 * An upgrade blocked by an older tab cannot make render-time lookup wait:
 * callers fall back to remote art immediately. The native open request cannot
 * be cancelled, so a late success is closed rather than becoming another
 * connection that blocks a future upgrade. Until that request settles, every
 * caller shares its outcome instead of queuing another native open behind it.
 */
function openDatabase(): Promise<IDBPDatabase<ScryfallVisualPackSchema>> {
  if (pendingDatabaseOpen) return pendingDatabaseOpen;
  let resolveResult!: (database: IDBPDatabase<ScryfallVisualPackSchema>) => void;
  let rejectResult!: (reason?: unknown) => void;
  let settled = false;
  const result = new Promise<IDBPDatabase<ScryfallVisualPackSchema>>((resolve, reject) => {
    resolveResult = resolve;
    rejectResult = reject;
  });
  pendingDatabaseOpen = result;
  const clearPendingOpen = () => {
    if (pendingDatabaseOpen === result) pendingDatabaseOpen = null;
  };
  const rejectBlockedUpgrade = () => {
    if (settled) return;
    settled = true;
    rejectResult(new VisualPackBackendError("unavailable"));
  };
  let opening: Promise<IDBPDatabase<ScryfallVisualPackSchema>>;
  try {
    opening = openDB<ScryfallVisualPackSchema>(DATABASE, DATABASE_VERSION, {
      upgrade(database, oldVersion, _newVersion, transaction) {
        if (oldVersion < 1) {
          database.createObjectStore("state", { keyPath: "id" });
          database.createObjectStore("packs", { keyPath: "id" }).createIndex("by-root", "root");
          database.createObjectStore("objects", { keyPath: "id" }).createIndex("by-pack", "packId");
          database.createObjectStore("operations", { keyPath: "id" });
          database.createObjectStore("operationObjects", { keyPath: "id" }).createIndex("by-operation", "operationId");
        }
        if (oldVersion < 2) {
          transaction.objectStore("objects").createIndex("by-candidate-key", "candidateKeys", { multiEntry: true });
        }
      },
      blocked: rejectBlockedUpgrade,
      blocking(_currentVersion, _blockedVersion, event) {
        (event.target as IDBDatabase | null)?.close();
      },
    });
  } catch (error) {
    settled = true;
    rejectResult(error);
    clearPendingOpen();
    return result;
  }
  void opening.then(
    (database) => {
      if (settled) {
        database.close();
        clearPendingOpen();
        return;
      }
      settled = true;
      resolveResult(database);
      clearPendingOpen();
    },
    (error: unknown) => {
      if (!settled) {
        settled = true;
        rejectResult(error);
      }
      clearPendingOpen();
    },
  );
  return result;
}

async function state(database: IDBPDatabase<ScryfallVisualPackSchema>): Promise<StateRecord> {
  return (await database.get("state", STATE)) ?? initialState();
}

/**
 * `navigator.storage`, or `undefined` where there is none.
 *
 * Read through `globalThis` and through optional chaining because the Storage
 * API is absent in older Safari, absent in some private windows, and absent in
 * the test environment. Every caller below turns both "absent" and "threw"
 * into the same answer, so that a pack download which would otherwise succeed
 * is never blocked by a failure to introspect the storage it writes to.
 *
 * The explicit `!manager` checks in the two callers are therefore REDUNDANT
 * with their `catch`, and MEASURED to be: deleting them turns no test red,
 * because the resulting TypeError lands in the same handler. They are kept
 * anyway, and only for the reason that an absent API is an ordinary condition
 * rather than a failure, so it should not be routed through an exception. Do
 * not read them as the thing that makes absence safe — the `catch` is.
 */
function storageManager(): StorageManager | undefined {
  return globalThis.navigator?.storage;
}

/** Whether this origin's storage is already exempt from eviction. */
async function currentPersistence(): Promise<StoragePersistence> {
  try {
    const manager = storageManager();
    if (!manager) return "unsupported";
    return (await manager.persisted()) ? "persisted" : "best_effort";
  } catch {
    return "unsupported";
  }
}

/**
 * Ask the browser to stop evicting this origin's storage, and report what it
 * said.
 *
 * A refusal is a normal outcome, not an error: without the grant Cache Storage
 * stays best-effort and the browser MAY discard the whole pack under disk
 * pressure, but the download itself works either way. Reporting `best_effort`
 * is how that stays visible instead of being assumed away.
 *
 * `persisted()` is consulted first so an origin that already holds the grant
 * does not ask for it a second time.
 */
async function requestPersistence(): Promise<StoragePersistence> {
  try {
    const manager = storageManager();
    if (!manager) return "unsupported";
    if (await manager.persisted()) return "persisted";
    return (await manager.persist()) ? "persisted" : "best_effort";
  } catch {
    return "unsupported";
  }
}

/** The origin's usage and quota, with `persistence` supplied by whichever of
 *  the two calls above the caller made. */
async function storageOutlook(persistence: StoragePersistence): Promise<StorageOutlook> {
  let usageBytes: number | null = null;
  let quotaBytes: number | null = null;
  try {
    const report = await storageManager()?.estimate();
    if (typeof report?.usage === "number") usageBytes = report.usage;
    if (typeof report?.quota === "number") quotaBytes = report.quota;
  } catch {
    // Reported as "would not say" below, exactly like an absent API.
  }
  const availableBytes = usageBytes === null || quotaBytes === null ? null : Math.max(quotaBytes - usageBytes, 0);
  return { usageBytes, quotaBytes, availableBytes, persistence };
}

/**
 * Whether a projected download fits the space the browser reports.
 *
 * `unknown` when the browser would not say, and callers must treat that as
 * "cannot tell" rather than as "fits": a browser with no Storage API must
 * still be able to install.
 */
function headroomFor(projectedBytes: number, outlook: StorageOutlook): StorageHeadroom {
  if (outlook.availableBytes === null) return "unknown";
  return projectedBytes <= outlook.availableBytes ? "sufficient" : "insufficient";
}

async function cacheContains(cache: Cache, path: string): Promise<boolean> {
  return (await cache.match(path)) !== undefined;
}

async function cacheMatchesObject(cache: Cache, record: ObjectRecord): Promise<boolean> {
  const response = await cache.match(record.path);
  if (!response) return false;
  const bytes = new Uint8Array(await response.arrayBuffer());
  return bytes.byteLength === record.byteLength && await sha256(bytes) === record.object;
}

const CONTENT_HASH = /^[0-9a-f]{64}$/;

function sameCandidateKeys(left: readonly CandidateKey[], right: readonly CandidateKey[]): boolean {
  return left.length === right.length && left.every((key, index) => key === right[index]);
}

function reusableDescriptorRow(
  record: ObjectRecord,
  id: string,
  root: CatalogRoot,
  descriptor: ScryfallAssetDescriptor,
): boolean {
  return record.id === id
    && record.root === root
    && record.packId === descriptor.packId
    && record.assetKey === descriptor.assetKey
    && record.sourceUrl === descriptor.sourceUrl
    && record.media === descriptor.media
    && CONTENT_HASH.test(record.object)
    && Number.isSafeInteger(record.byteLength)
    && record.byteLength > 0
    && record.path === syntheticCachePath(record.object, record.media);
}

async function allCachedObjectsMatch(cache: Cache, records: readonly ObjectRecord[]): Promise<boolean> {
  for (const record of records) {
    if (!await cacheMatchesObject(cache, record)) return false;
  }
  return true;
}

async function sha256(bytes: Uint8Array): Promise<CatalogRoot> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("") as CatalogRoot;
}

async function fetchImage(descriptor: ScryfallAssetDescriptor, signal: AbortSignal): Promise<Uint8Array> {
  const response = await fetch(descriptor.sourceUrl, {
    headers: { Accept: descriptor.media === "image/svg+xml" ? "image/svg+xml,*/*;q=0.8" : "image/jpeg,*/*;q=0.8" },
    credentials: "omit",
    redirect: "error",
    cache: "default",
    signal,
  });
  const contentType = response.headers.get("Content-Type")?.toLowerCase() ?? "";
  if (response.status !== 200 || response.type === "opaque" || !response.body || !contentType.startsWith(descriptor.media)) {
    throw new VisualPackBackendError("network");
  }
  return new Uint8Array(await response.arrayBuffer());
}

async function storeVerifiedObject(cache: Cache, path: string, media: VisualPackMedia, bytes: Uint8Array): Promise<void> {
  await cache.put(path, new Response(bytes, {
    headers: {
      "Content-Type": media,
      "Content-Length": String(bytes.byteLength),
      "Cache-Control": "public, max-age=31536000, immutable",
    },
  }));
}

export class ScryfallBrowserVisualPackBackend implements VisualPackBackend, DeckLibraryBackgroundLifecycle {
  private readonly progressListeners = new Set<(event: ProgressEvent) => void>();
  private readonly revisionListeners = new Set<(event: RevisionEvent) => void>();
  private lastPublishedRevision: string | null = null;
  /** Each running worker's abort handle AND the promise that settles when it
   *  has finished writing, so `cancel()` can wait for it. */
  private readonly workers = new Map<OperationId, {
    controller: AbortController;
    settled: Promise<void>;
    background: boolean;
    active: boolean;
  }>();
  private readonly workerFailures = new Map<OperationId, VisualPackBackendError>();
  private readonly reconciliationControllers = new Set<AbortController>();
  private readonly reconciliationTasks = new Map<Promise<void>, ReconciliationTaskOwnership>();
  /** Automatic deck-library work is opt-in through the scheduler, never on create(). */
  private deckLibraryBackgroundPaused = true;
  private backgroundLifecycle = Promise.resolve();
  private backgroundLifecycleGeneration = 0;
  private work = Promise.resolve();

  private constructor(private readonly database: IDBPDatabase<ScryfallVisualPackSchema>) {}

  static async create(): Promise<ScryfallBrowserVisualPackBackend> {
    const backend = new ScryfallBrowserVisualPackBackend(await openDatabase());
    const pending = await backend.database.getAll("operations");
    for (const operation of pending) {
      if (await backend.cancelStaleCandidateProjectionOperation(operation.id)) continue;
      if ((operation.state === "downloading" || operation.state === "finalizing") && !operation.background) {
        backend.run(operation.id);
      }
    }
    return backend;
  }

  /**
   * Suspend or permit only automatic deck-library dispatch. Manual operations
   * retain their own lifecycle, including an explicit Resume that takes
   * ownership of a formerly background record.
   */
  async setDeckLibraryBackgroundPaused(paused: boolean): Promise<void> {
    const generation = ++this.backgroundLifecycleGeneration;
    let suspension: readonly Promise<void>[] = [];
    if (paused) {
      // This guard is synchronous so a selection already waiting on a lock
      // cannot commit a new durable background operation after offline wins.
      this.deckLibraryBackgroundPaused = true;
      for (const controller of this.reconciliationControllers) controller.abort();
      const workers = [...this.workers.values()].filter((worker) => worker.background);
      for (const worker of workers) worker.controller.abort();
      suspension = [
        ...[...this.reconciliationTasks].filter(([, ownership]) => ownership.background).map(([task]) => task),
        ...workers.map((worker) => worker.settled),
      ];
    }

    const complete = async () => {
      if (paused) {
        await Promise.all(suspension);
        return;
      }
      // A newer pause must keep the effective guard closed even if this older
      // resume was already queued behind that pause's settlement.
      if (generation === this.backgroundLifecycleGeneration) this.deckLibraryBackgroundPaused = false;
    };
    const lifecycle = this.backgroundLifecycle.then(complete, complete);
    this.backgroundLifecycle = lifecycle.catch(() => undefined);
    await lifecycle;
  }

  private emit(event: ProgressEvent): void {
    for (const listener of this.progressListeners) {
      try { listener(event); } catch { /* UI listeners never affect installation. */ }
    }
  }

  private publish(event: RevisionEvent): void {
    this.lastPublishedRevision = event.revision;
    for (const listener of this.revisionListeners) {
      try { listener(event); } catch { /* UI listeners never affect installation. */ }
    }
  }

  private run(selectedOperation: OperationId, background = false): void {
    if (this.workers.has(selectedOperation)) return;
    this.workerFailures.delete(selectedOperation);
    const controller = new AbortController();
    const task = this.work.then(async () => this.resume(selectedOperation, controller.signal));
    this.work = task.catch(() => undefined);
    const settled = task.finally(() => {
      const worker = this.workers.get(selectedOperation);
      if (worker?.controller === controller) worker.active = false;
    }).catch(async (error) => {
      if (controller.signal.aborted) return;
      const kind = errorKind(error);
      // A retryable failure leaves the record `downloading`, which is exactly
      // what makes it resumable. A non-retryable one never can be, so leaving
      // it there wedges the panel: OperationProgress offers Resume instead of
      // Cancel, every durable mutation stays disabled, and `create()`'s
      // pending loop re-runs the same doomed operation on every launch.
      // Terminating the record restores those controls and stops the loop.
      const operation = retryable(kind)
        ? await this.database.get("operations", selectedOperation)
        : await this.updateOperation(selectedOperation, (current) =>
            current.state === "completed" ? current : { ...current, state: "cancelled" }).catch(() => undefined);
      this.workerFailures.set(selectedOperation, backendError(error));
      if (operation) this.emit({ phase: "failed", operation: operationStatus(operation), error: kind });
    }).catch(() => undefined).finally(() => this.workers.delete(selectedOperation));
    void settled;
    // Registered AFTER the chain is built so `settled` can be handed out, and
    // safe to do so: `run()` returns before any microtask, so the `has` guard
    // above still sees a worker that a synchronous caller registered first.
    this.workers.set(selectedOperation, { controller, settled, background, active: true });
  }

  private async runAndWait(selectedOperation: OperationId, background = false): Promise<void> {
    if (background && this.deckLibraryBackgroundPaused) return;
    const existing = this.workers.get(selectedOperation);
    if (existing && !existing.active) await existing.settled;
    if (background && this.deckLibraryBackgroundPaused) return;
    this.run(selectedOperation, background);
    await this.workers.get(selectedOperation)?.settled;
    const failure = this.workerFailures.get(selectedOperation);
    if (failure) throw failure;
  }

  private async updateOperation(
    selectedOperation: OperationId,
    update: (current: ScryfallOperationRecord) => ScryfallOperationRecord,
  ): Promise<ScryfallOperationRecord> {
    const transaction = this.database.transaction("operations", "readwrite");
    const current = await transaction.store.get(selectedOperation);
    if (!current) throw new VisualPackBackendError("invalid_input");
    const next = update(current);
    await transaction.store.put(next);
    await transaction.done;
    return next;
  }

  /** Fence old Deck Catalog work before either durable resume path can dispatch
   *  it. Aborting first also closes the small window where an already-running
   *  worker could promote rows after this operation has been superseded. */
  private async cancelStaleCandidateProjectionOperation(selectedOperation: OperationId): Promise<boolean> {
    const known = await this.database.get("operations", selectedOperation);
    if (
      !known
      || known.state === "completed"
      || known.state === "cancelled"
      || !hasStaleCandidateProjectionIntent(known)
    ) {
      return false;
    }
    const worker = this.workers.get(selectedOperation);
    worker?.controller.abort();
    let cancelled = false;
    await this.updateOperation(selectedOperation, (current) => {
      if (
        current.state === "completed"
        || current.state === "cancelled"
        || !hasStaleCandidateProjectionIntent(current)
      ) {
        return current;
      }
      cancelled = true;
      return { ...current, state: "cancelled" };
    });
    if (cancelled) await worker?.settled;
    return cancelled;
  }

  private async markSeen(selectedOperation: OperationId, selectedObject: string): Promise<OperationObjectRecord> {
    const transaction = this.database.transaction(["packs", "operations", "operationObjects"], "readwrite");
    const operation = await transaction.objectStore("operations").get(selectedOperation);
    if (!operation) throw new VisualPackBackendError("invalid_input");
    if (deckLibraryOperation(operation) && operation.state !== "downloading") throw new VisualPackBackendError("cancelled");
    if (operation.deckLibraryGeneration) {
      const receipt = await transaction.objectStore("packs").get(packId("deck_library"));
      if (!receipt || deckLibraryGeneration(receipt) !== operation.deckLibraryGeneration) {
        throw new VisualPackBackendError("cancelled");
      }
    }
    const id = operationObjectId(selectedOperation, selectedObject);
    const existing = await transaction.objectStore("operationObjects").get(id);
    if (existing) {
      await transaction.done;
      return existing;
    }
    const result: OperationObjectRecord = { id, operationId: selectedOperation, objectId: selectedObject, complete: false };
    await transaction.objectStore("operationObjects").put(result);
    await transaction.objectStore("operations").put({ ...operation, objectTotal: operation.objectTotal + 1 });
    await transaction.done;
    return result;
  }

  private async markComplete(selectedOperation: OperationId, selectedObject: string, metadata: ObjectRecord): Promise<void> {
    const transaction = this.database.transaction(["packs", "operations", "operationObjects", "objects"], "readwrite");
    const operation = await transaction.objectStore("operations").get(selectedOperation);
    const completion = await transaction.objectStore("operationObjects").get(operationObjectId(selectedOperation, selectedObject));
    if (!operation || !completion) throw new VisualPackBackendError("storage");
    if (deckLibraryOperation(operation) && operation.state !== "downloading") throw new VisualPackBackendError("cancelled");
    if (operation.deckLibraryGeneration) {
      const receipt = await transaction.objectStore("packs").get(packId("deck_library"));
      if (!receipt || deckLibraryGeneration(receipt) !== operation.deckLibraryGeneration) {
        throw new VisualPackBackendError("cancelled");
      }
    }
    await transaction.objectStore("objects").put(metadata);
    if (!completion.complete) {
      await transaction.objectStore("operationObjects").put({ ...completion, complete: true });
      await transaction.objectStore("operations").put({ ...operation, objectsPromoted: operation.objectsPromoted + 1 });
    }
    await transaction.done;
  }

  private async invalidateCompletion(selectedOperation: OperationId, selectedObject: string): Promise<void> {
    const transaction = this.database.transaction(["operations", "operationObjects"], "readwrite");
    const operation = await transaction.objectStore("operations").get(selectedOperation);
    const completion = await transaction.objectStore("operationObjects").get(operationObjectId(selectedOperation, selectedObject));
    if (operation && completion?.complete) {
      await transaction.objectStore("operationObjects").put({ ...completion, complete: false });
      await transaction.objectStore("operations").put({ ...operation, objectsPromoted: operation.objectsPromoted - 1 });
    }
    await transaction.done;
  }

  /**
   * Every already-downloaded image that a different root may reuse, keyed by
   * `contentId`.
   *
   * One streamed pass over `objects`, reduced to the three content fields.
   * `objects` is indexed by pack and not by asset key, and this step adds no
   * index — so the choice is one pass per selector or a full-store scan per
   * descriptor. Callers build it lazily, so an install whose rows are all
   * already present at its own root never pays for it at all.
   */
  private async adoptableContent(): Promise<Map<string, ObjectContent>> {
    const found = new Map<string, ObjectContent>();
    let cursor = await this.database.transaction("objects").store.openCursor();
    while (cursor) {
      const row = cursor.value;
      if (row.sourceUrl !== undefined) {
        const key = contentId(row.assetKey, row.sourceUrl);
        if (!found.has(key)) found.set(key, { object: row.object, byteLength: row.byteLength, path: row.path });
      }
      cursor = await cursor.continue();
    }
    return found;
  }

  /** `root` is the SELECTOR's root, supplied by the per-selector loop. It
   *  cannot be re-derived here: neither `descriptor.packId` nor the operation
   *  record carries a curated membership digest, so any local derivation would
   *  silently key and stamp curated objects at the bulk root.
   *
   *  `donors` is the per-selector reuse snapshot, deferred behind a thunk so it
   *  is built only if some descriptor actually misses at this root. */
  private async installObject(
    operation: ScryfallOperationRecord,
    descriptor: ScryfallAssetDescriptor,
    root: CatalogRoot,
    signal: AbortSignal,
    cache: Cache,
    donors: () => Promise<Map<string, ObjectContent>>,
  ): Promise<void> {
    const id = objectId(root, descriptor.packId, descriptor.assetKey);
    const completion = await this.markSeen(operation.id, id);
    const existing = await this.database.get("objects", id);
    if (existing) {
      const sameCandidates = sameCandidateKeys(existing.candidateKeys, descriptor.candidateKeys);
      const descriptorMetadataChanged = existing.sourceUrl !== descriptor.sourceUrl || existing.media !== descriptor.media;
      // Normal installs historically only ask whether their already-matched
      // candidate row remains in Cache Storage. Hashing every Complete resume
      // turns that cheap no-op into a full image scan. A current projection
      // operation, explicit repair, candidate drift, and source/media drift
      // instead need both descriptor and byte integrity before they may reuse
      // a row: a partial v2 operation can otherwise complete from corrupt
      // matching-key bytes and falsely stamp the receipt as current.
      const needsVerifiedReuse = hasCurrentCandidateProjectionIntent(operation)
        || operation.kind === "repair"
        || !sameCandidates
        || descriptorMetadataChanged;
      const reusable = needsVerifiedReuse
        ? reusableDescriptorRow(existing, id, root, descriptor) && await cacheMatchesObject(cache, existing)
        : await cacheContains(cache, existing.path);
      if (reusable) {
        // A seen-but-uncompleted object belongs to a fresh/retried operation.
        // Complete its guarded receipt even when its metadata already matches,
        // so reuse contributes to progress exactly once.
        if (completion.complete && sameCandidates) return;
        await this.markComplete(operation.id, id, sameCandidates
          ? existing
          : { ...existing, candidateKeys: [...descriptor.candidateKeys] });
        return;
      }
    }
    if (completion.complete) await this.invalidateCompletion(operation.id, id);
    // A preference change gives the curated pack a new root, so nothing above
    // matched even though most of the membership is byte-for-byte the images
    // already on disk. Reuse them; the cache, not the row, is the authority on
    // whether the bytes are really still there, since an eviction or a sweep
    // may have run since the snapshot was taken.
    //
    // This `cacheContains` is also the ONLY thing that repairs a curated row
    // whose cache entry has gone missing. `repair` cannot: it builds its
    // selector from the installed packs row and then filters against that same
    // row, so a curated repair is always removed before an operation exists and
    // returns `{status:"healthy"}` while the entry stays missing. MEASURED —
    // `verify` reports `missing_object`, `repair` reports healthy, and a second
    // `verify` still reports it. Recovery is a preference change (this check
    // fails and the asset is fetched again) or remove-and-reinstall. Do not
    // justify anything here by "verify/repair will fix it".
    const donor = existing ? undefined : (await donors()).get(contentId(descriptor.assetKey, descriptor.sourceUrl));
    let content: ObjectContent;
    if (donor && await cacheContains(cache, donor.path)) {
      content = donor;
    } else {
      const bytes = await fetchImage(descriptor, signal);
      const object = await sha256(bytes);
      content = { object, byteLength: bytes.byteLength, path: syntheticCachePath(object, descriptor.media) };
      await storeVerifiedObject(cache, content.path, descriptor.media, bytes);
    }
    const metadata: ObjectRecord = Object.freeze({
      id,
      // The stamp every deletion path filters on. It must agree with the key
      // `id` was built from, or the row is deletable by no path at all.
      root,
      packId: descriptor.packId,
      assetKey: descriptor.assetKey,
      candidateKeys: [...descriptor.candidateKeys],
      sourceUrl: descriptor.sourceUrl,
      object: content.object,
      byteLength: content.byteLength,
      media: descriptor.media,
      path: content.path,
    });
    // Reuse and download converge here deliberately: a reuse path that skipped
    // `markComplete` would never increment `objectsPromoted`. `finish()` does
    // not compare that counter against `objectTotal`, so the record would
    // still reach `completed` and the only symptom would be a progress figure
    // permanently short by however many images were reused — a wrong number no
    // fetch count can see. One exit is what makes that unrepresentable.
    try {
      await this.markComplete(operation.id, id, metadata);
      if (existing && existing.path !== content.path) {
        await this.sweepUnreferenced(new Set([existing.path]), cache);
      }
    } catch (error) {
      // Removal can win after the cache write but before this guarded durable
      // row write. Sweep only if no other pack references the content.
      await this.sweepUnreferenced(new Set([content.path]), cache);
      throw error;
    }
  }

  /** `root` is the SELECTOR's root, supplied by the per-selector loop — the
   *  same value `installObject` keyed and stamped this pack's objects with.
   *
   *  Returns the cache paths of the rows it deleted, so a caller that owns this
   *  pack's garbage can ask whether those images are now unreferenced. The
   *  deletion and the promotion stay in ONE transaction, which is why the paths
   *  are handed back rather than recollected afterwards — by then the rows that
   *  named them are gone. Callers that own no garbage simply ignore the value;
   *  nothing else about this method changed. */
  private async completePack(
    operation: ScryfallOperationRecord,
    selectedPack: PackId,
    root: CatalogRoot,
    signal?: AbortSignal,
  ): Promise<Set<string>> {
    const dropped = new Set<string>();
    if (signal?.aborted) return dropped;
    const transaction = this.database.transaction(["packs", "objects", "operations"], "readwrite");
    bindTransactionAbort(transaction, signal);
    const current = await transaction.objectStore("operations").get(operation.id);
    if (signal?.aborted) {
      abortTransaction(transaction);
      return dropped;
    }
    if (!current || current.state !== "downloading") {
      await transaction.done;
      return dropped;
    }
    const existing = await transaction.objectStore("packs").get(selectedPack);
    if (signal?.aborted) {
      abortTransaction(transaction);
      return dropped;
    }
    if (current.deckLibraryGeneration) {
      if (!existing || deckLibraryGeneration(existing) !== current.deckLibraryGeneration) {
        throw new VisualPackBackendError("cancelled");
      }
    }
    if (existing?.operationId === operation.id && existing.root === root) {
      await transaction.done;
      return dropped;
    }
    if (existing && existing.root !== root) {
      const index = transaction.objectStore("objects").index("by-pack");
      let cursor = await index.openCursor(existing.packId);
      while (cursor) {
        if (signal?.aborted) {
          abortTransaction(transaction);
          return dropped;
        }
        if (cursor.value.root === existing.root) {
          dropped.add(cursor.value.path);
          if (signal?.aborted) {
            abortTransaction(transaction);
            return dropped;
          }
          await cursor.delete();
          if (signal?.aborted) {
            abortTransaction(transaction);
            return dropped;
          }
        }
        cursor = await cursor.continue();
        if (signal?.aborted) {
          abortTransaction(transaction);
          return dropped;
        }
      }
    }
    if (signal?.aborted) {
      abortTransaction(transaction);
      return dropped;
    }
    transactionGate("complete-pack-before-write");
    if (signal?.aborted) {
      abortTransaction(transaction);
      return dropped;
    }
    await transaction.objectStore("packs").put({
      id: selectedPack,
      packId: selectedPack,
      root,
      dependencies: [],
      operationId: operation.id,
      ...(selectedPack === packId("deck_library")
        ? {
            optInGeneration: existing ? deckLibraryGeneration(existing) : current.deckLibraryGeneration ?? current.id,
            ...(existing?.candidateProjectionVersion === undefined
              ? {}
              : { candidateProjectionVersion: existing.candidateProjectionVersion }),
          }
        : {}),
    });
    if (signal?.aborted) {
      abortTransaction(transaction);
      return dropped;
    }
    await transaction.objectStore("operations").put({ ...current, packsPromoted: current.packsPromoted + 1 });
    if (signal?.aborted) {
      abortTransaction(transaction);
      return dropped;
    }
    transactionGate("complete-pack-before-commit");
    if (signal?.aborted) {
      abortTransaction(transaction);
      return dropped;
    }
    await transaction.done;
    return dropped;
  }

  /**
   * Delete every path in `paths` that no remaining objects row — in ANY pack —
   * still references.
   *
   * Content addressing means two packs that downloaded the same bytes share one
   * cache entry, so "this pack's row is gone" is never on its own a reason to
   * delete the image. `remove()` has always swept exactly this way; naming it
   * here lets the curated delta path reuse that sweep instead of growing a
   * second one that could disagree with it.
   *
   * The reference set is indexed rather than rescanned per path. `remove()`
   * swept a handful of paths on a rare user action, so a linear scan inside the
   * loop never showed; a curated preference change hands this every path of the
   * replaced membership, and both sides are then the whole membership. MEASURED
   * at 105,261 paths against 105,261 rows: 103.8 s scanning, 36.6 ms indexed,
   * with identical deletion sets. The predicate is unchanged — `path` is in the
   * set exactly when some remaining row references it — so WHICH paths are
   * deleted does not move for either caller.
   */
  private async sweepUnreferenced(paths: ReadonlySet<string>, cache: Cache): Promise<void> {
    if (paths.size === 0) return;
    const referenced = new Set((await this.database.getAll("objects")).map((entry) => entry.path));
    for (const path of paths) {
      if (!referenced.has(path)) await cache.delete(path);
    }
  }

  /**
   * Drop every curated objects row that no root still points at, then sweep the
   * images that leaves unreferenced.
   *
   * `markComplete` writes rows DURING the download, before `completePack`
   * promotes the pack, while every deletion path is scoped to a single root
   * (`completePack` to the row it replaces, `remove()` to the row it removes).
   * So a curated install at digest D2 that is cancelled or interrupted leaves
   * its rows at D2 while the packs row still names D1, and the next install at
   * D3 clears only D1. Nothing can ever reach the D2 rows again: they are keyed
   * and stamped at a root no pack names. The cache sweep cannot help either —
   * those rows still exist and still reference their paths, which is exactly
   * what makes the sweep keep them. Because a curated pack re-syncs on every
   * preference change, they accumulate against the disk budget the pack exists
   * to protect.
   *
   * A root is abandoned only if NOTHING still claims it, and three things can:
   * this operation, the installed packs row, and any operation that has not
   * reached a terminal state. The third is not a refinement — IDB is shared
   * across tabs while a backend instance is not, so another tab's install is
   * writing rows under a root that is neither of the first two, and it writes
   * them DURING its download. Without that clause this sweep deletes a live
   * membership out from under the tab installing it, and nothing downstream can
   * notice: `objectsPromoted` counts `operationObjects` completion flags, never
   * the surviving rows, so that tab still reports `completed` with every asset
   * promoted while its pack is missing whatever this sweep took. Terminal
   * operations are deliberately NOT kept — a cancelled or completed record's
   * root is exactly the garbage this collects.
   */
  private async collectLocalPackGarbage(
    selectedPack: PackId,
    root: CatalogRoot,
    dropped: ReadonlySet<string>,
    cache: Cache,
  ): Promise<void> {
    const paths = new Set(dropped);
    const transaction = this.database.transaction(["packs", "objects", "operations"], "readwrite");
    const installed = await transaction.objectStore("packs").get(selectedPack);
    const keep = new Set<CatalogRoot>([root]);
    if (installed) keep.add(installed.root);
    for (const operation of await transaction.objectStore("operations").getAll()) {
      if (operation.state === "completed" || operation.state === "cancelled") continue;
      for (const [position, selector] of operation.selectors.entries()) {
        if (operation.packIds[position] === selectedPack) keep.add(selectorRoot(selector, operation.catalog));
      }
    }
    const index = transaction.objectStore("objects").index("by-pack");
    let cursor = await index.openCursor(selectedPack);
    while (cursor) {
      if (!keep.has(cursor.value.root)) {
        paths.add(cursor.value.path);
        await cursor.delete();
      }
      cursor = await cursor.continue();
    }
    await transaction.done;
    await this.sweepUnreferenced(paths, cache);
  }

  private async collectCuratedGarbage(root: CatalogRoot, dropped: ReadonlySet<string>, cache: Cache): Promise<void> {
    await this.collectLocalPackGarbage(packId("curated"), root, dropped, cache);
  }

  private async finish(selectedOperation: OperationId, signal?: AbortSignal): Promise<void> {
    if (signal?.aborted) return;
    const finishing = await this.updateOperation(selectedOperation, (current) =>
      current.state === "downloading" ? { ...current, state: "finalizing" } : current,
    );
    if (finishing.state !== "finalizing" || signal?.aborted) return;
    const transaction = this.database.transaction(["state", "packs", "operations"], "readwrite");
    bindTransactionAbort(transaction, signal);
    const currentOperation = await transaction.objectStore("operations").get(selectedOperation);
    if (!currentOperation || currentOperation.state !== "finalizing") {
      await transaction.done;
      return;
    }
    if (signal?.aborted) {
      abortTransaction(transaction);
      return;
    }
    if (currentOperation.deckLibraryGeneration) {
      const receipt = await transaction.objectStore("packs").get(packId("deck_library"));
      if (signal?.aborted) {
        abortTransaction(transaction);
        return;
      }
      if (
        !receipt
        || deckLibraryGeneration(receipt) !== currentOperation.deckLibraryGeneration
        || receipt.operationId !== currentOperation.id
      ) {
        if (signal?.aborted) {
          abortTransaction(transaction);
          return;
        }
        await transaction.objectStore("operations").put({ ...currentOperation, state: "cancelled" });
        if (signal?.aborted) {
          abortTransaction(transaction);
          return;
        }
        await transaction.done;
        this.emit({ phase: "cancelled", operation: await this.operationStatus(selectedOperation), error: null });
        return;
      }
    }
    if (hasCurrentCandidateProjectionIntent(currentOperation)) {
      const receipt = await transaction.objectStore("packs").get(packId("deck_library"));
      if (signal?.aborted) {
        abortTransaction(transaction);
        return;
      }
      // A promotion can be superseded between its receipt write and finish.
      // Projection intent only becomes proof when this exact operation still
      // owns that receipt inside the same transaction as its completion.
      if (!receipt || receipt.operationId !== currentOperation.id) {
        await transaction.objectStore("operations").put({ ...currentOperation, state: "cancelled" });
        if (signal?.aborted) {
          abortTransaction(transaction);
          return;
        }
        await transaction.done;
        this.emit({ phase: "cancelled", operation: await this.operationStatus(selectedOperation), error: null });
        return;
      }
      await transaction.objectStore("packs").put({
        ...receipt,
        candidateProjectionVersion: CARD_CANDIDATE_PROJECTION_VERSION,
      });
      if (signal?.aborted) {
        abortTransaction(transaction);
        return;
      }
    }
    const currentState = (await transaction.objectStore("state").get(STATE)) ?? initialState();
    if (signal?.aborted) {
      abortTransaction(transaction);
      return;
    }
    const revision = String(BigInt(currentState.revision) + 1n);
    if (signal?.aborted) {
      abortTransaction(transaction);
      return;
    }
    transactionGate("finish-before-write");
    if (signal?.aborted) {
      abortTransaction(transaction);
      return;
    }
    await transaction.objectStore("state").put({ ...currentState, catalog: finishing.catalog, revision });
    if (signal?.aborted) {
      abortTransaction(transaction);
      return;
    }
    await transaction.objectStore("operations").put({ ...currentOperation, state: "completed", completedRevision: revision });
    if (signal?.aborted) {
      abortTransaction(transaction);
      return;
    }
    transactionGate("finish-before-commit");
    if (signal?.aborted) {
      abortTransaction(transaction);
      return;
    }
    await transaction.done;
    const completed = await this.operationStatus(selectedOperation);
    this.emit({ phase: "completed", operation: completed, error: null });
    this.publish({ cause: finishing.kind ?? "install", operationId: selectedOperation, catalogRoot: finishing.catalog.root, revision: installedRevision(revision) });
  }

  private async resume(
    selectedOperation: OperationId,
    signal: AbortSignal,
  ): Promise<void> {
    const operation = await this.database.get("operations", selectedOperation);
    if (!operation || !deckLibraryOperation(operation)) {
      return this.resumeOwned(selectedOperation, signal);
    }
    const locks = webLocks();
    if (!locks) {
      if (operation.background) throw new VisualPackBackendError("unavailable");
      return this.resumeOwned(selectedOperation, signal);
    }
    return locks.request(DECK_LIBRARY_LOCK, { mode: "exclusive", signal }, () =>
      this.resumeOwned(selectedOperation, signal));
  }

  private async resumeOwned(selectedOperation: OperationId, signal: AbortSignal): Promise<void> {
    let operation = await this.database.get("operations", selectedOperation);
    if (!operation || operation.state === "completed" || operation.state === "cancelled") return;
    if (operation.state === "cancel_requested") {
      await this.updateOperation(selectedOperation, (current) => ({ ...current, state: "cancelled" }));
      return;
    }
    if (signal.aborted) return;
    const cache = await caches.open(CACHE);
    for (const [index, selector] of operation.selectors.entries()) {
      operation = await this.database.get("operations", selectedOperation);
      if (!operation || operation.state === "cancel_requested" || operation.state === "cancelled" || signal.aborted) break;
      const selectedPack = operation.packIds[index];
      const root = selectorRoot(selector, operation.catalog);
      const installed = await this.database.get("packs", selectedPack);
      if (installed?.operationId === selectedOperation && installed.root === root) continue;
      const inFlight = new Set<Promise<void>>();
      // The reuse snapshot for this selector, taken at most once, only if some
      // descriptor misses at this root, and only for `curated`.
      //
      // Deferred rather than eager because a resume, where every row is already
      // present, must not pay a full pass over `objects` to learn that.
      //
      // Restricted to `curated` because curated is what this step was scoped
      // to make cheap, and because the snapshot is not free: MEASURED at ~520
      // bytes of retained Map per row, so ~55 MiB across a curated membership
      // and ~191 MiB across a `complete` pack's rows, held live for the whole
      // download on a client with mobile OOM history.
      //
      // The gate is NOT a claim that reuse would be worthless for the others.
      // A republished bulk catalog leaves most image URLs where they were, so
      // letting `complete` adopt would very likely turn a root bump into
      // almost no downloads at all. That is a feature with a memory bound to
      // establish and tests to write, and it is not something to acquire as a
      // side effect of a change whose rule was that the other selectors do not
      // move. Adopting ACROSS packs is a separate axis and stays fully
      // available: keying on content rather than on pack means a curated
      // install reads every row in the store, `complete`/`printing`/`locale`
      // included.
      let adoptable: Promise<Map<string, ObjectContent>> | null = null;
      // Repair trusts only the receipt row it is restoring. A cache donor may
      // be present yet corrupt, and validating every possible donor would turn
      // a targeted repair into a store-wide full verification pass.
      const donors = operation.kind !== "repair" && localPack(selectedPack)
        ? () => (adoptable ??= this.adoptableContent())
        : NO_DONORS;
      let failure: unknown = null;
      let lastProgressAt = 0;
      let progressUpdate = Promise.resolve();
      const reportProgress = (force = false) => {
        const now = performance.now();
        if (!force && now - lastProgressAt < PROGRESS_INTERVAL_MS) return;
        lastProgressAt = now;
        progressUpdate = progressUpdate.then(async () => {
          const updated = await this.operationStatus(selectedOperation);
          this.emit({ phase: "running", operation: updated, error: null });
        }).catch(() => undefined);
      };
      const schedule = (descriptor: ScryfallAssetDescriptor): Promise<void> | void => {
        if (failure) throw failure;
        const task: Promise<void> = (async () => {
          const current = await this.database.get("operations", selectedOperation);
          if (!current || current.state === "cancel_requested" || current.state === "cancelled" || signal.aborted) return;
          await this.installObject(current, descriptor, root, signal, cache, donors);
          reportProgress();
        })().catch((error) => {
          failure ??= error;
        }).finally(() => inFlight.delete(task));
        inFlight.add(task);
        return inFlight.size >= DOWNLOAD_CONCURRENCY ? Promise.race(inFlight) : undefined;
      };
      try {
        const repairDescriptors = operation.kind === "repair" && selector.kind === "deck_library"
          ? operation.repairDescriptors
          : undefined;
        if (repairDescriptors) {
          for (const descriptor of repairDescriptors) {
            if (signal.aborted) break;
            const pending = schedule(descriptor);
            if (pending) await pending;
          }
        } else {
          await forEachScryfallAsset(operation.catalog, selector, signal, schedule);
        }
      } finally {
        // `schedule` re-throws a recorded `failure` synchronously on its next
        // call, and that throw unwinds through `forEachScryfallAsset`. Without
        // this `finally` it would skip the settle below and leave the remaining
        // tasks running detached, each holding a live `cache` and IDB handle
        // and still writing rows at `root` — against a record `run()` is about
        // to terminate, and past the `collectCuratedGarbage` that would have
        // reclaimed them. Settling here makes the batch quiet on EVERY exit.
        await Promise.allSettled(inFlight);
      }
      // Every `installObject` for this selector has settled, so the reuse
      // snapshot has no reader left. Dropping the reference here rather than at
      // the end of the iteration keeps it from overlapping the sweep below,
      // which materialises every remaining row AND a path set of its own —
      // three structures the size of the membership, live at once, on the peak
      // this pack exists to keep small.
      adoptable = null;
      if (failure) throw failure;
      reportProgress(true);
      await progressUpdate;
      operation = await this.database.get("operations", selectedOperation);
      if (!operation || operation.state === "cancel_requested" || operation.state === "cancelled" || signal.aborted) break;
      const dropped = await this.completePack(operation, selectedPack, root, signal);
      if (signal.aborted) break;
      // Curated is the only pack whose root moves under the user rather than
      // under Scryfall, so it is the only one that strands rows and images
      // behind on a change of preference. Every other selector is left exactly
      // as it was: it discards `dropped` and collects no garbage.
      if (selectedPack === packId("curated")) await this.collectCuratedGarbage(root, dropped, cache);
      else if (selectedPack === packId("deck_library")) await this.collectLocalPackGarbage(selectedPack, root, dropped, cache);
      this.emit({ phase: "running", operation: await this.operationStatus(selectedOperation), error: null });
    }
    operation = await this.database.get("operations", selectedOperation);
    if (!operation) return;
    if (operation.state === "cancel_requested") {
      const cancelled = await this.updateOperation(selectedOperation, (current) => ({ ...current, state: "cancelled" }));
      this.emit({ phase: "cancelled", operation: operationStatus(cancelled), error: null });
      return;
    }
    if (operation.state === "cancelled" || signal.aborted) return;
    await this.finish(selectedOperation, signal);
  }

  /**
   * Secure the storage an operation is about to consume, and refuse one that
   * provably will not fit.
   *
   * The grant is requested for every USER-INITIATED operation that downloads,
   * not only for something judged "large". It is deliberately NOT requested on
   * the auto-resume path: `create()` calls `run()` directly only for manual records
   * left `downloading`/`finalizing`, so a launch never prompts without the user
   * having asked for anything. Without it Cache Storage is best-effort and the browser MAY
   * evict the pack under disk pressure — for a pack downloaded so the app works
   * offline, that is the exact failure the pack exists to prevent, and it is no
   * less a failure for a small pack than for a big one. It is requested BEFORE
   * any byte is written, since it protects the bytes that follow it.
   *
   * The size refusal is gated on `objectEstimate`, which only an `install`
   * request carries. `repair` re-fetches objects an install already accounted
   * for, and `resume` continues an operation that was checked when it started,
   * so neither has a figure to check and neither introduces bytes this could
   * have caught.
   *
   * IT REFUSES ON `minimumImageBytes`, NOT ON THE EXPECTED SIZE, and the gap
   * between the two is the whole design. `estimatedImageBytes` rests on six
   * CDN samples per rung; against ~35,000 faces of wildly varying card art the
   * real total could plausibly sit tens of percent either side of it. The two
   * outcomes are not symmetric:
   *
   *  - Refusing wrongly denies an install that would have worked, and there is
   *    no override anywhere in this backend or the panel to undo it.
   *  - Running out of quota mid-download is recoverable. A `QuotaExceededError`
   *    from `cache.put` is classified `storage`, `storage` is `retryable`, so
   *    the record stays `downloading` and the panel offers Resume; and the
   *    resume re-downloads almost nothing, because `installObject` returns
   *    early for every object already complete and still cached.
   *
   * So the expected figure is a WARNING, carried on `InstallEstimate.headroom`
   * for the UI to show, and this gate fires only where even the cheapest
   * possible reading of the same constants cannot fit — a case no error bar on
   * them can rescue.
   */
  private async reserveStorage(request: StartRequest): Promise<StoragePersistence> {
    const persistence = await requestPersistence();
    if (request.kind !== "install") return persistence;
    // The same delta the estimate reports, recomputed here rather than taken
    // from `request.objectEstimate`. Two reasons: `objectEstimate` is the
    // progress denominator and so is the whole membership by design, and a
    // caller's figure was read at whatever moment it ran its estimate — a gate
    // must compare the space free NOW against the work outstanding NOW. Only
    // curated has a recoverable installed membership to diff against; every
    // other selector keeps the caller's count.
    const floorBytes = minimumImageBytes(
      request.selector.kind === "curated" || request.selector.kind === "deck_library"
        ? await this.localFetchCount(selectorPack(request.selector), request.selector.membershipDigest)
        : request.objectEstimate,
    );
    const outlook = await storageOutlook(persistence);
    // `unknown` proceeds deliberately. A browser that will not report a quota
    // is not reporting that the download fails to fit, and refusing on silence
    // would make every browser without the Storage API unable to install
    // anything at all.
    //
    // Refused as its own kind rather than as `storage`, and with the two
    // figures as typed fields rather than interpolated into a message: the
    // panel renders them in the user's language and unit, and it must render
    // the numbers THIS comparison was made on. The alternative — a floor
    // recomputed in the panel from the estimate — reads the browser's quota at
    // a different instant than the gate did and can disagree with it.
    const { availableBytes } = outlook;
    if (availableBytes !== null && headroomFor(floorBytes, outlook) === "insufficient") {
      throw new VisualPackStorageRefusalError({ requiredBytes: floorBytes, availableBytes });
    }
    return persistence;
  }

  private async estimate(
    source: CatalogRecord,
    selector: InstallSelector,
    revision: string,
    onProgress?: (progress: CatalogScanProgress) => void,
  ): Promise<InstallEstimate> {
    if (selector.kind === "complete" && selector.rootSha256 !== source.root) throw new VisualPackBackendError("conflict");
    const assetRecords = await countScryfallAssets(source, selector, new AbortController().signal, onProgress);
    // The size question is "what will this DOWNLOAD", which for a pack already
    // installed is not the size of its membership. Step 4 made a re-sync nearly
    // free by skipping cached objects per object at download time, but that
    // saving was invisible ahead of the run, so a sync fetching a handful of
    // moved images reported the whole 6.5 GB — and `reserveStorage` could
    // refuse it at low disk.
    //
    // TWO COUNTS, TWO CONSUMERS, and they legitimately differ for a curated
    // re-sync:
    //
    //  - `assetRecords` is what the run will PROMOTE. It stays the WHOLE
    //    membership: the panel passes it as `objectEstimate`, and that is the
    //    denominator of a progress bar whose numerator counts every object the
    //    run promotes, reused ones included. A denominator smaller than the
    //    numerator's reach would run the bar past its end.
    //  - `uniqueObjects` is what the run will DOWNLOAD. It is the panel's only
    //    count row for a curated estimate, rendered directly beside
    //    `estimatedImageBytes` under a label that says "Images to download", so
    //    it must be the SAME figure that byte projection was computed from.
    //    Reporting the membership there put "Images to download: 105,165"
    //    beside "Estimated download size: 0 B" on an already-installed pack.
    //
    // For every non-curated selector the two are the same number, so nothing
    // but the curated re-sync moves.
    const downloadRecords = selector.kind === "curated" || selector.kind === "deck_library"
      ? await this.localFetchCount(selectorPack(selector), selector.membershipDigest)
      : assetRecords;
    const projectedBytes = estimatedImageBytes(downloadRecords);
    const storage = await storageOutlook(await currentPersistence());
    return {
      catalogRoot: source.root,
      installedRevision: installedRevision(revision),
      selector: selectorPack(selector),
      packIds: [selectorPack(selector)],
      assetRecords: String(assetRecords),
      uniqueObjects: String(downloadRecords),
      // Still "unknown", and NOT where the projection below goes. These two are
      // the ONLY write sites they have, and both hardcode this string: nothing
      // in the codebase has ever populated them, and their labels say so in no
      // language — `en` hedges with "(known after download)" and the other six
      // name a measurement outright. They are left alone
      // rather than repurposed only so a projection is never written into a
      // field whose label claims to be a measurement; deciding their fate
      // (populate or delete, with their seven locales) is open work.
      logicalImageBytes: "unknown",
      uniqueImageBytes: "unknown",
      // A curated pack reads no shard of the bulk archive, so reporting that
      // archive's compressed size would show the user the multi-gigabyte
      // download this selector exists to avoid.
      shardCount: selector.kind === "curated" || selector.kind === "deck_library" ? "0" : "1",
      shardBytes: selector.kind === "curated" || selector.kind === "deck_library" ? "unknown" : String(source.compressedBytes),
      // The size question every selector must answer, curated and bulk alike:
      // the whole point of the curated pack is that this figure is far smaller
      // than `complete`'s, and a user cannot see that unless both report one.
      estimatedImageBytes: projectedBytes,
      storage,
      headroom: headroomFor(projectedBytes, storage),
    };
  }

  /**
   * The curated selector for the preferences stored right now.
   *
   * Reads no catalog and opens no bulk stream: the digest comes from the same
   * `planCuratedPack()` memo that `start()`'s conflict guard and `run()`'s
   * descriptor pass go through, so a selector this returns is one those two
   * agree with rather than a second opinion about it.
   */
  async curatedSelector(): Promise<CuratedInstallSelector> {
    try {
      const { membershipDigest } = await planCuratedPack();
      return { kind: "curated", membershipDigest };
    } catch (error) {
      throw backendError(error);
    }
  }

  /**
   * What a curated sync would do right now: the planned membership against the
   * one on disk, in the three categories `CuratedDrift` documents.
   *
   * The installed membership needs no new storage to recover. `objects` rows
   * carry `assetKey` and `sourceUrl` and are indexed `by-pack`, and for curated
   * the pack's root IS its membership digest — so the `by-pack` rows filtered
   * to the installed root are exactly the installed `(assetKey, sourceUrl)` set.
   *
   * READ ONLY, so it takes them in ONE indexed request rather than cursoring.
   * `collectCuratedGarbage` and `adoptableContent` cursor the same rows, but
   * neither is precedent for doing so here: the first DELETES through its
   * cursor and the second streams while it builds. The read-only precedent in
   * this file is `sweepUnreferenced`, which went indexed with a measurement
   * attached — 105,261 rows, 103.8 s cursoring against 36.6 ms indexed. This
   * runs at that same scale and now runs twice per install start (`estimate()`
   * and `reserveStorage()`) plus once per `curatedDrift()`, all on the main
   * thread.
   *
   * Rows at any OTHER root under the curated pack are deliberately excluded:
   * those belong to a superseded or an in-flight membership, and counting them
   * would report a sync as having work to remove that `completePack` is going
   * to remove anyway.
   */
  private async membershipDiff(
    selectedPack: PackId,
    descriptors: readonly ScryfallAssetDescriptor[],
  ): Promise<MembershipDiff> {
    const installedPack = await this.database.get("packs", selectedPack);
    // Nothing installed: every descriptor is an add, which is what the
    // first-install estimate has always reported.
    if (!installedPack) return { installedDigest: null, add: descriptors.length, remove: 0, refresh: 0 };
    const installed = new Map<AssetKey, string | undefined>();
    for (const row of await this.database.getAllFromIndex("objects", "by-pack", selectedPack)) {
      if (row.root === installedPack.root) installed.set(row.assetKey, row.sourceUrl);
    }
    const planned = new Set<AssetKey>();
    let add = 0;
    let refresh = 0;
    for (const descriptor of descriptors) {
      planned.add(descriptor.assetKey);
      // `has` before `get`, so a row that stores `undefined` is told apart from
      // a row that is not there: the first is a refresh, the second an add.
      // Both are fetched, but they are different facts and the panel names them
      // differently.
      if (!installed.has(descriptor.assetKey)) add += 1;
      else if (installed.get(descriptor.assetKey) !== descriptor.sourceUrl) refresh += 1;
    }
    let remove = 0;
    for (const key of installed.keys()) if (!planned.has(key)) remove += 1;
    return { installedDigest: installedPack.root, add, remove, refresh };
  }

  /**
   * The images a curated install would actually FETCH, as opposed to the size
   * of its membership: `add + refresh`.
   *
   * This is a count of ROWS, and whether a fetch happens is a CACHE fact. Not
   * counted requires `installed.has(assetKey) && installed.get(assetKey) ===
   * descriptor.sourceUrl` — an `objects` row saying those bytes were stored;
   * `installObject` decides reuse by asking `cacheContains` whether they are
   * still there. So the figure moves in BOTH directions:
   *
   *  - It OVERSTATES when a row under a DIFFERENT pack holds the same content:
   *    that row donates and no request is made, while this counts only the
   *    curated pack's own rows.
   *  - It UNDERSTATES when a cache entry has been evicted out from under a
   *    surviving row. That state is reachable and permanent — see
   *    `installObject`, where the MEASURED note records `verify` reporting
   *    `missing_object` while `repair` reports healthy — so an eviction leaves
   *    the row counted as installed and the sync fetches it anyway.
   *
   * No guard, because the only consumer that can refuse a user gates on
   * `minimumImageBytes` of this — roughly a quarter of `estimatedImageBytes` —
   * and `reserveStorage`'s own doc block takes the asymmetry deliberately: a
   * wrong refusal has no override, while running out of quota mid-download is a
   * `storage` failure, which is retryable, and the resume re-fetches almost
   * nothing.
   */
  private async localFetchCount(selectedPack: PackId, digest: CatalogRoot): Promise<number> {
    const membership = selectedPack === packId("curated")
      ? { membershipDigest: digest, descriptors: await curatedDescriptors(digest) }
      : await planDeckLibraryPack(packId("deck_library"));
    if (membership.membershipDigest !== digest) throw new VisualPackBackendError("conflict");
    const diff = await this.membershipDiff(selectedPack, membership.descriptors);
    return diff.add + diff.refresh;
  }

  /**
   * The shared membership diff against the preferences and decks name right now,
   * for the panel to render before a sync runs.
   *
   * Reached through `VisualPackBackend`: the panel reads it whenever the
   * summary reports an installed curated pack, and again whenever the art
   * preferences behind the membership move. It only ever REPORTS — the sync it
   * describes waits for the user to press it.
   *
   * NULL WHEN MEASURING WOULD MEAN LOADING, and that is the point of the guard
   * rather than an incidental early return. `planCuratedPack()` awaits
   * `loadScryfallData()` and `loadPrintingsData()`; together those are a 76 MB
   * fetch and JSON parse, and the caller is a settings panel reading this on
   * mount. Opening Preferences having rendered no card is ordinary, passive
   * navigation with no progress indication and no way to cancel.
   *
   * Decided HERE and not in the panel: which data files are resident is engine
   * knowledge, and a display layer that reasoned about it would be deriving
   * state. Once anything has loaded them — a rendered card image, or the user
   * choosing the curated option, which resolves a selector through this same
   * planner — the next call measures for free.
   */
  async curatedDrift(): Promise<CuratedDrift | null> {
    try {
      if (!isCardDataResident()) return null;
      const { membershipDigest, descriptors } = await planCuratedPack();
      return { membershipDigest, ...await this.membershipDiff(packId("curated"), descriptors) };
    } catch (error) {
      throw backendError(error);
    }
  }

  async deckLibrarySelector(): Promise<DeckLibraryInstallSelector> {
    try {
      const { membershipDigest } = await planDeckLibraryPack(packId("deck_library"));
      return { kind: "deck_library", membershipDigest };
    } catch (error) {
      throw backendError(error);
    }
  }

  async deckLibraryDrift(): Promise<DeckLibraryDrift | null> {
    try {
      if (!isCardDataResident()) return null;
      const { membershipDigest, descriptors } = await planDeckLibraryPack(packId("deck_library"));
      return { membershipDigest, ...await this.membershipDiff(packId("deck_library"), descriptors) };
    } catch (error) {
      throw backendError(error);
    }
  }

  private assertDeckLibraryBackgroundGeneration(generation: number): void {
    if (this.deckLibraryBackgroundPaused || generation !== this.backgroundLifecycleGeneration) {
      throw new VisualPackBackendError("cancelled");
    }
  }

  /**
   * Interprets an awaited background value only while the lifecycle generation
   * that issued it remains open. A pause followed by resume must invalidate the
   * old request too, even though the final paused flag is false again.
   */
  private async awaitDeckLibraryBackgroundGeneration<T>(
    generation: number,
    request: Promise<T>,
  ): Promise<T> {
    try {
      const value = await request;
      this.assertDeckLibraryBackgroundGeneration(generation);
      return value;
    } catch (error) {
      this.assertDeckLibraryBackgroundGeneration(generation);
      throw error;
    }
  }

  /**
   * Reconciles an existing Deck Catalog and then proves the durable receipt is
   * still current. It never creates an opt-in, requests persistence, or owns
   * the background lifecycle; the mounted scheduler does that.
   */
  async prepareDeckLibraryForOffline(): Promise<DeckLibraryPreparationResult> {
    try {
      this.assertDeckLibraryBackgroundGeneration(this.backgroundLifecycleGeneration);
      const generation = this.backgroundLifecycleGeneration;
      const receipt = await this.awaitDeckLibraryBackgroundGeneration(
        generation,
        this.database.get("packs", packId("deck_library")),
      );
      if (!receipt) return "not-installed";

      const optInGeneration = deckLibraryGeneration(receipt);
      await this.awaitDeckLibraryBackgroundGeneration(generation, this.reconcileDeckLibrary());
      const drift = await this.awaitDeckLibraryBackgroundGeneration(generation, this.deckLibraryDrift());
      if (!drift) throw new VisualPackBackendError("unavailable");
      const finalReceipt = await this.awaitDeckLibraryBackgroundGeneration(
        generation,
        this.database.get("packs", packId("deck_library")),
      );

      // Deliberately no await after this point: these are one final atomic
      // observation of the receipt and the just-measured membership.
      this.assertDeckLibraryBackgroundGeneration(generation);
      if (!finalReceipt) return "not-installed";
      if (
        deckLibraryGeneration(finalReceipt) !== optInGeneration
        || finalReceipt.candidateProjectionVersion !== CARD_CANDIDATE_PROJECTION_VERSION
        || finalReceipt.root !== drift.installedDigest
        || drift.installedDigest !== drift.membershipDigest
        || drift.add !== 0
        || drift.remove !== 0
        || drift.refresh !== 0
      ) {
        throw new VisualPackBackendError("conflict");
      }
      return "ready";
    } catch (error) {
      throw backendError(error);
    }
  }

  /**
   * Reconcile only an extant deck-library receipt. This deliberately bypasses
   * `start()` because that public UI path requests persistence before writing;
   * a background refresh is not user activation and must never prompt.
   */
  async reconcileDeckLibrary(): Promise<void> {
    const ownership: ReconciliationTaskOwnership = { operationId: null, background: true };
    const task = this.reconcileDeckLibraryOwned(ownership);
    this.reconciliationTasks.set(task, ownership);
    try {
      await task;
    } finally {
      this.reconciliationTasks.delete(task);
    }
  }

  private async reconcileDeckLibraryOwned(ownership: ReconciliationTaskOwnership): Promise<void> {
    try {
      if (this.deckLibraryBackgroundPaused) return;
      const receipt = await this.database.get("packs", packId("deck_library"));
      if (!receipt || this.deckLibraryBackgroundPaused) return;
      const revisionBefore = (await state(this.database)).revision;
      const locks = webLocks();
      if (!locks) throw new VisualPackBackendError("unavailable");
      const controller = new AbortController();
      this.reconciliationControllers.add(controller);
      let selectedOperation: OperationId | null = null;
      try {
        selectedOperation = await locks.request(
          DECK_LIBRARY_LOCK,
          { mode: "exclusive", signal: controller.signal },
          () => this.selectDeckLibraryReconciliation(deckLibraryGeneration(receipt), controller.signal),
        );
      } catch (error) {
        if (controller.signal.aborted) return;
        throw error;
      } finally {
        this.reconciliationControllers.delete(controller);
      }
      if (this.deckLibraryBackgroundPaused) return;
      if (!selectedOperation) {
        const after = await this.database.get("packs", packId("deck_library"));
        if (after && deckLibraryGeneration(after) === deckLibraryGeneration(receipt)) {
          await this.publishObservedDeckLibraryRevision(revisionBefore, null);
        }
        return;
      }
      ownership.operationId = selectedOperation;
      const worker = this.workers.get(selectedOperation);
      if (worker && !worker.background) ownership.background = false;
      try {
        this.emit({ phase: "started", operation: await this.operationStatus(selectedOperation), error: null });
        if (this.deckLibraryBackgroundPaused) return;
        await this.runAndWait(selectedOperation, true);
      } catch (error) {
        const afterFailure = await this.database.get("packs", packId("deck_library"));
        if (!afterFailure || deckLibraryGeneration(afterFailure) !== deckLibraryGeneration(receipt)) return;
        throw error;
      }
      const after = await this.database.get("packs", packId("deck_library"));
      if (!after || deckLibraryGeneration(after) !== deckLibraryGeneration(receipt)) return;
      await this.publishObservedDeckLibraryRevision(revisionBefore, selectedOperation);
    } catch (error) {
      throw backendError(error);
    }
  }

  private async publishObservedDeckLibraryRevision(
    revisionBefore: string,
    selectedOperation: OperationId | null,
  ): Promise<void> {
    const current = await state(this.database);
    if (current.revision === revisionBefore || current.revision === this.lastPublishedRevision) return;
    this.publish({
      cause: "install",
      operationId: selectedOperation,
      catalogRoot: current.catalog?.root ?? null,
      revision: installedRevision(current.revision),
    });
  }

  private async selectDeckLibraryReconciliation(
    expectedGeneration: OperationId,
    signal: AbortSignal,
  ): Promise<OperationId | null> {
    const suspended = () => signal.aborted || this.deckLibraryBackgroundPaused;
    if (signal.aborted || this.deckLibraryBackgroundPaused) return null;
    const installed = await this.database.get("packs", packId("deck_library"));
    if (!installed || deckLibraryGeneration(installed) !== expectedGeneration) return null;

    // A completed receipt is the opt-in lease; only after proving it exists do
    // we invalidate/replan, so absent-pack calls cannot load Scryfall/card data.
    invalidateDeckLibraryPack();
    const membership = await planDeckLibraryPack(packId("deck_library"));
    if (signal.aborted || this.deckLibraryBackgroundPaused) return null;

    const transaction = this.database.transaction(["state", "packs", "operations"], "readwrite");
    bindTransactionAbort(transaction, signal);
    const receipt = await transaction.objectStore("packs").get(packId("deck_library"));
    if (suspended()) {
      abortTransaction(transaction);
      return null;
    }
    if (!receipt || deckLibraryGeneration(receipt) !== expectedGeneration) {
      await transaction.done;
      return null;
    }
    const operations = await transaction.objectStore("operations").getAll();
    transactionGate("selector-after-operations-read");
    if (suspended()) {
      abortTransaction(transaction);
      return null;
    }
    let superseded = false;
    let cancelled: OperationStatus | null = null;
    const active = operations.find((operation) =>
      deckLibraryOperation(operation)
      && operation.deckLibraryGeneration === expectedGeneration
      && operation.state !== "completed"
      && operation.state !== "cancelled");
    if (active) {
      const activeRoot = selectorRoot(active.selectors[active.packIds.indexOf(packId("deck_library"))]!, active.catalog);
      if (activeRoot === membership.membershipDigest && hasCurrentCandidateProjectionIntent(active)) {
        if (suspended()) {
          abortTransaction(transaction);
          return null;
        }
        // A durable retry record becomes automatic work only while it is idle.
        // Never relabel a live manual worker: its explicit Resume owns this
        // dispatch until it settles.
        if (!active.background && !this.workers.get(active.id)?.active) {
          await transaction.objectStore("operations").put({ ...active, background: true });
          if (suspended()) {
            abortTransaction(transaction);
            return null;
          }
        }
        if (suspended()) {
          abortTransaction(transaction);
          return null;
        }
        transactionGate("selector-before-commit");
        if (suspended()) {
          abortTransaction(transaction);
          return null;
        }
        await transaction.done;
        return active.id;
      }
      if (suspended()) {
        abortTransaction(transaction);
        return null;
      }
      await transaction.objectStore("operations").put({ ...active, state: "cancelled" });
      if (suspended()) {
        abortTransaction(transaction);
        return null;
      }
      superseded = true;
      cancelled = operationStatus({ ...active, state: "cancelled" });
    }
    if (
      receipt.root === membership.membershipDigest
      && receipt.candidateProjectionVersion === CARD_CANDIDATE_PROJECTION_VERSION
    ) {
      if (suspended()) {
        abortTransaction(transaction);
        return null;
      }
      transactionGate("selector-before-commit");
      if (suspended()) {
        abortTransaction(transaction);
        return null;
      }
      await transaction.done;
      if (cancelled) this.emit({ phase: "cancelled", operation: cancelled, error: null });
      if (superseded) {
        await this.collectLocalPackGarbage(
          packId("deck_library"),
          receipt.root,
          new Set(),
          await caches.open(CACHE),
        );
      }
      return null;
    }
    const current = (await transaction.objectStore("state").get(STATE)) ?? initialState();
    if (suspended()) {
      abortTransaction(transaction);
      return null;
    }
    if (!current.catalog) {
      await transaction.done;
      throw new VisualPackBackendError("unavailable");
    }
    const selectedOperation = operationToken();
    const operation = Object.freeze({
      id: selectedOperation,
      kind: "install",
      state: "downloading",
      catalog: current.catalog,
      selectors: [{ kind: "deck_library", membershipDigest: membership.membershipDigest }],
      packIds: [packId("deck_library")],
      packTotal: 1,
      packsPromoted: 0,
      objectTotal: 0,
      objectEstimate: membership.descriptors.length,
      objectsPromoted: 0,
      completedRevision: null,
      deckLibraryGeneration: expectedGeneration,
      background: true,
      candidateProjectionVersion: CARD_CANDIDATE_PROJECTION_VERSION,
    } satisfies ScryfallOperationRecord);
    if (suspended()) {
      abortTransaction(transaction);
      return null;
    }
    transactionGate("selector-before-commit");
    if (suspended()) {
      abortTransaction(transaction);
      return null;
    }
    await transaction.objectStore("operations").put(operation);
    // IndexedDB can yield while the write is queued. Do not leave a newly
    // selected automatic operation durable if suspension won that interval.
    if (suspended()) {
      abortTransaction(transaction);
      return null;
    }
    transactionGate("selector-before-commit");
    if (suspended()) {
      abortTransaction(transaction);
      return null;
    }
    await transaction.done;
    if (cancelled) this.emit({ phase: "cancelled", operation: cancelled, error: null });
    return selectedOperation;
  }

  async catalogStatus(): Promise<CatalogStatus> {
    try {
      const current = await state(this.database);
      return current.catalog ? { status: "ready", summary: await this.catalogSummary() } : { status: "empty" };
    } catch (error) {
      throw backendError(error);
    }
  }

  async refreshCatalog(): Promise<CatalogSummary> {
    try {
      const catalog = await loadScryfallBulkSource();
      const transaction = this.database.transaction("state", "readwrite");
      const current = (await transaction.store.get(STATE)) ?? initialState();
      await transaction.store.put({ ...current, catalog });
      await transaction.done;
      return this.catalogSummary();
    } catch (error) {
      throw backendError(error);
    }
  }

  async catalogSummary(): Promise<CatalogSummary> {
    const current = await state(this.database);
    if (!current.catalog) throw new VisualPackBackendError("unavailable");
    const installed = await this.database.getAll("packs");
    return {
      catalogRoot: current.catalog.root,
      epoch: 0,
      selectorCount: 6,
      shardCount: 1,
      installedRevision: installedRevision(current.revision),
      installedPacks: installed.map((entry) => ({ packId: entry.packId, catalogRoot: entry.root })),
      // `currentPersistence`, never `requestPersistence`: this is a read, and a
      // summary refresh must not ask the user for a storage grant. The grant is
      // requested exactly where a user has started an operation that writes.
      storage: await storageOutlook(await currentPersistence()),
    };
  }

  async estimateInstall(selector: InstallSelector, onProgress?: (progress: CatalogScanProgress) => void): Promise<InstallEstimate> {
    try {
      const current = await state(this.database);
      const catalog = current.catalog ?? await loadScryfallBulkSource();
      return this.estimate(catalog, selector, current.revision, onProgress);
    } catch (error) {
      throw backendError(error);
    }
  }

  async start(request: StartRequest): Promise<StartResponse> {
    try {
      const current = await state(this.database);
      const installed = await this.database.getAll("packs");
      let catalog: CatalogRecord;
      let selectors: InstallSelector[];
      let repairDescriptors: readonly ScryfallAssetDescriptor[] | undefined;
      let deckLibraryRepairNeedsWork = false;
      if (request.kind === "resume") {
        const operation = await this.database.get("operations", request.operationId);
        if (!operation) throw new VisualPackBackendError("invalid_input");
        if (operation.state === "completed") return { status: "healthy" };
        if (operation.state === "cancelled") throw new VisualPackBackendError("cancelled");
        if (await this.cancelStaleCandidateProjectionOperation(request.operationId)) {
          throw new VisualPackBackendError("cancelled");
        }
        const resumePersistence = await this.reserveStorage(request);
        // A user pressing Resume owns the dispatch from this point onward.
        // Persist that transfer before starting the worker so a concurrent
        // automatic pause cannot abort a manual retry.
        await this.updateOperation(request.operationId, (currentOperation) => ({
          ...currentOperation,
          background: false,
        }));
        for (const ownership of this.reconciliationTasks.values()) {
          if (ownership.operationId === request.operationId) ownership.background = false;
        }
        const worker = this.workers.get(request.operationId);
        if (worker?.active && !worker.controller.signal.aborted) {
          worker.background = false;
        } else {
          await worker?.settled;
          this.run(request.operationId);
        }
        return {
          status: "started",
          operationId: request.operationId,
          catalogRoot: operation.catalog.root,
          persistence: resumePersistence,
        };
      }
      if (request.kind === "repair") {
        if (!current.catalog) throw new VisualPackBackendError("unavailable");
        catalog = current.catalog;
        // A repair must target the root the pack was INSTALLED at, not the
        // current bulk root. For a curated pack the two differ by definition,
        // and repairing at the bulk root would write objects there and then
        // let completePack cursor-delete the entire installed membership.
        selectors = [];
        for (const selectedPack of request.packIds) {
          const receipt = installed.find((entry) => entry.packId === selectedPack);
          if (selectedPack === packId("deck_library") && receipt) {
            const rows = (await this.database.getAllFromIndex("objects", "by-pack", selectedPack))
              .filter((row) => row.root === receipt.root)
            repairDescriptors = rows.map(descriptorFromObject);
            const cache = await caches.open(CACHE);
            deckLibraryRepairNeedsWork = !await allCachedObjectsMatch(cache, rows);
          }
          selectors.push(selectorForPack(selectedPack, receipt?.root ?? catalog.root));
        }
      } else {
        catalog = current.catalog ?? await loadScryfallBulkSource();
        if (request.selector.kind === "complete" && request.selector.rootSha256 !== catalog.root) {
          throw new VisualPackBackendError("conflict");
        }
        // The curated counterpart of the guard above, and it belongs HERE for
        // the same reason: a conflict is not retryable, so it must reach the
        // caller as a rejected request rather than as a failed operation
        // record. This is the common case — estimate, change the art chain,
        // then install — and catching it before any record exists leaves
        // nothing behind to unwedge.
        if (request.selector.kind === "curated") await curatedDescriptors(request.selector.membershipDigest);
        if (request.selector.kind === "deck_library") {
          const membership = await planDeckLibraryPack(packId("deck_library"));
          if (membership.membershipDigest !== request.selector.membershipDigest) throw new VisualPackBackendError("conflict");
        }
        selectors = [request.selector];
      }
      // Filtered over the SELECTORS rather than their pack ids: a pack id
      // alone cannot say which root that selector installs at, and mapping
      // first discards the curated selector's digest.
      selectors = selectors.filter((selector) => {
        if (request.kind === "repair" && selector.kind === "deck_library") return deckLibraryRepairNeedsWork;
        return !installed.some((entry) =>
          entry.packId === selectorPack(selector)
          && entry.root === selectorRoot(selector, catalog)
          && (selector.kind !== "deck_library"
            || entry.candidateProjectionVersion === CARD_CANDIDATE_PROJECTION_VERSION));
      });
      if (selectors.length === 0) return { status: "healthy" };
      // After the short-circuit above, never before it: a sync that turns out
      // to have nothing to do must not ask for a persistence grant, and it
      // cannot run out of room for bytes it will not download.
      const persistence = await this.reserveStorage(request);
      const packIds = selectors.map(selectorPack);
      const selectedOperation = operationToken();
      const existingDeckLibrary = installed.find((entry) => entry.packId === packId("deck_library"));
      const operation: ScryfallOperationRecord = Object.freeze({
        id: selectedOperation,
        kind: request.kind === "repair" ? "repair" : "install",
        state: "downloading",
        catalog,
        selectors,
        packIds,
        packTotal: packIds.length,
        packsPromoted: 0,
        objectTotal: 0,
        objectEstimate: request.kind === "install" ? request.objectEstimate : undefined,
        objectsPromoted: 0,
        completedRevision: null,
        repairDescriptors,
        deckLibraryGeneration: packIds.includes(packId("deck_library"))
          ? existingDeckLibrary ? deckLibraryGeneration(existingDeckLibrary) : undefined
          : undefined,
        ...(request.kind === "install" && packIds.includes(packId("deck_library"))
          ? { candidateProjectionVersion: CARD_CANDIDATE_PROJECTION_VERSION }
          : {}),
      });
      const transaction = this.database.transaction(["state", "operations"], "readwrite");
      await transaction.objectStore("state").put({ ...current, catalog });
      await transaction.objectStore("operations").put(operation);
      await transaction.done;
      this.emit({ phase: "started", operation: operationStatus(operation), error: null });
      this.run(selectedOperation);
      return { status: "started", operationId: selectedOperation, catalogRoot: catalog.root, persistence };
    } catch (error) {
      throw backendError(error);
    }
  }

  async cancel(selectedOperation: OperationId): Promise<OperationStatus> {
    try {
      // Abort first: a finalizing transaction can be holding the operations
      // store while it waits at a gate, so waiting to persist cancellation
      // before signalling the worker would let it commit a stale receipt.
      const worker = this.workers.get(selectedOperation);
      worker?.controller.abort();
      const requested = await this.updateOperation(selectedOperation, (current) =>
        current.state === "completed" || current.state === "cancelled" ? current : { ...current, state: "cancel_requested" });
      // Let the worker put its downloads down before this record goes terminal.
      // `installObject` consults `signal` only inside `fetchImage` — the
      // donor-reuse and cache-hit paths never do — so a task already past the
      // guard finishes into `markComplete` whatever the abort says, writing an
      // `objects` row at `root` and incrementing `objectsPromoted`. Waiting
      // here puts those writes BEFORE the terminal state rather than after
      // `cancel()` has returned and the panel has published the outcome.
      await worker?.settled;
      if (requested.state === "cancel_requested") {
        await this.updateOperation(selectedOperation, (current) => ({ ...current, state: "cancelled" }));
      }
      const result = await this.operationStatus(selectedOperation);
      if (result.state === "cancelled") this.emit({ phase: "cancelled", operation: result, error: null });
      return result;
    } catch (error) {
      throw backendError(error);
    }
  }

  async operationStatus(selectedOperation: OperationId): Promise<OperationStatus> {
    const operation = await this.database.get("operations", selectedOperation);
    if (!operation) throw new VisualPackBackendError("invalid_input");
    return operationStatus(operation);
  }

  async remove(selector: RemovalSelector, _mode: RemovalMode): Promise<RemovalResponse> {
    try {
      const installed = await this.database.getAll("packs");
      const selected = selector.kind === "all_installed" ? new Set(installed.map((entry) => entry.packId))
        : selector.kind === "complete" ? new Set(installed.filter((entry) => entry.root === selector.rootSha256).map((entry) => entry.packId))
          : new Set(selector.packIds);
      const removed = installed.filter((entry) => selected.has(entry.packId));
      const paths = new Set<string>();
      const transaction = this.database.transaction(["state", "packs", "objects", "operations"], "readwrite");
      const deckLibrary = packId("deck_library");
      const deckLibrarySelected = selected.has(deckLibrary);
      const invalidatedDeckLibraryOperations: OperationId[] = [];
      if (deckLibrarySelected) {
        for (const operation of await transaction.objectStore("operations").getAll()) {
          if (operation.state === "completed" || operation.state === "cancelled") continue;
          for (const [position] of operation.selectors.entries()) {
            if (operation.packIds[position] === deckLibrary) {
              invalidatedDeckLibraryOperations.push(operation.id);
              await transaction.objectStore("operations").put({ ...operation, state: "cancelled" });
              break;
            }
          }
        }
      }
      for (const entry of removed) {
        await transaction.objectStore("packs").delete(entry.id);
        const index = transaction.objectStore("objects").index("by-pack");
        let cursor = await index.openCursor(entry.packId);
        while (cursor) {
          if (cursor.value.root === entry.root || entry.packId === deckLibrary) {
            paths.add(cursor.value.path);
            await cursor.delete();
          }
          cursor = await cursor.continue();
        }
      }
      if (deckLibrarySelected && !removed.some((entry) => entry.packId === deckLibrary)) {
        const index = transaction.objectStore("objects").index("by-pack");
        let cursor = await index.openCursor(deckLibrary);
        while (cursor) {
          paths.add(cursor.value.path);
          await cursor.delete();
          cursor = await cursor.continue();
        }
      }
      const current = (await transaction.objectStore("state").get(STATE)) ?? initialState();
      const revision = String(BigInt(current.revision) + 1n);
      await transaction.objectStore("state").put({ ...current, revision });
      await transaction.done;
      // The installed membership has committed. Publish it before cache cleanup,
      // which can be slow on WebKit, so every observer can refresh its receipt
      // state while the reference-aware sweep continues in the background.
      this.publish({ cause: "remove", operationId: null, catalogRoot: null, revision: installedRevision(revision) });
      if (deckLibrarySelected) {
        for (const controller of this.reconciliationControllers) controller.abort();
        for (const selectedOperation of invalidatedDeckLibraryOperations) {
          this.workers.get(selectedOperation)?.controller.abort();
        }
        await Promise.all([...new Set(invalidatedDeckLibraryOperations)].map(async (selectedOperation) =>
          this.workers.get(selectedOperation)?.settled));
      }
      await this.sweepUnreferenced(paths, await caches.open(CACHE));
      return { removed: removed.map((entry) => ({ packId: entry.packId, catalogRoot: entry.root })), revision: installedRevision(revision), cleanupIssues: [] };
    } catch (error) {
      throw backendError(error);
    }
  }

  async verify(mode: VerificationMode): Promise<VerificationResponse> {
    try {
      const current = await state(this.database);
      const objects = await this.database.getAll("objects");
      const cache = await caches.open(CACHE);
      const issues: VerificationResponse["issues"] = [];
      for (const object of objects) {
        const response = await cache.match(object.path);
        if (!response) {
          issues.push({ kind: "missing_object" });
          continue;
        }
        if (mode === "full") {
          const bytes = new Uint8Array(await response.arrayBuffer());
          if (bytes.byteLength !== object.byteLength || await sha256(bytes) !== object.object) issues.push({ kind: "corrupt_object" });
        }
      }
      return { revision: installedRevision(current.revision), issues };
    } catch (error) {
      throw backendError(error);
    }
  }

  async resolve(keys: ResolutionKey[]): Promise<ResolutionResponse> {
    try {
      const current = await state(this.database);
      const installed = await this.database.getAll("packs");
      const cache = await caches.open(CACHE);
      const entries: ResolutionResponse["entries"] = [];
      for (const [ordinal, key] of keys.entries()) {
        const matches: ResolutionResponse["entries"][number]["matches"] = [];
        const candidateObjects = key.kind === "candidate"
          ? await this.database.getAllFromIndex("objects", "by-candidate-key", key.key)
          : null;
        for (const selectedPack of installed) {
          const objects = candidateObjects?.filter((object) => object.packId === selectedPack.packId)
            ?? await this.database.getAllFromIndex("objects", "by-pack", selectedPack.packId);
          for (const object of objects) {
            if (object.root !== selectedPack.root || !sameResolutionKey(object, key) || !await cache.match(object.path)) continue;
            matches.push({ packId: object.packId, assetKey: object.assetKey, catalogRoot: object.root, url: object.path, media: object.media });
          }
        }
        entries.push({ ordinal, key, matches });
      }
      return { revision: installedRevision(current.revision), entries };
    } catch (error) {
      throw backendError(error);
    }
  }

  async subscribeProgress(listener: (event: ProgressEvent) => void): Promise<() => void> {
    const liveOperations = new Set<OperationId>();
    const forwarded = (event: ProgressEvent) => {
      liveOperations.add(event.operation.operationId);
      listener(event);
    };
    this.progressListeners.add(forwarded);
    try {
      const operations = await this.database.getAll("operations");
      for (const operation of operations) {
        if (
          liveOperations.has(operation.id)
          || (operation.state !== "downloading" && operation.state !== "cancel_requested" && operation.state !== "finalizing")
        ) continue;
        const failure = this.workerFailures.get(operation.id);
        listener({ phase: failure ? "failed" : "running", operation: operationStatus(operation), error: failure?.kind ?? null });
      }
    } catch (error) {
      this.progressListeners.delete(forwarded);
      throw error;
    }
    this.progressListeners.delete(forwarded);
    this.progressListeners.add(listener);
    return () => this.progressListeners.delete(listener);
  }

  async subscribeRevision(listener: (event: RevisionEvent) => void): Promise<() => void> {
    this.revisionListeners.add(listener);
    return () => this.revisionListeners.delete(listener);
  }
}
