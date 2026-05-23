import type { GameFormat, TokenImageRef } from "../adapter/types";

interface ScryfallDataEntry {
  oracle_id: string;
  /** Lowercased face names in Scryfall's `card_faces` order; one entry for
   * single-faced cards. Used to resolve `faceIndex` from an engine-reported
   * `printed_ref.face_name`. */
  face_names: string[];
  faces: Array<{ normal: string; art_crop: string }>;
  layout?: string;
  name: string;
  mana_cost: string;
  cmc: number;
  type_line: string;
  colors: string[];
  color_identity: string[];
  keywords: string[];
}

/**
 * Scryfall's default MTG card back image.
 *
 * Scryfall identifies the generic MTG card back with a fixed ID
 * (`0aeebaf5-8c7d-4636-9e82-8c27447861f7`) served from the `backs.scryfall.io`
 * CDN subdomain. This URL is stable across Scryfall versions — it is not
 * regenerated with each bulk data refresh, so it lives here as a constant
 * rather than in `scryfall-data.json`.
 *
 * Hotlinking (rather than bundling a `card-back.png`) keeps the repo free of
 * WotC-copyrighted raster assets; the user's browser fetches directly from
 * Scryfall at runtime, matching the pattern used for every other card image.
 */
export const CARD_BACK_URL =
  "https://backs.scryfall.io/normal/0/a/0aeebaf5-8c7d-4636-9e82-8c27447861f7.jpg";

export interface PrintingEntry {
  id: string;
  set: string;
  set_name: string;
  collector_number: string;
  border_color: string;
  frame_effects: string[];
  full_art: boolean;
  faces: Array<{ normal: string; art_crop: string }>;
}

type ScryfallDataMap = Record<string, ScryfallDataEntry>;
type PrintingsDataMap = Record<string, PrintingEntry[]>;
type TokenImagesDataMap = Record<string, ScryfallDataEntry & { scryfall_id: string; layout: string }>;

let scryfallDataPromise: Promise<ScryfallDataMap | null> | null = null;
let scryfallDataResolved: ScryfallDataMap | null = null;
let printingsDataPromise: Promise<PrintingsDataMap | null> | null = null;
let tokenImagesDataPromise: Promise<TokenImagesDataMap | null> | null = null;
let scryfallQueue: Promise<void> = Promise.resolve();

function loadScryfallData(): Promise<ScryfallDataMap | null> {
  if (!scryfallDataPromise) {
    scryfallDataPromise = fetch(__SCRYFALL_DATA_URL__)
      .then((r) => r.json() as Promise<ScryfallDataMap>)
      .then((data) => {
        scryfallDataResolved = data;
        return data;
      })
      .catch(() => null);
  }
  return scryfallDataPromise;
}

let printingsDataResolved: PrintingsDataMap | null = null;

export function loadPrintingsData(): Promise<PrintingsDataMap | null> {
  if (!printingsDataPromise) {
    printingsDataPromise = fetch(__SCRYFALL_PRINTINGS_URL__)
      .then((r) => r.json() as Promise<PrintingsDataMap>)
      .then((data) => {
        printingsDataResolved = data;
        return data;
      })
      .catch(() => null);
  }
  return printingsDataPromise;
}

function loadTokenImagesData(): Promise<TokenImagesDataMap | null> {
  if (!tokenImagesDataPromise) {
    tokenImagesDataPromise = fetch(__SCRYFALL_TOKEN_IMAGES_URL__)
      .then((r) => r.json() as Promise<TokenImagesDataMap>)
      .catch(() => null);
  }
  return tokenImagesDataPromise;
}

export function hasAlternatePrintingsSync(oracleId: string): boolean {
  if (!printingsDataResolved) return false;
  const printings = printingsDataResolved[oracleId];
  if (!printings) return false;
  const nonList = printings.filter((p) => p.set !== "plst");
  return nonList.length > 1;
}

export async function getCardPrintings(oracleId: string): Promise<PrintingEntry[]> {
  const data = await loadPrintingsData();
  const printings = data?.[oracleId] ?? [];
  return printings.filter((p) => p.set !== "plst");
}

export async function getCardPrintingsByName(cardName: string): Promise<PrintingEntry[]> {
  const data = await loadScryfallData();
  const entry = data?.[cardName.toLowerCase()];
  if (!entry) return [];
  return getCardPrintings(entry.oracle_id);
}

export function resolvePrintingImageUrl(
  printing: PrintingEntry,
  faceIndex: number,
  size: ImageSize,
): string | null {
  const face = printing.faces[faceIndex] ?? printing.faces[0];
  return face?.[size === "small" || size === "large" ? "normal" : size] ?? null;
}

export function findPrintingById(
  printings: PrintingEntry[],
  scryfallId: string,
): PrintingEntry | undefined {
  return printings.find((p) => p.id === scryfallId);
}

export function resolveOracleIdSync(cardName: string): string | null {
  if (!scryfallDataResolved) return null;
  return scryfallDataResolved[cardName.toLowerCase()]?.oracle_id ?? null;
}

export function isCardImageRotatedSync(oracleId: string, cardName: string): boolean {
  if (!scryfallDataResolved) return false;
  const entry = scryfallDataResolved[oracleId.toLowerCase()]
    ?? scryfallDataResolved[normalizeCardName(cardName).toLowerCase()];
  return isSidewaysLayout(entry?.layout);
}

const SCRYFALL_DELAY_MS = 100;
const MAX_RETRIES = 3;
const BASE_BACKOFF_MS = 1000;

export type ImageSize = "small" | "normal" | "large" | "art_crop";

export interface CardImageAsset {
  src: string;
  isRotated: boolean;
}

function isSidewaysLayout(layout: string | undefined): boolean {
  return layout === "split";
}

export interface ScryfallCard {
  id?: string;
  name: string;
  mana_cost: string;
  cmc: number;
  type_line: string;
  oracle_text?: string;
  colors?: string[];
  color_identity: string[];
  keywords?: string[];
  legalities?: Record<string, string>;
  image_uris?: Record<string, string>;
  card_faces?: Array<{
    name: string;
    image_uris?: Record<string, string>;
  }>;
}

const SCRYFALL_LEGALITY_KEY_OVERRIDES: Partial<Record<GameFormat, string | null>> = {
  Brawl: "standardbrawl",
  DuelCommander: "duel",
  FreeForAll: null,
  HistoricBrawl: "brawl",
  Limited: null,
  TinyLeaders: null,
  TwoHeadedGiant: null,
};

export function scryfallLegalityKey(format: GameFormat): string | undefined {
  const override = SCRYFALL_LEGALITY_KEY_OVERRIDES[format];
  if (override === null) return undefined;
  return override ?? format.toLowerCase();
}

interface ScryfallSearchResponse {
  data: ScryfallCard[];
  total_cards: number;
  has_more: boolean;
}

let nextRequestAt = 0;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function claimScryfallQueueSlot(): Promise<() => void> {
  const prior = scryfallQueue.catch(() => undefined);
  let release!: () => void;
  scryfallQueue = new Promise<void>((resolve) => {
    release = resolve;
  });
  await prior;
  return release;
}

function parseRetryDelayMs(retryAfter: string | null, attempt: number): number {
  if (!retryAfter) {
    return BASE_BACKOFF_MS * 2 ** attempt;
  }

  const retryAfterSeconds = Number.parseInt(retryAfter, 10);
  if (Number.isFinite(retryAfterSeconds)) {
    return retryAfterSeconds * 1000;
  }

  const retryAfterAt = Date.parse(retryAfter);
  if (Number.isFinite(retryAfterAt)) {
    return Math.max(0, retryAfterAt - Date.now());
  }

  return BASE_BACKOFF_MS * 2 ** attempt;
}

/**
 * Rate-limited fetch with 429 backoff and retry.
 *
 * Enforces a minimum delay between requests (Scryfall asks for 50-100ms),
 * and automatically retries on 429 using the Retry-After header with
 * exponential backoff as a fallback.
 *
 * On 429, the queue slot is held during the backoff sleep so that no other
 * requests can interleave and overwrite the backoff timestamp.
 */
async function rateLimitedFetch(
  url: string,
): Promise<Response> {
  let attempt = 0;

  const release = await claimScryfallQueueSlot();
  try {
    while (true) {
      const delayMs = Math.max(0, nextRequestAt - Date.now());
      if (delayMs > 0) {
        await sleep(delayMs);
      }

      try {
        const response = await fetch(url);
        if (response.status === 429) {
          const backoffMs = parseRetryDelayMs(
            response.headers.get("Retry-After"),
            attempt,
          );
          nextRequestAt = Date.now() + backoffMs;
          if (attempt >= MAX_RETRIES) {
            return response;
          }
          attempt += 1;
          continue;
        }

        nextRequestAt = Date.now() + SCRYFALL_DELAY_MS;
        return response;
      } catch (error) {
        // Network errors (including CORS-blocked 429s) — apply backoff
        // before both retries AND final throw so the next queued request
        // doesn't fire immediately into another rate limit.
        nextRequestAt = Date.now() + BASE_BACKOFF_MS * 2 ** attempt;
        if (attempt >= MAX_RETRIES) {
          throw error;
        }
        attempt += 1;
      }
    }
  } finally {
    release();
  }
}

/**
 * Strip deck-format decorators that are not part of the card's official name.
 *
 * Handles: set codes `[UZ]`, treatment tags `<retro>`, collector numbers
 * `<288>`, and foil markers `(F)`.
 *
 * Examples:
 *   "Goblin Lackey [UZ]"                      → "Goblin Lackey"
 *   "Abrade <retro>"                           → "Abrade"
 *   "Krenko, Mob Boss <retro> [RVR] (F)"       → "Krenko, Mob Boss"
 *   "Mountain <288>"                            → "Mountain"
 */
export function normalizeCardName(name: string): string {
  return name
    .replace(/\s*(?:<[^>]*>|\[[^\]]*\]|\(F\))\s*/g, " ")
    .trim();
}

export async function fetchCardData(cardName: string): Promise<ScryfallCard> {
  const name = normalizeCardName(cardName);
  const localMap = await loadScryfallData();
  const entry = localMap?.[name.toLowerCase()];
  if (!entry) {
    throw new Error(`Card not in local data: "${name}"`);
  }
  return {
    name: entry.name,
    mana_cost: entry.mana_cost,
    cmc: entry.cmc,
    type_line: entry.type_line,
    colors: entry.colors,
    color_identity: entry.color_identity,
    keywords: entry.keywords,
  };
}

function getImageUrl(
  card: ScryfallCard,
  size: ImageSize,
  faceIndex: number,
): string {
  if (card.card_faces?.[faceIndex]?.image_uris?.[size]) {
    return card.card_faces[faceIndex].image_uris![size];
  }
  if (card.image_uris?.[size]) {
    return card.image_uris[size];
  }
  throw new Error("No image URI found for card");
}

export async function fetchCardImageUrl(
  cardName: string,
  faceIndex: number,
  size: ImageSize = "normal",
): Promise<string> {
  return (await fetchCardImageAsset(cardName, faceIndex, size)).src;
}

export async function fetchCardImageAsset(
  cardName: string,
  faceIndex: number,
  size: ImageSize = "normal",
): Promise<CardImageAsset> {
  const data = await loadScryfallData();
  const name = normalizeCardName(cardName).toLowerCase();
  const entry = data?.[name];
  if (!entry) {
    throw new Error(`Card image not in local data: "${name}"`);
  }
  return resolveImageAsset(entry, faceIndex, size, name);
}

/**
 * Canonical image lookup by Scryfall `oracle_id` + face name.
 *
 * Used for battlefield game objects, which carry `printed_ref` from the
 * engine. This path is preferred over name-based lookup because:
 *   - oracle_id is unambiguous (no front/back-face name asymmetry)
 *   - it resolves images correctly for MDFCs played as Scryfall's back face
 *     (e.g. Mystic Peak from Pinnacle Monk // Mystic Peak)
 *   - it sidesteps the entire class of name-collision bugs
 *
 * `faceIndex` is resolved by matching `faceName` (case-insensitive) against
 * the entry's `face_names` array. If no match (defensive — should not happen
 * if scryfall-data.json was generated alongside the engine's printed_ref),
 * we fall back to face 0.
 */
export async function fetchCardImageByOracleId(
  oracleId: string,
  faceName: string | undefined,
  size: ImageSize = "normal",
): Promise<string> {
  return (await fetchCardImageAssetByOracleId(oracleId, faceName, size)).src;
}

export async function fetchCardImageAssetByOracleId(
  oracleId: string,
  faceName: string | undefined,
  size: ImageSize = "normal",
): Promise<CardImageAsset> {
  const data = await loadScryfallData();
  const key = oracleId.toLowerCase();
  const entry = data?.[key];
  if (!entry) {
    throw new Error(`Card image not in local data: oracle_id "${key}"`);
  }
  const faceIndex = faceName
    ? Math.max(0, entry.face_names.indexOf(faceName.toLowerCase()))
    : 0;
  return resolveImageAsset(entry, faceIndex, size, entry.name);
}

function resolveImageAsset(
  entry: ScryfallDataEntry,
  faceIndex: number,
  size: ImageSize,
  diagnosticName: string,
): CardImageAsset {
  return {
    src: resolveImageUrl(entry, faceIndex, size, diagnosticName),
    isRotated: isSidewaysLayout(entry.layout),
  };
}

function resolveImageUrl(
  entry: ScryfallDataEntry,
  faceIndex: number,
  size: ImageSize,
  diagnosticName: string,
): string {
  const face = entry.faces[faceIndex] ?? entry.faces[0];
  const url = face?.[size === "small" || size === "large" ? "normal" : size];
  if (!url) {
    throw new Error(`No ${size} image for "${diagnosticName}"`);
  }
  return url;
}

const MANA_COLOR_TO_SCRYFALL: Record<string, string> = {
  White: "w", Blue: "u", Black: "b", Red: "r", Green: "g",
};

export interface TokenSearchFilters {
  power?: number | null;
  toughness?: number | null;
  colors?: string[];
  /// Token creature/artifact/etc. subtypes (e.g. ["Goblin", "Warrior"]).
  /// Threaded into the Scryfall query as `t:<subtype>` clauses so that two
  /// distinct tokens that share a P/T + color shape but differ in type
  /// resolve to distinct art. Scryfall's `t:` matches the full type line,
  /// so `t:goblin t:warrior` narrows; the ladder below relaxes it
  /// progressively when narrow queries miss.
  subtypes?: string[];
  /** Whether the engine token carries any abilities (keywords, granted
   *  abilities, or printed rules text). When false, the Scryfall query is
   *  narrowed with `is:vanilla` so an ability-less engine token resolves to a
   *  vanilla printing — never an arbitrary same-shape printing that carries
   *  extra abilities (e.g. a Doctor Who 1/1 Human token with Ward 2). */
  hasAbilities?: boolean;
}

export async function fetchTokenImageUrl(
  tokenName: string,
  size: ImageSize = "normal",
  filters?: TokenSearchFilters,
): Promise<string> {
  const localUrl = await fetchTokenImageFromLocal(tokenName, size);
  if (localUrl) return localUrl;

  const colorClause = buildTokenColorClause(filters?.colors);
  const subtypes = filters?.subtypes ?? [];

  // Progressive fallback ladder:
  //   1. Most specific: name + P/T + colors + every subtype.
  //   2. Drop trailing subtypes one at a time (keeps the leading subtype
  //      longest — for MTG creature tokens the first subtype is the race
  //      (e.g. "Spirit" in "Spirit Soldier"), and Scryfall token printings
  //      most reliably index the race rather than the class).
  //   3. Drop subtypes entirely.
  //   4. Drop P/T (existing fallback shape).
  // Each step relaxes exactly one axis. Stop at the first non-empty hit.
  //
  // When the engine token has no abilities (`hasAbilities === false`), every
  // rung above is narrowed with `is:vanilla`, and a single terminal
  // last-resort rung — identical shape to the widest rung but WITHOUT
  // `is:vanilla` — is appended. That guarantees a vanilla token resolves to a
  // vanilla printing whenever one exists, while still degrading gracefully (to
  // pre-fix behavior) for a token type whose only printings carry abilities,
  // rather than producing no image at all. See issue #502.
  const vanillaOnly = filters?.hasAbilities === false;
  const queries: string[] = [];
  for (let n = subtypes.length; n >= 0; n--) {
    queries.push(
      buildTokenQuery(
        tokenName,
        filters?.power,
        filters?.toughness,
        colorClause,
        subtypes.slice(0, n),
        vanillaOnly,
      ),
    );
  }
  if (filters?.power != null || filters?.toughness != null) {
    queries.push(
      buildTokenQuery(tokenName, null, null, colorClause, [], vanillaOnly),
    );
  }
  if (vanillaOnly) {
    // Terminal last-resort rung: same shape as the widest rung, `is:vanilla`
    // dropped. Reached only when no vanilla printing of any relaxed shape
    // matched — degrades to pre-fix behavior instead of a missing image.
    queries.push(buildTokenQuery(tokenName, null, null, colorClause, [], false));
  }

  for (const query of queries) {
    const url = `https://api.scryfall.com/cards/search?q=${encodeURIComponent(query)}&order=released&dir=desc`;
    const response = await rateLimitedFetch(url);
    if (!response.ok) continue;
    const data: ScryfallSearchResponse = await response.json();
    if (data.data.length > 0) {
      return getImageUrl(data.data[0], size, 0);
    }
  }

  throw new Error(`No token image found for "${tokenName}"`);
}

export async function fetchTokenImageByRef(
  ref: TokenImageRef,
  size: ImageSize = "normal",
): Promise<string | null> {
  const data = await loadTokenImagesData();
  if (!data) return null;

  const idEntry = data[`scryfall:${ref.scryfall_id.toLowerCase()}`];
  if (idEntry) {
    const faceIndex = ref.face_name
      ? Math.max(0, idEntry.face_names.indexOf(ref.face_name.toLowerCase()))
      : 0;
    return resolveImageUrl(idEntry, faceIndex, size, idEntry.name);
  }

  if (ref.scryfall_oracle_id) {
    const faceKey = ref.face_name?.toLowerCase() ?? "";
    const oracleEntry = data[`oracle:${ref.scryfall_oracle_id.toLowerCase()}:${faceKey}`];
    if (oracleEntry) {
      const faceIndex = ref.face_name
        ? Math.max(0, oracleEntry.face_names.indexOf(ref.face_name.toLowerCase()))
        : 0;
      return resolveImageUrl(oracleEntry, faceIndex, size, oracleEntry.name);
    }
  }

  return null;
}

async function fetchTokenImageFromLocal(
  tokenName: string,
  size: ImageSize,
): Promise<string | null> {
  const data = await loadScryfallData();
  const key = `token:${tokenName.toLowerCase()}`;
  const entry = data?.[key];
  if (!entry) return null;
  const face = entry.faces[0];
  return face?.[size === "small" || size === "large" ? "normal" : size] ?? null;
}

function buildTokenQuery(
  name: string,
  power: number | null | undefined,
  toughness: number | null | undefined,
  colorClause: string,
  subtypes: string[],
  vanillaOnly: boolean,
): string {
  let query = `t:token !"${name}"`;
  if (power != null) query += ` pow=${power}`;
  if (toughness != null) query += ` tou=${toughness}`;
  query += colorClause;
  for (const s of subtypes) {
    // Scryfall's `t:` is case-insensitive and matches the full type line.
    // Quote to defend against subtypes with spaces (e.g. multi-word
    // creature types from supplemental sets).
    query += ` t:"${s.toLowerCase()}"`;
  }
  // `is:vanilla` (a documented Scryfall predicate — a card with no abilities)
  // narrows the search to ability-less printings so an ability-less engine
  // token never resolves to an arbitrary same-shape printing carrying extra
  // abilities. The caller decides per-rung whether it applies — the terminal
  // last-resort rung deliberately drops it. See issue #502.
  if (vanillaOnly) query += ` is:vanilla`;
  return query;
}

function buildTokenColorClause(colors: string[] | undefined | null): string {
  if (colors == null) return "";
  const colorStr = colors.map((c) => MANA_COLOR_TO_SCRYFALL[c] ?? "").join("");
  return colorStr ? ` c=${colorStr}` : " c=c";
}

/**
 * Search Scryfall for cards matching query. Uses rate limiting and handles 429s.
 */
export async function searchScryfall(
  query: string,
  signal?: AbortSignal,
): Promise<{ cards: ScryfallCard[]; total: number }> {
  const url = `https://api.scryfall.com/cards/search?q=${encodeURIComponent(query)}`;
  const response = await rateLimitedFetch(url);

  if (signal?.aborted) {
    return { cards: [], total: 0 };
  }

  if (response.status === 404) {
    return { cards: [], total: 0 };
  }

  if (!response.ok) {
    throw new Error(`Scryfall search error: ${response.status}`);
  }

  const data: ScryfallSearchResponse = await response.json();
  return { cards: data.data, total: data.total_cards };
}

/** Build Scryfall query string from filter options. */
export function buildScryfallQuery(options: {
  text?: string;
  colors?: string[];
  type?: string;
  cmcMax?: number;
  cmcMin?: number;
  sets?: string[];
  format?: string;
}): string {
  const parts: string[] = [];

  if (options.text) parts.push(options.text);
  if (options.colors?.length) parts.push(`c:${options.colors.join("")}`);
  if (options.type) parts.push(`t:${options.type}`);
  if (options.cmcMin !== undefined) parts.push(`cmc>=${options.cmcMin}`);
  if (options.cmcMax !== undefined) parts.push(`cmc<=${options.cmcMax}`);
  if (options.sets?.length) {
    const uniqueSetCodes = [...new Set(
      options.sets
        .map((setCode) => setCode.trim().toLowerCase())
        .filter(Boolean),
    )];
    if (uniqueSetCodes.length === 1) {
      parts.push(`set:${uniqueSetCodes[0]}`);
    } else if (uniqueSetCodes.length > 1) {
      parts.push(`(${uniqueSetCodes.map((setCode) => `set:${setCode}`).join(" OR ")})`);
    }
  }
  if (options.format) parts.push(`f:${options.format}`);

  return parts.join(" ");
}

/** Get the best image URI for a card (handles double-faced cards). */
export function getCardImageSmall(card: ScryfallCard): string {
  return card.image_uris?.small
    ?? card.card_faces?.[0]?.image_uris?.small
    ?? "";
}
