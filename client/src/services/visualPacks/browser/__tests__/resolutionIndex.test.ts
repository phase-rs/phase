import "fake-indexeddb/auto";

import { IDBFactory } from "fake-indexeddb";
import { openDB } from "idb";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { cardBackCandidate, manaSymbolCandidate } from "../../candidateKeys.ts";
import { assetKey, catalogRoot, operationId, packId } from "../../types.ts";
import { ScryfallBrowserVisualPackBackend } from "../scryfallBackend.ts";

const DATABASE = "phase-visual-packs-scryfall-v1";
const ROOT = catalogRoot("a".repeat(64));
const PACK = packId("deck_library");
const OPERATION = operationId("b".repeat(32));

class MemoryCache {
  readonly entries = new Map<string, Uint8Array>();
  readonly matchPaths: string[] = [];

  async put(request: string, response: Response): Promise<void> {
    this.entries.set(request, new Uint8Array(await response.arrayBuffer()));
  }

  async match(request: string): Promise<Response | undefined> {
    this.matchPaths.push(request);
    const bytes = this.entries.get(request);
    return bytes ? new Response(bytes, { headers: { "Content-Type": "image/jpeg" } }) : undefined;
  }

  async delete(request: string): Promise<boolean> {
    return this.entries.delete(request);
  }
}

let cache = new MemoryCache();

function objectRow(asset: string, candidateKeys: readonly string[], path: string) {
  return {
    id: `${ROOT}:${PACK}:${asset}`,
    root: ROOT,
    packId: PACK,
    assetKey: assetKey(asset),
    candidateKeys,
    object: ROOT,
    byteLength: 3,
    media: "image/jpeg" as const,
    path,
  };
}

async function openLegacyDatabase() {
  return openDB(DATABASE, 1, {
    upgrade(database) {
      database.createObjectStore("state", { keyPath: "id" });
      database.createObjectStore("packs", { keyPath: "id" }).createIndex("by-root", "root");
      database.createObjectStore("objects", { keyPath: "id" }).createIndex("by-pack", "packId");
      database.createObjectStore("operations", { keyPath: "id" });
      database.createObjectStore("operationObjects", { keyPath: "id" }).createIndex("by-operation", "operationId");
    },
  });
}

function installReceipt() {
  return { id: PACK, packId: PACK, root: ROOT, dependencies: [], operationId: OPERATION };
}

async function currentDatabaseVersion(): Promise<number> {
  const database = await openDB(DATABASE);
  try {
    return database.version;
  } finally {
    database.close();
  }
}

function upgradeToVersionThree(): Promise<void> {
  let resolveResult!: () => void;
  let rejectResult!: (reason?: unknown) => void;
  let settled = false;
  const result = new Promise<void>((resolve, reject) => {
    resolveResult = resolve;
    rejectResult = reject;
  });
  const opening = openDB(DATABASE, 3, {
    blocked() {
      if (settled) return;
      settled = true;
      rejectResult(new Error("v2 connection blocked a future upgrade"));
    },
  });
  void opening.then(
    (database) => {
      database.close();
      if (settled) return;
      settled = true;
      resolveResult();
    },
    (error: unknown) => {
      if (settled) return;
      settled = true;
      rejectResult(error);
    },
  );
  return result;
}

describe("Scryfall browser resolution index", () => {
  beforeEach(() => {
    globalThis.indexedDB = new IDBFactory();
    cache = new MemoryCache();
    vi.stubGlobal("caches", { open: async () => cache } as unknown as CacheStorage);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("migrates v1 rows into the candidate-key index without replacing cached bytes", async () => {
    const key = cardBackCandidate();
    const path = "/visual-packs/v1/v1-card-back.jpg";
    const legacy = await openLegacyDatabase();
    await legacy.put("packs", installReceipt());
    await legacy.put("objects", objectRow("asset:v1:card_back:default", [key], path));
    legacy.close();
    cache.entries.set(path, new TextEncoder().encode("old"));

    const backend = await ScryfallBrowserVisualPackBackend.create();
    expect(new TextDecoder().decode(cache.entries.get(path)!)).toBe("old");
    const migrated = await openDB(DATABASE);
    expect(migrated.version).toBe(2);
    expect(Array.from(migrated.transaction("objects").store.indexNames)).toContain("by-candidate-key");
    expect(await migrated.getAllFromIndex("objects", "by-candidate-key", key)).toHaveLength(1);
    migrated.close();

    const response = await backend.resolve([{ kind: "candidate", key }]);

    expect(response.entries[0].matches.map((match) => match.url)).toEqual([path]);
    expect(cache.matchPaths).toEqual([path]);
  });

  it("queries only indexed candidate matches before checking the cache", async () => {
    const key = cardBackCandidate();
    const decoyKey = manaSymbolCandidate("u");
    const matchingPath = "/visual-packs/v1/matching.jpg";
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const database = await openDB(DATABASE);
    const transaction = database.transaction(["objects", "packs"], "readwrite");
    await transaction.objectStore("packs").put(installReceipt());
    for (let index = 0; index < 2_000; index += 1) {
      await transaction.objectStore("objects").put(objectRow(
        `asset:v1:canonical_card:decoy-${index}`,
        [decoyKey],
        `/visual-packs/v1/decoy-${index}.jpg`,
      ));
    }
    await transaction.objectStore("objects").put(objectRow("asset:v1:card_back:default", [key], matchingPath));
    await transaction.done;
    database.close();
    cache.entries.set(matchingPath, new TextEncoder().encode("hit"));

    const backendDatabase = (backend as unknown as {
      database: { getAllFromIndex: (store: string, index: string, query: string) => Promise<unknown[]> };
    }).database;
    const indexedLookups: [string, string, string][] = [];
    const getAllFromIndex = backendDatabase.getAllFromIndex.bind(backendDatabase);
    backendDatabase.getAllFromIndex = async (store, index, query) => {
      indexedLookups.push([store, index, query]);
      return getAllFromIndex(store, index, query);
    };
    const response = await backend.resolve([{ kind: "candidate", key }]);

    expect(indexedLookups).toEqual([["objects", "by-candidate-key", key]]);
    expect(response.entries[0].matches.map((match) => match.url)).toEqual([matchingPath]);
    expect(cache.matchPaths).toEqual([matchingPath]);
  });

  it("shares a blocked v1 upgrade outcome, then closes delayed and active v2 connections", async () => {
    const legacy = await openLegacyDatabase();
    const first = ScryfallBrowserVisualPackBackend.create();

    await expect(first).rejects.toMatchObject({ kind: "unavailable" });
    const second = ScryfallBrowserVisualPackBackend.create();
    await expect(second).rejects.toMatchObject({ kind: "unavailable" });
    legacy.close();

    await vi.waitFor(async () => {
      expect(await currentDatabaseVersion()).toBe(2);
    });
    await ScryfallBrowserVisualPackBackend.create();

    await expect(upgradeToVersionThree()).resolves.toBeUndefined();
  });
});
