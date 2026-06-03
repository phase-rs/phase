import type { ScryfallCard } from "../../services/scryfall";
import type { DeckEntry } from "../../services/deckParser";
import { BASIC_LAND_NAMES } from "../../constants/game";

const WUBRG_COLORS = ["W", "U", "B", "R", "G"] as const;

// CR 702.124: Partner-pairing legality (including Doctor's Companion / Choose a
// Background) is owned by the engine (`can_pair_commanders`) and consumed via
// `commanderPartnerCandidates` in engineRuntime — never re-derived here.

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

/**
 * CR 903.5b: Names appearing above their effective copy cap in a singleton
 * (Commander) deck, excluding basic lands. The cap comes from the engine-backed
 * `getEffectiveCap` resolver (1 by default; raised for "up to N" override cards
 * like Nazgûl → 9) — never inferred from Oracle text client-side.
 */
export function getSingletonViolations(
  deck: DeckEntry[],
  _cardDataCache: Map<string, ScryfallCard>,
  getEffectiveCap: (name: string) => number,
): string[] {
  return deck
    .filter((e) => e.count > getEffectiveCap(e.name) && !BASIC_LAND_NAMES.has(e.name))
    .map((e) => e.name);
}
