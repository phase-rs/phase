import { useEffect, useRef } from "react";

import {
  FEED_DECK_ORIGINS_KEY,
  FEED_SUBSCRIPTIONS_KEY,
  PREFERENCES_KEY,
  STORAGE_KEY_PREFIX,
} from "../../constants/storage.ts";
import { refreshFeedCache, useFeedCacheHydrated, useFeedCacheSnapshot } from "../feedPersistence.ts";
import { loadVisualPackBackend } from "../platform.ts";
import { watchUserStorage } from "../cloudSync/storageWatcher.ts";
import { PROFILE_REPLACED_EVENT } from "../../stores/cloudSyncStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { invalidateDeckLibraryPack } from "./deckLibraryPack.ts";

const DEBOUNCE_MS = 500;

function watchesCatalogKey(key: string): boolean {
  return key.startsWith(STORAGE_KEY_PREFIX)
    || key === FEED_SUBSCRIPTIONS_KEY
    || key === FEED_DECK_ORIGINS_KEY;
}

/**
 * Reconciles the already opted-in deck-library pack after its catalog inputs
 * change. Membership, installation, persistence, and durable concurrency stay
 * backend-owned; this hook only schedules the existing backend entry point.
 */
export function useDeckLibraryAutoSync(): void {
  const feedHydrated = useFeedCacheHydrated();
  const feedCache = useFeedCacheSnapshot();
  const feedHydratedRef = useRef(feedHydrated);
  const signalRef = useRef<(preferencesFresh: boolean) => void>(() => undefined);
  feedHydratedRef.current = feedHydrated;

  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let loading = false;
    let hydrating = false;
    let refreshingFeeds = false;
    let requested = false;
    let preferencesFresh = false;
    let preferencesGeneration = 0;
    let feedsFresh = false;
    let feedFreshnessGeneration = 0;

    const schedule = () => {
      if (disposed || timer || loading || hydrating || refreshingFeeds) return;
      timer = setTimeout(() => {
        timer = null;
        void drain();
      }, DEBOUNCE_MS);
    };

    const signal = (requireFreshPreferences: boolean, requireFreshFeeds = false) => {
      if (disposed) return;
      invalidateDeckLibraryPack();
      requested = true;
      preferencesFresh ||= requireFreshPreferences;
      if (requireFreshPreferences) preferencesGeneration += 1;
      feedsFresh ||= requireFreshFeeds;
      if (requireFreshFeeds) feedFreshnessGeneration += 1;
      schedule();
    };

    const drain = async (): Promise<void> => {
      if (disposed || loading || hydrating || refreshingFeeds || !requested || !feedHydratedRef.current) return;
      const requestedPreferencesGeneration = preferencesGeneration;
      if (preferencesFresh || !usePreferencesStore.persist.hasHydrated()) {
        hydrating = true;
        let rehydrated = false;
        try {
          await usePreferencesStore.persist.rehydrate();
          rehydrated = true;
        } catch (error) {
          console.warn("Deck-library preferences rehydration failed:", error);
        } finally {
          hydrating = false;
        }
        if (disposed) return;
        if (!rehydrated || !usePreferencesStore.persist.hasHydrated()) return;
        if (preferencesGeneration !== requestedPreferencesGeneration) {
          schedule();
          return;
        }
        preferencesFresh = false;
      }
      if (disposed || !feedHydratedRef.current || !requested) return;

      const requestedFeedFreshnessGeneration = feedFreshnessGeneration;
      if (feedsFresh) {
        refreshingFeeds = true;
        let refreshed = false;
        try {
          await refreshFeedCache();
          refreshed = true;
        } catch (error) {
          console.warn("Deck-library feed cache refresh failed:", error);
        } finally {
          refreshingFeeds = false;
        }
        if (disposed || !refreshed) return;
        if (
          feedFreshnessGeneration !== requestedFeedFreshnessGeneration
          || preferencesFresh
          || preferencesGeneration !== requestedPreferencesGeneration
        ) {
          schedule();
          return;
        }
        feedsFresh = false;
      }
      if (disposed || !feedHydratedRef.current || !requested) return;

      requested = false;
      loading = true;
      try {
        const loadedPreferencesGeneration = preferencesGeneration;
        const loadedFeedFreshnessGeneration = feedFreshnessGeneration;
        const backend = await loadVisualPackBackend();
        if (disposed) return;
        if (
          preferencesFresh
          || preferencesGeneration !== loadedPreferencesGeneration
          || feedsFresh
          || feedFreshnessGeneration !== loadedFeedFreshnessGeneration
        ) {
          requested = true;
          return;
        }
        await backend?.reconcileDeckLibrary();
      } catch (error) {
        console.warn("Deck-library background reconciliation failed:", error);
      } finally {
        loading = false;
        if (requested) schedule();
      }
    };

    signalRef.current = signal;
    const unwatchStorage = watchUserStorage((key) => {
      if (watchesCatalogKey(key)) signal(false);
    });
    const unwatchPreferences = usePreferencesStore.subscribe((next, previous) => {
      if (next.artChain !== previous.artChain || next.artOverrides !== previous.artOverrides) signal(false);
    });
    const onStorage = (event: StorageEvent) => {
      if (event.storageArea !== localStorage || !event.key) return;
      if (event.key === PREFERENCES_KEY) signal(true);
      else if (watchesCatalogKey(event.key)) signal(false, true);
    };
    const onProfileReplaced = () => signal(true, true);
    const onOnline = () => signal(true, true);
    window.addEventListener("storage", onStorage);
    window.addEventListener(PROFILE_REPLACED_EVENT, onProfileReplaced);
    window.addEventListener("online", onOnline);

    return () => {
      disposed = true;
      signalRef.current = () => undefined;
      if (timer) clearTimeout(timer);
      unwatchStorage();
      unwatchPreferences();
      window.removeEventListener("storage", onStorage);
      window.removeEventListener(PROFILE_REPLACED_EVENT, onProfileReplaced);
      window.removeEventListener("online", onOnline);
    };
  }, []);

  useEffect(() => {
    if (feedHydrated) signalRef.current(false);
  }, [feedCache, feedHydrated]);
}
