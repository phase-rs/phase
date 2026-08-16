import type { GameAction, ObjectId, PlayerId } from "../adapter/types";
import { useGameStore } from "../stores/gameStore";
import { applySpellPaymentPreference } from "./castPaymentMode";

/**
 * Ask the active adapter for the engine's automatic-payment source preview.
 * A `null` result means this action has no automatic payment preview, the
 * adapter cannot provide one, or the engine advanced while the request ran.
 */
export async function previewAutomaticManaPayment(
  action: GameAction,
  actor: PlayerId,
): Promise<ObjectId[] | null> {
  const submittedAction = applySpellPaymentPreference(action);
  if (
    submittedAction.type !== "CastSpell"
    || submittedAction.data.payment_mode?.type === "Manual"
  ) {
    return null;
  }

  const store = useGameStore.getState();
  if (!store.adapter?.previewManaPayment) return null;

  const previewEpoch = store.engineCommitEpoch;
  const sourceIds = await store.adapter.previewManaPayment(submittedAction, actor);
  return useGameStore.getState().engineCommitEpoch === previewEpoch ? sourceIds : null;
}
