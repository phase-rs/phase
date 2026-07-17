/**
 * One-shot, boot-time deck migrations.
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
import { STORAGE_KEY_PREFIX } from "../constants/storage";
import { withStorageWatchSuppressed } from "./cloudSync/storageWatcher";
import { projectSavedDeckSpecialSlots } from "./savedDeckProjection";

/**
 * Walk every saved deck, repair its JSON, and persist the repaired form when
 * it differs from what's on disk. Idempotent: a second call is effectively
 * a no-op (each deck's repair is already on disk).
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

    let parsed: ParsedDeck & Record<string, unknown>;
    try {
      parsed = JSON.parse(raw) as ParsedDeck & Record<string, unknown>;
    } catch {
      continue;
    }
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
