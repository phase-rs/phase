import type { ManaCost } from "../adapter/types.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { spellCostDisplay } from "../viewmodel/costLabel.ts";

/**
 * CR 709.3 + CR 712.11b: the OTHER castable spell face's cost badge for a card
 * whose player chooses a face at cast time (a Room or other split card, a
 * spell//spell MDFC). The engine publishes that face's live cost in
 * `DerivedViews.back_face_spell_costs`; its presence is the engine's statement
 * that the card has two payable spell faces, so nothing here inspects layouts.
 * `spellCostDisplay` styles it against the face's printed cost exactly as the
 * live face's badge is styled. `undefined` when the engine published nothing —
 * and when the FRONT has no live cost (`spellCosts` empty: replay, or a state
 * outside the viewer's priority), because a pair must not set a printed front
 * beside a live back.
 */
export function useBackFaceSpellCost(
  objectId: number | undefined,
  printedBackFaceCost: ManaCost | undefined,
): { cost: ManaCost; isReduced: boolean } | undefined {
  const effective = useGameStore((s) =>
    objectId == null ? undefined : s.gameState?.derived?.back_face_spell_costs?.[String(objectId)],
  );
  const frontIsLive = useGameStore((s) =>
    objectId != null && s.spellCosts[String(objectId)] != null,
  );
  if (!effective || !printedBackFaceCost || !frontIsLive) return undefined;
  const { displayCost, isReduced } = spellCostDisplay(effective, printedBackFaceCost);
  return { cost: displayCost, isReduced };
}
