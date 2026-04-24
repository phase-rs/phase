import type { Feed, FeedDeck, FeedSubscription } from "../types/feed";
import { repairParsedDeck, type ParsedDeck } from "./deckParser";
import { FEED_REGISTRY } from "../data/feedRegistry";
import {
  ACTIVE_DECK_KEY,
  STORAGE_KEY_PREFIX,
  loadDeckOrigins,
  loadFeedSubscriptions,
  removeDeckMeta,
  saveDeckOrigins,
  saveFeedSubscriptions,
  stampDeckMeta,
} from "../constants/storage";
import {
  getCachedFeed,
  hydrateFeedCache,
  removeCachedFeed,
  setCachedFeed,
} from "./feedPersistence";

// --- Validation ---

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function isNonEmptyString(v: unknown): v is string {
  return typeof v === "string" && v.length > 0;
}

function isValidDeckEntry(v: unknown): boolean {
  return isObject(v) && typeof v.count === "number" && isNonEmptyString(v.name);
}

function isValidFeedDeck(v: unknown): v is FeedDeck {
  if (!isObject(v)) return false;
  if (!isNonEmptyString(v.name)) return false;
  if (!Array.isArray(v.colors)) return false;
  if (!Array.isArray(v.main) || !v.main.every(isValidDeckEntry)) return false;
  if (!Array.isArray(v.sideboard) || !v.sideboard.every(isValidDeckEntry)) return false;
  return true;
}

export function validateFeed(data: unknown): Feed | null {
  if (!isObject(data)) return null;
  if (!isNonEmptyString(data.id)) return null;
  if (!isNonEmptyString(data.name)) return null;
  if (typeof data.version !== "number") return null;
  if (!isNonEmptyString(data.updated)) return null;
  if (!Array.isArray(data.decks)) return null;
  if (!data.decks.every(isValidFeedDeck)) return null;
  return data as unknown as Feed;
}

// --- Constants ---

/** Auto-refresh any subscription whose cached data is older than this. */
const FEED_STALE_AFTER_MS = 24 * 60 * 60 * 1000;

// --- Internal helpers ---

async function fetchFeed(url: string): Promise<Feed> {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Failed to fetch feed: ${response.status} ${response.statusText}`);
  }
  const data: unknown = await response.json();
  const feed = validateFeed(data);
  if (!feed) {
    throw new Error("Invalid feed format: missing required fields or malformed deck entries");
  }
  return normalizeFeed(feed);
}

/**
 * For commander-format feeds, MTGGoldfish-style decks store `commander: null`
 * with the convention that the deck NAME is the commander (and is included in
 * `main`). Without this normalization, deck-compatibility evaluation gets an
 * empty commander array and rejects the deck as commander-illegal.
 */
function normalizeFeed(feed: Feed): Feed {
  if (feed.format !== "commander") return feed;
  return {
    ...feed,
    decks: feed.decks.map((deck) => {
      if (!deck.main.some((entry) => entry.name === deck.name)) return deck;
      const parsed = repairParsedDeck({
        main: deck.main,
        sideboard: deck.sideboard,
        commander: deck.commander?.length ? deck.commander : [deck.name],
        companion: deck.companion,
      });
      return { ...deck, ...parsed };
    }),
  };
}

export function feedDeckToParsedDeck(deck: FeedDeck): ParsedDeck {
  return repairParsedDeck({
    main: deck.main,
    sideboard: deck.sideboard,
    commander: deck.commander ?? undefined,
    companion: deck.companion,
  });
}

function syncFeedDecksToStorage(feed: Feed): void {
  const origins = loadDeckOrigins();

  // Add/update decks from the feed
  for (const deck of feed.decks) {
    const key = STORAGE_KEY_PREFIX + deck.name;
    const existingOrigin = origins[deck.name];

    if (existingOrigin === feed.id) {
      // Origin matches this feed — overwrite (feed is authoritative)
      localStorage.setItem(key, JSON.stringify(feedDeckToParsedDeck(deck)));
    } else if (existingOrigin) {
      // Origin is a different feed — skip
      continue;
    } else if (localStorage.getItem(key)) {
      // User deck with same name — skip
      continue;
    } else {
      // New deck — write it
      localStorage.setItem(key, JSON.stringify(feedDeckToParsedDeck(deck)));
      stampDeckMeta(deck.name, 0);
    }

    origins[deck.name] = feed.id;
  }

  // Remove stale decks that are no longer in the feed
  const feedDeckNames = new Set(feed.decks.map((d) => d.name));
  for (const [deckName, feedId] of Object.entries(origins)) {
    if (feedId === feed.id && !feedDeckNames.has(deckName)) {
      localStorage.removeItem(STORAGE_KEY_PREFIX + deckName);
      removeDeckMeta(deckName);
      delete origins[deckName];

      // Clear active deck if it was removed
      if (localStorage.getItem(ACTIVE_DECK_KEY) === deckName) {
        localStorage.removeItem(ACTIVE_DECK_KEY);
      }
    }
  }

  saveDeckOrigins(origins);
}

// --- Public API ---

export async function initializeFeeds(): Promise<void> {
  await hydrateFeedCache();

  const subs = loadFeedSubscriptions();
  const subscribedIds = new Set(subs.map((s) => s.sourceId));
  let changed = false;

  // Auto-subscribe to any bundled feeds not yet subscribed
  for (const source of FEED_REGISTRY) {
    if (source.type !== "bundled") continue;
    if (subscribedIds.has(source.id)) continue;

    try {
      const feed = await fetchFeed(source.url);
      // Use the registry ID as the canonical key — this ensures the
      // subscription sourceId always matches the cache key even if the
      // fetched feed.id differs from the registry.
      const feedId = source.id;
      const normalizedFeed = { ...feed, id: feedId, format: source.format ?? feed.format };
      await setCachedFeed(feedId, normalizedFeed);
      syncFeedDecksToStorage(normalizedFeed);

      subs.push({
        sourceId: feedId,
        url: source.url,
        type: "bundled",
        subscribedAt: Date.now(),
        lastRefreshedAt: Date.now(),
        lastVersion: feed.version,
      });
      changed = true;
    } catch (err) {
      console.error(`Failed to initialize feed "${source.id}":`, err);
    }
  }

  // Auto-refresh any subscription whose cache is older than FEED_STALE_AFTER_MS.
  // Applies to both bundled (local static assets) and remote (network) feeds.
  // Manual "Refresh all" / per-feed Refresh buttons bypass the TTL via refreshFeed().
  const now = Date.now();
  for (const sub of subs) {
    if (!subscribedIds.has(sub.sourceId)) continue;

    const isStale = now - sub.lastRefreshedAt >= FEED_STALE_AFTER_MS;
    if (!isStale) {
      const cached = getCachedFeed(sub.sourceId);
      if (cached) syncFeedDecksToStorage(cached);
      continue;
    }

    try {
      const feed = await fetchFeed(sub.url);
      const registrySource = FEED_REGISTRY.find((r) => r.id === sub.sourceId);
      const normalizedFeed = { ...feed, id: sub.sourceId, format: registrySource?.format ?? feed.format };
      await setCachedFeed(sub.sourceId, normalizedFeed);
      syncFeedDecksToStorage(normalizedFeed);
      sub.lastVersion = feed.version;
      sub.lastRefreshedAt = Date.now();
      if (sub.error !== undefined) sub.error = undefined;
      changed = true;
    } catch {
      // Fall back to cached data
      const cached = getCachedFeed(sub.sourceId);
      if (cached) {
        syncFeedDecksToStorage(cached);
      }
    }
  }

  if (changed) {
    saveFeedSubscriptions(subs);
  }
}

export async function subscribe(sourceOrUrl: string): Promise<Feed> {
  // Check if it's a registry feed ID
  const registrySource = FEED_REGISTRY.find((s) => s.id === sourceOrUrl);
  const url = registrySource?.url ?? sourceOrUrl;
  const type = registrySource?.type ?? "remote";

  const feed = await fetchFeed(url);

  await setCachedFeed(feed.id, feed);
  syncFeedDecksToStorage(feed);

  const subs = loadFeedSubscriptions();
  const existing = subs.find((s) => s.sourceId === feed.id);
  if (existing) {
    existing.lastRefreshedAt = Date.now();
    existing.lastVersion = feed.version;
    existing.error = undefined;
  } else {
    subs.push({
      sourceId: feed.id,
      url,
      type,
      subscribedAt: Date.now(),
      lastRefreshedAt: Date.now(),
      lastVersion: feed.version,
    });
  }

  saveFeedSubscriptions(subs);
  return feed;
}

export function unsubscribe(feedId: string): void {
  const origins = loadDeckOrigins();

  // Remove all decks belonging to this feed
  for (const [deckName, originFeedId] of Object.entries(origins)) {
    if (originFeedId === feedId) {
      localStorage.removeItem(STORAGE_KEY_PREFIX + deckName);
      removeDeckMeta(deckName);
      delete origins[deckName];

      if (localStorage.getItem(ACTIVE_DECK_KEY) === deckName) {
        localStorage.removeItem(ACTIVE_DECK_KEY);
      }
    }
  }

  saveDeckOrigins(origins);
  removeCachedFeed(feedId);

  const subs = loadFeedSubscriptions().filter((s) => s.sourceId !== feedId);
  saveFeedSubscriptions(subs);
}

export function listSubscriptions(): FeedSubscription[] {
  return loadFeedSubscriptions();
}

export { getCachedFeed } from "./feedPersistence";

export function getDeckFeedOrigin(deckName: string): string | null {
  return loadDeckOrigins()[deckName] ?? null;
}

export async function refreshFeed(feedId: string): Promise<Feed> {
  const subs = loadFeedSubscriptions();
  const sub = subs.find((s) => s.sourceId === feedId);
  if (!sub) throw new Error(`Not subscribed to feed "${feedId}"`);

  try {
    const feed = await fetchFeed(sub.url);
    await setCachedFeed(feed.id, feed);
    syncFeedDecksToStorage(feed);

    sub.lastRefreshedAt = Date.now();
    sub.lastVersion = feed.version;
    sub.error = undefined;
    saveFeedSubscriptions(subs);
    return feed;
  } catch (err) {
    sub.error = err instanceof Error ? err.message : String(err);
    saveFeedSubscriptions(subs);
    throw err;
  }
}

export async function refreshAllFeeds(): Promise<Map<string, Feed | Error>> {
  const results = new Map<string, Feed | Error>();
  const subs = loadFeedSubscriptions();

  for (const sub of subs) {
    try {
      const feed = await refreshFeed(sub.sourceId);
      results.set(sub.sourceId, feed);
    } catch (err) {
      results.set(sub.sourceId, err instanceof Error ? err : new Error(String(err)));
    }
  }

  return results;
}

export function adoptFeedDeck(deckName: string, newName?: string): string {
  const origins = loadDeckOrigins();
  const targetName = newName ?? deckName;

  if (newName && newName !== deckName) {
    // Copy deck data to new name
    const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
    if (raw) {
      localStorage.setItem(STORAGE_KEY_PREFIX + targetName, raw);
      stampDeckMeta(targetName);
    }
  }

  // Remove feed origin tracking (deck is now user-owned)
  delete origins[deckName];
  if (newName && newName !== deckName) {
    // Don't track the new name either
    delete origins[targetName];
  }
  saveDeckOrigins(origins);

  return targetName;
}

export function getFeedDecksByFeed(): Map<string, string[]> {
  const origins = loadDeckOrigins();
  const result = new Map<string, string[]>();

  for (const [deckName, feedId] of Object.entries(origins)) {
    const list = result.get(feedId);
    if (list) {
      list.push(deckName);
    } else {
      result.set(feedId, [deckName]);
    }
  }

  return result;
}
