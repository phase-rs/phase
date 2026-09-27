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

/**
 * The engine's canonical spelling of each distinct name in `names`, holding
 * only the names it spells differently. Rejects if the engine cannot answer.
 */
export async function canonicalNameMap(names: readonly string[]): Promise<Map<string, string>> {
  const distinct = [...new Set(names)];
  const renamed = new Map<string, string>();
  if (distinct.length === 0) return renamed;
  const answers = await getSharedAdapter().canonicalCardNames(distinct);
  if (!Array.isArray(answers) || answers.length !== distinct.length) {
    throw new Error("canonicalCardNames returned a list that does not match its input");
  }
  distinct.forEach((name, index) => {
    const canonical = answers[index];
    if (typeof canonical === "string" && canonical !== name) renamed.set(name, canonical);
  });
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
