import type { CounterRowView, ObjectCounterDisplay, ObjectId } from "../adapter/types.ts";
import { useGameStore } from "../stores/gameStore.ts";

// CR 732.2a / CR 701.34a: stable empty refs so an object with no counter row (the dominant
// case) never re-renders on identity churn.
const EMPTY_DISPLAY: ObjectCounterDisplay = {};
const EMPTY_PILLS: ReadonlyArray<CounterRowView> = [];

/**
 * CR 122.1 + CR 306.5c: every counter row this object renders, exactly as the engine
 * partitioned and ordered them.
 *
 * The engine's `counter_display` projection is the SINGLE authority for counter display. It
 * already split the loyalty TOTAL out of the pill strip (CR 306.5c), deduplicated across seats,
 * and ordered the rows (`∞` first, then `CounterType` order). So this hook joins nothing, filters
 * nothing, sorts nothing, and interprets no counter type — it is one keyed lookup.
 *
 * ZERO COUNTS ARE DROPPED IN THE FINITE PASS ONLY, and a consumer that re-filters on
 * `count > 0` therefore deletes real rows. `counter_display_views`' finite pass admits through
 * `positive_counter_entries` (CR 122.1 — a zero map entry is not a marker), so no `Finite` row
 * carries `count: 0`; its UNBOUNDED pass has NO zero filter and reads the live count for a
 * REGISTERED pair, so an `Unbounded` row legitimately carries `count: 0` for a pair the loop
 * pumps `0 -> 1`.
 *
 * THERE IS NO FALLBACK TO `objects[id].counters`, AND ONE MUST NOT BE ADDED — not here, not in a
 * render site, not in `groupKey`. A frame that arrives with no `derived` renders NO counter pills
 * at all, where the superseded hook still rendered the finite ones. That is the correct outcome
 * of deleting a second authority: `adapter/types.ts` states of `derived` that "Consumers MUST
 * treat absence as 'no data' and MUST NOT synthesize grouped values client-side — that's a
 * CLAUDE.md violation", and the deleted fallback was itself a standing violation of that
 * contract. The consequence is that a dropped-`derived` adapter regression — `ws-adapter.ts`
 * records a real past one — now fails VISIBLY instead of silently half-correct.
 *
 * ZUSTAND v5 HAZARD, eliminated rather than mitigated: there is no equality argument and no
 * `shallow` default in v5 — the selector result IS React's `getSnapshot` return, compared with
 * `Object.is`. A selector that ALLOCATES returns a fresh reference on every store read, fails
 * React's getSnapshot cache check, and produces "The result of getSnapshot should be cached to
 * avoid an infinite loop" plus a render loop. `tsc` cannot see it. The single selector below
 * returns only a store-owned ref or a module constant, so there is nothing left to memoize.
 *
 * SUPERSEDED — kept as a record of why, because deleting it would invite the same design again.
 * This hook's doc once prescribed an `.every()` intersection through `groupByName`/`AttackerStack`,
 * mirroring `isUnboundedPile`. That solves only the FALSE-`∞` half: `.every()` degrades a group
 * whose members disagree to `×N`, which HIDES a real `∞` and contradicts the polarity
 * `derive_views` states for this subsystem. `groupKey` instead keys on the engine's rendered rows,
 * so members that render differently never group at all — no false `∞` and no hidden real one, and
 * the fix lands at every `groupByName` consumer at once instead of per chip. `isUnboundedPile`'s
 * `.every()` stays as written: it is a fail-safe over a channel `groupKey` does not key on.
 *
 * Subscribed today by exactly SIX render sites — EVERY counter DISPLAY surface in the client:
 * `board/PermanentCard`, `card/ArtCropCard`, `card/CardPreview`'s `CardInfoPanel`,
 * `controls/AttackTargetPicker`'s `StackLabel`, `hud/DialogAttachmentCard`, and
 * `zone/ZoneViewer`'s public-exile time-counter badge. FU-B (the last
 * pre-ZoneViewer one) landed with this ledger revision; it previously enumerated and re-filtered
 * `obj.counters`, so it could not express an `Unbounded` pill at all.
 *
 * Every surviving reader of the raw `objects[id].counters` map, measured with `git grep` and
 * cross-checked with `ast-grep` (an `Object.entries`-shaped pattern alone MISSES the indexed
 * reads, which is how an earlier census undercounted this list). BOTH are DELIBERATE, PERMANENT
 * EXCLUSIONS — neither is a display site, and neither is a pending conversion. There is no
 * remaining conversion work:
 *   - `modal/CardChoiceModal` (`:1712`, `:1715`, in `removableCounterCostEntries`) — NOT a
 *     display site and NOT a pending conversion. It enumerates which counters are legal to
 *     REMOVE AS A COST — CR 118.3, a player can't pay a cost without the resources to pay it
 *     fully, which is exactly the `count > 0` filter — so it must read the live payable map. It
 *     is reached for ability costs as well as spell costs, so the general cost rule governs, not
 *     the spell-casting payment step. It deliberately keeps
 *     `loyalty` (removing loyalty counters is a payable cost) where the display projection
 *     splits loyalty out per CR 306.5c, and a cost must be paid in real counters, so an
 *     `Unbounded` magnitude would be actively wrong here.
 *   - `chrome/DebugCardContextMenu` (`:253`, `:254`, `:258`, `:261`) — NOT a display site and NOT
 *     a pending conversion. It is a debug counter EDITOR: each `CounterRow` reads the current
 *     value of the counter its own +/- buttons are about to `ModifyCounters`, `loyalty`
 *     included. It needs the writable map it mutates, not the CR 306.5c-partitioned view.
 *
 * This hook now covers every counter render site. A NEW raw-map DISPLAY reader is a regression,
 * not an omission — add it to the subscribed list above, or justify it here as a third exclusion.
 */
export function useCounterDisplay(objectId: ObjectId): ObjectCounterDisplay {
  return useGameStore(
    (s) => s.gameState?.derived?.counter_display?.[String(objectId)] ?? EMPTY_DISPLAY,
  );
}

/** The pill rows, in engine order. Never sort or filter the result. */
export const pillsOf = (display: ObjectCounterDisplay): ReadonlyArray<CounterRowView> =>
  display.pills ?? EMPTY_PILLS;

/**
 * The engine-projected time-counter row, if the object has one. This is deliberately only a
 * display-row lookup: callers must not use it to infer a keyword or rules status (for example,
 * whether an exiled card is suspended).
 */
export const timeCounterOf = (display: ObjectCounterDisplay): CounterRowView | undefined =>
  pillsOf(display).find((row) => row.counter === "time");

/**
 * The single spelling of the engine enum → render-time distinction, so the six render sites
 * cannot drift. An absent `magnitude` is the serde default, `"Finite"`.
 */
export const isUnbounded = (row?: CounterRowView): boolean => row?.magnitude === "Unbounded";
