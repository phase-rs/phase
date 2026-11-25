import type { ScryfallCard } from "../../services/scryfall";

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
