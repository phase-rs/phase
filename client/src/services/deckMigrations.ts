/**
 * Saved-deck migrations run from `App.tsx`.
 *
 * The legacy `loadSavedDeck` helper used to call `repairParsedDeck` and write
 * the repaired JSON back to localStorage as a side effect of every read. That
 * pattern was a React-rule violation (writes during render via `DeckTile`'s
 * read helpers) AND interacted catastrophically with cloud sync: the
 * `Storage.prototype` watcher correctly flagged every render-time write as a
 * profile change, which produced a CDC echo, which produced a remote-apply on
 * peer tabs, which reloaded those tabs, which re-ran the render, which wrote
 * again — a two-tab ping-pong reload loop.
 *
 * The fix is to do the repair exactly once on app boot, with the storage
 * watcher suppressed so the migration write is not surfaced to cloud sync as
 * a user-initiated change. Subsequent reads return the (now persisted)
 * repaired form straight from localStorage with no side effect.
 */
import { repairParsedDeck, type ParsedDeck } from "./deckParser";
import {
  captureSavedDeck,
  listSavedDeckNames,
  loadDeckOrigins,
  notifySavedDeckRewritten,
  savedDeckUnchanged,
  writeSavedDeckData,
  STORAGE_KEY_PREFIX,
  type SavedDeckRewrite,
} from "../constants/storage";
import { withSavedDeckLibraryOrSkip } from "./savedDeckTransaction";
import { withStorageWatchSuppressed } from "./cloudSync/storageWatcher";
import { projectSavedDeckSpecialSlots } from "./savedDeckProjection";
import {
  applyCanonicalNames,
  canonicalNameMap,
  deckCardNames,
  isCanonicalizableDeck,
  isNameEntryArray,
  isObject,
} from "./canonicalCardNames";

function isNullableStringArray(v: unknown): boolean {
  return v === undefined || v === null || (Array.isArray(v) && v.every((entry) => typeof entry === "string"));
}

/**
 * True when `repairParsedDeck` and `projectSavedDeckSpecialSlots` can read `deck` without
 * throwing: `main`/`sideboard` as arrays of objects with a string `name`, and the optional
 * name-list fields absent, `null`, or an array of strings. Looser than `isCanonicalizableDeck`
 * about `null`, which that guard rejects — legacy records written before 5093be87c stored
 * `commander: null`, so it is as safe to migrate as an absent field.
 */
function isRepairableDeckRecord(value: unknown): value is ParsedDeck & Record<string, unknown> {
  if (!isObject(value)) return false;
  if (!isNameEntryArray(value.main) || !isNameEntryArray(value.sideboard)) return false;
  if (!isNullableStringArray(value.commander)) return false;
  if (!isNullableStringArray(value.signature_spell)) return false;
  if (!isNullableStringArray(value.planar_deck)) return false;
  if (!isNullableStringArray(value.scheme_deck)) return false;
  if (!isNullableStringArray(value.sticker_sheets)) return false;
  if (value.companion !== undefined && value.companion !== null && typeof value.companion !== "string") return false;
  return true;
}

/**
 * Walk every saved deck, repair the JSON of each that `isRepairableDeckRecord` accepts, and
 * persist the repaired form when it differs from what's on disk. Idempotent: a second call is
 * effectively a no-op (each deck's repair is already on disk).
 *
 * Safe to call multiple times; cheap when nothing needs repair (a JSON parse
 * + structural compare per deck, no writes).
 */
export function migrateSavedDecks(): void {
  const repairs: Array<[string, string]> = [];

  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (!key?.startsWith(STORAGE_KEY_PREFIX)) continue;
    const raw = localStorage.getItem(key);
    if (!raw) continue;

    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      continue;
    }
    // A malformed record is left untouched: `repairParsedDeck` assumes this shape, and one bad
    // record must not abort the repair of every other saved deck.
    if (!isRepairableDeckRecord(parsed)) continue;
    const repaired = projectSavedDeckSpecialSlots(parsed, repairParsedDeck(parsed));
    const repairedRaw = JSON.stringify({ ...parsed, ...repaired });
    if (repairedRaw !== raw) repairs.push([key, repairedRaw]);
  }

  if (repairs.length === 0) return;
  // Suppress the watcher: these writes are an internal migration, not a
  // user-initiated profile change, and pushing them to cloud sync would
  // mark every device as dirty on boot.
  withStorageWatchSuppressed(() => {
    for (const [key, value] of repairs) localStorage.setItem(key, value);
  });
}

/**
 * Rewrite saved decks' card names to the engine's canonical spelling. Needs
 * the loaded card database, so `App.tsx` runs it once the card data is ready.
 * Feed-owned decks are skipped: feed sync rewrites them from the feed.
 * A deck is written only if it still holds what was read before the engine
 * call, under the saved-deck library lock and, like `migrateSavedDecks`,
 * with the storage watcher suppressed. Each deck written is announced with
 * `SAVED_DECK_REWRITTEN_EVENT` in the same transaction, so a deck builder open in this window
 * keeps it as its Save baseline.
 */
export async function canonicalizeSavedDeckNames(): Promise<void> {
  const feedOwned = new Set(Object.keys(loadDeckOrigins()));
  const saved = listSavedDeckNames()
    .filter((name) => !feedOwned.has(name))
    .map(captureSavedDeck)
    .flatMap((snapshot) => {
      if (snapshot.raw === null) return [];
      let deck: unknown;
      try {
        deck = JSON.parse(snapshot.raw);
      } catch {
        return [];
      }
      // A malformed record (wrong `main`/`sideboard` shape, a non-string
      // name field) is left untouched rather than thrown on: `deckCardNames`
      // and `applyCanonicalNames` below assume this shape and one bad
      // record must not abort the repair of every other saved deck.
      if (!isCanonicalizableDeck(deck)) return [];
      return [{ snapshot, deck }];
    });
  const renamed = await canonicalNameMap(saved.flatMap(({ deck }) => deckCardNames(deck)));
  const rewrites = saved
    .filter(({ deck }) => deckCardNames(deck).some((name) => renamed.has(name)))
    .map(({ snapshot, deck }) => ({
      snapshot,
      raw: JSON.stringify({ ...deck, ...applyCanonicalNames(deck, renamed) }),
    }));
  if (rewrites.length === 0) return;
  await withSavedDeckLibraryOrSkip((txn) => {
    const written: SavedDeckRewrite[] = [];
    withStorageWatchSuppressed(() => {
      for (const { snapshot, raw } of rewrites) {
        const previousRaw = snapshot.raw;
        if (previousRaw === null || !savedDeckUnchanged(txn, snapshot)) continue;
        writeSavedDeckData(txn, snapshot.name, raw);
        written.push({ name: snapshot.name, previousRaw, raw });
      }
    });
    for (const rewrite of written) notifySavedDeckRewritten(txn, rewrite);
  }, "run-unguarded");
}
