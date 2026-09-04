import type { GameFormat, TokenImageRef } from "../adapter/types";
import type { CardImageSource, ImageRungs } from "./visualPacks/types.ts";

interface ScryfallImageFace {
  small?: string | null;
  normal?: string | null;
  art_crop?: string | null;
}

interface ScryfallDataEntry {
  oracle_id: string;
  /** Lowercased face names in Scryfall's `card_faces` order; one entry for
   * single-faced cards. Used to resolve `faceIndex` from an engine-reported
   * `printed_ref.face_name`. */
  face_names: string[];
  faces: ScryfallImageFace[];
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

/** Build the authoritative Scryfall source URL for an admitted mana shard. */
export function manaSymbolSourceUrl(shard: string): string {
  const code = shard === "∞"
    ? "INFINITY"
    : shard === "½"
      ? "HALF"
      : shard.replace(/\//g, "");
  return `https://svgs.scryfall.io/card-symbols/${encodeURIComponent(code)}.svg`;
}

export interface PrintingEntry {
  id: string;
  set: string;
  set_name: string;
  collector_number: string;
  released_at: string;
  border_color: string;
  frame_effects: string[];
  full_art: boolean;
  faces: ScryfallImageFace[];
}

type ScryfallDataMap = Record<string, ScryfallDataEntry>;
type PrintingsDataMap = Record<string, PrintingEntry[]>;
type TokenImagesDataMap = Record<string, ScryfallDataEntry & { scryfall_id: string; layout: string }>;

let scryfallDataPromise: Promise<ScryfallDataMap | null> | null = null;
let scryfallDataResolved: ScryfallDataMap | null = null;
/** Maps diacritic-folded lowercase names to canonical scryfall-data keys. */
let scryfallFoldedNameIndex: Map<string, string> | null = null;
let printingsDataPromise: Promise<PrintingsDataMap | null> | null = null;
let tokenImagesDataPromise: Promise<TokenImagesDataMap | null> | null = null;
let scryfallQueue: Promise<void> = Promise.resolve();

function isNonEmptyRecord(value: unknown): value is Record<string, unknown> {
  return value !== null
    && typeof value === "object"
    && !Array.isArray(value)
    && Object.keys(value).length > 0;
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === "string");
}

function isScryfallImageFace(value: unknown): value is ScryfallImageFace {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const face = value as ScryfallImageFace;
  return (face.small === undefined || face.small === null || typeof face.small === "string")
    && (face.normal === undefined || face.normal === null || typeof face.normal === "string")
    && (face.art_crop === undefined || face.art_crop === null || typeof face.art_crop === "string");
}

function isScryfallDataEntry(value: unknown): value is ScryfallDataEntry {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const entry = value as Partial<ScryfallDataEntry>;
  return typeof entry.oracle_id === "string"
    && typeof entry.name === "string"
    && typeof entry.mana_cost === "string"
    && typeof entry.cmc === "number"
    && typeof entry.type_line === "string"
    && isStringArray(entry.face_names)
    && isStringArray(entry.colors)
    && isStringArray(entry.color_identity)
    && isStringArray(entry.keywords)
    && (entry.layout === undefined || typeof entry.layout === "string")
    && Array.isArray(entry.faces)
    && entry.faces.length > 0
    && entry.faces.every(isScryfallImageFace);
}

function isScryfallDataMap(value: unknown): value is ScryfallDataMap {
  return isNonEmptyRecord(value) && Object.values(value).every(isScryfallDataEntry);
}

function isPrintingEntry(value: unknown): value is PrintingEntry {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const entry = value as Partial<PrintingEntry>;
  return typeof entry.id === "string"
    && typeof entry.set === "string"
    && typeof entry.set_name === "string"
    && typeof entry.collector_number === "string"
    && typeof entry.released_at === "string"
    && typeof entry.border_color === "string"
    && isStringArray(entry.frame_effects)
    && typeof entry.full_art === "boolean"
    && Array.isArray(entry.faces)
    && entry.faces.length > 0
    && entry.faces.every(isScryfallImageFace);
}

function isPrintingsDataMap(value: unknown): value is PrintingsDataMap {
  return isNonEmptyRecord(value)
    && Object.values(value).every((printings) =>
      Array.isArray(printings) && printings.length > 0 && printings.every(isPrintingEntry));
}

export function loadScryfallData(): Promise<ScryfallDataMap | null> {
  if (!scryfallDataPromise) {
    const pending = (async () => {
      const response = await fetch(__SCRYFALL_DATA_URL__);
      if (!response.ok) return null;
      const data: unknown = await response.json();
      if (!isScryfallDataMap(data)) return null;
      const foldedNameIndex = buildFoldedNameIndex(data);
      scryfallDataResolved = data;
      scryfallFoldedNameIndex = foldedNameIndex;
      return data;
    })()
      .catch(() => null);
    scryfallDataPromise = pending;
    void pending.then((data) => {
      if (!data && scryfallDataPromise === pending) scryfallDataPromise = null;
    });
  }
  return scryfallDataPromise;
}

let printingsDataResolved: PrintingsDataMap | null = null;

export function loadPrintingsData(): Promise<PrintingsDataMap | null> {
  if (!printingsDataPromise) {
    const pending = (async () => {
      const response = await fetch(__SCRYFALL_PRINTINGS_URL__);
      if (!response.ok) return null;
      const data: unknown = await response.json();
      if (!isPrintingsDataMap(data)) return null;
      printingsDataResolved = data;
      return data;
    })()
      .catch(() => null);
    printingsDataPromise = pending;
    void pending.then((data) => {
      if (!data && printingsDataPromise === pending) printingsDataPromise = null;
    });
  }
  return printingsDataPromise;
}

/**
 * True when both card-data maps are already in memory, so a caller that needs
 * them can run without a fetch.
 *
 * `scryfall-data.json` is 36,748,238 bytes and `scryfall-printings.json` is
 * 39,541,979 bytes, and the two loaders above memoize at module scope — so this
 * is the difference between a free read and a 76 MB download-and-parse. It
 * exists so a PASSIVE surface can decline to be what triggers that: the
 * visual-pack panel measures curated drift on mount, and a user who opens
 * Preferences without having rendered a card must not pay for a measurement
 * they never asked for.
 *
 * WHICH SESSIONS REACH THE RESIDENT STATE, precisely, because the two maps are
 * NOT loaded together and the conjunction is much narrower than "has drawn a
 * card":
 *
 *  - `scryfall-data.json` is the common one. `fetchCardImageAsset` and
 *    `fetchCardImageAssetByOracleId` each await `loadScryfallData` and nothing
 *    else, so any rendered card image has it.
 *  - `scryfall-printings.json` is reached only on CONDITIONAL paths: the
 *    placeholder fallback in `resolveImageAssetWithPrintingFallback` (only when
 *    the resolved art is a placeholder), `resolveStrategyInBackground` in
 *    `useCardImage` (only inside the `artChain.length > 0` branch), and the
 *    deck-pinned lookup (only when a `sourcePrinting` is set).
 *
 * So a user on the DEFAULT empty art chain, with no overrides and no
 * `(SET) NUM` annotations in their decks, can play a whole game and still have
 * the printings map unloaded — and this stays false for the life of the tab.
 * That user is not exotic; `PackSelector` ships a `curatedDefaultNote` written
 * for exactly them. For them the drift badge does not appear on mount, and
 * becomes available only when something else loads printings or when they
 * select the curated option, which plans a membership and loads both.
 *
 * The conjunction is still the right test and must NOT be widened:
 * `planCuratedPack` needs both maps, so either one missing means measuring
 * would fetch. Its failure direction is the safe one — it declines to measure
 * rather than declining to protect.
 *
 * Same shape as `isLocaleArtReady`: a synchronous predicate over this module's
 * own resolved state, doubling as the caller's "do I need to load?" gate.
 */
export function isCardDataResident(): boolean {
  return scryfallDataResolved !== null && printingsDataResolved !== null;
}

function loadTokenImagesData(): Promise<TokenImagesDataMap | null> {
  if (!tokenImagesDataPromise) {
    tokenImagesDataPromise = fetch(__SCRYFALL_TOKEN_IMAGES_URL__)
      .then((r) => r.json() as Promise<TokenImagesDataMap>)
      .catch(() => null);
  }
  return tokenImagesDataPromise;
}

/**
 * Per-locale card-art map: English Scryfall printing id → the same printing's
 * exact localized face URLs (`scryfall-images.v2.<lng>.json`, generated by
 * `scripts/gen-scryfall-locale-images.sh` by joining MTGJSON `foreignData`
 * to Scryfall's all-cards bulk export).
 *
 * Only one locale is resolved at a time — the UI renders in exactly one
 * language — mirroring how `scryfallDataResolved` holds a single module-global
 * map rather than threading data through every call site.
 */
interface LocalizedArtEntry {
  id: string;
  faces: Array<{ small?: string; normal?: string; art_crop?: string }>;
}

function isLocalizedArtEntry(value: unknown): value is LocalizedArtEntry {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const entry = value as Partial<LocalizedArtEntry>;
  return typeof entry.id === "string"
    && Array.isArray(entry.faces)
    && entry.faces.length > 0
    && entry.faces.every((face) =>
      Boolean(face)
      && typeof face.small === "string"
      && typeof face.normal === "string"
      && typeof face.art_crop === "string"
    );
}

function parseLocalizedArtMap(value: unknown): Map<string, LocalizedArtEntry> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return new Map();
  const map = new Map<string, LocalizedArtEntry>();
  for (const [key, entry] of Object.entries(value)) {
    if (isLocalizedArtEntry(entry)) map.set(key, entry);
  }
  return map;
}

let localeArtResolved: { lang: string; map: Map<string, LocalizedArtEntry> } | null = null;
const localeArtPromises = new Map<string, Promise<Map<string, LocalizedArtEntry>>>();
/**
 * The locale the app currently wants. Set synchronously on every request so a
 * slow fetch for a language the user has already switched away from cannot
 * install itself over the newer one (de → fr → de resolves out of order).
 */
let desiredArtLang = "en";

/**
 * True when `localizeImageUrl` already resolves in `lang` — i.e. the installed
 * map matches it. English is ready only when *no* map is installed, because
 * English is defined by the absence of one: reporting it ready unconditionally
 * would let a de → en switch skip the load that clears the German map, leaving
 * `localizeImageUrl` serving German art under an English key.
 *
 * This doubles as the caller's "do I need to load?" gate, so the two meanings
 * must not diverge.
 */
export function isLocaleArtReady(lang: string): boolean {
  return lang === "en" ? localeArtResolved === null : localeArtResolved?.lang === lang;
}

/**
 * Load the card-art map for `lang`. English clears any resolved map and resolves
 * immediately. A missing file (404 for a locale not yet published) resolves to an
 * empty map, so every card falls back to English art — localized art is
 * best-effort display data, never a hard dependency.
 */
export function loadLocaleArt(lang: string): Promise<Map<string, LocalizedArtEntry>> {
  desiredArtLang = lang;
  if (lang === "en") {
    localeArtResolved = null;
    return Promise.resolve(new Map<string, LocalizedArtEntry>());
  }
  let promise = localeArtPromises.get(lang);
  if (!promise) {
    // Same shape as `ensureCardLocale` (engineRuntime.ts), the content-sidecar
    // sibling of this loader: an async IIFE with an early return on !ok, so the
    // "missing file" path yields an empty map without widening the value type.
    promise = (async () => {
      const resp = await fetch(
        __SCRYFALL_IMAGES_LOCALE_URL_TEMPLATE__.replace("{lng}", lang),
      );
      if (!resp.ok) return new Map<string, LocalizedArtEntry>();
      return parseLocalizedArtMap(await resp.json());
    })().catch(() => new Map<string, LocalizedArtEntry>());
    localeArtPromises.set(lang, promise);
  }
  return promise.then((map) => {
    if (desiredArtLang === lang) localeArtResolved = { lang, map };
    return map;
  });
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
  await loadScryfallData();
  const entry = lookupEntryByName(cardName);
  if (!entry) return [];
  return getCardPrintings(entry.oracle_id);
}

export function resolvePrintingImageUrl(
  printing: PrintingEntry,
  faceIndex: number,
  size: ImageSize,
): string | null {
  const face = printing.faces[faceIndex] ?? printing.faces[0];
  const url = localFaceImageUrl(face, size) ?? null;
  return url && !isPlaceholderImageUrl(url) ? url : null;
}

export function findPrintingById(
  printings: PrintingEntry[],
  scryfallId: string,
): PrintingEntry | undefined {
  return printings.find((p) => p.id === scryfallId);
}

/** Pick the earliest printing by release date, breaking ties by collector number. */
export function pickOldestPrinting(printings: PrintingEntry[]): PrintingEntry {
  return [...printings].sort((a, b) => {
    const byDate = a.released_at.localeCompare(b.released_at);
    if (byDate !== 0) return byDate;
    return a.collector_number.localeCompare(b.collector_number, undefined, {
      numeric: true,
    });
  })[0];
}

export function resolveOracleIdSync(cardName: string): string | null {
  if (!scryfallDataResolved) return null;
  return lookupEntryByName(cardName)?.oracle_id ?? null;
}

export interface AlternateCardFace {
  name: string;
  faceIndex: number;
  side: "front" | "back";
}

const PHYSICAL_MULTI_FACE_LAYOUTS = new Set([
  "transform",
  "modal_dfc",
  "meld",
  "double_faced_token",
  "reversible_card",
]);

export function resolveAlternateCardFaceSync(
  cardName: string,
): AlternateCardFace | null | undefined {
  if (!scryfallDataResolved) return undefined;
  const entry = lookupEntryByName(cardName);
  if (!entry) return undefined;
  if (entry.faces.length < 2 || !entry.layout || !PHYSICAL_MULTI_FACE_LAYOUTS.has(entry.layout)) {
    return null;
  }

  const normalizedName = normalizeCardName(cardName).toLowerCase();
  const currentFaceIndex = entry.face_names.indexOf(normalizedName);
  const activeFaceIndex = currentFaceIndex >= 0 ? currentFaceIndex : 0;
  const alternateFaceIndex = activeFaceIndex === 0 ? 1 : 0;
  const displayFaceNames = entry.name.split(" // ");
  return {
    name: displayFaceNames[alternateFaceIndex] ?? entry.face_names[alternateFaceIndex],
    faceIndex: alternateFaceIndex,
    side: alternateFaceIndex === 0 ? "front" : "back",
  };
}

/**
 * Resolve the numeric Scryfall face index for an engine-reported `faceName`.
 *
 * The printings/art-strategy path (`resolvePrintingImageUrl`) keys off a raw
 * numeric `faceIndex`, but for a DFC/MDFC the engine only knows the *active
 * face's name* — and for an MDFC cast as its back face, `transformed` stays
 * `false`, so `cardImageLookup` yields `faceIndex: 0` (the front). This helper
 * recovers the correct index by matching `faceName` against the entry's
 * `face_names` array, the same way the canonical oracle-id image path does.
 * Returns `null` when the data isn't loaded yet or the face can't be matched,
 * so callers fall back to their provided `faceIndex`.
 */
export function resolveFaceIndexSync(
  oracleId: string,
  faceName: string | undefined,
): number | null {
  if (!scryfallDataResolved || !faceName) return null;
  const entry = scryfallDataResolved[oracleId.toLowerCase()];
  if (!entry) return null;
  const idx = entry.face_names.indexOf(faceName.toLowerCase());
  return idx >= 0 ? idx : null;
}

export function isCardImageRotatedSync(oracleId: string, cardName: string): boolean {
  if (!scryfallDataResolved) return false;
  const entry = scryfallDataResolved[oracleId.toLowerCase()]
    ?? lookupEntryByName(cardName);
  return isSidewaysLayout(entry?.layout);
}

/** Kamigawa-style flip cards (Scryfall `layout: "flip"`) print both halves in a
 * single image, the alternate half rotated 180°. The preview lets the user spin
 * the image to read that half; this reports whether a card is that layout. */
export function isCardImageFlipLayoutSync(oracleId: string, cardName: string): boolean {
  if (!scryfallDataResolved) return false;
  const entry = scryfallDataResolved[oracleId.toLowerCase()]
    ?? lookupEntryByName(cardName);
  return isFlipLayout(entry?.layout);
}

const SCRYFALL_DELAY_MS = 100;
const MAX_RETRIES = 3;
const BASE_BACKOFF_MS = 1000;

const IMAGE_SIZES = ["small", "normal", "large", "art_crop"] as const;

export type ImageSize = (typeof IMAGE_SIZES)[number];

/**
 * Intrinsic pixel width of each Scryfall image variant, measured from the JPEG
 * SOF markers of the assets themselves. Only the two rungs the `srcset` ladder
 * offers are listed — see `deriveImageUrl` for why `large` is not one of them.
 */
export const IMAGE_SIZE_WIDTHS: Record<"small" | "normal", number> = {
  small: 146,
  normal: 488,
};

/**
 * A Scryfall CDN image URL carries exactly five path segments, the first of
 * which is the size variant:
 *
 *   `https://cards.scryfall.io/normal/front/w/r/war-room.jpg?1783905318`
 *                              └─ 1 ──┘└─ 2 ┘└3┘└4┘└──── 5 ────┘
 *
 * Splitting (rather than `new URL()`) is deliberate: this runs on every image
 * the client renders, including `""` for face-down cards and bare filenames
 * from test mocks, and `new URL()` *throws* on both. `URL.parse()` returns null
 * instead of throwing but is Chrome 126+/Safari 18+ only — narrower than the
 * browsers this ladder's `200px` fallback exists to serve. String splitting
 * has no failure mode: unrecognized input simply isn't size-derivable.
 *
 * Returns null for anything that is not a five-segment sized URL, which
 * correctly excludes `CARD_BACK_URL` (four segments) and the
 * `errors.scryfall.com/soon.jpg` placeholder (one) — the latter must stay
 * byte-identical or `isPlaceholderImageUrl`'s `===` stops gating the
 * printing-fallback chain.
 */
function splitSizedImageUrl(
  url: string | null | undefined,
): { scheme: string; segments: string[]; size: ImageSize } | null {
  if (!url) return null;
  const [scheme, rest, ...extra] = url.split("://");
  if (rest === undefined || extra.length > 0) return null;
  // `segments[0]` is the host; the five path segments follow it.
  const segments = rest.split("/");
  if (segments.length !== 6) return null;
  const size = segments[1];
  if (!IMAGE_SIZES.includes(size as ImageSize)) return null;
  return { scheme, segments, size: size as ImageSize };
}

/** The size variant a Scryfall image URL serves, or null if it isn't one. */
export function imageUrlSize(url: string | null | undefined): ImageSize | null {
  return splitSizedImageUrl(url)?.size ?? null;
}

/**
 * Rewrite a Scryfall image URL to a different size variant, returning the input
 * unchanged when it isn't a size-derivable URL. The query string rides on the
 * final path segment, so it is preserved without special handling.
 */
export function deriveImageUrl(url: string, size: ImageSize): string {
  const parsed = splitSizedImageUrl(url);
  if (!parsed) return url;
  const segments = [...parsed.segments];
  segments[1] = size;
  return `${parsed.scheme}://${segments.join("/")}`;
}

/**
 * Rewrite a Scryfall image URL to the active locale's printing of the same card,
 * returning the input unchanged when there is no locale loaded, the URL is not a
 * sized Scryfall URL, or that printing has no localized sibling.
 *
 * Reusing `splitSizedImageUrl` is load-bearing, not stylistic. It rejects
 * `CARD_BACK_URL` (four path segments) and the `errors.scryfall.com/soon.jpg`
 * placeholder (one) — and the placeholder MUST come back byte-identical or
 * `isPlaceholderImageUrl`'s `===` stops gating the printing-fallback chain,
 * silently disabling art fallback for every card with missing art. A regex that
 * merely found a UUID in the path would rewrite both.
 *
 * The trailing `?<timestamp>` is dropped: it is the *English* printing's
 * cache-buster and means nothing for a different Scryfall object. Omitting it
 * costs only the ability to notice a re-scan of that art.
 */
function localizeImageUrl(url: string): string {
  if (!localeArtResolved) return url;
  const parsed = splitSizedImageUrl(url);
  if (!parsed) return url;
  // segments: [host, size, face, id[0], id[1], "<id>.jpg?<timestamp>"]
  const filename = parsed.segments[5];
  // A UUID contains no `.`, so the first dot always ends the id.
  const dot = filename.indexOf(".");
  if (dot < 0) return url;
  const localized = localeArtResolved.map.get(filename.slice(0, dot));
  if (!localized) return url;
  const faceIndex = parsed.segments[2] === "back" ? 1 : 0;
  const face = localized.faces[faceIndex] ?? localized.faces[0];
  if (!face) return url;
  const localizedUrl = parsed.size === "art_crop"
    ? face.art_crop
    : parsed.size === "small"
      ? face.small
      : face.normal;
  return localizedUrl ?? url;
}

/**
 * Resolve one stored local face to a URL for the requested size.
 *
 * `scryfall-data.json` stores only `normal` and `art_crop` per face; `small` is
 * derived from the `normal` URL. `large` deliberately collapses to `normal`:
 * the 672px asset is ~+51 KB and ~+90% more decoded bitmap per card, and the
 * only `large` consumer (`AttachmentFan`) renders at 176-384 CSS px where it
 * would buy nothing — on a platform with this app's documented Safari/iOS OOM
 * history. Do not "fix" this without measuring that memory cost.
 */
function localFaceImageUrl(
  face: ScryfallImageFace | undefined,
  size: ImageSize,
): string | undefined {
  if (!face) return undefined;
  // Localization is applied here, at the single funnel every stored-URL path
  // reaches (`resolveImageUrl`, `resolvePrintingImageUrl`, and the local token
  // lookup), rather than in each caller. Size derivation and localization
  // commute — `deriveImageUrl` rewrites segment 1, `localizeImageUrl` rewrites
  // segments 3-5 — so the order below is immaterial.
  if (size === "art_crop") {
    return face.art_crop ? localizeImageUrl(face.art_crop) : undefined;
  }
  if (size === "small") {
    const url = face.small ?? (face.normal ? deriveImageUrl(face.normal, "small") : undefined);
    return url ? localizeImageUrl(url) : undefined;
  }
  return face.normal ? localizeImageUrl(face.normal) : undefined;
}

export interface CardImageAsset {
  src: string;
  isRotated: boolean;
  source: CardImageSource;
  rungs?: ImageRungs;
  semantic: {
    oracleId?: string;
    englishPrintingId?: string;
    faceIndex: number;
    alias?: string;
  };
}

function remoteImageSource(src: string, size: ImageSize): { source: CardImageSource; rungs?: ImageRungs } {
  const rungs = size === "art_crop" || imageUrlSize(src) === null
    ? undefined
    : { small: deriveImageUrl(src, "small"), normal: deriveImageUrl(src, "normal") };
  return { source: { kind: "remote", src, rungs }, rungs };
}

function isSidewaysLayout(layout: string | undefined): boolean {
  return layout === "split";
}

function isFlipLayout(layout: string | undefined): boolean {
  return layout === "flip";
}

export interface ScryfallCard {
  id?: string;
  oracle_id?: string;
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
  Archenemy: null,
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

/** Strip combining marks so "Eomer" matches "Éomer" in local image data. */
function foldDiacritics(value: string): string {
  return value.normalize("NFD").replace(/\p{M}/gu, "");
}

function buildFoldedNameIndex(data: ScryfallDataMap): Map<string, string> {
  const index = new Map<string, string>();
  for (const key of Object.keys(data)) {
    const folded = foldDiacritics(key);
    if (!index.has(folded)) {
      index.set(folded, key);
    }
  }
  return index;
}

function resolveNameLookupKey(name: string): string {
  const normalized = normalizeCardName(name).toLowerCase();
  if (!scryfallDataResolved) return normalized;
  if (scryfallDataResolved[normalized]) return normalized;
  const folded = foldDiacritics(normalized);
  const foldedHit = scryfallFoldedNameIndex?.get(folded);
  if (foldedHit) return foldedHit;
  // A combined multi-face name ("Front // Back", or a hand-typed glued
  // "Front//Back") is not itself an export key — multi-face cards are keyed by
  // oracle id, spaced display name, and front-face name. When the combined form
  // misses, fall back to the front face so the card still resolves to its
  // entry. A single card whose own name contains "//" (e.g. "SP//dr, Piloted by
  // Peni") is a primary key and already returned above, so it never splits here.
  if (normalized.includes("//")) {
    const frontFace = normalized.split("//")[0].trim();
    if (frontFace && frontFace !== normalized) {
      if (scryfallDataResolved[frontFace]) return frontFace;
      const frontFolded = scryfallFoldedNameIndex?.get(foldDiacritics(frontFace));
      if (frontFolded) return frontFolded;
    }
  }
  return normalized;
}

function lookupEntryByName(name: string): ScryfallDataEntry | undefined {
  if (!scryfallDataResolved) return undefined;
  return scryfallDataResolved[resolveNameLookupKey(name)];
}

export async function fetchCardData(cardName: string): Promise<ScryfallCard> {
  await loadScryfallData();
  const entry = lookupEntryByName(cardName);
  if (!entry) {
    throw new Error(`Card not in local data: "${normalizeCardName(cardName)}"`);
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

/**
 * Engine-authoritative fields for a card-search result. The engine owns these
 * (mana value, color identity, legality) — see `crates/engine/src/database/search.rs`.
 */
export interface LocalSearchCardOverrides {
  oracleId?: string;
  name: string;
  cmc: number;
  colorIdentity: string[];
  legalities: Record<string, string>;
}

function localFaceImageUris(face: ScryfallImageFace): Record<string, string> | undefined {
  const imageUris: Record<string, string> = {};
  const artCrop = localFaceImageUrl(face, "art_crop");
  const normal = localFaceImageUrl(face, "normal");
  const small = localFaceImageUrl(face, "small");
  const large = localFaceImageUrl(face, "large");
  if (artCrop) imageUris.art_crop = artCrop;
  if (normal) imageUris.normal = normal;
  if (small) imageUris.small = small;
  if (large) imageUris.large = large;
  return Object.keys(imageUris).length > 0 ? imageUris : undefined;
}

/**
 * Build a display `ScryfallCard` for an engine search result. Rules data comes
 * from the engine (the `overrides`); presentation data — artwork, printed type
 * line, colors, mana cost, keywords — is hydrated from the already-loaded local
 * image map, keyed by `oracleId` (falling back to name). Requires
 * `loadScryfallData()` to have resolved; returns a usable card even when the
 * image entry is missing (the grid renders a text-tile fallback).
 */
export function buildLocalSearchCard(overrides: LocalSearchCardOverrides): ScryfallCard {
  const entry =
    (overrides.oracleId
      ? scryfallDataResolved?.[overrides.oracleId.toLowerCase()]
      : undefined) ?? scryfallDataResolved?.[overrides.name.toLowerCase()];
  const face = entry?.faces[0];
  // Use the same stored-URL funnel as every card sink so deck-builder search
  // preserves exact small/localized URLs rather than reconstructing CDN paths.
  return {
    name: entry?.name ?? overrides.name,
    mana_cost: entry?.mana_cost ?? "",
    cmc: overrides.cmc,
    type_line: entry?.type_line ?? "",
    colors: entry?.colors ?? [],
    color_identity: overrides.colorIdentity,
    keywords: entry?.keywords ?? [],
    legalities: overrides.legalities,
    image_uris: face ? localFaceImageUris(face) : undefined,
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
  await loadScryfallData();
  const entry = lookupEntryByName(cardName);
  if (!entry) {
    throw new Error(`Card image not in local data: "${normalizeCardName(cardName)}"`);
  }
  const name = resolveNameLookupKey(cardName);
  return resolveImageAssetWithPrintingFallback(entry, faceIndex, size, name);
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
  return resolveImageAssetWithPrintingFallback(entry, faceIndex, size, entry.name);
}

function resolveImageAsset(
  entry: ScryfallDataEntry,
  faceIndex: number,
  size: ImageSize,
  diagnosticName: string,
): CardImageAsset {
  const src = resolveImageUrl(entry, faceIndex, size, diagnosticName);
  const remote = remoteImageSource(src, size);
  return {
    src,
    isRotated: isSidewaysLayout(entry.layout),
    ...remote,
    semantic: {
      oracleId: entry.oracle_id.toLowerCase(),
      faceIndex,
      alias: diagnosticName.toLowerCase().normalize("NFC"),
    },
  };
}

function isPlaceholderImageUrl(url: string): boolean {
  return url === "https://errors.scryfall.com/soon.jpg";
}

function resolvePrintingFallback(
  oracleId: string,
  faceIndex: number,
  size: ImageSize,
): { id: string; url: string } | null {
  const printings = printingsDataResolved?.[oracleId.toLowerCase()] ?? [];
  for (const printing of printings) {
    if (printing.set === "plst") continue;
    const url = resolvePrintingImageUrl(printing, faceIndex, size);
    if (url && !isPlaceholderImageUrl(url)) return { id: printing.id, url };
  }
  return null;
}

async function resolveImageAssetWithPrintingFallback(
  entry: ScryfallDataEntry,
  faceIndex: number,
  size: ImageSize,
  diagnosticName: string,
): Promise<CardImageAsset> {
  const asset = resolveImageAsset(entry, faceIndex, size, diagnosticName);
  if (!isPlaceholderImageUrl(asset.src)) return asset;

  await loadPrintingsData();
  const fallback = resolvePrintingFallback(entry.oracle_id, faceIndex, size);
  if (!fallback) return asset;
  return {
    ...asset,
    src: fallback.url,
    ...remoteImageSource(fallback.url, size),
    semantic: { ...asset.semantic, englishPrintingId: fallback.id.toLowerCase() },
  };
}

function resolveImageUrl(
  entry: ScryfallDataEntry,
  faceIndex: number,
  size: ImageSize,
  diagnosticName: string,
): string {
  const face = entry.faces[faceIndex] ?? entry.faces[0];
  const url = localFaceImageUrl(face, size);
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

export interface TokenImageAssetByRef {
  src: string;
  faceIndex: number;
}

export async function fetchTokenImageAssetByRef(
  ref: TokenImageRef,
  size: ImageSize = "normal",
): Promise<TokenImageAssetByRef | null> {
  const data = await loadTokenImagesData();
  if (!data) return null;

  const idEntry = data[`scryfall:${ref.scryfall_id.toLowerCase()}`];
  if (idEntry) {
    const faceIndex = ref.face_name
      ? Math.max(0, idEntry.face_names.indexOf(ref.face_name.toLowerCase()))
      : 0;
    return { src: resolveImageUrl(idEntry, faceIndex, size, idEntry.name), faceIndex };
  }

  if (ref.scryfall_oracle_id) {
    const faceKey = ref.face_name?.toLowerCase() ?? "";
    const oracleEntry = data[`oracle:${ref.scryfall_oracle_id.toLowerCase()}:${faceKey}`];
    if (oracleEntry) {
      const faceIndex = ref.face_name
        ? Math.max(0, oracleEntry.face_names.indexOf(ref.face_name.toLowerCase()))
        : 0;
      return { src: resolveImageUrl(oracleEntry, faceIndex, size, oracleEntry.name), faceIndex };
    }
  }

  return null;
}

export async function fetchTokenImageByRef(
  ref: TokenImageRef,
  size: ImageSize = "normal",
): Promise<string | null> {
  return (await fetchTokenImageAssetByRef(ref, size))?.src ?? null;
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
  return localFaceImageUrl(face, size) ?? null;
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

/** Get the best image URI for a card (handles double-faced cards). */
export function getCardImageSmall(card: ScryfallCard): string {
  return card.image_uris?.small
    ?? card.card_faces?.[0]?.image_uris?.small
    ?? "";
}
