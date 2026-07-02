import { useMemo } from "react";

import type {
  DungeonId,
  ObjectId,
  PendingNextSpellModifier,
  PendingSpellCostReduction,
  PlayerId,
  PlayerStatusView,
  UnboundedResourceView,
} from "../adapter/types.ts";
import { useGameStore } from "../stores/gameStore.ts";

export interface PlayerDesignations {
  isMonarch: boolean;
  hasInitiative: boolean;
  hasCityBlessing: boolean;
  ringLevel: number;
  ringBearerId: ObjectId | null;
  ringBearerName: string | null;
  energy: number;
  /** The active dungeon, or null when the player is not currently venturing.
   *  `dungeon_progress` may carry a stale entry with `current_dungeon: null`
   *  after a dungeon is completed, so this is the only safe presence signal. */
  activeDungeon: DungeonId | null;
  currentRoom: number;
  /** Engine-aggregated continuous conditions afflicting this player (can't gain
   *  life, can't cast, etc.). Shared empty array when none, so the memoized
   *  result stays stable in the dominant case. */
  statusConditions: PlayerStatusView[];
  /** CR 601.2f: pending one-shot modifiers for this player's next spell. */
  pendingSpellModifiers: PendingNextSpellModifier[];
  /** CR 601.2f: pending one-shot cost reductions for this player's next spell. */
  pendingSpellReductions: PendingSpellCostReduction[];
  /** CR 732.2a: engine-attributed unbounded-resource (`∞`) rows for this player.
   *  Shared empty array when none, so the memoized result stays stable. */
  unboundedResources: UnboundedResourceView[];
  hasAny: boolean;
}

// `PlayerId` is a `u8` newtype, but serde stringifies it for HashMap keys.
// Equality checks (monarch === playerId) and array indexing (players[playerId])
// use the raw number; map lookups (ring_level, dungeon_progress) need the string.
const playerKey = (id: PlayerId): string => String(id);

// Shared empty arrays: returned by reference when a player has no conditions /
// pending modifiers (the common case) so the memoized result can reuse stable
// references. A fresh `.filter([])` result would defeat that.
const NO_CONDITIONS: PlayerStatusView[] = [];
const NO_MODIFIERS: PendingNextSpellModifier[] = [];
const NO_REDUCTIONS: PendingSpellCostReduction[] = [];
const NO_UNBOUNDED: UnboundedResourceView[] = [];

const EMPTY: PlayerDesignations = {
  isMonarch: false,
  hasInitiative: false,
  hasCityBlessing: false,
  ringLevel: 0,
  ringBearerId: null,
  ringBearerName: null,
  energy: 0,
  activeDungeon: null,
  currentRoom: 0,
  statusConditions: NO_CONDITIONS,
  pendingSpellModifiers: NO_MODIFIERS,
  pendingSpellReductions: NO_REDUCTIONS,
  unboundedResources: NO_UNBOUNDED,
  hasAny: false,
};

/** Filter a per-player wire list to `playerId`, returning the shared empty
 *  constant (stable ref) when nothing matches. */
function forPlayer<T extends { player: PlayerId }>(
  all: T[] | undefined,
  playerId: PlayerId,
  empty: T[],
): T[] {
  if (!all || !all.some((entry) => entry.player === playerId)) return empty;
  return all.filter((entry) => entry.player === playerId);
}

export function usePlayerDesignations(playerId: PlayerId): PlayerDesignations {
  const gameState = useGameStore((s) => s.gameState);

  return useMemo(() => {
    const gs = gameState;
    if (!gs) return EMPTY;
    const dungeon = gs.dungeon_progress?.[playerKey(playerId)];
    const activeDungeon = dungeon?.current_dungeon ?? null;
    const isMonarch = gs.monarch != null && gs.monarch === playerId;
    const hasInitiative = gs.initiative != null && gs.initiative === playerId;
    const hasCityBlessing = gs.city_blessing?.includes(playerId) ?? false;
    const ringLevel = gs.ring_level?.[playerKey(playerId)] ?? 0;
    const ringBearerId = gs.ring_bearer?.[playerKey(playerId)] ?? null;
    const ringBearerName = ringBearerId != null ? (gs.objects[String(ringBearerId)]?.name ?? null) : null;
    const energy = gs.players[playerId]?.energy ?? 0;
    const statusConditions = forPlayer(gs.derived?.player_status, playerId, NO_CONDITIONS);
    const pendingSpellModifiers = forPlayer(
      gs.pending_next_spell_modifiers,
      playerId,
      NO_MODIFIERS,
    );
    const pendingSpellReductions = forPlayer(
      gs.pending_next_spell_cost_reductions,
      playerId,
      NO_REDUCTIONS,
    );
    const unboundedResources = forPlayer(
      gs.derived?.unbounded_resources,
      playerId,
      NO_UNBOUNDED,
    );
    const hasAny =
      isMonarch
      || hasInitiative
      || hasCityBlessing
      || activeDungeon != null
      || ringLevel > 0
      || energy > 0
      || statusConditions.length > 0
      || pendingSpellModifiers.length > 0
      || pendingSpellReductions.length > 0
      || unboundedResources.length > 0;
    return {
      isMonarch,
      hasInitiative,
      hasCityBlessing,
      ringLevel,
      ringBearerId,
      ringBearerName,
      energy,
      activeDungeon,
      currentRoom: dungeon?.current_room ?? 0,
      statusConditions,
      pendingSpellModifiers,
      pendingSpellReductions,
      unboundedResources,
      hasAny,
    };
  }, [gameState, playerId]);
}
