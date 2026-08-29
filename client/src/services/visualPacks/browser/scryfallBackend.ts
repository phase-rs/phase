import { openDB, type DBSchema, type IDBPDatabase } from "idb";

import { VisualPackBackendError, type VisualPackBackend } from "../backend.ts";
import {
  installedRevision,
  operationId,
  packId,
  type AssetKey,
  type CandidateKey,
  type CatalogRoot,
  type CatalogStatus,
  type CatalogSummary,
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
  type VerificationMode,
  type VerificationResponse,
  type VisualPackErrorKind,
  type VisualPackMedia,
} from "../types.ts";
import { syntheticCachePath } from "./records.ts";
import {
  forEachScryfallAsset,
  loadScryfallBulkSource,
  ScryfallBulkError,
  type ScryfallAssetDescriptor,
  type ScryfallBulkSource,
} from "./scryfallBulk.ts";

const DATABASE = "phase-visual-packs-scryfall-v1";
const CACHE = "phase-visual-pack-scryfall-images-v1";
const STATE = "state";

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
}>;

type ObjectRecord = Readonly<{
  id: string;
  root: CatalogRoot;
  packId: PackId;
  assetKey: AssetKey;
  candidateKeys: readonly CandidateKey[];
  object: CatalogRoot;
  byteLength: number;
  media: VisualPackMedia;
  path: string;
}>;

type OperationObjectRecord = Readonly<{
  id: string;
  operationId: OperationId;
  objectId: string;
  complete: boolean;
}>;

type ScryfallOperationRecord = Readonly<{
  id: OperationId;
  kind: "install";
  state: "downloading" | "cancel_requested" | "finalizing" | "completed" | "cancelled";
  catalog: CatalogRecord;
  selectors: readonly InstallSelector[];
  packIds: readonly PackId[];
  packTotal: number;
  packsPromoted: number;
  objectTotal: number;
  objectsPromoted: number;
  completedRevision: string | null;
}>;

interface ScryfallVisualPackSchema extends DBSchema {
  state: { key: string; value: StateRecord };
  packs: { key: string; value: PackRecord; indexes: { "by-root": CatalogRoot } };
  objects: { key: string; value: ObjectRecord; indexes: { "by-pack": string } };
  operations: { key: OperationId; value: ScryfallOperationRecord };
  operationObjects: { key: string; value: OperationObjectRecord; indexes: { "by-operation": OperationId } };
}

function objectId(root: CatalogRoot, selectedPack: PackId, selectedAsset: AssetKey): string {
  return `${root}:${selectedPack}:${selectedAsset}`;
}

function operationObjectId(selectedOperation: OperationId, selectedObject: string): string {
  return `${selectedOperation}:${selectedObject}`;
}

function operationStatus(record: ScryfallOperationRecord): OperationStatus {
  return {
    operationId: record.id,
    catalogRoot: record.catalog.root,
    kind: record.kind,
    state: record.state,
    packTotal: record.packTotal,
    packsPromoted: record.packsPromoted,
    objectTotal: record.objectTotal,
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
  }
}

function selectorForPack(selectedPack: PackId, root: CatalogRoot): InstallSelector {
  if (selectedPack === packId("core")) return { kind: "core" };
  if (selectedPack === packId("complete")) return { kind: "complete", rootSha256: root };
  const printing = /^printing:([a-z0-9]{3,6})$/.exec(selectedPack);
  if (printing) return { kind: "printing", set: printing[1] };
  const locale = /^locale:(de|es|fr|it|pt):([a-z0-9]{3,6})$/.exec(selectedPack);
  if (locale) return { kind: "locale", language: locale[1], set: locale[2] };
  throw new VisualPackBackendError("invalid_input");
}

function sameResolutionKey(record: ObjectRecord, key: ResolutionKey): boolean {
  return key.kind === "asset" ? record.assetKey === key.key : record.candidateKeys.includes(key.key);
}

async function openDatabase(): Promise<IDBPDatabase<ScryfallVisualPackSchema>> {
  return openDB<ScryfallVisualPackSchema>(DATABASE, 1, {
    upgrade(database) {
      database.createObjectStore("state", { keyPath: "id" });
      database.createObjectStore("packs", { keyPath: "id" }).createIndex("by-root", "root");
      database.createObjectStore("objects", { keyPath: "id" }).createIndex("by-pack", "packId");
      database.createObjectStore("operations", { keyPath: "id" });
      database.createObjectStore("operationObjects", { keyPath: "id" }).createIndex("by-operation", "operationId");
    },
  });
}

async function state(database: IDBPDatabase<ScryfallVisualPackSchema>): Promise<StateRecord> {
  return (await database.get("state", STATE)) ?? initialState();
}

async function cacheContains(path: string): Promise<boolean> {
  return (await (await caches.open(CACHE)).match(path)) !== undefined;
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

async function storeVerifiedObject(path: string, media: VisualPackMedia, bytes: Uint8Array): Promise<void> {
  const cache = await caches.open(CACHE);
  await cache.put(path, new Response(bytes, {
    headers: {
      "Content-Type": media,
      "Content-Length": String(bytes.byteLength),
      "Cache-Control": "public, max-age=31536000, immutable",
    },
  }));
}

export class ScryfallBrowserVisualPackBackend implements VisualPackBackend {
  private readonly progressListeners = new Set<(event: ProgressEvent) => void>();
  private readonly revisionListeners = new Set<(event: RevisionEvent) => void>();
  private readonly workers = new Map<OperationId, AbortController>();
  private work = Promise.resolve();

  private constructor(private readonly database: IDBPDatabase<ScryfallVisualPackSchema>) {}

  static async create(): Promise<ScryfallBrowserVisualPackBackend> {
    const backend = new ScryfallBrowserVisualPackBackend(await openDatabase());
    const pending = await backend.database.getAll("operations");
    for (const operation of pending) {
      if (operation.state === "downloading" || operation.state === "finalizing") backend.run(operation.id);
    }
    return backend;
  }

  private emit(event: ProgressEvent): void {
    for (const listener of this.progressListeners) {
      try { listener(event); } catch { /* UI listeners never affect installation. */ }
    }
  }

  private publish(event: RevisionEvent): void {
    for (const listener of this.revisionListeners) {
      try { listener(event); } catch { /* UI listeners never affect installation. */ }
    }
  }

  private run(selectedOperation: OperationId): void {
    if (this.workers.has(selectedOperation)) return;
    const controller = new AbortController();
    this.workers.set(selectedOperation, controller);
    const task = this.work.then(async () => this.resume(selectedOperation, controller.signal));
    this.work = task.catch(() => undefined);
    void task.catch(async (error) => {
      if (controller.signal.aborted) return;
      const operation = await this.database.get("operations", selectedOperation);
      if (operation) this.emit({ phase: "failed", operation: operationStatus(operation), error: errorKind(error) });
    }).finally(() => this.workers.delete(selectedOperation));
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

  private async markSeen(selectedOperation: OperationId, selectedObject: string): Promise<OperationObjectRecord> {
    const transaction = this.database.transaction(["operations", "operationObjects"], "readwrite");
    const operation = await transaction.objectStore("operations").get(selectedOperation);
    if (!operation) throw new VisualPackBackendError("invalid_input");
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
    const transaction = this.database.transaction(["operations", "operationObjects", "objects"], "readwrite");
    const operation = await transaction.objectStore("operations").get(selectedOperation);
    const completion = await transaction.objectStore("operationObjects").get(operationObjectId(selectedOperation, selectedObject));
    if (!operation || !completion) throw new VisualPackBackendError("storage");
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

  private async installObject(operation: ScryfallOperationRecord, descriptor: ScryfallAssetDescriptor, signal: AbortSignal): Promise<void> {
    const id = objectId(operation.catalog.root, descriptor.packId, descriptor.assetKey);
    const completion = await this.markSeen(operation.id, id);
    const existing = await this.database.get("objects", id);
    if (completion.complete && existing && await cacheContains(existing.path)) return;
    if (completion.complete) await this.invalidateCompletion(operation.id, id);
    if (existing && await cacheContains(existing.path)) {
      await this.markComplete(operation.id, id, existing);
      return;
    }
    const bytes = await fetchImage(descriptor, signal);
    const object = await sha256(bytes);
    const metadata: ObjectRecord = Object.freeze({
      id,
      root: operation.catalog.root,
      packId: descriptor.packId,
      assetKey: descriptor.assetKey,
      candidateKeys: [...descriptor.candidateKeys],
      object,
      byteLength: bytes.byteLength,
      media: descriptor.media,
      path: syntheticCachePath(object, descriptor.media),
    });
    await storeVerifiedObject(metadata.path, metadata.media, bytes);
    await this.markComplete(operation.id, id, metadata);
  }

  private async completePack(operation: ScryfallOperationRecord, selectedPack: PackId): Promise<void> {
    const transaction = this.database.transaction(["packs", "objects", "operations"], "readwrite");
    const current = await transaction.objectStore("operations").get(operation.id);
    if (!current || current.state !== "downloading") {
      await transaction.done;
      return;
    }
    const existing = await transaction.objectStore("packs").get(selectedPack);
    if (existing?.operationId === operation.id && existing.root === operation.catalog.root) {
      await transaction.done;
      return;
    }
    if (existing) {
      const index = transaction.objectStore("objects").index("by-pack");
      let cursor = await index.openCursor(existing.packId);
      while (cursor) {
        if (cursor.value.root === existing.root) await cursor.delete();
        cursor = await cursor.continue();
      }
    }
    await transaction.objectStore("packs").put({
      id: selectedPack,
      packId: selectedPack,
      root: operation.catalog.root,
      dependencies: [],
      operationId: operation.id,
    });
    await transaction.objectStore("operations").put({ ...current, packsPromoted: current.packsPromoted + 1 });
    await transaction.done;
  }

  private async finish(selectedOperation: OperationId): Promise<void> {
    const finishing = await this.updateOperation(selectedOperation, (current) =>
      current.state === "downloading" ? { ...current, state: "finalizing" } : current,
    );
    if (finishing.state !== "finalizing") return;
    const transaction = this.database.transaction(["state", "operations"], "readwrite");
    const currentOperation = await transaction.objectStore("operations").get(selectedOperation);
    if (!currentOperation || currentOperation.state !== "finalizing") {
      await transaction.done;
      return;
    }
    const currentState = (await transaction.objectStore("state").get(STATE)) ?? initialState();
    const revision = String(BigInt(currentState.revision) + 1n);
    await transaction.objectStore("state").put({ ...currentState, catalog: finishing.catalog, revision });
    await transaction.objectStore("operations").put({ ...currentOperation, state: "completed", completedRevision: revision });
    await transaction.done;
    const completed = await this.operationStatus(selectedOperation);
    this.emit({ phase: "completed", operation: completed, error: null });
    this.publish({ cause: "install", operationId: selectedOperation, catalogRoot: finishing.catalog.root, revision: installedRevision(revision) });
  }

  private async resume(selectedOperation: OperationId, signal: AbortSignal): Promise<void> {
    let operation = await this.database.get("operations", selectedOperation);
    if (!operation || operation.state === "completed" || operation.state === "cancelled") return;
    if (operation.state === "cancel_requested" || signal.aborted) {
      await this.updateOperation(selectedOperation, (current) => ({ ...current, state: "cancelled" }));
      return;
    }
    for (const [index, selector] of operation.selectors.entries()) {
      operation = await this.database.get("operations", selectedOperation);
      if (!operation || operation.state === "cancel_requested" || signal.aborted) break;
      const selectedPack = operation.packIds[index];
      const installed = await this.database.get("packs", selectedPack);
      if (installed?.operationId === selectedOperation && installed.root === operation.catalog.root) continue;
      await forEachScryfallAsset(operation.catalog, selector, signal, async (descriptor) => {
        const current = await this.database.get("operations", selectedOperation);
        if (!current || current.state === "cancel_requested" || signal.aborted) return;
        await this.installObject(current, descriptor, signal);
        const updated = await this.operationStatus(selectedOperation);
        this.emit({ phase: "running", operation: updated, error: null });
      });
      operation = await this.database.get("operations", selectedOperation);
      if (!operation || operation.state === "cancel_requested" || signal.aborted) break;
      await this.completePack(operation, selectedPack);
      this.emit({ phase: "running", operation: await this.operationStatus(selectedOperation), error: null });
    }
    operation = await this.database.get("operations", selectedOperation);
    if (!operation) return;
    if (operation.state === "cancel_requested" || signal.aborted) {
      const cancelled = await this.updateOperation(selectedOperation, (current) => ({ ...current, state: "cancelled" }));
      this.emit({ phase: "cancelled", operation: operationStatus(cancelled), error: null });
      return;
    }
    await this.finish(selectedOperation);
  }

  private async estimate(source: CatalogRecord, selector: InstallSelector, revision: string): Promise<InstallEstimate> {
    if (selector.kind === "complete" && selector.rootSha256 !== source.root) throw new VisualPackBackendError("conflict");
    let assetRecords = 0;
    await forEachScryfallAsset(source, selector, new AbortController().signal, async () => { assetRecords += 1; });
    return {
      catalogRoot: source.root,
      installedRevision: installedRevision(revision),
      selector: selectorPack(selector),
      packIds: [selectorPack(selector)],
      assetRecords: String(assetRecords),
      uniqueObjects: String(assetRecords),
      logicalImageBytes: "unknown",
      uniqueImageBytes: "unknown",
      shardCount: "1",
      shardBytes: String(source.compressedBytes),
    };
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
      selectorCount: 4,
      shardCount: 1,
      installedRevision: installedRevision(current.revision),
      installedPacks: installed.map((entry) => ({ packId: entry.packId, catalogRoot: entry.root })),
    };
  }

  async estimateInstall(selector: InstallSelector): Promise<InstallEstimate> {
    try {
      const current = await state(this.database);
      const catalog = current.catalog ?? await loadScryfallBulkSource();
      return this.estimate(catalog, selector, current.revision);
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
      if (request.kind === "resume") {
        const operation = await this.database.get("operations", request.operationId);
        if (!operation) throw new VisualPackBackendError("invalid_input");
        if (operation.state === "completed") return { status: "healthy" };
        if (operation.state === "cancelled") throw new VisualPackBackendError("cancelled");
        this.run(request.operationId);
        return { status: "started", operationId: request.operationId, catalogRoot: operation.catalog.root };
      }
      if (request.kind === "repair") {
        if (!current.catalog) throw new VisualPackBackendError("unavailable");
        catalog = current.catalog;
        selectors = request.packIds.map((selectedPack) => selectorForPack(selectedPack, catalog.root));
      } else {
        catalog = current.catalog ?? await loadScryfallBulkSource();
        if (request.selector.kind === "complete" && request.selector.rootSha256 !== catalog.root) {
          throw new VisualPackBackendError("conflict");
        }
        selectors = [request.selector];
      }
      const packIds = selectors.map(selectorPack).filter((selectedPack) =>
        !installed.some((entry) => entry.packId === selectedPack && entry.root === catalog.root));
      if (packIds.length === 0) return { status: "healthy" };
      selectors = selectors.filter((selector) => packIds.includes(selectorPack(selector)));
      const selectedOperation = operationToken();
      const operation: ScryfallOperationRecord = Object.freeze({
        id: selectedOperation,
        kind: "install",
        state: "downloading",
        catalog,
        selectors,
        packIds,
        packTotal: packIds.length,
        packsPromoted: 0,
        objectTotal: 0,
        objectsPromoted: 0,
        completedRevision: null,
      });
      const transaction = this.database.transaction(["state", "operations"], "readwrite");
      await transaction.objectStore("state").put({ ...current, catalog });
      await transaction.objectStore("operations").put(operation);
      await transaction.done;
      this.emit({ phase: "started", operation: operationStatus(operation), error: null });
      this.run(selectedOperation);
      return { status: "started", operationId: selectedOperation, catalogRoot: catalog.root };
    } catch (error) {
      throw backendError(error);
    }
  }

  async cancel(selectedOperation: OperationId): Promise<OperationStatus> {
    try {
      const requested = await this.updateOperation(selectedOperation, (current) =>
        current.state === "completed" || current.state === "cancelled" ? current : { ...current, state: "cancel_requested" });
      this.workers.get(selectedOperation)?.abort();
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
      const transaction = this.database.transaction(["state", "packs", "objects"], "readwrite");
      for (const entry of removed) {
        await transaction.objectStore("packs").delete(entry.id);
        const index = transaction.objectStore("objects").index("by-pack");
        let cursor = await index.openCursor(entry.packId);
        while (cursor) {
          if (cursor.value.root === entry.root) {
            paths.add(cursor.value.path);
            await cursor.delete();
          }
          cursor = await cursor.continue();
        }
      }
      const current = (await transaction.objectStore("state").get(STATE)) ?? initialState();
      const revision = String(BigInt(current.revision) + 1n);
      await transaction.objectStore("state").put({ ...current, revision });
      await transaction.done;
      const cache = await caches.open(CACHE);
      const remaining = await this.database.getAll("objects");
      for (const path of paths) {
        if (!remaining.some((entry) => entry.path === path)) await cache.delete(path);
      }
      this.publish({ cause: "remove", operationId: null, catalogRoot: null, revision: installedRevision(revision) });
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
        for (const selectedPack of installed) {
          const objects = await this.database.getAllFromIndex("objects", "by-pack", selectedPack.packId);
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
    this.progressListeners.add(listener);
    return () => this.progressListeners.delete(listener);
  }

  async subscribeRevision(listener: (event: RevisionEvent) => void): Promise<() => void> {
    this.revisionListeners.add(listener);
    return () => this.revisionListeners.delete(listener);
  }
}
