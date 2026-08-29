import { Gunzip } from "fflate";

import { cardBackCandidate, cardCandidateGroups, setIconCandidate } from "../candidateKeys.ts";
import { assetKey, catalogRoot, packId, type AssetKey, type CandidateKey, type CatalogRoot, type InstallSelector, type PackId, type VisualPackMedia } from "../types.ts";
import { CARD_BACK_URL } from "../../scryfall.ts";

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

export interface ScryfallAssetDescriptor {
  readonly packId: PackId;
  readonly assetKey: AssetKey;
  readonly candidateKeys: readonly CandidateKey[];
  readonly sourceUrl: string;
  readonly media: VisualPackMedia;
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

interface CardIdentity {
  readonly id: string;
  readonly oracleId: string;
  readonly set: string;
  readonly collector: string;
  readonly name: string;
  readonly faces: readonly { readonly name: string; readonly images: Record<string, string> }[];
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

function descriptor(
  selectedPack: PackId,
  card: CardIdentity,
  faceIndex: number,
  variant: "full_card" | "art_crop",
  rung: "small" | "normal" | "art_crop",
  sourceUrl: string,
  candidates: CandidateKey[],
): ScryfallAssetDescriptor {
  const asset = assetKey(`asset:v1:exact_printing:${card.id}-${faceIndex}-${variant}-${rung}`);
  return { packId: selectedPack, assetKey: asset, candidateKeys: candidates, sourceUrl, media: "image/jpeg" };
}

function englishDescriptors(selectedPack: PackId, card: CardIdentity): ScryfallAssetDescriptor[] {
  return card.faces.flatMap((face, faceIndex) => {
    const result: ScryfallAssetDescriptor[] = [];
    const add = (variant: "full_card" | "art_crop", rung: "small" | "normal" | "art_crop", url: string | undefined) => {
      if (!url) return;
      const candidates = cardCandidateGroups({
        englishPrintingId: card.id,
        oracleId: card.oracleId,
        englishAliases: [card.name, face.name],
        oracleAliases: [card.name, face.name],
        faceIndex,
        variant,
        rung,
      }).flatMap((group) => group.keys);
      result.push(descriptor(selectedPack, card, faceIndex, variant, rung, url, candidates));
    };
    add("full_card", "small", face.images.small);
    add("full_card", "normal", face.images.normal);
    add("art_crop", "art_crop", face.images.art_crop);
    return result;
  });
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

function coreDescriptors(): ScryfallAssetDescriptor[] {
  return [{
    packId: packId("core"),
    assetKey: assetKey("asset:v1:card_back:default"),
    candidateKeys: [cardBackCandidate()],
    sourceUrl: CARD_BACK_URL,
    media: "image/jpeg",
  }];
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

async function* jsonLines(response: Response, signal: AbortSignal): AsyncGenerator<unknown> {
  if (!response.body) throw new ScryfallBulkError("network");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  const gunzip = new Gunzip((chunk) => chunks.push(chunk));
  let trailing = "";
  let trailingUtf8 = new Uint8Array();
  let lineNumber = 0;
  let stage = "reading the bulk response";
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
              lineNumber += 1;
              if (signal.aborted) return;
              if (!line) continue;
              try { yield JSON.parse(line); } catch (error) {
                throw new ScryfallBulkError("network", `JSONL record ${lineNumber}: ${errorDetail(error) ?? "invalid JSON"}`);
              }
            }
          }
        }
      }
      if (next.done) break;
    }
    stage = "finishing JSONL decoding";
    trailing += new TextDecoder().decode(trailingUtf8);
    if (trailing) {
      lineNumber += 1;
      try { yield JSON.parse(trailing); } catch (error) {
        throw new ScryfallBulkError("network", `JSONL record ${lineNumber}: ${errorDetail(error) ?? "invalid JSON"}`);
      }
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
  visit: (descriptor: ScryfallAssetDescriptor) => Promise<void>,
  fetcher: typeof fetch = globalThis.fetch,
): Promise<void> {
  if (selector.kind === "core") {
    for (const value of coreDescriptors()) await visit(value);
    return;
  }
  try {
    const selectedPack = selector.kind === "complete" ? packId("complete")
      : selector.kind === "printing" ? packId(`printing:${selector.set}`)
        : packId(`locale:${selector.language}:${selector.set}`);
    if (selector.kind === "printing") await visit(setIconDescriptor(selectedPack, selector.set));
    const english = new Map<string, CardIdentity>();
    const localized = new Map<string, CardIdentity>();
    for await (const value of jsonLines(await bulkResponse(source, signal, fetcher), signal)) {
      if (signal.aborted) return;
      const raw = value as ScryfallCard;
      const card = cardIdentity(raw);
      if (!card) continue;
      if (selector.kind === "locale") {
        if (raw.set !== selector.set) continue;
        if (raw.lang === "en") english.set(identityKey(card), card);
        if (raw.lang === selector.language) localized.set(identityKey(card), card);
      } else if (selected(selector, raw)) {
        for (const descriptorValue of englishDescriptors(selectedPack, card)) await visit(descriptorValue);
      }
    }
    if (selector.kind === "locale") {
      for (const [key, local] of localized) {
        const base = english.get(key);
        if (!base) continue;
        for (const descriptorValue of localizedDescriptors(selectedPack, base, local, selector.language)) await visit(descriptorValue);
      }
    }
  } catch (error) {
    if (error instanceof Error && error.name === "VisualPackBackendError") throw error;
    throw sourceError(error);
  }
}
