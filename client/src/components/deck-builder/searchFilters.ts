import type { CardSearchFilters } from "./CardSearch";

/**
 * True when the user has entered real search criteria. Deliberately excludes
 * `browseFormat` (a legality lens, not a query). This is the single authority
 * for "is a search active" — the search trigger, the debounce, and the deck
 * builder's deck-vs-results canvas swap all key off it, so they can't desync.
 *
 * Lives in its own module (not CardSearch.tsx) so consumers can import it
 * without pulling in the CardSearch component — and so test mocks of CardSearch
 * don't shadow it.
 */
export function hasSearchCriteria(filters: CardSearchFilters): boolean {
  return Boolean(
    // `text` is the raw, untrimmed input box value, so a whitespace-only query
    // ("   ") is not real criteria — trim before testing, or it would trip the
    // search trigger + deck-vs-results canvas swap for an effectively empty query.
    filters.text.trim()
      || filters.colors.length > 0
      || filters.type
      || filters.cmcMax !== undefined
      || filters.sets.length > 0,
  );
}
