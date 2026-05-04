import type { ScryfallCard } from "../../services/scryfall";
import type { DeckEntry } from "../../services/deckParser";
import { BASIC_LAND_NAMES } from "../../constants/game";

const WUBRG_COLORS = ["W", "U", "B", "R", "G"] as const;

function isLegendaryCreature(card: ScryfallCard): boolean {
  const typeLine = card.type_line.toLowerCase();
  return typeLine.includes("legendary") && typeLine.includes("creature");
}

/** CR 702.124: All partner-family keywords that allow co-commander pairing. */
const PARTNER_KEYWORDS = new Set([
  "Partner", "Partner with", "Friends forever",
  "Choose a Background", "Doctor's companion",
]);

function hasPartner(card: ScryfallCard): boolean {
  if (card.keywords) {
    return card.keywords.some((kw) => PARTNER_KEYWORDS.has(kw));
  }
  // Fallback for cards without keywords array
  return card.oracle_text?.toLowerCase().includes("partner") ?? false;
}

export function getCombinedColorIdentity(
  commanders: string[],
  cardDataCache: Map<string, ScryfallCard>,
): string[] {
  const identity = new Set<string>();
  for (const name of commanders) {
    const card = cardDataCache.get(name);
    if (card) {
      for (const c of card.color_identity) {
        identity.add(c);
      }
    }
  }
  return WUBRG_COLORS.filter((c) => identity.has(c));
}

function isInColorIdentity(card: ScryfallCard, identity: string[]): boolean {
  if (identity.length === 0) return true;
  const identitySet = new Set(identity);
  return card.color_identity.every((c) => identitySet.has(c));
}

export function getColorIdentityViolations(
  deck: DeckEntry[],
  commanders: string[],
  cardDataCache: Map<string, ScryfallCard>,
): string[] {
  if (commanders.length === 0) return [];
  const identity = getCombinedColorIdentity(commanders, cardDataCache);
  const violations: string[] = [];
  for (const entry of deck) {
    const card = cardDataCache.get(entry.name);
    if (card && !isInColorIdentity(card, identity)) {
      violations.push(entry.name);
    }
  }
  return violations;
}

export function getSingletonViolations(deck: DeckEntry[]): string[] {
  return deck
    .filter((e) => e.count > 1 && !BASIC_LAND_NAMES.has(e.name))
    .map((e) => e.name);
}

export function canBeCommander(card: ScryfallCard): boolean {
  return isLegendaryCreature(card) ||
    card.type_line.toLowerCase().includes("can be your commander");
}

export function canAddPartner(
  commanders: string[],
  card: ScryfallCard,
  cardDataCache: Map<string, ScryfallCard>,
): boolean {
  if (commanders.length === 0) return true;
  if (commanders.length >= 2) return false;
  const firstCard = cardDataCache.get(commanders[0]);
  return (firstCard ? hasPartner(firstCard) : false) && hasPartner(card);
}
