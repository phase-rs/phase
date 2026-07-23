/**
 * Content-equality + structural diff for PhaseBackup envelopes.
 *
 * Why this exists: every backup carries a fresh `exportedAt` timestamp, so naive
 * byte comparison would always say "different." The digest below is computed
 * over the *payload* fields only, giving us a real equality answer that lets the
 * sync reconciler suppress false conflicts when nothing actually changed.
 *
 * When the digests differ, `summarizeBackupDiff` reports per-envelope-section
 * counts (decks added/changed/removed; prefs/feeds same-or-different) so the UI
 * can tell the user *what* differs, not just that something does.
 */
import type { PhaseBackup } from "../backup";

/**
 * Stable serialization of the payload fields (everything except the volatile
 * `exportedAt` timestamp). Deck keys are sorted so JSON object key-order does
 * not produce digest churn between platforms.
 */
function canonicalPayload(b: PhaseBackup): string {
  const sortedDecks: Record<string, string> = {};
  for (const k of Object.keys(b.decks).sort()) sortedDecks[k] = b.decks[k];
  return JSON.stringify({
    version: b.version,
    preferences: b.preferences,
    decks: sortedDecks,
    deckMetadata: b.deckMetadata,
    // `deckFolders` is a persisted payload field (`buildBackup` always writes
    // it; `applyBackup` restores it). Omitting it from the digest made a
    // folder reorganization hash-equal to the old state, so the reconciler
    // suppressed the "conflict" and the change never propagated. `?? null`
    // keeps pre-folders backups (which omit the field) digest-stable.
    deckFolders: b.deckFolders ?? null,
    activeDeck: b.activeDeck,
    feedSubscriptions: b.feedSubscriptions,
    feedDeckOrigins: b.feedDeckOrigins,
  });
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
  /** Catch-all: deckMetadata + activeDeck + feedDeckOrigins differences. */
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
  const localKeys = new Set(Object.keys(local.decks));
  const remoteKeys = new Set(Object.keys(remote.decks));
  let decksAdded = 0;
  let decksRemoved = 0;
  let decksModified = 0;
  for (const k of remoteKeys) if (!localKeys.has(k)) decksAdded++;
  for (const k of localKeys) {
    if (!remoteKeys.has(k)) decksRemoved++;
    else if (local.decks[k] !== remote.decks[k]) decksModified++;
  }
  return {
    decksAdded,
    decksRemoved,
    decksModified,
    prefsChanged: local.preferences !== remote.preferences,
    feedsChanged: local.feedSubscriptions !== remote.feedSubscriptions,
    otherChanged:
      local.deckMetadata !== remote.deckMetadata ||
      (local.deckFolders ?? null) !== (remote.deckFolders ?? null) ||
      local.activeDeck !== remote.activeDeck ||
      local.feedDeckOrigins !== remote.feedDeckOrigins,
  };
}
