import { useMemo } from "react";

import type { GameObject, PlayerId } from "../adapter/types.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { collectObjectActions, isManaObjectAction } from "../viewmodel/cardActionChoice.ts";

/**
 * Castable / activatable objects in a player's graveyard or exile — engine
 * authority. Only objects the engine surfaces a legal action for (CastSpell /
 * PlayLand / an activated ability) are returned, so foretell / flashback /
 * escape / adventure-from-exile etc. appear exactly when their timing and
 * permission predicates pass.
 *
 * CR 702.143a: foretold cards in exile are face-down but their owner may cast
 * them later — the engine only emits `CastSpell` once timing allows, so
 * `legalActionsByObject` is the single source of truth here. There is
 * deliberately NO client-side `!face_down` filter (that was an override that
 * hid foretold cards from their owner — issue #320).
 *
 * CR 702.66 (Delve) / CR 702.51 (Convoke): a mana-payment tap (`TapForConvoke`,
 * classified by `isManaObjectAction`) is NOT a cast/play affordance — it exiles
 * the graveyard card to pay a cost. Those are excluded here so a Delve payment
 * doesn't dump the entire graveyard into the hand fan; the player exiles them
 * from the graveyard delve modal (`ZoneViewer`, `canDelveFromGraveyard`), which
 * keeps exactly the actions this hook drops.
 *
 * Consumed by `PlayerHand` to render the in-fan castable graveyard/exile wings.
 */
export function useCastableZoneObjects(
  zone: "exile" | "graveyard",
  playerId: PlayerId,
): GameObject[] {
  const objects = useGameStore((s) => s.gameState?.objects);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const graveyard = useGameStore((s) => s.gameState?.players[playerId]?.graveyard);
  const exile = useGameStore((s) => s.gameState?.exile);

  const zoneObjectIds = useMemo(() => {
    if (zone === "graveyard") return graveyard ?? [];
    if (!exile || !objects) return [];
    return exile.filter((id) => objects[id]?.owner === playerId);
  }, [zone, graveyard, exile, objects, playerId]);

  return useMemo(() => {
    if (!objects) return [];
    return zoneObjectIds
      .map((id) => objects[id])
      .filter((obj): obj is GameObject => {
        if (!obj) return false;
        // Keep only cards the engine surfaces a genuine cast/play affordance
        // for. A lone mana-payment tap (delve/convoke) is excluded — see the
        // doc comment above.
        return collectObjectActions(legalActionsByObject, obj.id).some(
          (action) => !isManaObjectAction(action, obj),
        );
      });
  }, [zoneObjectIds, objects, legalActionsByObject]);
}
