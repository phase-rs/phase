import type { GameFormat } from "../adapter/types";
import {
  getDeckFeedOrigin,
  getCachedFeed,
  listSubscriptions,
  feedDeckToParsedDeck,
} from "./feedService";
import { FEED_REGISTRY } from "../data/feedRegistry";
import { FORMAT_REGISTRY } from "../data/formatRegistry";
import {
  listSavedDeckNames,
  loadDeckOrigins,
  loadSavedDeck,
  loadSavedDeckBracket,
} from "../constants/storage";
import type { CommanderBracket } from "../types/bracket";
import { getPreconBracket } from "../data/preconBrackets";
import type { Feed, FeedDeck } from "../types/feed";
import {
  isCommanderPreconDeck,
  loadPreconDeckMap,
  type DeckEntry as PreconDeckEntry,
} from "../hooks/useDecks";
import type { ParsedDeck } from "./deckParser";
import { preconDeckEntryToParsedDeck } from "./preconDecks";

export type DeckCatalogSource =
  | { type: "saved"; feedId?: string }
  | { type: "feed"; feedId: string }
  | { type: "precon"; deckId: string; code: string; releaseDate?: string };

export interface DeckCatalogCandidate {
  id: string;
  name: string;
  source: DeckCatalogSource;
  deck: ParsedDeck;
  knownFormat?: GameFormat;
  coveragePct?: number | null;
  bracket?: CommanderBracket | null;
  feedDeck?: FeedDeck;
  preconDeck?: PreconDeckEntry;
}

export interface DeckCatalogOptions {
  savedDeckNames?: string[];
  feedCache?: Record<string, Feed>;
  includePrecons?: boolean;
}

const FORMAT_BY_SOURCE_KEY = new Map(
  FORMAT_REGISTRY.flatMap((m) => [
    [m.format.toLowerCase(), m.format],
    [m.label.toLowerCase(), m.format],
    [m.short_label.toLowerCase(), m.format],
  ] as const),
);

export function sourceFormatToGameFormat(format: string | undefined): GameFormat | undefined {
  return format ? FORMAT_BY_SOURCE_KEY.get(format.trim().toLowerCase()) : undefined;
}

function registeredFeedFormat(feedId: string, feedFormat?: string): GameFormat | undefined {
  return sourceFormatToGameFormat(feedFormat ?? FEED_REGISTRY.find((source) => source.id === feedId)?.format);
}

export function savedDeckCatalogId(name: string): string {
  return `saved:${name}`;
}

export function feedDeckCatalogId(feedId: string, name: string): string {
  return `feed:${feedId}:${name}`;
}

export function preconDeckCatalogId(deckId: string): string {
  return `precon:${deckId}`;
}

export function knownFormatForSavedDeck(
  deckName: string,
  feedCache?: Record<string, Feed>,
): GameFormat | undefined {
  const origin = getDeckFeedOrigin(deckName);
  if (!origin) return undefined;
  const feed = feedCache?.[origin] ?? getCachedFeed(origin);
  return registeredFeedFormat(origin, feed?.format);
}

function cachedFeed(feedId: string, feedCache?: Record<string, Feed>): Feed | null {
  return feedCache?.[feedId] ?? getCachedFeed(feedId);
}

export async function buildDeckCatalog({
  savedDeckNames = listSavedDeckNames(),
  feedCache,
  includePrecons = true,
}: DeckCatalogOptions = {}): Promise<DeckCatalogCandidate[]> {
  const origins = loadDeckOrigins();
  const candidates: DeckCatalogCandidate[] = [];
  const savedDisplayNames = new Set<string>();
  const mirroredFeedNames = new Set<string>();

  for (const name of savedDeckNames) {
    const deck = loadSavedDeck(name);
    if (!deck) continue;
    const origin = origins[name];
    if (origin) mirroredFeedNames.add(name);
    candidates.push({
      id: savedDeckCatalogId(name),
      name,
      source: origin ? { type: "saved", feedId: origin } : { type: "saved" },
      deck,
      knownFormat: origin ? registeredFeedFormat(origin, cachedFeed(origin, feedCache)?.format) : undefined,
      bracket: loadSavedDeckBracket(name),
    });
    savedDisplayNames.add(name);
  }

  for (const sub of listSubscriptions()) {
    const feed = cachedFeed(sub.sourceId, feedCache);
    if (!feed) continue;
    const knownFormat = registeredFeedFormat(sub.sourceId, feed.format);
    for (const deck of feed.decks) {
      if (mirroredFeedNames.has(deck.name) || savedDisplayNames.has(deck.name)) continue;
      candidates.push({
        id: feedDeckCatalogId(sub.sourceId, deck.name),
        name: deck.name,
        source: { type: "feed", feedId: sub.sourceId },
        deck: feedDeckToParsedDeck(deck),
        knownFormat,
        bracket: null,
        feedDeck: deck,
      });
    }
  }

  if (!includePrecons) return candidates;

  const decks = await loadPreconDeckMap();
  if (!decks) return candidates;

  for (const [deckId, deck] of Object.entries(decks)) {
    if (!isCommanderPreconDeck(deck)) continue;
    const name = `${deck.name} (${deck.code})`;
    if (savedDisplayNames.has(name)) continue;
    savedDisplayNames.add(name);
    candidates.push({
      id: preconDeckCatalogId(deckId),
      name,
      source: { type: "precon", deckId, code: deck.code, releaseDate: deck.releaseDate },
      deck: preconDeckEntryToParsedDeck(deck),
      knownFormat: "Commander",
      coveragePct: deck.coveragePct,
      bracket: getPreconBracket(deckId),
      preconDeck: deck,
    });
  }

  return candidates;
}
