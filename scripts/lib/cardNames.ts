// Card-name resolution for the bug-report extractor.
//
// Discord users refer to cards three ways the naive substring scan misses:
//   1. `[[Card Name]]` bracket syntax (often truncated: `[[welcome to...]]`).
//   2. Scryfall links: https://scryfall.com/card/rex/7/welcome-to-jurassic-park
//   3. Punctuation the card-data key spells differently (the REX Saga's key is
//      literally `welcome to . . .` with spaced dots + an ellipsis).
//
// `card-data.json` keys are the canonical form used everywhere downstream. This
// module normalizes both sides for matching while always returning the ORIGINAL
// canonical key, so nothing downstream has to learn the normalized form.

const CARD_DATA_PATH = "client/public/card-data.json";

export interface CardNameIndex {
  /** Raw lowercased `card-data.json` keys (the canonical form). */
  rawKeys: Set<string>;
  /** normalizeCardName(key) → canonical key, minus ambiguous collisions. */
  byNormalized: Map<string, string>;
  /** Same, but with every space removed (recovers apostrophe-elided input). */
  byNormalizedNoSpace: Map<string, string>;
  /** Sorted normalized keys, for word-boundary prefix matching. */
  sortedNormalized: string[];
}

// Normalize a card name for fuzzy matching: lowercase, then collapse every run
// of non-alphanumeric characters (dots, ellipses, `//`, commas, apostrophes,
// hyphens) to a single space. Unicode letters/numbers are preserved so accented
// names (Éomer) survive — JS toLowerCase() matches the Rust-lowercased keys.
//
// Examples (all map a Discord reference onto its card-data key):
//   "welcome to . . ."      → "welcome to"        (the card-data key)
//   "welcome to..."         → "welcome to"        (truncated bracket text)
//   "SP//dr, Piloted by Peni" → "sp dr piloted by peni"
//   "welcome-to-jurassic-park" → "welcome to jurassic park"  (URL slug)
export function normalizeCardName(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^\p{L}\p{N}]+/gu, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function normalizeNoSpace(name: string): string {
  return normalizeCardName(name).replace(/ /g, "");
}

// Build the index from raw card-data keys. Pure (no I/O) so it is unit-testable.
export function buildCardNameIndex(rawKeys: Iterable<string>): CardNameIndex {
  const keys = new Set(rawKeys);
  const byNormalized = new Map<string, string>();
  const byNormalizedNoSpace = new Map<string, string>();
  const ambiguous = new Set<string>();
  const ambiguousNoSpace = new Set<string>();

  for (const rawKey of keys) {
    const norm = normalizeCardName(rawKey);
    // Never index an empty normalized form — cards like "_____" collapse to ""
    // and would otherwise swallow every unmatched `[[...]]` reference.
    if (norm === "") continue;

    const existing = byNormalized.get(norm);
    if (existing === undefined) {
      byNormalized.set(norm, rawKey);
    } else if (existing !== rawKey) {
      // Two distinct keys share a normalized form (e.g. "lava axe" / "lava,
      // axe"). Mark ambiguous so a fuzzy hit never silently picks one; an exact
      // raw-key match in resolveCardReference is still allowed to win.
      ambiguous.add(norm);
    }

    const noSpace = normalizeNoSpace(rawKey);
    if (noSpace !== "") {
      const existingNoSpace = byNormalizedNoSpace.get(noSpace);
      if (existingNoSpace === undefined) {
        byNormalizedNoSpace.set(noSpace, rawKey);
      } else if (existingNoSpace !== rawKey) {
        ambiguousNoSpace.add(noSpace);
      }
    }
  }

  for (const norm of ambiguous) byNormalized.delete(norm);
  for (const noSpace of ambiguousNoSpace) byNormalizedNoSpace.delete(noSpace);

  return {
    rawKeys: keys,
    byNormalized,
    byNormalizedNoSpace,
    sortedNormalized: [...byNormalized.keys()].sort(),
  };
}

// Resolve a single explicit card reference (bracket inner text or URL slug) to
// canonical card-data keys. Returns [] on no match, [key] on a hit, or two keys
// when the reference is a double-faced card's combined name ("A B" where "A"
// and "B" are each a face key). Only call this for EXPLICIT references — never
// the free-text substring scan — because the DFC split is greedy.
export function resolveCardReference(rawQuery: string, index: CardNameIndex): string[] {
  const lowerRaw = rawQuery.toLowerCase().trim();
  // Exact raw-key hit wins even over an ambiguous normalized collision.
  if (index.rawKeys.has(lowerRaw)) return [lowerRaw];

  const norm = normalizeCardName(rawQuery);
  if (norm === "") return [];

  const exact = index.byNormalized.get(norm);
  if (exact !== undefined) return [exact];

  const noSpace = index.byNormalizedNoSpace.get(normalizeNoSpace(rawQuery));
  if (noSpace !== undefined) return [noSpace];

  // Truncated reference (`[[lightning bo...]]`): a character prefix, but only
  // when exactly ONE card name starts with it. "lightning bo" → Lightning Bolt;
  // "lightning b" is ambiguous (Bolt + Blast) and resolves to nothing. Exact
  // matches were handled above, so a query that is itself a full name never
  // gets hijacked by a longer one.
  const prefixHits = index.sortedNormalized.filter((k) => k.startsWith(norm));
  if (prefixHits.length === 1) {
    const key = index.byNormalized.get(prefixHits[0]);
    if (key !== undefined) return [key];
  }

  // Double-faced combined name: split at each word boundary and accept the first
  // split whose BOTH halves resolve to a face key.
  const words = norm.split(" ");
  for (let i = 1; i < words.length; i++) {
    const front = index.byNormalized.get(words.slice(0, i).join(" "));
    const back = index.byNormalized.get(words.slice(i).join(" "));
    if (front !== undefined && back !== undefined && front !== back) {
      return [front, back];
    }
  }

  return [];
}

const SCRYFALL_URL = /https?:\/\/scryfall\.com\/card\/[\w\d]+\/[\w\d]+(?:\/([\w-]+))?/gi;
const BRACKET_REF = /\[\[(.+?)\]\]/g;

// Extract every explicit card reference from a message and resolve to canonical
// keys. Covers `[[...]]` brackets and Scryfall card URLs (slug → card name).
export function extractCardReferences(text: string, index: CardNameIndex): string[] {
  const found = new Set<string>();

  for (const match of text.matchAll(BRACKET_REF)) {
    for (const key of resolveCardReference(match[1], index)) found.add(key);
  }

  for (const match of text.matchAll(SCRYFALL_URL)) {
    const slug = match[1];
    if (slug === undefined || slug === "") continue;
    for (const key of resolveCardReference(slug.replace(/-/g, " "), index)) found.add(key);
  }

  return [...found];
}

let indexCache: CardNameIndex | null = null;

export async function loadCardNameIndex(): Promise<CardNameIndex> {
  if (indexCache !== null) return indexCache;
  const file = Bun.file(CARD_DATA_PATH);
  const data = (await file.json()) as Record<string, unknown>;
  indexCache = buildCardNameIndex(Object.keys(data));
  return indexCache;
}
