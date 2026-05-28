import { useShallow } from "zustand/react/shallow";

import type { DungeonId, ObjectId, PlayerId } from "../adapter/types.ts";
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
  hasAny: boolean;
}

// `PlayerId` is a `u8` newtype, but serde stringifies it for HashMap keys.
// Equality checks (monarch === playerId) and array indexing (players[playerId])
// use the raw number; map lookups (ring_level, dungeon_progress) need the string.
const playerKey = (id: PlayerId): string => String(id);

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
  hasAny: false,
};

export function usePlayerDesignations(playerId: PlayerId): PlayerDesignations {
  return useGameStore(
    useShallow((s) => {
      const gs = s.gameState;
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
      const hasAny =
        isMonarch
        || hasInitiative
        || hasCityBlessing
        || activeDungeon != null
        || ringLevel > 0
        || energy > 0;
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
        hasAny,
      };
    }),
  );
}
