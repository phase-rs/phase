import type { ObjectId } from "../adapter/types.ts";
import { useGameStore } from "../stores/gameStore.ts";

// CR 732.2a / CR 701.34a: stable empty ref so a permanent with no unbounded counter
// (the dominant case) never re-renders on identity churn.
const EMPTY_UNBOUNDED_COUNTERS: string[] = [];

/**
 * CR 732.2a / CR 701.34a: the counter-type keys the engine marks as `∞` for this
 * object (`DerivedViews::unbounded_counters`). Keys match the object's `counters`
 * map (e.g. "charge"). DISPLAY-only — the object's real counter count is
 * deliberately left finite (`game/engine.rs::materialize_object_growth_shortcut`),
 * so a site that reads `obj.counters` alone shows a stale pre-shortcut number.
 *
 * Subscribed today by exactly three render sites: `board/PermanentCard`,
 * `card/ArtCropCard`, and `card/CardPreview`'s `CardInfoPanel`. Two counter render
 * sites remain unsubscribed, each with a measured blocker:
 *   - FU-A `controls/AttackTargetPicker` (`StackLabel`) — a chip stands for N grouped
 *     objects and the mark is per object id, so it needs an `.every()` intersection
 *     threaded through `groupByName`/`AttackerStack` (mirroring `isUnboundedPile`),
 *     not a representative lookup, or it renders a FALSE `∞`.
 *   - FU-B `hud/DialogAttachmentCard` — no component test file exists for it.
 * Do not claim this hook covers every counter render site until both land.
 */
export function useUnboundedCounterTypes(objectId: ObjectId): string[] {
  return useGameStore(
    (s) => s.gameState?.derived?.unbounded_counters?.[String(objectId)] ?? EMPTY_UNBOUNDED_COUNTERS,
  );
}
