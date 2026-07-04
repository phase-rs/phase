import { useCallback, useEffect, useState } from "react";

import {
  computeHasUnread,
  fetchChangelogEntries,
  fetchChangelogMeta,
  type ChangelogEntry,
} from "../services/changelog";
import { usePreferencesStore } from "../stores/preferencesStore";

export interface UseChangelog {
  /** True when there is a published entry newer than the user's watermark. */
  hasUnread: boolean;
  /** Loaded entries (newest-first). Empty until the modal is opened. */
  entries: ChangelogEntry[];
  loading: boolean;
  /** True when the lazy entry fetch failed — the modal shows a retry state. */
  failed: boolean;
  /** Lazy-load the full changelog (modal open). Advances the unread watermark
   * ONLY on success, so a failed load leaves the dot intact for a retry. */
  openAndLoad: () => void;
}

/**
 * Drives the "What's New" affordance. On mount it fetches the tiny meta pointer
 * and, for first-run / freshly-upgraded users (no watermark yet), silently
 * seeds the watermark to the latest id so they get no dot for pre-existing
 * entries. The full entry list is fetched only when {@link openAndLoad} runs.
 */
export function useChangelog(): UseChangelog {
  const lastSeenId = usePreferencesStore((s) => s.lastSeenChangelogId);
  const setLastSeenId = usePreferencesStore((s) => s.setLastSeenChangelogId);

  const [latestId, setLatestId] = useState<number | null>(null);
  const [entries, setEntries] = useState<ChangelogEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let active = true;
    fetchChangelogMeta().then((meta) => {
      if (active && meta) setLatestId(meta.latestId);
    });
    return () => {
      active = false;
    };
  }, []);

  // First-run / post-upgrade seed: a user with no watermark adopts the current
  // latest, so the dot only ever lights for entries published AFTER this visit.
  useEffect(() => {
    if (latestId != null && lastSeenId == null) setLastSeenId(latestId);
  }, [latestId, lastSeenId, setLastSeenId]);

  const openAndLoad = useCallback(() => {
    setLoading(true);
    setFailed(false);
    fetchChangelogEntries()
      .then((loaded) => {
        setEntries(loaded);
        // Watermark advances to the newest loaded id (entries are newest-first),
        // clearing the dot — only now that the user has actually seen them.
        const newest = loaded[0]?.id;
        if (newest != null && (lastSeenId == null || newest > lastSeenId)) {
          setLastSeenId(newest);
        }
      })
      .catch(() => setFailed(true))
      .finally(() => setLoading(false));
  }, [lastSeenId, setLastSeenId]);

  return {
    hasUnread: computeHasUnread(latestId, lastSeenId),
    entries,
    loading,
    failed,
    openAndLoad,
  };
}
