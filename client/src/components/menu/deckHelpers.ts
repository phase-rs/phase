import { loadSavedDeck } from "../../constants/storage";
import { getDeckFeedOrigin, getCachedFeed } from "../../services/feedService";
import type { ParsedDeck } from "../../services/deckParser";
import { BASIC_LAND_NAMES } from "../../constants/game";

export function loadDeck(deckName: string): ParsedDeck | null {
  return loadSavedDeck(deckName);
}

export function getDeckColorIdentity(deckName: string): string[] | null {
  const feedId = getDeckFeedOrigin(deckName);
  if (feedId) {
    const feed = getCachedFeed(feedId);
    const feedDeck = feed?.decks.find((d) => d.name === deckName);
    if (feedDeck) return feedDeck.colors;
  }
  return null;
}

/** Mana-symbol shards for a resolved deck identity. Empty identities are colorless. */
export function getDeckColorIdentityPips(colors: string[] | null): string[] | null {
  if (colors === null) return null;
  return colors.length > 0 ? colors : ["C"];
}

export function getDeckCardCount(deckName: string): number {
  const deck = loadDeck(deckName);
  if (!deck) return 0;

  const mainCount = deck.main.reduce((sum, entry) => sum + entry.count, 0);
  const commanders = deck.commander ?? [];
  const representedInMain = commanders.filter((name) =>
    deck.main.some((entry) => entry.name.toLowerCase() === name.toLowerCase()),
  ).length;
  return mainCount + (commanders.length - representedInMain);
}

export function getRepresentativeCard(deckName: string): string | null {
  const deck = loadDeck(deckName);
  if (!deck) return null;
  if (deck.commander && deck.commander.length > 0) {
    return deck.commander[0];
  }
  const entry = deck.main.find((item) => !BASIC_LAND_NAMES.has(item.name));
  return entry?.name ?? null;
}

export function isBundledDeck(deckName: string): boolean {
  return getDeckFeedOrigin(deckName) !== null;
}
