// Card image + header info, sourced exactly like the app (services/scryfall.ts):
// load scryfall-data.json from R2, cache it, and do pure in-memory lookups. No
// live Scryfall API — no per-card network call, no rate limits, no stalls.
//
// The stable manifest path is overwritten in place each deploy, so we keep the
// cache fresh with the same ETag stale-while-revalidate the coverage cache uses
// (hourly — Scryfall bulk data only changes on set releases).

import { scryfallDataUrl } from "./config";

interface RawEntry {
  oracle_id: string;
  faces?: Array<{ normal: string; art_crop: string }>;
  name: string;
  mana_cost?: string;
  type_line?: string;
}

export interface ScryfallCard {
  /** Canonical display name (with " // " for multi-face cards). */
  name: string;
  /** Normal-size front-face image, or null. */
  image: string | null;
  /** e.g. "Creature — Bear"; null if unavailable. */
  typeLine: string | null;
  /** e.g. "{1}{G}" — includes " // " between split/adventure faces. */
  manaCost: string | null;
  /** Scryfall page link (by oracle id). */
  scryfallUri: string | null;
}

const REFRESH_MS = 60 * 60 * 1000;

interface Cache {
  map: Map<string, ScryfallCard>;
  etag: string | null;
  checkedAt: number;
}

let cache: Cache | null = null;
let loading: Promise<Cache | null> | null = null;

function compact(e: RawEntry): ScryfallCard {
  return {
    name: e.name,
    image: e.faces?.[0]?.normal ?? null,
    typeLine: e.type_line ?? null,
    manaCost: e.mana_cost ?? null,
    scryfallUri: e.oracle_id
      ? `https://scryfall.com/search?q=oracleid%3A${e.oracle_id}`
      : null,
  };
}

type Fetched = { map: Map<string, ScryfallCard>; etag: string | null };

/** Fetches + indexes the export. Returns "unchanged" on 304, null on error. */
async function fetchCards(prevEtag: string | null): Promise<Fetched | "unchanged" | null> {
  try {
    const res = await fetch(scryfallDataUrl(), {
      headers: prevEtag ? { "If-None-Match": prevEtag } : {},
      signal: AbortSignal.timeout(30000),
    });
    if (res.status === 304) return "unchanged";
    if (!res.ok) return null;
    const etag = res.headers.get("etag");
    const raw = (await res.json()) as Record<string, RawEntry>;
    const map = new Map<string, ScryfallCard>();
    for (const [key, entry] of Object.entries(raw)) map.set(key, compact(entry));
    console.log(`[scryfall] loaded ${map.size} entries`);
    return { map, etag };
  } catch (err) {
    console.error("[scryfall] fetch failed:", err);
    return null;
  }
}

async function loadCache(): Promise<Cache | null> {
  const fetched = await fetchCards(null);
  if (fetched && fetched !== "unchanged") {
    cache = { map: fetched.map, etag: fetched.etag, checkedAt: Date.now() };
  }
  return cache;
}

/** Stale-while-revalidate: refresh the cache in the background when stale. */
async function revalidate(): Promise<void> {
  if (!cache) return;
  cache.checkedAt = Date.now();
  const fetched = await fetchCards(cache.etag);
  if (fetched && fetched !== "unchanged") {
    cache.map = fetched.map;
    cache.etag = fetched.etag;
  }
}

async function ensure(): Promise<Cache | null> {
  if (cache) {
    if (Date.now() - cache.checkedAt > REFRESH_MS) void revalidate().catch(() => {});
    return cache;
  }
  if (!loading) {
    loading = loadCache().finally(() => {
      loading = null;
    });
  }
  return loading;
}

/** Looks up a card's image + header from the cached Scryfall export. */
export async function lookupScryfall(name: string): Promise<ScryfallCard | null> {
  const c = await ensure();
  return c?.map.get(name.toLowerCase()) ?? null;
}

/** Pre-loads the Scryfall export so the first query is instant. */
export async function warmScryfall(): Promise<void> {
  await ensure();
}
