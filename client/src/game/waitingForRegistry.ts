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

import type { GameState, WaitingFor } from "../adapter/types";

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
    "ModalFaceChoice",
    "AlternativeCastChoice",
    "SpliceOffer",
    // CR 702.140c + CR 730.2a: mutate top/bottom merge choice (MutateMergeChoiceModal).
    "MutateMergeChoice",
    // CR 702.99a: cipher encode-on-resolve choice (CipherEncodeChoiceModal).
    "CipherEncodeChoice",
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
    // CR 715.3a + CR 702.94a + CR 702.35a + CR 702.85a + CR 701.57a + CR 702.xxx:
    // unified special-cast offer (Adventure / Miracle / Madness / Cascade /
    // Discover / Paradigm); dispatches on `data.kind.type`.
    "CastOffer",
    // CR 701.36a: choose a creature token to copy (board click via TargetingOverlay).
    "PopulateChoice",
    // Mana abilities (cost-selection prompts now route through PayCost above).
    "PayManaAbilityMana",
    "ChooseManaColor",
    // Combat
    "DeclareAttackers",
    "DeclareBlockers",
    "AssignCombatDamage",
    // CR 702.22k: active player divides a banded blocker's combat damage
    // (BlockerDamageAssignmentModal, rendered via CardChoiceModal).
    "AssignBlockerDamage",
    "CombatTaxPayment",
    // Triggers / resolution-time choices
    "OrderTriggers",
    "ReplacementChoice",
    "CopyTargetChoice",
    "CopyRetarget",
    "ExploreChoice",
    // CR 303.4 + CR 115.1: return-as-Aura / non-spell Aura entry host pick.
    // Resolved on the board (object hosts) or via player HUD glow (Curse /
    // enchant-player Auras) — see TargetingOverlay + PlayerHud/OpponentHud.
    "ReturnAsAuraTarget",
    "EquipTarget",
    "CrewVehicle",
    "StationTarget",
    "SaddleMount",
    "ScryChoice",
    "CoinFlipKeepChoice",
    "DigChoice",
    "SurveilChoice",
    "RevealChoice",
    "SearchChoice",
    "SearchPartitionChoice",
    "OutsideGameChoice",
    "ChooseFromZoneChoice",
    // CR 701.4a: behold a [quality] — single-pick from a mixed-zone candidate
    // list (BeholdChoiceModal, rendered via CardChoiceModal).
    "BeholdChoice",
    "ChooseOneOfBranch",
    "ConniveDiscard",
    "DiscardChoice",
    "EffectZoneChoice",
    "DrawnThisTurnTopdeckChoice",
    "LearnChoice",
    "SpellbookDraft",
    "ManifestDreadChoice",
    "ClashChooseOpponent",
    "ClashCardPlacement",
    // CR 702.132a: Assist — caster picks a helper (AssistChoosePlayerModal),
    // then the helper commits generic mana (AssistPaymentUI).
    "AssistChoosePlayer",
    "AssistPayment",
    "TopOrBottomChoice",
    "ProliferateChoice",
    "TimeTravelChoice",
    "ChooseObjectsSelection",
    "CategoryChoice",
    "EachPlayerCopyChosenSelection",
    "KeepWithinTotalPowerChoice",
    "DistributeAmong",
    // CR 119.7 + CR 119.8: controller-chosen life-total redistribution permutation
    // (Reverse the Sands, The Doctor's Tomb) — rendered by LifeRedistributionModal.
    "RedistributeLifeTotals",
    "MoveCountersDistribution",
    // CR 107.1c: "remove any number of counters" (Rhys, Tetravus) — rendered by
    // MoveCountersDistributionModal in no-destination removal mode.
    "RemoveCountersChoice",
    "RetargetChoice",
    "CopyRetarget",
    "DamageSourceChoice",
    "DiscardToHandSize",
    "MiracleReveal",
    "TributeChoice",
    "PairChoice",
    "OpponentMayChoice",
    "OptionalEffectChoice",
    "UnlessPayment",
    "UnlessPaymentChooseCost",
    "WardDiscardChoice",
    "WardSacrificeChoice",
    "UnlessBounceChoice",
    "RevealUntilKeptChoice",
    "RepeatDecision",
    "VoteChoice",
    "SeparatePilesPartition",
    "SeparatePilesChoice",
    "ChooseRingBearer",
    "ChooseDungeon",
    "ChooseDungeonRoom",
    "SpecializeColor",
    // CR 709.5f-g: lock/unlock-door resolution choice (RoomDoorChoiceModal).
    "ChooseRoomDoor",
    "ChooseLegend",
    "CommanderZoneChoice",
    "BattleProtectorChoice",
    "NamedChoice",
    "OpponentGuess",
    "CostTypeChoice",
    "UntapChoice",
    "ChooseUntapSubset",
    "ExertChoice",
    "EnlistChoice",
    "CompanionReveal",
    // Game lifecycle
    "GameOver",
    "MulliganDecision",
    "OpeningHandBottomCards",
    "BetweenGamesSideboard",
    "BetweenGamesChoosePlayDraw",
  ]);

/**
 * Return true if `waitingFor.type` has a UI handler. Used by the safety-net
 * diagnostic modal to detect orphan WaitingFor states that would otherwise
 * silently hang the game.
 */
export function isWaitingForHandled(
  waitingFor: WaitingFor | null | undefined,
): boolean {
  if (!waitingFor) return true;
  return HANDLED_WAITING_FOR_TYPES.has(waitingFor.type);
}

/** A localized-reason descriptor: an i18n key plus optional interpolation params. */
export interface WaitingReason {
  key: string;
  params?: Record<string, unknown>;
}

/**
 * Map a pending decision to a human-readable *reason* (an i18n key under
 * `status.reason.*`) describing WHY the game is waiting. This is display
 * formatting over engine-provided facts (the `waiting_for` variant, plus
 * `phase` / `stack` for the generic priority window) — it labels a state the
 * engine already decided; it never infers game state.
 *
 * Returns `null` when there is nothing to narrate (no pending decision or the
 * game is over). Unknown variants fall through to a generic key so a new
 * engine `WaitingFor` variant degrades gracefully instead of breaking the
 * build (coverage of the common variants is asserted in tests, not via a
 * compile-time exhaustiveness check).
 */
export function waitingForReason(
  waitingFor: WaitingFor | null,
  gameState: GameState | null,
): WaitingReason | null {
  if (!waitingFor || waitingFor.type === "GameOver") return null;

  switch (waitingFor.type) {
    case "DeclareAttackers":
      return { key: "status.reason.declareAttackers" };
    case "DeclareBlockers":
      return { key: "status.reason.declareBlockers" };
    case "AssignCombatDamage":
    case "AssignBlockerDamage":
    case "DistributeAmong":
      return { key: "status.reason.assigningDamage" };
    case "TargetSelection":
    case "TriggerTargetSelection":
    case "MultiTargetSelection":
    case "CopyRetarget":
    case "RetargetChoice":
      return { key: "status.reason.choosingTargets" };
    case "ManaPayment":
    case "PhyrexianPayment":
    case "PayCost":
    case "PayManaAbilityMana":
    case "UnlessPayment":
      return { key: "status.reason.payingCost" };
    case "MulliganDecision":
    case "OpeningHandBottomCards":
      return { key: "status.reason.mulligan" };
    case "DiscardToHandSize":
    case "DiscardChoice":
      return { key: "status.reason.discarding" };
    case "OrderTriggers":
      return { key: "status.reason.orderingTriggers" };
    case "Priority": {
      // CR 117: the priority window. The engine-provided stack depth and phase
      // tell us what kind of window this is — purely descriptive labeling.
      if ((gameState?.stack.length ?? 0) > 0) {
        return { key: "status.reason.respondingToStack" };
      }
      switch (gameState?.phase) {
        case "BeginCombat":
        case "DeclareAttackers":
        case "DeclareBlockers":
        case "CombatDamage":
        case "EndCombat":
          return { key: "status.reason.priorityCombat" };
        case "PreCombatMain":
        case "PostCombatMain":
          return { key: "status.reason.priorityMain" };
        default:
          return { key: "status.reason.priority" };
      }
    }
    default:
      return { key: "status.reason.thinking" };
  }
}

/**
 * Map a reason to its standalone seat-badge key. `status.seat.*` mirrors
 * `status.reason.*` key-for-key but holds self-contained chip labels
 * ("Responding") instead of sentence fragments meant for composition
 * ("Your priority — responding to the stack").
 */
export function seatStatusKey(reason: WaitingReason | null): string {
  return (reason?.key ?? "status.reason.thinking").replace(
    "status.reason.",
    "status.seat.",
  );
}
