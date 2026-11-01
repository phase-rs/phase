import { getSharedAdapter } from "../adapter/wasm-adapter";
import { deduplicateEntries, type DeckEntry, type ParsedDeck } from "./deckParser";

export function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

export function isNameEntryArray(v: unknown): boolean {
  return Array.isArray(v) && v.every((entry) => isObject(entry) && typeof entry.name === "string");
}

function isOptionalStringArray(v: unknown): boolean {
  return v === undefined || (Array.isArray(v) && v.every((entry) => typeof entry === "string"));
}

/**
 * True when `value` has `main`/`sideboard` as arrays of entries with a string
 * `name`, the other name-list fields as arrays of strings when present, and
 * `companion` as a string when present.
 */
export function isCanonicalizableDeck(value: unknown): value is ParsedDeck & Record<string, unknown> {
  if (!isObject(value)) return false;
  if (!isNameEntryArray(value.main) || !isNameEntryArray(value.sideboard)) return false;
  if (!isOptionalStringArray(value.commander)) return false;
  if (!isOptionalStringArray(value.signature_spell)) return false;
  if (!isOptionalStringArray(value.planar_deck)) return false;
  if (!isOptionalStringArray(value.scheme_deck)) return false;
  if (!isOptionalStringArray(value.sticker_sheets)) return false;
  if (value.companion !== undefined && typeof value.companion !== "string") return false;
  return true;
}

/** Every card name in `deck`, across all of its name fields. */
export function deckCardNames(deck: ParsedDeck): string[] {
  return [
    ...deck.main.map((entry) => entry.name),
    ...deck.sideboard.map((entry) => entry.name),
    ...(deck.commander ?? []),
    ...(deck.companion !== undefined ? [deck.companion] : []),
    ...(deck.signature_spell ?? []),
    ...(deck.planar_deck ?? []),
    ...(deck.scheme_deck ?? []),
    ...(deck.sticker_sheets ?? []),
  ];
}

const SET_ONLY_PRINTING_SUFFIX_RE = /^(.*\S)\s+\(([a-z0-9]+|[A-Z0-9]+)\)$/;

/** The base of `name` when it ends in a lowercase or uppercase set-only suffix. */
function baseNameWithoutSetOnlySuffix(name: string): string | null {
  const match = name.match(SET_ONLY_PRINTING_SUFFIX_RE);
  return match ? match[1] : null;
}

/**
 * The engine's canonical spelling of each distinct name in `names`, holding
 * only the names it spells differently. Rejects if the engine cannot answer.
 *
 * A name that carries a set-only printing suffix (`Name (SET)`) falls back to
 * the base name when the suffixed spelling does not resolve but the base does.
 * The suffixed spelling is checked first, so a real parenthesized card name
 * (`Hazmat Suit (Used)`) is kept whenever the engine resolves it.
 */
export async function canonicalNameMap(names: readonly string[]): Promise<Map<string, string>> {
  const distinct = [...new Set(names)];
  const renamed = new Map<string, string>();
  if (distinct.length === 0) return renamed;

  const bases = new Map<string, string>();
  for (const name of distinct) {
    const base = baseNameWithoutSetOnlySuffix(name);
    if (base !== null) bases.set(name, base);
  }
  const candidates = [...new Set([...distinct, ...bases.values()])];
  const answers = await getSharedAdapter().canonicalCardNames(candidates);
  if (!Array.isArray(answers) || answers.length !== candidates.length) {
    throw new Error("canonicalCardNames returned a list that does not match its input");
  }
  const canonicalByName = new Map(candidates.map((name, index) => [name, answers[index]]));

  for (const name of distinct) {
    const canonical = canonicalByName.get(name);
    if (typeof canonical === "string") {
      if (canonical !== name) renamed.set(name, canonical);
      continue;
    }
    const base = bases.get(name);
    if (base === undefined) continue;
    const canonicalBase = canonicalByName.get(base);
    if (typeof canonicalBase === "string") renamed.set(name, canonicalBase);
  }
  return renamed;
}

/**
 * `deck` with each name in `renamed` replaced. Main and sideboard entries that
 * end up with the same name are merged; only fields `deck` has are written.
 */
export function applyCanonicalNames(
  deck: ParsedDeck,
  renamed: ReadonlyMap<string, string>,
): ParsedDeck {
  if (renamed.size === 0) return deck;
  const rename = (name: string) => renamed.get(name) ?? name;
  const renameEntries = (entries: DeckEntry[]) =>
    deduplicateEntries(entries.map((entry) => ({ ...entry, name: rename(entry.name) })));
  const next: ParsedDeck = {
    ...deck,
    main: renameEntries(deck.main),
    sideboard: renameEntries(deck.sideboard),
  };
  if (deck.commander) next.commander = deck.commander.map(rename);
  if (deck.companion !== undefined) next.companion = rename(deck.companion);
  if (deck.signature_spell) next.signature_spell = deck.signature_spell.map(rename);
  if (deck.planar_deck) next.planar_deck = deck.planar_deck.map(rename);
  if (deck.scheme_deck) next.scheme_deck = deck.scheme_deck.map(rename);
  if (deck.sticker_sheets) next.sticker_sheets = deck.sticker_sheets.map(rename);
  return next;
}

/**
 * `deck` with the engine's canonical names, or `deck`
 * unchanged when the engine cannot answer (its names still resolve as typed,
 * per `deck_validation.rs::slash_spellings_are_known_and_share_one_copy_count`
 * / `deck_loading.rs::resolve_names_groups_slash_spellings_of_one_card`).
 */
export async function canonicalizeDeckNames(deck: ParsedDeck): Promise<ParsedDeck> {
  try {
    return applyCanonicalNames(deck, await canonicalNameMap(deckCardNames(deck)));
  } catch {
    return deck;
  }
}
