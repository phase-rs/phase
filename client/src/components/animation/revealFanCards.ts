import type { GameObject, ObjectId } from "../../adapter/types.ts";
import { getCardColors } from "../../animation/wubrgColors.ts";
import type { MillCard } from "./MillRevealAnimation.tsx";
import { visibleAnimationImageSnapshot } from "./ResolvedAnimationImage.tsx";

/**
 * The fan slots for a `CardsRevealed` event. The engine omits the id of any
 * revealed card that now sits in a library at a hidden position (CR 401.2: no
 * player may know a library's order) and keeps every name. When each name has
 * its id, each slot resolves the viewer's visible snapshot of that object;
 * when ids are missing, every slot renders from the engine-provided names
 * through the same name-keyed image lookup.
 */
export function revealFanCards(
  cardIds: readonly ObjectId[],
  cardNames: readonly string[],
  objects: Record<string, GameObject> | undefined,
): MillCard[] {
  if (cardIds.length === cardNames.length) {
    return cardIds.map((id) => {
      const object = objects?.[id];
      const snapshot = visibleAnimationImageSnapshot(object);
      return {
        objectId: id,
        snapshot,
        colors: snapshot ? getCardColors(object?.color ?? []) : [],
      };
    });
  }
  return cardNames.map((cardName, index) => {
    // A slot key only; never an engine object id.
    const slot = -(index + 1);
    return {
      objectId: slot,
      snapshot: { objectId: slot, cardName, faceIndex: 0, isToken: false },
      colors: [],
    };
  });
}
