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
  DECK_FOLDERS_KEY,
  DECK_METADATA_KEY,
  FEED_DECK_ORIGINS_KEY,
  FEED_SUBSCRIPTIONS_KEY,
  isUserOwnedStorageKey,
  PREFERENCES_KEY,
  STORAGE_KEY_PREFIX,
  type DeckFolder,
  type DeckMeta,
} from "../constants/storage";

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
  /**
   * Raw JSON of the deck-folder registry, or null. Optional on read so
   * backups exported before folders existed still validate (treated as
   * "no folders"); always present on write.
   */
  deckFolders?: string | null;
  /** Currently-active deck name, or null. */
  activeDeck: string | null;
  /** Raw JSON of the feed subscriptions array, or null. */
  feedSubscriptions: string | null;
  /** Raw JSON of the deck→feed origin map, or null. */
  feedDeckOrigins: string | null;
}

export type PhaseBackup = PhaseBackupV1;

/**
 * Reconcile deck collections without discarding either device's deck data.
 *
 * The local collection keeps its names. A cloud deck with the same name but
 * different contents is retained under a stable, unique "(Cloud)" suffix;
 * exact duplicates need only one copy. Profile-level fields deliberately stay
 * local because their opaque serialized formats do not have a safe structural
 * merge contract.
 */
export function mergeDeckCollections(
  local: PhaseBackup,
  cloud: PhaseBackup,
): PhaseBackupV1 {
  const decks = { ...local.decks };
  const cloudDeckNames = new Map<string, string>();

  for (const [name, raw] of Object.entries(cloud.decks)) {
    const existing = decks[name];
    if (existing === undefined || existing === raw) {
      decks[name] = raw;
      cloudDeckNames.set(name, name);
      continue;
    }

    let suffix = 1;
    let mergedName = `${name} (Cloud)`;
    while (decks[mergedName] !== undefined) {
      suffix += 1;
      mergedName = `${name} (Cloud ${suffix})`;
    }
    decks[mergedName] = raw;
    cloudDeckNames.set(name, mergedName);
  }

  const { folders, folderIds } = mergeFolders(local.deckFolders, cloud.deckFolders);
  const deckMetadata = mergeDeckMetadata(
    local.deckMetadata,
    cloud.deckMetadata,
    cloudDeckNames,
    folderIds,
  );
  const feedDeckOrigins = mergeDeckRecord(
    local.feedDeckOrigins,
    cloud.feedDeckOrigins,
    cloudDeckNames,
  );

  return {
    ...local,
    exportedAt: new Date().toISOString(),
    decks,
    deckMetadata,
    deckFolders: folders,
    feedDeckOrigins,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function parseRecord<T>(
  raw: string | null,
  isValue?: (value: unknown) => value is T,
): Record<string, T> | null {
  if (raw == null) return {};
  try {
    const value: unknown = JSON.parse(raw);
    if (!isRecord(value)) return null;
    if (isValue && !Object.values(value).every(isValue)) return null;
    return value as Record<string, T>;
  } catch {
    return null;
  }
}

function isDeckMeta(value: unknown): value is DeckMeta {
  if (!isRecord(value) || typeof value.addedAt !== "number") return false;
  return (
    (value.lastPlayedAt === undefined || typeof value.lastPlayedAt === "number") &&
    (value.folderId === undefined || typeof value.folderId === "string") &&
    (value.starred === undefined || typeof value.starred === "boolean")
  );
}

function isDeckFolder(value: unknown): value is DeckFolder {
  return (
    isRecord(value) &&
    typeof value.id === "string" &&
    typeof value.name === "string" &&
    typeof value.order === "number"
  );
}

function parseFolders(raw: string | null | undefined): DeckFolder[] | null {
  if (raw == null) return [];
  try {
    const value: unknown = JSON.parse(raw);
    return Array.isArray(value) && value.every(isDeckFolder) ? value : null;
  } catch {
    return null;
  }
}

function mergeFolders(
  localRaw: string | null | undefined,
  cloudRaw: string | null | undefined,
): { folders: string | null; folderIds: Map<string, string> } {
  const local = parseFolders(localRaw);
  const cloud = parseFolders(cloudRaw);
  if (local === null || cloud === null) {
    return { folders: localRaw ?? null, folderIds: new Map() };
  }

  const merged = [...local];
  const folderIds = new Map<string, string>();
  for (const folder of cloud) {
    const localFolder = merged.find((candidate) => candidate.id === folder.id);
    if (localFolder === undefined || (localFolder.name === folder.name && localFolder.order === folder.order)) {
      if (localFolder === undefined) merged.push(folder);
      folderIds.set(folder.id, folder.id);
      continue;
    }

    let suffix = 1;
    let id = `${folder.id}-cloud`;
    while (merged.some((candidate) => candidate.id === id)) {
      suffix += 1;
      id = `${folder.id}-cloud-${suffix}`;
    }
    merged.push({ ...folder, id });
    folderIds.set(folder.id, id);
  }
  return { folders: JSON.stringify(merged), folderIds };
}

function mergeDeckMetadata(
  localRaw: string | null,
  cloudRaw: string | null,
  cloudDeckNames: ReadonlyMap<string, string>,
  folderIds: ReadonlyMap<string, string>,
): string | null {
  const local = parseRecord(localRaw, isDeckMeta);
  const cloud = parseRecord(cloudRaw, isDeckMeta);
  if (local === null || cloud === null) return localRaw;

  for (const [name, meta] of Object.entries(cloud)) {
    const mergedName = cloudDeckNames.get(name);
    if (mergedName === undefined || local[mergedName] !== undefined) continue;
    const folderId = meta.folderId === undefined ? undefined : (folderIds.get(meta.folderId) ?? meta.folderId);
    local[mergedName] = { ...meta, ...(folderId === undefined ? {} : { folderId }) };
  }
  return JSON.stringify(local);
}

function mergeDeckRecord<T>(
  localRaw: string | null,
  cloudRaw: string | null,
  cloudDeckNames: ReadonlyMap<string, string>,
): string | null {
  const local = parseRecord<T>(localRaw);
  const cloud = parseRecord<T>(cloudRaw);
  if (local === null || cloud === null) return localRaw;

  for (const [name, value] of Object.entries(cloud)) {
    const mergedName = cloudDeckNames.get(name);
    if (mergedName === undefined || local[mergedName] !== undefined) continue;
    local[mergedName] = value;
  }
  return JSON.stringify(local);
}

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
    deckFolders: localStorage.getItem(DECK_FOLDERS_KEY),
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
  // `deckFolders` is optional on read: pre-folders backups omit it entirely.
  const optionalStringOrNull = (field: unknown): boolean =>
    field === undefined || stringOrNull(field);
  return (
    stringOrNull(v.preferences) &&
    stringOrNull(v.deckMetadata) &&
    optionalStringOrNull(v.deckFolders) &&
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
      if (key && isUserOwnedStorageKey(key)) toRemove.push(key);
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
  writeValidated(DECK_FOLDERS_KEY, backup.deckFolders ?? null, true);
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
