import type { DeckEntry } from "../services/deckParser";

export interface FeedDeck {
  name: string;
  author?: string;
  description?: string;
  colors: string[];
  tags?: string[];
  main: DeckEntry[];
  sideboard: DeckEntry[];
  commander?: string[] | null;
  companion?: string;
}

export interface Feed {
  id: string;
  name: string;
  description?: string;
  icon?: string;
  format?: string;
  version: number;
  updated: string;
  source?: string;
  decks: FeedDeck[];
}

export interface FeedSource {
  id: string;
  name: string;
  description?: string;
  icon?: string;
  format?: string;
  type: "bundled" | "remote";
  url: string;
  /** Data provider credited with a link wherever the feed is listed. */
  attribution?: { name: string; url: string };
}

export interface FeedSubscription {
  sourceId: string;
  url: string;
  type: "bundled" | "remote";
  subscribedAt: number;
  lastRefreshedAt: number;
  lastVersion: number;
  error?: string;
}
