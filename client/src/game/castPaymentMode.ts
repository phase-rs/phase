import type { CastPaymentMode, GameAction } from "../adapter/types";
import { usePreferencesStore } from "../stores/preferencesStore";
import { useUiStore } from "../stores/uiStore";

const MANUAL_CAST_PAYMENT_MODE: CastPaymentMode = { type: "Manual" };
const AUTO_EXCEPT_SACRIFICIAL_MANA_PAYMENT_MODE: CastPaymentMode = {
  type: "AutoExceptSacrificialMana",
};

export function applySpellPaymentPreference(action: GameAction): GameAction {
  // Manual is the per-game escape hatch and deliberately dominates the saved
  // preference. Otherwise stamp every cast-family action with the exact engine
  // mode so all alternate casting routes share the same payment semantics.
  const preference = usePreferencesStore.getState().spellPaymentMode;
  const mode = useUiStore.getState().manualManaOverride || preference === "manual"
    ? MANUAL_CAST_PAYMENT_MODE
    : preference === "autoExceptSacrificialMana"
      ? AUTO_EXCEPT_SACRIFICIAL_MANA_PAYMENT_MODE
      : null;
  if (!mode) return action;

  switch (action.type) {
    case "CastSpell":
      return {
        ...action,
        data: { ...action.data, payment_mode: mode },
      };
    case "CastSpellForFree":
      return {
        ...action,
        data: { ...action.data, payment_mode: mode },
      };
    case "CastSpellAsMiracle":
      return {
        ...action,
        data: { ...action.data, payment_mode: mode },
      };
    case "CastSpellAsMadness":
      return {
        ...action,
        data: { ...action.data, payment_mode: mode },
      };
    case "CastSpellAsSneak":
      return {
        ...action,
        data: { ...action.data, payment_mode: mode },
      };
    case "CastSpellAsWebSlinging":
      return {
        ...action,
        data: { ...action.data, payment_mode: mode },
      };
    default:
      return action;
  }
}
