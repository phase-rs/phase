import { useEffect, useRef } from "react";

import {
  FEED_DECK_ORIGINS_KEY,
  FEED_SUBSCRIPTIONS_KEY,
  PREFERENCES_KEY,
  STORAGE_KEY_PREFIX,
} from "../../constants/storage.ts";
import { getFeedCacheState, refreshFeedCache, subscribeFeedCache } from "../feedPersistence.ts";
import { loadVisualPackBackend } from "../platform.ts";
import {
  isDeckLibraryBackgroundLifecycle,
  VisualPackBackendError,
  type DeckLibraryBackgroundLifecycle,
  type DeckLibraryPreparationResult,
  type VisualPackBackend,
} from "./backend.ts";
import { watchUserStorage } from "../cloudSync/storageWatcher.ts";
import { PROFILE_REPLACED_EVENT } from "../../stores/cloudSyncStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { invalidateDeckLibraryPack } from "./deckLibraryPack.ts";

const DEBOUNCE_MS = 500;

type DeckLibraryPreparationAttempt = {
  readonly promise: Promise<DeckLibraryPreparationResult>;
  readonly resolve: (result: DeckLibraryPreparationResult) => void;
  readonly reject: (error: VisualPackBackendError) => void;
};

let prepareDeckLibraryDelegate: (() => Promise<DeckLibraryPreparationResult>) | null = null;

/**
 * Awaits the mounted Deck Catalog scheduler's current reconciliation attempt.
 * The scheduler is deliberately the lifecycle owner: an unmounted or
 * unsupported page has no background capability to delegate to.
 */
export function prepareDeckLibraryForOffline(): Promise<DeckLibraryPreparationResult> {
  return prepareDeckLibraryDelegate?.()
    ?? Promise.reject(new VisualPackBackendError("unavailable"));
}

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
export function useDeckLibraryAutoSync(effectiveOffline = false, feedInitializationReady = true): void {
  const signalRef = useRef<(
    preferencesFresh: boolean,
    requireFreshFeeds?: boolean,
    recordBackgroundRetry?: boolean,
  ) => void>(() => undefined);
  const generationRef = useRef(0);

  useEffect(() => {
    const generation = ++generationRef.current;
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
    let startupFeedFallbackGeneration: number | null = null;
    let backgroundReady = false;
    let backgroundAcquisition: Promise<void> | null = null;
    let backgroundRetryGeneration = 0;
    let consumedBackgroundRetryGeneration = 0;
    let feedHydrated = false;
    let catalogInputGeneration = 0;
    let preparationAttempt: DeckLibraryPreparationAttempt | null = null;
    let backend: (VisualPackBackend & DeckLibraryBackgroundLifecycle) | null = null;

    const current = () => !disposed && generation === generationRef.current;
    const preparationError = (error: unknown): VisualPackBackendError =>
      error instanceof VisualPackBackendError ? error : new VisualPackBackendError("unavailable");
    const settlePreparation = (
      attempt: DeckLibraryPreparationAttempt,
      result: DeckLibraryPreparationResult | VisualPackBackendError,
    ) => {
      if (preparationAttempt !== attempt) return;
      preparationAttempt = null;
      if (result instanceof VisualPackBackendError) attempt.reject(result);
      else attempt.resolve(result);
    };
    const cancelPreparation = () => {
      if (preparationAttempt) settlePreparation(preparationAttempt, new VisualPackBackendError("cancelled"));
    };
    const preparationCurrent = (inputGeneration: number) =>
      current() && inputGeneration === catalogInputGeneration;

    const loadBackgroundBackend = async (): Promise<typeof backend> => {
      if (backend) return backend;
      const loaded = await loadVisualPackBackend();
      if (!loaded || !isDeckLibraryBackgroundLifecycle(loaded)) return null;
      backend = loaded;
      return backend;
    };

    const pauseBackground = async () => {
      const loaded = await loadBackgroundBackend();
      if (loaded) await loaded.setDeckLibraryBackgroundPaused(true);
    };

    const startBackground = (): Promise<void> => {
      if (!current() || backgroundReady) return Promise.resolve();
      if (backgroundAcquisition) return backgroundAcquisition;
      if (backgroundRetryGeneration === consumedBackgroundRetryGeneration) return Promise.resolve();
      const retryGeneration = backgroundRetryGeneration;
      // Consume only the intent that existed before this acquisition began.
      // A catalog signal while it is pending advances the generation and is
      // therefore still available if this attempt fails.
      consumedBackgroundRetryGeneration = retryGeneration;
      const acquisition = (async () => {
        try {
          const loaded = await loadBackgroundBackend();
          if (!current() || !loaded) return;
          await loaded.setDeckLibraryBackgroundPaused(false);
          if (!current()) return;
          backgroundReady = true;
          schedule();
        } catch (error) {
          // A transient lifecycle failure should not spin the loader. A newer
          // catalog signal already recorded during this attempt gets exactly
          // one coalesced retry; otherwise we remain paused until another one.
          if (current() && backgroundRetryGeneration !== retryGeneration) schedule();
          throw error;
        }
      })();
      backgroundAcquisition = acquisition;
      void acquisition.then(
        () => { if (backgroundAcquisition === acquisition) backgroundAcquisition = null; },
        () => { if (backgroundAcquisition === acquisition) backgroundAcquisition = null; },
      );
      return acquisition;
    };

    // Offline and feed-initializing generations own no observation. They do
    // still close the backend gate, including on a reused browser backend.
    if (effectiveOffline || !feedInitializationReady) {
      void pauseBackground().catch((error) => {
        if (current()) console.warn("Deck-library background suspension failed:", error);
      });
      return () => {
        disposed = true;
        signalRef.current = () => undefined;
      };
    }

    const schedule = () => {
      if (!current() || timer || loading || hydrating || refreshingFeeds) return;
      timer = setTimeout(() => {
        timer = null;
        void drain();
      }, DEBOUNCE_MS);
    };

    const signal = (
      requireFreshPreferences: boolean,
      requireFreshFeeds = false,
      recordBackgroundRetry = true,
    ) => {
      if (!current()) return;
      catalogInputGeneration += 1;
      invalidateDeckLibraryPack();
      requested = true;
      if (recordBackgroundRetry) backgroundRetryGeneration += 1;
      preferencesFresh ||= requireFreshPreferences;
      if (requireFreshPreferences) preferencesGeneration += 1;
      feedsFresh ||= requireFreshFeeds;
      if (requireFreshFeeds) feedFreshnessGeneration += 1;
      schedule();
    };

    const drain = async (): Promise<void> => {
      if (!current() || loading || hydrating || refreshingFeeds || !requested || !feedHydrated) return;
      const preparation = preparationAttempt;
      if (!backgroundReady && !preparation) {
        if (backgroundRetryGeneration === consumedBackgroundRetryGeneration) return;
        void startBackground().catch((error) => {
          if (current()) console.warn("Deck-library background lifecycle failed:", error);
        });
        return;
      }
      const requestedPreferencesGeneration = preferencesGeneration;
      if (preferencesFresh || !usePreferencesStore.persist.hasHydrated()) {
        hydrating = true;
        let rehydrated = false;
        try {
          await usePreferencesStore.persist.rehydrate();
          rehydrated = true;
        } catch (error) {
          if (preparation) settlePreparation(preparation, new VisualPackBackendError("unavailable"));
          else console.warn("Deck-library preferences rehydration failed:", error);
        } finally {
          hydrating = false;
        }
        if (!current()) return;
        if (!rehydrated || !usePreferencesStore.persist.hasHydrated()) {
          if (preparation) settlePreparation(preparation, new VisualPackBackendError("unavailable"));
          else if (preparationAttempt) {
            requested = true;
            schedule();
          }
          return;
        }
        if (preferencesGeneration !== requestedPreferencesGeneration) {
          schedule();
          return;
        }
        preferencesFresh = false;
      }
      if (!current() || !feedHydrated || !requested) return;

      const requestedFeedFreshnessGeneration = feedFreshnessGeneration;
      let usingStartupFeedFallback = false;
      if (feedsFresh) {
        refreshingFeeds = true;
        let refreshed = false;
        let refreshError: unknown = null;
        try {
          await refreshFeedCache();
          refreshed = true;
        } catch (error) {
          refreshError = error;
          if (!preparation) console.warn("Deck-library feed cache refresh failed:", error);
        } finally {
          refreshingFeeds = false;
        }
        if (!current()) return;
        if (!refreshed) {
          if (preparation) {
            settlePreparation(preparation, preparationError(refreshError));
            return;
          }
          if (preparationAttempt) {
            requested = true;
            schedule();
            return;
          }
          usingStartupFeedFallback = startupFeedFallbackGeneration === requestedFeedFreshnessGeneration;
          if (!usingStartupFeedFallback) return;
          startupFeedFallbackGeneration = null;
          if (feedFreshnessGeneration === requestedFeedFreshnessGeneration) feedsFresh = false;
        }
        if (
          preferencesFresh
          || preferencesGeneration !== requestedPreferencesGeneration
          || (!usingStartupFeedFallback && feedFreshnessGeneration !== requestedFeedFreshnessGeneration)
        ) {
          schedule();
          return;
        }
        if (refreshed) feedsFresh = false;
      }
      if (!current() || !feedHydrated || !requested) return;

      // Rehydrate and feed refresh legitimately notify their subscribers. A
      // preparation pass adopts that post-freshness state as its baseline;
      // only later external inputs make backend work stale.
      if (!preparation && preparationAttempt) {
        requested = true;
        schedule();
        return;
      }
      const preparationInputGeneration = catalogInputGeneration;

      requested = false;
      loading = true;
      try {
        if (preparation) {
          if (!backgroundReady) {
            await startBackground();
            if (!current()) return;
            if (!preparationCurrent(preparationInputGeneration)) {
              requested = true;
              return;
            }
            if (!backgroundReady) {
              settlePreparation(preparation, new VisualPackBackendError("unavailable"));
              return;
            }
          }
          const loaded = await loadBackgroundBackend();
          if (!current()) return;
          if (!preparationCurrent(preparationInputGeneration)) {
            requested = true;
            return;
          }
          if (!loaded) {
            settlePreparation(preparation, new VisualPackBackendError("unavailable"));
            return;
          }
          const result = await loaded.prepareDeckLibraryForOffline();
          if (!current()) return;
          if (!preparationCurrent(preparationInputGeneration)) {
            requested = true;
            return;
          }
          settlePreparation(preparation, result);
          return;
        }
        const loadedPreferencesGeneration = preferencesGeneration;
        const loadedFeedFreshnessGeneration = feedFreshnessGeneration;
        const loaded = await loadBackgroundBackend();
        if (!current()) return;
        if (preparationAttempt) {
          requested = true;
          return;
        }
        if (
          preferencesFresh
          || preferencesGeneration !== loadedPreferencesGeneration
          || (!usingStartupFeedFallback && (feedsFresh || feedFreshnessGeneration !== loadedFeedFreshnessGeneration))
        ) {
          requested = true;
          return;
        }
        await loaded?.reconcileDeckLibrary();
      } catch (error) {
        if (preparation) {
          if (preparationCurrent(preparationInputGeneration)) settlePreparation(preparation, preparationError(error));
          else requested = true;
        } else {
          console.warn("Deck-library background reconciliation failed:", error);
        }
      } finally {
        loading = false;
        if (requested || feedsFresh) {
          requested = true;
          schedule();
        }
      }
    };

    const requestPreparation = (): Promise<DeckLibraryPreparationResult> => {
      if (!current()) return Promise.reject(new VisualPackBackendError("cancelled"));
      if (preparationAttempt) return preparationAttempt.promise;
      signal(true, true);
      let resolve!: (result: DeckLibraryPreparationResult) => void;
      let reject!: (error: VisualPackBackendError) => void;
      const attempt: DeckLibraryPreparationAttempt = {
        promise: new Promise<DeckLibraryPreparationResult>((resolveAttempt, rejectAttempt) => {
          resolve = resolveAttempt;
          reject = rejectAttempt;
        }),
        resolve,
        reject,
      };
      preparationAttempt = attempt;
      schedule();
      return attempt.promise;
    };

    signalRef.current = signal;
    const delegate = requestPreparation;
    prepareDeckLibraryDelegate = delegate;
    feedHydrated = getFeedCacheState().hydrated;
    const unwatchFeedCache = subscribeFeedCache((next, previous) => {
      feedHydrated = next.hydrated;
      if (next.hydrated !== previous.hydrated || next.cache !== previous.cache) signal(false);
    });
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
    window.addEventListener("storage", onStorage);
    window.addEventListener(PROFILE_REPLACED_EVENT, onProfileReplaced);
    const cleanupListeners = () => {
      unwatchFeedCache();
      unwatchStorage();
      unwatchPreferences();
      window.removeEventListener("storage", onStorage);
      window.removeEventListener(PROFILE_REPLACED_EVENT, onProfileReplaced);
    };

    // Startup is deliberately just another freshness request. Signals arriving
    // while the lifecycle is loading/unpausing accumulate into this request
    // and the one drain below observes the final preference and durable-feed
    // generations.
    signal(true, true);
    startupFeedFallbackGeneration = feedFreshnessGeneration;
    void startBackground().catch((error) => {
      if (current()) console.warn("Deck-library background lifecycle failed:", error);
    });

    return () => {
      disposed = true;
      cancelPreparation();
      signalRef.current = () => undefined;
      if (prepareDeckLibraryDelegate === delegate) prepareDeckLibraryDelegate = null;
      if (timer) clearTimeout(timer);
      cleanupListeners();
      void pauseBackground().catch(() => undefined);
    };
  }, [effectiveOffline, feedInitializationReady]);

}
