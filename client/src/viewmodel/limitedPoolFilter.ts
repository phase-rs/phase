import {
  DraftAdapter,
  type DraftCardInstance,
  type DraftPoolGroup,
  type DraftPoolGroupKind,
  type PoolFilter,
  type PoolFilterOptions,
} from "../adapter/draft-adapter";

// ── Limited build screen pool filters (#7507) ───────────────────────────
//
// The ENGINE is the single filtering authority (#7546 review):
// `draft_core::view::filter_pool_listing` decides which instances match, and
// this module only carries the display's PRESENTATION state — the typed
// `PoolFilter` the user has assembled, chip toggling, and the delegation call.
// Nothing here reads `DraftCardInstance` game data.

export const EMPTY_POOL_FILTER: PoolFilter = {
  query: "",
  types: [],
  colors: [],
  rarities: [],
};

/** Mirror of `PoolFilter::is_active`: is there anything to ask the engine? */
export function poolFilterActive(filter: PoolFilter): boolean {
  return (
    filter.query.trim() !== "" ||
    filter.types.length > 0 ||
    filter.colors.length > 0 ||
    filter.rarities.length > 0
  );
}

/** Toggle one kind within an axis selection. */
export function toggleKind(
  selected: DraftPoolGroupKind[],
  kind: DraftPoolGroupKind,
): DraftPoolGroupKind[] {
  return selected.includes(kind)
    ? selected.filter((k) => k !== kind)
    : [...selected, kind];
}

/**
 * The chips one axis offers: exactly the groups the engine delivered for this
 * pool, in engine order — no hand-kept kind list, no empty chips.
 */
export function axisKinds(groups: DraftPoolGroup[]): DraftPoolGroupKind[] {
  return groups.map((group) => group.kind);
}

const adapter = new DraftAdapter();

/**
 * Ask the engine which instances of `listing` the filter keeps, in listing
 * order. The display renders exactly this result. Classification happens
 * inside draft-core, so no wire-delivered groups are involved.
 */
export function filterPoolListing(
  listing: DraftCardInstance[],
  filter: PoolFilter,
): Promise<string[]> {
  return adapter.filterPoolListing(listing, filter);
}

/**
 * The engine-owned filter option lists for a pool whose delivered view
 * predates the option fields (legacy). Computed by draft-core from the
 * instances alone — never reconstructed here.
 */
export function fetchPoolFilterOptions(
  pool: DraftCardInstance[],
): Promise<PoolFilterOptions> {
  return adapter.poolFilterOptions(pool);
}
