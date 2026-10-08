import type { TFunction } from "i18next";

import type { SerializedAbilityCost } from "../../adapter/types.ts";

/**
 * CR 601.2f-h: Compact display copy for the non-mana portion of an
 * alternative cost (Solitude's Evoke "Exile a white card from your hand",
 * Sabin's Blitz "Discard a card", Tenacious Underdog's Blitz "Pay 2 life",
 * Detective's Phoenix's Bestow "Collect evidence 6"). Mirrors the engine's
 * typed `AbilityCost` taxonomy 1:1 by the discriminant `type` field and shows
 * the amounts the engine provides — the FE does not interpret game state, it
 * just renders the engine-provided variant.
 */
export function describeAdditionalCost(
  cost: SerializedAbilityCost,
  t: TFunction<"game">,
): string {
  switch (cost.type) {
    case "Exile":
      return t("alternativeCost.additionalExile");
    case "Sacrifice":
      return t("alternativeCost.additionalSacrifice");
    case "PayLife": {
      const amount = fixedAmount(cost.amount);
      return amount === null
        ? t("alternativeCost.additionalPayLife")
        : t("alternativeCost.additionalPayLifeAmount", { amount });
    }
    case "Discard":
      return t("alternativeCost.additionalDiscard");
    case "TapCreatures":
      return t("alternativeCost.additionalTapCreatures");
    case "CollectEvidence":
      return typeof cost.amount === "number"
        ? t("alternativeCost.additionalCollectEvidence", { amount: cost.amount })
        : t("alternativeCost.additionalGeneric", { type: cost.type });
    default:
      return t("alternativeCost.additionalGeneric", { type: cost.type });
  }
}

/** The engine-provided number of a `QuantityExpr` the engine sent as `Fixed`. */
function fixedAmount(amount: unknown): number | null {
  if (
    typeof amount === "object"
    && amount !== null
    && (amount as { type?: unknown }).type === "Fixed"
    && typeof (amount as { value?: unknown }).value === "number"
  ) {
    return (amount as { value: number }).value;
  }
  return null;
}
