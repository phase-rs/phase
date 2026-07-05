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
  face_names?: string[];
  layout?: string;
  name: string;
  mana_cost?: string;
  type_line?: string;
  oracle_text?: string | null;
  colors?: string[];
  keywords?: string[];
  power?: string | null;
  toughness?: string | null;
}

export interface ScryfallCard {
  /** Canonical display name (with " // " for multi-face cards). */
  name: string;
  /** Front-face normal-size image, or null. Equal to faceImages[0]. */
  image: string | null;
  /** Per-face normal-size images, in Scryfall face order. */
  faceImages: Array<string | null>;
  /** Lowercased face names, in Scryfall order; single-element for one-faced cards. */
  faceNames: string[];
  /** Scryfall layout ("transform", "modal_dfc", "token", …), or null. */
  layout: string | null;
  /** e.g. "Creature — Bear"; null if unavailable. */
  typeLine: string | null;
  /** e.g. "{1}{G}" — includes " // " between split/adventure faces. */
  manaCost: string | null;
  /** Rules text — only populated for token entries (playable cards use coverage). */
  oracleText: string | null;
  /** Colors (WUBRG letters); populated for tokens, may be empty otherwise. */
  colors: string[];
  /** Keyword abilities; populated for tokens, may be empty otherwise. */
  keywords: string[];
  /** Power / toughness as strings (tokens / creatures), or null. */
  power: string | null;
  toughness: string | null;
  /** Scryfall page link (by oracle id). */
  scryfallUri: string | null;
}

const REFRESH_MS = 60 * 60 * 1000;

interface Indexed {
  /** Primary keys: oracle id, display name, front-face name, and token:<name>. */
  map: Map<string, ScryfallCard>;
  /** Back-face name → card (back-face names are NOT primary keys in the export). */
  byFaceName: Map<string, ScryfallCard>;
  /** Sorted token display names, for autocomplete. */
  tokenNames: string[];
}

interface Cache extends Indexed {
  etag: string | null;
  checkedAt: number;
}

let cache: Cache | null = null;
let loading: Promise<Cache | null> | null = null;

const TOKEN_PREFIX = "token:";

function compact(e: RawEntry): ScryfallCard {
  const faceImages = e.faces?.map((f) => f.normal ?? null) ?? [];
  return {
    name: e.name,
    image: faceImages[0] ?? null,
    faceImages,
    faceNames: e.face_names ?? [e.name.toLowerCase()],
    layout: e.layout ?? null,
    typeLine: e.type_line ?? null,
    manaCost: e.mana_cost ?? null,
    oracleText: e.oracle_text ?? null,
    colors: e.colors ?? [],
    keywords: e.keywords ?? [],
    power: e.power ?? null,
    toughness: e.toughness ?? null,
    scryfallUri: e.oracle_id
      ? `https://scryfall.com/search?q=oracleid%3A${e.oracle_id}`
      : null,
  };
}

// Build the three lookup structures from the raw export. Primary keys always win
// over the aux back-face index; on the rare duplicate back-face name (verified:
// only "virtuous"), last-write-wins deterministically.
function indexRaw(raw: Record<string, RawEntry>): Indexed {
  const map = new Map<string, ScryfallCard>();
  for (const [key, entry] of Object.entries(raw)) map.set(key, compact(entry));

  const byFaceName = new Map<string, ScryfallCard>();
  const tokenNames: string[] = [];
  for (const [key, card] of map) {
    if (key.startsWith(TOKEN_PREFIX)) {
      tokenNames.push(card.name);
      continue;
    }
    for (const faceName of card.faceNames) {
      if (!map.has(faceName)) byFaceName.set(faceName, card);
    }
  }
  tokenNames.sort((a, b) => a.localeCompare(b));
  return { map, byFaceName, tokenNames };
}

type Fetched = { index: Indexed; etag: string | null };

/** Fetches + indexes the export. Returns "unchanged" on 304, null on error. */
async function fetchCards(prevEtag: string | null): Promise<Fetched | "unchanged" | null> {
  // Local-file override for smoke tests / self-host: read a scryfall-data.json
  // straight off disk instead of R2. Skips ETag revalidation (returns no etag).
  const localFile = Bun.env.CARD_BOT_SCRYFALL_FILE;
  if (localFile) {
    if (prevEtag) return "unchanged";
    try {
      const raw = JSON.parse(await Bun.file(localFile).text()) as Record<string, RawEntry>;
      const index = indexRaw(raw);
      console.log(`[scryfall] loaded ${index.map.size} entries from ${localFile}`);
      return { index, etag: null };
    } catch (err) {
      console.error("[scryfall] local file read failed:", err);
      return null;
    }
  }

  try {
    const res = await fetch(scryfallDataUrl(), {
      headers: prevEtag ? { "If-None-Match": prevEtag } : {},
      signal: AbortSignal.timeout(30000),
    });
    if (res.status === 304) return "unchanged";
    if (!res.ok) return null;
    const etag = res.headers.get("etag");
    const raw = (await res.json()) as Record<string, RawEntry>;
    const index = indexRaw(raw);
    console.log(`[scryfall] loaded ${index.map.size} entries (${index.tokenNames.length} tokens)`);
    return { index, etag };
  } catch (err) {
    console.error("[scryfall] fetch failed:", err);
    return null;
  }
}

async function loadCache(): Promise<Cache | null> {
  const fetched = await fetchCards(null);
  if (fetched && fetched !== "unchanged") {
    cache = { ...fetched.index, etag: fetched.etag, checkedAt: Date.now() };
  }
  return cache;
}

/** Stale-while-revalidate: refresh the cache in the background when stale. */
async function revalidate(): Promise<void> {
  if (!cache) return;
  cache.checkedAt = Date.now();
  const fetched = await fetchCards(cache.etag);
  if (fetched && fetched !== "unchanged") {
    cache.map = fetched.index.map;
    cache.byFaceName = fetched.index.byFaceName;
    cache.tokenNames = fetched.index.tokenNames;
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

/**
 * Looks up a card's image + header. Resolves by primary key (oracle id / display
 * name / front-face name), then falls back to the back-face-name index so a
 * query for a DFC's back face (e.g. "Amazing Spider-Man") finds the whole card.
 */
export async function lookupScryfall(name: string): Promise<ScryfallCard | null> {
  const c = await ensure();
  if (!c) return null;
  const key = name.toLowerCase();
  return c.map.get(key) ?? c.byFaceName.get(key) ?? null;
}

/** Looks up a token by name (the export keys tokens under a "token:" prefix). */
export async function lookupToken(name: string): Promise<ScryfallCard | null> {
  const c = await ensure();
  return c?.map.get(`${TOKEN_PREFIX}${name.toLowerCase()}`) ?? null;
}

/**
 * Non-blocking snapshot of known token display names, for autocomplete. Returns
 * [] until the export has loaded — autocomplete has a hard 3s budget and must
 * never await the cold R2 fetch (which can take up to 30s).
 */
export function peekTokenNames(): string[] {
  return cache?.tokenNames ?? [];
}

/** Pre-loads the Scryfall export so the first query is instant. */
export async function warmScryfall(): Promise<void> {
  await ensure();
}
