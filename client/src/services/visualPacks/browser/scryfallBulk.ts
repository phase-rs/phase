import { Gunzip } from "fflate";

import { VisualPackBackendError } from "../backend.ts";
import { cardBackCandidate, cardCandidateGroups, manaSymbolCandidate, setIconCandidate } from "../candidateKeys.ts";
import { curatedDescriptors } from "../curatedPack.ts";
import { planDeckLibraryPack } from "../deckLibraryPack.ts";
import { assetKey, catalogRoot, packId, type CatalogRoot, type CatalogScanProgress, type InstallSelector, type PackId } from "../types.ts";
import { descriptor, englishDescriptors } from "./descriptors.ts";
import type { CardIdentity, ScryfallAssetDescriptor } from "./descriptors.ts";
import { CARD_BACK_URL, MANA_SYMBOL_SHARDS, manaSymbolCode, manaSymbolSourceUrl } from "../../scryfall.ts";

// `ScryfallAssetDescriptor` moved to `descriptors.ts` alongside the builders
// that produce it; re-exported so existing importers of this module are
// unaffected by the extraction.
export type { ScryfallAssetDescriptor };

const BULK_INDEX_URL = "https://api.scryfall.com/bulk-data";
const GZIP_INPUT_CHUNK_BYTES = 1024 * 1024;
const JSONL_INPUT_CHUNK_BYTES = 1024 * 1024;

export class ScryfallBulkError extends Error {
  constructor(readonly kind: "network" | "storage" | "unsupported", detail?: string) {
    super(detail ? `Scryfall bulk source failed: ${kind}: ${detail}` : `Scryfall bulk source failed: ${kind}`);
    this.name = "ScryfallBulkError";
  }
}

export interface ScryfallBulkSource {
  readonly root: CatalogRoot;
  readonly downloadUrl: string;
  readonly updatedAt: string;
  readonly compressedBytes: number;
}

interface BulkRecord {
  readonly type?: unknown;
  readonly updated_at?: unknown;
  readonly compressed_size?: unknown;
  readonly jsonl_download_uri?: unknown;
}

interface ScryfallFace {
  readonly name?: unknown;
  readonly image_uris?: unknown;
}

interface ScryfallCard {
  readonly id?: unknown;
  readonly oracle_id?: unknown;
  readonly set?: unknown;
  readonly lang?: unknown;
  readonly name?: unknown;
  readonly collector_number?: unknown;
  readonly image_uris?: unknown;
  readonly card_faces?: unknown;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function errorDetail(error: unknown): string | undefined {
  if (error instanceof Error) return `${error.name}: ${error.message}`;
  if (typeof error === "string" && error) return error;
  return undefined;
}

function completeUtf8Length(bytes: Uint8Array): number {
  let start = bytes.byteLength - 1;
  while (start >= 0 && (bytes[start] & 0xc0) === 0x80) start -= 1;
  if (start < 0) return 0;
  const first = bytes[start];
  const expectedLength = first < 0x80 ? 1 : first < 0xe0 ? 2 : first < 0xf0 ? 3 : first < 0xf8 ? 4 : 1;
  return bytes.byteLength - start < expectedLength ? start : bytes.byteLength;
}

function sourceError(error: unknown): ScryfallBulkError {
  if (error instanceof ScryfallBulkError) return error;
  if (error instanceof DOMException && error.name === "QuotaExceededError") return new ScryfallBulkError("storage");
  return new ScryfallBulkError("network", errorDetail(error));
}

async function sha256(value: string): Promise<CatalogRoot> {
  const bytes = new TextEncoder().encode(value);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return catalogRoot(Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join(""));
}

function imageUris(value: unknown): Record<string, string> {
  const record = asRecord(value);
  if (!record) return {};
  return Object.fromEntries(Object.entries(record).filter((entry): entry is [string, string] => typeof entry[1] === "string"));
}

function cardIdentity(value: unknown): CardIdentity | null {
  const card = value as ScryfallCard;
  if (typeof card.id !== "string" || typeof card.oracle_id !== "string" || typeof card.set !== "string"
    || typeof card.collector_number !== "string" || typeof card.name !== "string") return null;
  const faces = Array.isArray(card.card_faces)
    ? card.card_faces.map((face) => {
        const entry = face as ScryfallFace;
        return typeof entry.name === "string" ? { name: entry.name, images: imageUris(entry.image_uris) } : null;
      }).filter((face): face is { name: string; images: Record<string, string> } => face !== null)
    : [{ name: card.name, images: imageUris(card.image_uris) }];
  return faces.length === 0 ? null : { id: card.id, oracleId: card.oracle_id, set: card.set, collector: card.collector_number, name: card.name, faces };
}

function selected(selector: InstallSelector, card: ScryfallCard): boolean {
  if (selector.kind === "complete") return card.lang === "en";
  return selector.kind === "printing" && card.lang === "en" && card.set === selector.set;
}

function identityKey(card: CardIdentity): string {
  return `${card.oracleId}:${card.set}:${card.collector}`;
}

function localizedDescriptors(selectedPack: PackId, english: CardIdentity, localized: CardIdentity, language: string): ScryfallAssetDescriptor[] {
  return localized.faces.flatMap((face, faceIndex) => {
    const englishFace = english.faces[faceIndex];
    if (!englishFace) return [];
    const result: ScryfallAssetDescriptor[] = [];
    const add = (variant: "full_card" | "art_crop", rung: "small" | "normal" | "art_crop", url: string | undefined) => {
      if (!url) return;
      const candidates = cardCandidateGroups({
        language,
        englishPrintingId: english.id,
        localizedAliases: [english.name, englishFace.name],
        faceIndex,
        variant,
        rung,
      }).flatMap((group) => group.keys);
      result.push(descriptor(selectedPack, localized, faceIndex, variant, rung, url, candidates));
    };
    add("full_card", "small", face.images.small);
    add("full_card", "normal", face.images.normal);
    add("art_crop", "art_crop", face.images.art_crop);
    return result;
  });
}

function imageCount(card: CardIdentity, faceLimit = card.faces.length): number {
  let count = 0;
  for (const [index, face] of card.faces.entries()) {
    if (index >= faceLimit) break;
    if (face.images.small) count += 1;
    if (face.images.normal) count += 1;
    if (face.images.art_crop) count += 1;
  }
  return count;
}

/** Every finite mana-shard SVG (`{W}`, `{2/U}`, `{∞}`, …) — set- and
 *  deck-independent, so it belongs in `core` alongside the card back rather
 *  than any per-printing pack. */
function manaSymbolDescriptors(): ScryfallAssetDescriptor[] {
  return MANA_SYMBOL_SHARDS.map((shard) => ({
    packId: packId("core"),
    assetKey: assetKey(`asset:v1:mana_symbol:${manaSymbolCode(shard)}`),
    candidateKeys: [manaSymbolCandidate(shard)],
    sourceUrl: manaSymbolSourceUrl(shard),
    media: "image/svg+xml",
  }));
}

function coreDescriptors(): ScryfallAssetDescriptor[] {
  return [
    {
      packId: packId("core"),
      assetKey: assetKey("asset:v1:card_back:default"),
      candidateKeys: [cardBackCandidate()],
      sourceUrl: CARD_BACK_URL,
      media: "image/jpeg",
    },
    ...manaSymbolDescriptors(),
  ];
}

function setIconDescriptor(selectedPack: PackId, set: string): ScryfallAssetDescriptor {
  return {
    packId: selectedPack,
    assetKey: assetKey(`asset:v1:set_icon:${set}`),
    candidateKeys: [setIconCandidate(set)],
    sourceUrl: `https://svgs.scryfall.io/sets/${encodeURIComponent(set)}.svg`,
    media: "image/svg+xml",
  };
}

export async function loadScryfallBulkSource(fetcher: typeof fetch = globalThis.fetch): Promise<ScryfallBulkSource> {
  try {
    const response = await fetcher(BULK_INDEX_URL, {
      headers: { Accept: "application/json;q=0.9,*/*;q=0.8" },
      credentials: "omit",
      cache: "no-store",
    });
    if (!response.ok || response.type === "opaque") throw new ScryfallBulkError("network");
    const payload = await response.json() as { data?: unknown };
    if (!Array.isArray(payload.data)) throw new ScryfallBulkError("network");
    const record = payload.data.find((value): value is BulkRecord => asRecord(value)?.type === "all_cards");
    const compressedBytes = typeof record?.compressed_size === "number" && Number.isSafeInteger(record.compressed_size)
      ? record.compressed_size
      : null;
    if (!record || typeof record.updated_at !== "string" || typeof record.jsonl_download_uri !== "string"
      || compressedBytes === null || compressedBytes <= 0) {
      throw new ScryfallBulkError("network");
    }
    const root = await sha256(`${record.updated_at}\n${record.jsonl_download_uri}\n${compressedBytes}\n`);
    return Object.freeze({ root, downloadUrl: record.jsonl_download_uri, updatedAt: record.updated_at, compressedBytes });
  } catch (error) {
    throw sourceError(error);
  }
}

async function bulkResponse(source: ScryfallBulkSource, signal: AbortSignal, fetcher: typeof fetch): Promise<Response> {
  const response = await fetcher(source.downloadUrl, {
    headers: { Accept: "application/gzip,application/octet-stream;q=0.9,*/*;q=0.8" },
    credentials: "omit",
    redirect: "error",
    cache: "default",
    signal,
  });
  if (response.status !== 200 || response.type === "opaque" || !response.body) throw new ScryfallBulkError("network");
  return response;
}

async function scanJsonLines(
  response: Response,
  signal: AbortSignal,
  visit: (value: unknown) => Promise<void> | void,
  onCompressedBytesRead?: (bytes: number) => void,
): Promise<void> {
  if (!response.body) throw new ScryfallBulkError("network");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  const gunzip = new Gunzip((chunk) => chunks.push(chunk));
  let trailing = "";
  let trailingUtf8 = new Uint8Array();
  let lineNumber = 0;
  let compressedBytesRead = 0;
  let lastYieldAt = performance.now();
  let stage = "reading the bulk response";
  const yieldToUi = async () => {
    if (performance.now() - lastYieldAt < 16) return;
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    lastYieldAt = performance.now();
  };
  const visitLine = (line: string): Promise<void> | void => {
    lineNumber += 1;
    if (!line) return;
    let value: unknown;
    try { value = JSON.parse(line); } catch (error) {
      throw new ScryfallBulkError("network", `JSONL record ${lineNumber}: ${errorDetail(error) ?? "invalid JSON"}`);
    }
    return visit(value);
  };
  try {
    while (true) {
      if (signal.aborted) return;
      const next = await reader.read();
      const input = next.done ? new Uint8Array() : next.value;
      for (let offset = 0; offset < input.byteLength || (next.done && offset === 0); offset += GZIP_INPUT_CHUNK_BYTES) {
        const end = Math.min(offset + GZIP_INPUT_CHUNK_BYTES, input.byteLength);
        chunks.length = 0;
        stage = `decompressing bytes ${offset}-${end}`;
        gunzip.push(input.slice(offset, end), next.done && end === input.byteLength);
        onCompressedBytesRead?.(compressedBytesRead + end);
        for (const chunk of chunks) {
          for (let outputOffset = 0; outputOffset < chunk.byteLength; outputOffset += JSONL_INPUT_CHUNK_BYTES) {
            stage = `decoding JSONL after byte ${end}`;
            const outputEnd = Math.min(outputOffset + JSONL_INPUT_CHUNK_BYTES, chunk.byteLength);
            const inputBytes = chunk.subarray(outputOffset, outputEnd);
            let bytes = inputBytes;
            if (trailingUtf8.byteLength > 0) {
              bytes = new Uint8Array(trailingUtf8.byteLength + inputBytes.byteLength);
              bytes.set(trailingUtf8);
              bytes.set(inputBytes, trailingUtf8.byteLength);
            }
            const completeLength = completeUtf8Length(bytes);
            trailingUtf8 = bytes.slice(completeLength);
            const lines = `${trailing}${new TextDecoder().decode(bytes.subarray(0, completeLength))}`.split("\n");
            trailing = lines.pop() ?? "";
            for (const line of lines) {
              if (signal.aborted) return;
              const pending = visitLine(line);
              if (pending) await pending;
            }
            await yieldToUi();
          }
        }
        await yieldToUi();
      }
      compressedBytesRead += input.byteLength;
      if (next.done) break;
    }
    stage = "finishing JSONL decoding";
    trailing += new TextDecoder().decode(trailingUtf8);
    if (trailing) {
      const pending = visitLine(trailing);
      if (pending) await pending;
    }
  } catch (error) {
    throw new ScryfallBulkError("network", `${stage}: ${errorDetail(error) ?? "unknown error"}`);
  } finally {
    await reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}

export async function forEachScryfallAsset(
  source: ScryfallBulkSource,
  selector: InstallSelector,
  signal: AbortSignal,
  visit: (descriptor: ScryfallAssetDescriptor) => Promise<void> | void,
  fetcher: typeof fetch = globalThis.fetch,
): Promise<void> {
  if (selector.kind === "core") {
    for (const value of coreDescriptors()) {
      const pending = visit(value);
      if (pending) await pending;
    }
    return;
  }
  // The curated membership is planned from local data, so `source` is unused
  // here and the multi-gigabyte bulk stream is never opened.
  if (selector.kind === "curated") {
    for (const value of await curatedDescriptors(selector.membershipDigest)) {
      if (signal.aborted) return;
      const pending = visit(value);
      if (pending) await pending;
    }
    return;
  }
  // Deck-library membership is likewise planned from local data. Its digest
  // must still name the current plan before any objects can be written under
  // it, so stale selectors fail without opening the bulk archive.
  if (selector.kind === "deck_library") {
    const membership = await planDeckLibraryPack(packId("deck_library"));
    if (membership.membershipDigest !== selector.membershipDigest) throw new VisualPackBackendError("conflict");
    for (const value of membership.descriptors) {
      if (signal.aborted) return;
      const pending = visit(value);
      if (pending) await pending;
    }
    return;
  }
  try {
    const selectedPack = selector.kind === "complete" ? packId("complete")
      : selector.kind === "printing" ? packId(`printing:${selector.set}`)
        : packId(`locale:${selector.language}:${selector.set}`);
    if (selector.kind === "printing") {
      const pending = visit(setIconDescriptor(selectedPack, selector.set));
      if (pending) await pending;
    }
    const english = new Map<string, CardIdentity>();
    const localized = new Map<string, CardIdentity>();
    await scanJsonLines(await bulkResponse(source, signal, fetcher), signal, (value) => {
      if (signal.aborted) return;
      const raw = value as ScryfallCard;
      const card = cardIdentity(raw);
      if (!card) return;
      if (selector.kind === "locale") {
        if (raw.set !== selector.set) return;
        if (raw.lang === "en") english.set(identityKey(card), card);
        if (raw.lang === selector.language) localized.set(identityKey(card), card);
      } else if (selected(selector, raw)) {
        let pending: Promise<void> | undefined;
        for (const descriptorValue of englishDescriptors(selectedPack, card)) {
          if (pending) {
            pending = pending.then(() => visit(descriptorValue));
          } else {
            const next = visit(descriptorValue);
            if (next) pending = next;
          }
        }
        return pending;
      }
    });
    if (selector.kind === "locale") {
      for (const [key, local] of localized) {
        const base = english.get(key);
        if (!base) continue;
        for (const descriptorValue of localizedDescriptors(selectedPack, base, local, selector.language)) {
          const pending = visit(descriptorValue);
          if (pending) await pending;
        }
      }
    }
  } catch (error) {
    if (error instanceof Error && error.name === "VisualPackBackendError") throw error;
    throw sourceError(error);
  }
}

export async function countScryfallAssets(
  source: ScryfallBulkSource,
  selector: InstallSelector,
  signal: AbortSignal,
  onProgress?: (progress: CatalogScanProgress) => void,
  fetcher: typeof fetch = globalThis.fetch,
): Promise<number> {
  if (selector.kind === "core") return coreDescriptors().length;
  if (selector.kind === "curated") return (await curatedDescriptors(selector.membershipDigest)).length;
  if (selector.kind === "deck_library") {
    const membership = await planDeckLibraryPack(packId("deck_library"));
    if (membership.membershipDigest !== selector.membershipDigest) throw new VisualPackBackendError("conflict");
    return membership.descriptors.length;
  }
  try {
    let count = selector.kind === "printing" ? 1 : 0;
    let compressedBytesRead = 0;
    let recordsScanned = 0;
    let lastReportedAt = 0;
    const report = (force = false) => {
      const now = performance.now();
      if (!force && now - lastReportedAt < 100) return;
      lastReportedAt = now;
      onProgress?.({
        compressedBytesRead: Math.min(compressedBytesRead, source.compressedBytes),
        compressedBytesTotal: source.compressedBytes,
        recordsScanned,
        assetRecords: count,
      });
    };
    const english = new Map<string, CardIdentity>();
    const localized = new Map<string, CardIdentity>();
    await scanJsonLines(await bulkResponse(source, signal, fetcher), signal, (value) => {
      if (signal.aborted) return;
      recordsScanned += 1;
      const raw = value as ScryfallCard;
      const card = cardIdentity(raw);
      if (!card) return;
      if (selector.kind === "locale") {
        if (raw.set !== selector.set) return;
        if (raw.lang === "en") english.set(identityKey(card), card);
        if (raw.lang === selector.language) localized.set(identityKey(card), card);
      } else if (selected(selector, raw)) {
        count += imageCount(card);
      }
      report();
    }, (bytes) => {
      compressedBytesRead = bytes;
      report();
    });
    if (signal.aborted) return count;
    if (selector.kind === "locale") {
      for (const [key, local] of localized) {
        const base = english.get(key);
        if (base) count += imageCount(local, base.faces.length);
      }
    }
    report(true);
    return count;
  } catch (error) {
    throw sourceError(error);
  }
}
