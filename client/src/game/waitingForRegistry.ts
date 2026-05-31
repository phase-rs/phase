// Centralized registry of every WaitingFor variant the frontend can present
// to the active player. Used by the unhandled-state safety net: if the engine
// emits a WaitingFor whose `type` is not in this set, the diagnostic modal
// surfaces a fail-loud prompt so the user can concede out instead of
// silently hanging on an orphan state.
//
// Adding a new player-facing WaitingFor variant on the engine side REQUIRES
// adding it here and wiring a corresponding modal/overlay in GamePage. Variants
// present in the TS WaitingFor union but absent from this set deliberately
// surface the diagnostic modal instead of silently hanging.

import type { WaitingFor } from "../adapter/types";

/**
 * CR 601.2g + CR 107.4f: WaitingFor variants resolved by the single
 * `ManaPaymentUI` overlay. The generic `ManaPayment` prompt and the per-shard
 * `PhyrexianPayment` prompt share one panel because both are caster-only cost
 * decisions for the same spell — `ManaPaymentUI` discriminates internally.
 *
 * This set is the single source of truth: `GamePage` gates the overlay's
 * mount on it, and `HANDLED_WAITING_FOR_TYPES` spreads it. Wiring the overlay
 * and registering it as "handled" therefore cannot drift apart.
 */
export const MANA_PAYMENT_WAITING_FOR_TYPES: ReadonlySet<WaitingFor["type"]> =
  new Set<WaitingFor["type"]>(["ManaPayment", "PhyrexianPayment"]);

/**
 * Discriminator strings the frontend has a user-facing UI handler for.
 * Every entry must correspond to a rendered modal, overlay, or in-line
 * affordance that resolves the prompt.
 */
export const HANDLED_WAITING_FOR_TYPES: ReadonlySet<WaitingFor["type"]> =
  new Set<WaitingFor["type"]>([
    // Active priority — passes via PassButton / mana payment / cast.
    "Priority",
    // Cast / activation chain — ManaPayment + PhyrexianPayment share ManaPaymentUI.
    ...MANA_PAYMENT_WAITING_FOR_TYPES,
    "ChooseXValue",
    "PayAmountChoice",
    "TargetSelection",
    "TriggerTargetSelection",
    "OptionalCostChoice",
    "ActivationCostOneOfChoice",
    "DefilerPayment",
    "ModeChoice",
    "AbilityModeChoice",
    "AdventureCastChoice",
    "ModalFaceChoice",
    "AlternativeCastChoice",
    "CastingVariantChoice",
    "ChoosePermanentTypeSlot",
    // CR 118.3 + CR 601.2b + CR 605.3b: unified cost-payment selection
    // (replaces the 11 old per-cost variants; dispatches on `kind`).
    "PayCost",
    "BlightChoice",
    "HarmonizeTapChoice",
    "CollectEvidenceChoice",
    // Multi-step target / offer choices rendered by CardChoiceModal.
    "MultiTargetSelection", // verified rendered: CardChoiceModal.tsx:216 case → :218 → MultiTargetSelectionModal (:1448)
    "ParadigmCastOffer", // verified rendered: CardChoiceModal.tsx:219 case → :221 → ParadigmCastOfferModal (:1515)
    // Note: `PopulateChoice` is intentionally NOT registered — it has no
    // renderer anywhere in client/src/, so the safety-net modal must fire for it.
    // Mana abilities (cost-selection prompts now route through PayCost above).
    "PayManaAbilityMana",
    "ChooseManaColor",
    // Combat
    "DeclareAttackers",
    "DeclareBlockers",
    "AssignCombatDamage",
    "CombatTaxPayment",
    // Triggers / resolution-time choices
    "OrderTriggers",
    "ReplacementChoice",
    "CopyTargetChoice",
    "CopyRetarget",
    "ExploreChoice",
    "EquipTarget",
    "CrewVehicle",
    "StationTarget",
    "SaddleMount",
    "ScryChoice",
    "DigChoice",
    "SurveilChoice",
    "RevealChoice",
    "SearchChoice",
    "SearchPartitionChoice",
    "OutsideGameChoice",
    "ChooseFromZoneChoice",
    "ChooseOneOfBranch",
    "ConniveDiscard",
    "DiscardChoice",
    "EffectZoneChoice",
    "DrawnThisTurnTopdeckChoice",
    "LearnChoice",
    "ManifestDreadChoice",
    "ClashCardPlacement",
    "TopOrBottomChoice",
    "ProliferateChoice",
    "ChooseObjectsSelection",
    "CategoryChoice",
    "DistributeAmong",
    "MoveCountersDistribution",
    "RetargetChoice",
    "CopyRetarget",
    "DamageSourceChoice",
    "DiscardToHandSize",
    "MiracleReveal",
    "MiracleCastOffer",
    "MadnessCastOffer",
    "TributeChoice",
    "PairChoice",
    "OpponentMayChoice",
    "OptionalEffectChoice",
    "UnlessPayment",
    "UnlessPaymentChooseCost",
    "WardDiscardChoice",
    "WardSacrificeChoice",
    "UnlessBounceChoice",
    "DiscoverChoice",
    "RevealUntilKeptChoice",
    "RepeatDecision",
    "CascadeChoice",
    "VoteChoice",
    "SeparatePilesPartition",
    "SeparatePilesChoice",
    "ChooseRingBearer",
    "ChooseDungeon",
    "ChooseDungeonRoom",
    "ChooseLegend",
    "CommanderZoneChoice",
    "BattleProtectorChoice",
    "NamedChoice",
    "UntapChoice",
    "ExertChoice",
    "CompanionReveal",
    // Game lifecycle
    "GameOver",
    "MulliganDecision",
    "MulliganBottomCards",
    "OpeningHandBottomCards",
    "BetweenGamesSideboard",
    "BetweenGamesChoosePlayDraw",
  ]);

/**
 * Return true if `waitingFor.type` has a UI handler. Used by the safety-net
 * diagnostic modal to detect orphan WaitingFor states that would otherwise
 * silently hang the game.
 */
export function isWaitingForHandled(waitingFor: WaitingFor | null | undefined): boolean {
  if (!waitingFor) return true;
  return HANDLED_WAITING_FOR_TYPES.has(waitingFor.type);
}
