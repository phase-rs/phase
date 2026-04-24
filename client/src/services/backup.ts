/**
 * Export/import of user-owned localStorage data so a player can migrate
 * preferences + decks + feed subscriptions between machines.
 *
 * Design note — each field is a raw JSON string (or null) rather than a
 * decoded object. The backup service never computes on this data; it just
 * round-trips the exact on-disk representation. This avoids coupling the
 * backup format to internal store shapes (which evolve independently) and
 * lets each store's own versioning machinery handle forward migration when
 * the restored data lands in localStorage.
 *
 * IndexedDB caches (feed cache, audio cache, game state checkpoints) are
 * intentionally NOT exported — they rehydrate at runtime from source.
 */
import {
  ACTIVE_DECK_KEY,
  DECK_METADATA_KEY,
  FEED_DECK_ORIGINS_KEY,
  FEED_SUBSCRIPTIONS_KEY,
  STORAGE_KEY_PREFIX,
} from "../constants/storage";

const PREFERENCES_KEY = "phase-preferences";

/** Versioned envelope. Future shapes go in a `PhaseBackupV2 | …` union. */
export interface PhaseBackupV1 {
  version: 1;
  exportedAt: string;
  /** Raw JSON of the preferences store (`phase-preferences` key), or null. */
  preferences: string | null;
  /** Map from deck name → raw JSON of the ParsedDeck. */
  decks: Record<string, string>;
  /** Raw JSON of the deck metadata store, or null. */
  deckMetadata: string | null;
  /** Currently-active deck name, or null. */
  activeDeck: string | null;
  /** Raw JSON of the feed subscriptions array, or null. */
  feedSubscriptions: string | null;
  /** Raw JSON of the deck→feed origin map, or null. */
  feedDeckOrigins: string | null;
}

export type PhaseBackup = PhaseBackupV1;

/** Build a backup envelope by snapshotting all user-owned localStorage keys. */
export function buildBackup(): PhaseBackupV1 {
  const decks: Record<string, string> = {};
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (!key?.startsWith(STORAGE_KEY_PREFIX)) continue;
    const name = key.slice(STORAGE_KEY_PREFIX.length);
    const raw = localStorage.getItem(key);
    if (raw != null) decks[name] = raw;
  }

  return {
    version: 1,
    exportedAt: new Date().toISOString(),
    preferences: localStorage.getItem(PREFERENCES_KEY),
    decks,
    deckMetadata: localStorage.getItem(DECK_METADATA_KEY),
    activeDeck: localStorage.getItem(ACTIVE_DECK_KEY),
    feedSubscriptions: localStorage.getItem(FEED_SUBSCRIPTIONS_KEY),
    feedDeckOrigins: localStorage.getItem(FEED_DECK_ORIGINS_KEY),
  };
}

/** Trigger a browser download of the backup payload. */
export function downloadBackup(): void {
  const backup = buildBackup();
  const json = JSON.stringify(backup, null, 2);
  const blob = new Blob([json], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `phase-backup-${new Date().toISOString().slice(0, 10)}.json`;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

/**
 * Narrow an unknown value to PhaseBackupV1. Does not deeply validate the
 * inner payloads — those are opaque to this module and restored verbatim.
 */
function isBackupV1(value: unknown): value is PhaseBackupV1 {
  if (value == null || typeof value !== "object") return false;
  const v = value as Record<string, unknown>;
  if (v.version !== 1) return false;
  if (typeof v.exportedAt !== "string") return false;
  if (v.decks == null || typeof v.decks !== "object") return false;
  for (const entry of Object.values(v.decks as Record<string, unknown>)) {
    if (typeof entry !== "string") return false;
  }
  const stringOrNull = (field: unknown): boolean =>
    field === null || typeof field === "string";
  return (
    stringOrNull(v.preferences) &&
    stringOrNull(v.deckMetadata) &&
    stringOrNull(v.activeDeck) &&
    stringOrNull(v.feedSubscriptions) &&
    stringOrNull(v.feedDeckOrigins)
  );
}

export type ImportMode = "merge" | "overwrite";

export interface ImportResult {
  decksImported: number;
  decksSkippedMalformed: number;
  preferencesReplaced: boolean;
  malformedKeys: string[];
}

/**
 * Reject inner payloads that aren't parseable JSON. Each store (Zustand,
 * custom JSON) rehydrates from localStorage on next boot — a truncated or
 * otherwise corrupt inner payload would crash rehydration. Catching it
 * here keeps a bad backup from poisoning the app.
 */
function isParseableJson(raw: string | null): boolean {
  if (raw == null) return true;
  try {
    JSON.parse(raw);
    return true;
  } catch {
    return false;
  }
}

/**
 * Apply a backup envelope to localStorage. In `overwrite` mode, clears all
 * user-owned keys before writing; in `merge` mode, leaves existing decks
 * alone (backup decks with the same name are ignored to avoid surprise
 * replacement). Preferences, metadata, and feed state are always replaced
 * when present in the backup — there is no meaningful merge for a
 * serialized Zustand snapshot.
 *
 * After applying, callers should trigger a full reload so Zustand stores
 * re-hydrate from the new localStorage contents.
 */
export function applyBackup(
  backup: PhaseBackupV1,
  mode: ImportMode,
): ImportResult {
  if (mode === "overwrite") {
    const toRemove: string[] = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (!key) continue;
      if (
        key === PREFERENCES_KEY ||
        key === DECK_METADATA_KEY ||
        key === ACTIVE_DECK_KEY ||
        key === FEED_SUBSCRIPTIONS_KEY ||
        key === FEED_DECK_ORIGINS_KEY ||
        key.startsWith(STORAGE_KEY_PREFIX)
      ) {
        toRemove.push(key);
      }
    }
    for (const key of toRemove) localStorage.removeItem(key);
  }

  let decksImported = 0;
  let decksSkippedMalformed = 0;
  const malformedKeys: string[] = [];
  for (const [name, raw] of Object.entries(backup.decks)) {
    const storageKey = STORAGE_KEY_PREFIX + name;
    if (mode === "merge" && localStorage.getItem(storageKey) != null) continue;
    if (!isParseableJson(raw)) {
      decksSkippedMalformed += 1;
      malformedKeys.push(storageKey);
      continue;
    }
    localStorage.setItem(storageKey, raw);
    decksImported += 1;
  }

  // Outer-level fields: skip any that fail to parse and record the key so
  // the caller can surface the corruption to the user. The activeDeck
  // field is a plain string, not JSON — exempt it from the parse check.
  const writeValidated = (
    key: string,
    raw: string | null,
    jsonExpected: boolean,
  ): boolean => {
    if (raw == null) return false;
    if (jsonExpected && !isParseableJson(raw)) {
      malformedKeys.push(key);
      return false;
    }
    localStorage.setItem(key, raw);
    return true;
  };

  const preferencesReplaced = writeValidated(
    PREFERENCES_KEY,
    backup.preferences,
    true,
  );
  writeValidated(DECK_METADATA_KEY, backup.deckMetadata, true);
  writeValidated(ACTIVE_DECK_KEY, backup.activeDeck, false);
  writeValidated(FEED_SUBSCRIPTIONS_KEY, backup.feedSubscriptions, true);
  writeValidated(FEED_DECK_ORIGINS_KEY, backup.feedDeckOrigins, true);

  return { decksImported, decksSkippedMalformed, preferencesReplaced, malformedKeys };
}

/**
 * Parse a user-supplied file and apply it. Throws with a user-friendly
 * message on malformed input; the caller shows the message to the user.
 */
export async function importBackupFromFile(
  file: File,
  mode: ImportMode,
): Promise<ImportResult> {
  const text = await file.text();
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new Error("File is not valid JSON.");
  }
  if (!isBackupV1(parsed)) {
    throw new Error(
      "File is not a phase backup, or its version is not supported.",
    );
  }
  return applyBackup(parsed, mode);
}
