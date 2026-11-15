import { useMemo, useState } from "react";

import type { GameObject, ObjectId } from "../../../adapter/types.ts";
import {
  filterCards,
  filterCardsByName,
  groupCards,
  orderCards,
  type FilterKey,
  type GroupKey,
  type SortKey,
} from "./gridSelection.ts";

type ObjLookup = Record<ObjectId, GameObject | undefined>;

/** A controlled binding for one organize axis. When supplied, the hook reads and
 *  writes through it (e.g. a persisted preference or an ephemeral UI slice); when
 *  omitted, the hook owns that axis with internal state seeded to "none". */
export interface AxisBinding<K> {
  value: K;
  onChange: (next: K) => void;
}

export interface UseCardOrganizerArgs {
  cards: ObjectId[];
  objects: ObjLookup;
  /** Engine-provided playable Set, consumed only by the `"playable"` filter.
   *  Never re-derived here — the engine owns legality. */
  playableIds?: ReadonlySet<ObjectId>;
  sort?: AxisBinding<SortKey>;
  group?: AxisBinding<GroupKey>;
  filter?: AxisBinding<FilterKey>;
  /** Optional display-name query binding. Like every organizer axis, this is
   * local presentation state and never changes engine-provided membership. */
  query?: AxisBinding<string>;
}

export interface CardOrganizer {
  sort: SortKey;
  setSort: (sort: SortKey) => void;
  group: GroupKey;
  setGroup: (group: GroupKey) => void;
  filter: FilterKey;
  setFilter: (filter: FilterKey) => void;
  query: string;
  setQuery: (query: string) => void;
  /** `cards` after the hide-filter (insertion order preserved). */
  filtered: ObjectId[];
  /** `filtered` after the optional display-name query. */
  matched: ObjectId[];
  /** `matched` after the sort. */
  ordered: ObjectId[];
  /** `ordered` bucketed by the group key (a single unnamed group when "none"). */
  groups: { key: string; ids: ObjectId[] }[];
}

// Stable empty Set so an absent `playableIds` never busts the `filtered` memo.
const EMPTY_PLAYABLE: ReadonlySet<ObjectId> = new Set();

/** Controllable per-axis state: a supplied binding wins; otherwise the hook owns
 *  the axis internally. `useState` is always called so hook order is stable
 *  regardless of whether the axis is controlled this render. */
function useControllableAxis<K>(
  binding: AxisBinding<K> | undefined,
  fallback: K,
): [K, (next: K) => void] {
  const [internal, setInternal] = useState<K>(fallback);
  return binding ? [binding.value, binding.onChange] : [internal, setInternal];
}

/**
 * Single client-side mechanism for organizing a list of card objects for DISPLAY
 * ONLY — shared by card-choice surfaces and the player's hand. Composes the
 * pure `filterCards` → `filterCardsByName` → `orderCards` → `groupCards`
 * building blocks and owns (or proxies, per axis) the sort/group/filter state.
 * It never mutates input, reorders `player.hand`, or touches the engine:
 * organizing is a view concern.
 */
export function useCardOrganizer({
  cards,
  objects,
  playableIds = EMPTY_PLAYABLE,
  sort: sortBinding,
  group: groupBinding,
  filter: filterBinding,
  query: queryBinding,
}: UseCardOrganizerArgs): CardOrganizer {
  const [sort, setSort] = useControllableAxis<SortKey>(sortBinding, "none");
  const [group, setGroup] = useControllableAxis<GroupKey>(groupBinding, "none");
  const [filter, setFilter] = useControllableAxis<FilterKey>(filterBinding, "none");
  const [query, setQuery] = useControllableAxis<string>(queryBinding, "");

  const filtered = useMemo(
    () => filterCards(cards, objects, filter, playableIds),
    [cards, objects, filter, playableIds],
  );
  const matched = useMemo(
    () => filterCardsByName(filtered, objects, query),
    [filtered, objects, query],
  );
  const ordered = useMemo(
    () => orderCards(matched, objects, sort),
    [matched, objects, sort],
  );
  const groups = useMemo(
    () => groupCards(ordered, objects, group),
    [ordered, objects, group],
  );

  return {
    sort,
    setSort,
    group,
    setGroup,
    filter,
    setFilter,
    query,
    setQuery,
    filtered,
    matched,
    ordered,
    groups,
  };
}
