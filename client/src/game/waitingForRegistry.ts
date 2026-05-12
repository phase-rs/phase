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
 * Discriminator strings the frontend has a user-facing UI handler for.
 * Every entry must correspond to a rendered modal, overlay, or in-line
 * affordance that resolves the prompt.
 */
export const HANDLED_WAITING_FOR_TYPES: ReadonlySet<WaitingFor["type"]> =
  new Set<WaitingFor["type"]>([
    // Active priority — passes via PassButton / mana payment / cast.
    "Priority",
    // Cast / activation chain
    "ManaPayment",
    "ChooseXValue",
    "PayAmountChoice",
    "PhyrexianPayment",
    "TargetSelection",
    "TriggerTargetSelection",
    "OptionalCostChoice",
    "DefilerPayment",
    "ModeChoice",
    "AbilityModeChoice",
    "AdventureCastChoice",
    "ModalFaceChoice",
    "WarpCostChoice",
    "EvokeCostChoice",
    "BestowCostChoice",
    "ChoosePermanentTypeSlot",
    "DiscardForCost",
    "SacrificeForCost",
    "ReturnToHandForCost",
    "BlightChoice",
    "BeholdForCost",
    "TapCreaturesForSpellCost",
    "ExileForCost",
    "HarmonizeTapChoice",
    "CollectEvidenceChoice",
    // Mana abilities
    "TapCreaturesForManaAbility",
    "DiscardForManaAbility",
    "ExileFromBattlefieldForManaAbility",
    "SacrificeForManaAbility",
    "PayManaAbilityMana",
    "ChooseManaColor",
    // Combat
    "DeclareAttackers",
    "DeclareBlockers",
    "AssignCombatDamage",
    "CombatTaxPayment",
    // Triggers / resolution-time choices
    "ReplacementChoice",
    "CopyTargetChoice",
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
    "CategoryChoice",
    "DistributeAmong",
    "RetargetChoice",
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
    "WardDiscardChoice",
    "WardSacrificeChoice",
    "UnlessBounceChoice",
    "DiscoverChoice",
    "CascadeChoice",
    "VoteChoice",
    "ChooseRingBearer",
    "ChooseDungeon",
    "ChooseDungeonRoom",
    "ChooseLegend",
    "CommanderZoneChoice",
    "BattleProtectorChoice",
    "NamedChoice",
    "UntapChoice",
    "CompanionReveal",
    // Game lifecycle
    "GameOver",
    "MulliganDecision",
    "MulliganBottomCards",
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
