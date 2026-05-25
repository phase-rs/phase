// Per-build cache of the engine's coverage export (coverage-data.json on R2).
//
// Each card entry carries `parse_details` — the exact `ParsedItem` tree the
// frontend's Alt-hover "ENGINE PARSE" overlay renders (engine-authoritative,
// built by crates/engine/src/game/coverage.rs). We read it straight from R2 so
// the bot never re-derives parse logic and always reflects the deployed build.
//
// Memory: the file is ~52MB. We parse it once per (re)load, then keep each card
// as a compact JSON string keyed by lowercased name — a lookup parses one small
// entry on demand. This avoids holding a 34k-node live object graph resident
// (the OOM shape noted in project memory) while keeping steady RAM ≈ file size.

import {
  type Build,
  DEFAULT_BUILD,
  coverageUrl,
  metaUrl,
} from "./config";

/** A node in the engine's hierarchical parse tree. Mirrors Rust `ParsedItem`. */
export interface ParsedItem {
  category: "keyword" | "ability" | "trigger" | "static" | "replacement" | "cost";
  label: string;
  source_text?: string | null;
  supported: boolean;
  details?: [string, string][];
  children?: ParsedItem[];
}

/** A single card's entry in coverage-data.json `cards[]`. */
export interface CoverageEntry {
  card_name: string;
  set_code?: string;
  oracle_text?: string | null;
  supported: boolean;
  gap_count: number;
  gap_details?: { handler: string; source_text: string }[];
  parse_details: ParsedItem[];
  printings?: string[];
}

/** Build provenance from card-data-meta.json (shown in the embed footer). */
export interface BuildMeta {
  commit_short?: string;
  mtgjson_date?: string;
  data_hash?: string;
  generated_at?: string;
}

interface BuildCache {
  /** lowercased card name → JSON.stringify(CoverageEntry). */
  byName: Map<string, string>;
  /** Display-cased names, sorted, for autocomplete. */
  names: string[];
  meta: BuildMeta | null;
  etag: string | null;
  lastAccess: number;
  checkedAt: number;
}

/** How long a cached build serves before a background revalidation is kicked off. */
const REFRESH_MS = 5 * 60 * 1000;
/** Idle non-default builds are evicted after this long without access. */
const IDLE_MS = 30 * 60 * 1000;

const caches = new Map<Build, BuildCache>();
const loading = new Map<Build, Promise<BuildCache>>();

async function fetchMeta(build: Build): Promise<BuildMeta | null> {
  try {
    const res = await fetch(metaUrl(build), { headers: { "cache-control": "no-cache" } });
    if (!res.ok) return null;
    return (await res.json()) as BuildMeta;
  } catch {
    return null;
  }
}

interface FetchedCoverage {
  byName: Map<string, string>;
  names: string[];
  etag: string | null;
}

/**
 * Fetches and indexes coverage-data.json. With `prevEtag`, sends a conditional
 * request; returns null on 304 (caller keeps its existing index).
 */
async function fetchCoverage(
  build: Build,
  prevEtag: string | null,
): Promise<FetchedCoverage | null> {
  const res = await fetch(coverageUrl(build), {
    headers: prevEtag ? { "If-None-Match": prevEtag } : {},
  });
  if (res.status === 304) return null;
  if (!res.ok) {
    throw new Error(`coverage fetch ${build} → ${res.status} ${res.statusText}`);
  }

  const etag = res.headers.get("etag");
  const json = (await res.json()) as { cards?: CoverageEntry[] };
  const cards = json.cards ?? [];

  const byName = new Map<string, string>();
  const names: string[] = [];
  for (const entry of cards) {
    if (!entry?.card_name) continue;
    byName.set(entry.card_name.toLowerCase(), JSON.stringify(entry));
    names.push(entry.card_name);
  }
  names.sort((a, b) => a.localeCompare(b));
  return { byName, names, etag };
}

async function loadBuild(build: Build): Promise<BuildCache> {
  const [fetched, meta] = await Promise.all([fetchCoverage(build, null), fetchMeta(build)]);
  // fetched is non-null here: no prevEtag means no 304 path.
  const now = Date.now();
  const cache: BuildCache = {
    byName: fetched!.byName,
    names: fetched!.names,
    meta,
    etag: fetched!.etag,
    lastAccess: now,
    checkedAt: now,
  };
  caches.set(build, cache);
  console.log(`[coverage] loaded ${build}: ${cache.byName.size} cards`);
  return cache;
}

/** Stale-while-revalidate: refresh an already-loaded build in the background. */
async function revalidate(build: Build): Promise<void> {
  const cache = caches.get(build);
  if (!cache) return;
  cache.checkedAt = Date.now(); // mark attempt now so we don't stack revalidations
  const [fetched, meta] = await Promise.all([
    fetchCoverage(build, cache.etag),
    fetchMeta(build),
  ]);
  if (fetched) {
    cache.byName = fetched.byName;
    cache.names = fetched.names;
    cache.etag = fetched.etag;
  }
  if (meta) cache.meta = meta;
}

/**
 * Returns a ready build cache. Serves a loaded build immediately (kicking off a
 * background revalidation when stale); otherwise loads it, deduping concurrent
 * callers so the 52MB fetch happens once.
 */
async function ensureBuild(build: Build): Promise<BuildCache> {
  const existing = caches.get(build);
  if (existing) {
    existing.lastAccess = Date.now();
    if (Date.now() - existing.checkedAt > REFRESH_MS) {
      void revalidate(build).catch(() => {});
    }
    return existing;
  }

  let pending = loading.get(build);
  if (!pending) {
    pending = loadBuild(build);
    loading.set(build, pending);
    void pending.finally(() => loading.delete(build));
  }
  return pending;
}

/** Looks up one card's parsed coverage entry, or null if unknown. */
export async function lookupCard(
  build: Build,
  name: string,
): Promise<CoverageEntry | null> {
  const cache = await ensureBuild(build);
  const raw = cache.byName.get(name.toLowerCase());
  return raw ? (JSON.parse(raw) as CoverageEntry) : null;
}

/** Returns the build's provenance manifest (may be null if unavailable). */
export async function getMeta(build: Build): Promise<BuildMeta | null> {
  const cache = await ensureBuild(build);
  return cache.meta;
}

/** Autocomplete: up to `limit` card names matching `query` (prefix first). */
export async function suggestNames(
  build: Build,
  query: string,
  limit = 25,
): Promise<string[]> {
  const cache = await ensureBuild(build);
  const q = query.trim().toLowerCase();
  if (!q) return cache.names.slice(0, limit);

  const prefix: string[] = [];
  const contains: string[] = [];
  for (const name of cache.names) {
    const lower = name.toLowerCase();
    if (lower.startsWith(q)) prefix.push(name);
    else if (lower.includes(q)) contains.push(name);
    if (prefix.length >= limit) break;
  }
  return [...prefix, ...contains].slice(0, limit);
}

/** Pre-loads the default build so the common path never pays a cold-fetch. */
export async function warmDefaultBuild(): Promise<void> {
  await ensureBuild(DEFAULT_BUILD);
}

/** Periodically drops idle non-default builds to bound memory. */
export function startEvictionLoop(): ReturnType<typeof setInterval> {
  return setInterval(() => {
    const now = Date.now();
    for (const [build, cache] of caches) {
      if (build === DEFAULT_BUILD) continue;
      if (now - cache.lastAccess > IDLE_MS) caches.delete(build);
    }
  }, IDLE_MS);
}
