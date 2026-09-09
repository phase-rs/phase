/**
 * Content-equality + structural diff for PhaseBackup envelopes.
 *
 * Why this exists: every backup carries a fresh `exportedAt` timestamp, so naive
 * byte comparison would always say "different." The digest below is computed
 * over the portable payload fields only, giving us a real equality answer that
 * lets the sync reconciler suppress false conflicts when nothing actually
 * changed. Feed-owned decks are rebuilt from their subscriptions, so their
 * cached deck bodies, origin index, and generated metadata are not portable
 * profile changes.
 *
 * When the digests differ, `summarizeBackupDiff` reports per-envelope-section
 * counts (decks added/changed/removed; prefs/feeds same-or-different) so the UI
 * can tell the user *what* differs, not just that something does.
 */
import { projectCloudBackup, type PhaseBackup } from "../backup";

/**
 * Stable serialization of the portable payload fields. The volatile
 * `exportedAt` timestamp and regenerated feed catalog fields are excluded;
 * personal deck keys are sorted so object key-order does not produce digest
 * churn between platforms.
 */
function portablePayload(b: PhaseBackup) {
  const portable = projectCloudBackup(b);
  const sortedDecks: Record<string, string> = {};
  for (const k of Object.keys(portable.decks).sort()) {
    sortedDecks[k] = portable.decks[k];
  }
  return {
    version: portable.version,
    preferences: portable.preferences,
    draftWorkspacePreferences: portable.draftWorkspacePreferences ?? null,
    decks: sortedDecks,
    deckMetadata: portable.deckMetadata,
    // `deckFolders` is a persisted payload field (`buildBackup` always writes
    // it; `applyBackup` restores it). Omitting it from the digest made a
    // folder reorganization hash-equal to the old state, so the reconciler
    // suppressed the "conflict" and the change never propagated. `?? null`
    // keeps pre-folders backups (which omit the field) digest-stable.
    deckFolders: portable.deckFolders ?? null,
    activeDeck: portable.activeDeck,
    feedSubscriptions: portable.feedSubscriptions,
    feedDeckOrigins: portable.feedDeckOrigins,
  };
}

function canonicalPayload(b: PhaseBackup): string {
  return JSON.stringify(portablePayload(b));
}

/** SHA-256 hex of the canonical payload. Stable across runs and platforms. */
export async function computeBackupDigest(b: PhaseBackup): Promise<string> {
  const bytes = new TextEncoder().encode(canonicalPayload(b));
  const hash = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(hash))
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

export interface ConflictDiffSummary {
  /** Decks present in remote but not on this device. */
  decksAdded: number;
  /** Decks present on this device but not in remote. */
  decksRemoved: number;
  /** Decks present on both sides whose contents differ. */
  decksModified: number;
  prefsChanged: boolean;
  feedsChanged: boolean;
  /** Catch-all: workspace preferences + deck metadata/folders + active deck. */
  otherChanged: boolean;
}

/**
 * Per-envelope-section difference summary. Reported to the conflict UI so the
 * user can see what they would lose by picking the other copy. Order-of-keys
 * within a deck JSON is not normalized — two decks that re-encode to the same
 * data with different key order will show as "modified", which is acceptable
 * (deck JSONs are produced by one writer so this doesn't happen in practice).
 */
export function summarizeBackupDiff(
  local: PhaseBackup,
  remote: PhaseBackup,
): ConflictDiffSummary {
  const localPayload = portablePayload(local);
  const remotePayload = portablePayload(remote);
  const localKeys = new Set(Object.keys(localPayload.decks));
  const remoteKeys = new Set(Object.keys(remotePayload.decks));
  let decksAdded = 0;
  let decksRemoved = 0;
  let decksModified = 0;
  for (const k of remoteKeys) if (!localKeys.has(k)) decksAdded++;
  for (const k of localKeys) {
    if (!remoteKeys.has(k)) decksRemoved++;
    else if (localPayload.decks[k] !== remotePayload.decks[k]) decksModified++;
  }
  return {
    decksAdded,
    decksRemoved,
    decksModified,
    prefsChanged: localPayload.preferences !== remotePayload.preferences,
    feedsChanged: localPayload.feedSubscriptions !== remotePayload.feedSubscriptions,
    otherChanged:
      localPayload.deckMetadata !== remotePayload.deckMetadata ||
      localPayload.draftWorkspacePreferences !== remotePayload.draftWorkspacePreferences ||
      localPayload.deckFolders !== remotePayload.deckFolders ||
      localPayload.activeDeck !== remotePayload.activeDeck,
  };
}
