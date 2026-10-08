import type { AbilityBlockKind } from "../adapter/types";

/**
 * CR 602.5 + CR 118.3: Maps an engine `AbilityBlockKind` to its i18n reason key.
 *
 * Pure display formatting — no game logic. Exhaustive over the union, so a new
 * engine kind is a compile error until a key is added here.
 *
 * **Two channels, one map — and every consumer carries keys it can never use.**
 * The engine publishes these kinds on two independent surfaces:
 *
 *   * `CantBeActivated` / `CantActivateDuring` / `Prohibited` ride
 *     `GameObject.blocked_abilities`, populated by the `derived.rs` CR 602.5
 *     prohibition sweep and rendered by the board badge and the inspect panel.
 *   * `CostNotPayableNow` rides the legal-actions payload
 *     (`LegalActionsResult.activationBlockReasons`), is scoped to the acting
 *     player, and is rendered by the ability picker.
 *
 * The channel is the filter: a consumer of one surface will NEVER receive the
 * other's kinds, so there is deliberately no `BOARD_BADGE_BLOCK_KINDS`-style
 * allowlist to keep in sync. The board badge is not broken because it has a
 * `costNotPayableNow` key it never renders, and the picker is not broken
 * because it has three prohibition keys it never renders.
 */
export const ABILITY_BLOCK_REASON_KEY: Record<AbilityBlockKind, string> = {
  CantBeActivated: "abilityBlock.cantBeActivated",
  CantActivateDuring: "abilityBlock.cantActivateDuring",
  Prohibited: "abilityBlock.prohibited",
  CostNotPayableNow: "abilityBlock.costNotPayableNow",
};
