import { useMemo } from "react";

import type {
  DungeonRoomView,
  ObjectId,
  PendingNextSpellModifier,
  PendingSpellCostReduction,
  PlayerId,
  PlayerStatusView,
  UnboundedFamilyView,
  UnboundedResourceView,
} from "../adapter/types.ts";
import { useGameStore } from "../stores/gameStore.ts";

export interface PlayerDesignations {
  isMonarch: boolean;
  hasInitiative: boolean;
  hasCityBlessing: boolean;
  hasEnduringStory: boolean;
  ringLevel: number;
  ringBearerId: ObjectId | null;
  ringBearerName: string | null;
  energy: number;
  /** CR 309.4b-c: the engine's naming of the room the venture marker is on —
   *  dungeon, room name, printed effect, and the dungeon's room count. Null
   *  when the player is not venturing, which is also the only safe presence
   *  signal: `dungeon_progress` keeps a stale entry with `current_dungeon:
   *  null` after a dungeon is completed (CR 309.7), and the engine projects
   *  `derived.dungeon_rooms` only for an ACTIVE dungeon. The FE never derives
   *  the room's name or effect; that table lives in the engine. */
  dungeonRoom: DungeonRoomView | null;
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
  /** CR 732.2a: the engine's per-display-family collapse state for this player's `∞` badges.
   *  Shared empty array when none. The FE never re-derives these — the engine resolves them on
   *  the producing controller key, which does not survive onto the wire. */
  unboundedFamilies: UnboundedFamilyView[];
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
const NO_FAMILIES: UnboundedFamilyView[] = [];

const EMPTY: PlayerDesignations = {
  isMonarch: false,
  hasInitiative: false,
  hasCityBlessing: false,
  hasEnduringStory: false,
  ringLevel: 0,
  ringBearerId: null,
  ringBearerName: null,
  energy: 0,
  dungeonRoom: null,
  statusConditions: NO_CONDITIONS,
  pendingSpellModifiers: NO_MODIFIERS,
  pendingSpellReductions: NO_REDUCTIONS,
  unboundedResources: NO_UNBOUNDED,
  unboundedFamilies: NO_FAMILIES,
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
    const dungeonRoom = gs.derived?.dungeon_rooms?.[playerKey(playerId)] ?? null;
    const isMonarch = gs.monarch != null && gs.monarch === playerId;
    const hasInitiative = gs.initiative != null && gs.initiative === playerId;
    const hasCityBlessing = gs.city_blessing?.includes(playerId) ?? false;
    const hasEnduringStory = gs.enduring_story?.includes(playerId) ?? false;
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
    const unboundedFamilies = forPlayer(gs.derived?.unbounded_families, playerId, NO_FAMILIES);
    // The collapse question is answered per (seat, family) by the engine, on the producing
    // controller key before attribution rewrites `player`. Nothing here derives or joins it.
    const hasAny =
      isMonarch
      || hasInitiative
      || hasCityBlessing
      || hasEnduringStory
      || dungeonRoom != null
      || ringLevel > 0
      || energy > 0
      || statusConditions.length > 0
      || pendingSpellModifiers.length > 0
      || pendingSpellReductions.length > 0
      || unboundedResources.length > 0
      || unboundedFamilies.length > 0;
    return {
      isMonarch,
      hasInitiative,
      hasCityBlessing,
      hasEnduringStory,
      ringLevel,
      ringBearerId,
      ringBearerName,
      energy,
      dungeonRoom,
      statusConditions,
      pendingSpellModifiers,
      pendingSpellReductions,
      unboundedResources,
      unboundedFamilies,
      hasAny,
    };
  }, [gameState, playerId]);
}

/** Engine-projected player designation; this hook deliberately performs only membership lookup. */
export function useHasEnduringStory(playerId: PlayerId): boolean {
  return useGameStore((state) => state.gameState?.enduring_story?.includes(playerId) ?? false);
}

/** CR 732.2a: the engine-published repetition ceiling of the open loop-shortcut window, or
 *  `null` when none is open. Seat-free: `waiting_for` holds at most one such window. */
export function useBoundedLoopRepetitions(): number | null {
  return useGameStore((s) => s.gameState?.derived?.bounded_loop_max_repetitions ?? null);
}
