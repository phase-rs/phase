//! Wire-payload bounds for in-game `GameAction` bodies on the native WebSocket
//! path.
//!
//! The engine validates action *legality*, but a client controls the *size* of
//! the lists and strings inside a `GameAction`, and those reach clone-heavy
//! reducers before legality is fully resolved. This mirrors
//! `draft_action_payload_guard` (which bounds `DraftAction` lists) for the main
//! game action: reject adversarial multi-thousand-entry payloads up front.
//!
//! The cap is deliberately generous — far above any realistic game state,
//! including degenerate token-army boards — so it never rejects legitimate play;
//! it only blocks payloads engineered to force large allocations/clones.
use engine::types::actions::{DebugAction, DebugTokenRequest, GameAction};
use engine::types::game_state::ManaChoice;
use engine::types::proposed_event::TokenCharacteristics;

/// Max number of entries accepted in any single client-supplied action list
/// (targets, attackers, blockers, selections, reorder permutations, pile
/// partitions, distributions, ...). Chosen far above any realistic action list
/// while still rejecting adversarial payloads.
pub const MAX_ACTION_LIST_LEN: usize = 10_000;

/// Max length, in bytes, of a free-form choice string on the wire (a chosen
/// option / named card / mode label). Comfortably above the longest real card
/// name.
pub const MAX_CHOICE_LEN: usize = 256;

fn bound_list(field: &str, len: usize) -> Result<(), String> {
    if len > MAX_ACTION_LIST_LEN {
        return Err(format!(
            "{field} has {len} entries; at most {MAX_ACTION_LIST_LEN} allowed"
        ));
    }
    Ok(())
}

fn bound_batch_count(field: &str, count: u32) -> Result<(), String> {
    bound_list(field, count as usize)
}

fn bound_string(field: &str, value: &str) -> Result<(), String> {
    if value.len() > MAX_CHOICE_LEN {
        return Err(format!(
            "{field} is {} bytes; at most {MAX_CHOICE_LEN} allowed",
            value.len()
        ));
    }
    Ok(())
}

fn guard_mana_choice_payload(field: &str, choice: &ManaChoice) -> Result<(), String> {
    match choice {
        ManaChoice::SingleColor(_) => {}
        ManaChoice::Combination(mana) => {
            bound_list(field, mana.len())?;
        }
    }
    Ok(())
}

fn guard_token_characteristics_payload(
    field: &str,
    characteristics: &TokenCharacteristics,
) -> Result<(), String> {
    bound_string(
        &format!("{field}.display_name"),
        &characteristics.display_name,
    )?;
    bound_list(
        &format!("{field}.core_types"),
        characteristics.core_types.len(),
    )?;
    bound_list(&format!("{field}.subtypes"), characteristics.subtypes.len())?;
    for subtype in &characteristics.subtypes {
        bound_string(&format!("{field}.subtypes[]"), subtype)?;
    }
    bound_list(
        &format!("{field}.supertypes"),
        characteristics.supertypes.len(),
    )?;
    bound_list(&format!("{field}.colors"), characteristics.colors.len())?;
    bound_list(&format!("{field}.keywords"), characteristics.keywords.len())?;
    Ok(())
}

fn guard_debug_token_request_payload(request: &DebugTokenRequest) -> Result<(), String> {
    match request {
        DebugTokenRequest::Preset {
            preset_id,
            enter_with_counters,
            ..
        } => {
            bound_string("Debug.CreateToken.request.preset_id", preset_id)?;
            bound_list(
                "Debug.CreateToken.request.enter_with_counters",
                enter_with_counters.len(),
            )?;
        }
        DebugTokenRequest::Custom {
            characteristics,
            enter_with_counters,
            ..
        } => {
            guard_token_characteristics_payload(
                "Debug.CreateToken.request.characteristics",
                characteristics,
            )?;
            bound_list(
                "Debug.CreateToken.request.enter_with_counters",
                enter_with_counters.len(),
            )?;
        }
    }
    Ok(())
}

fn guard_debug_action_payload(action: &DebugAction) -> Result<(), String> {
    match action {
        DebugAction::CreateCard { card_name, .. } => {
            bound_string("Debug.CreateCard.card_name", card_name)?;
        }
        DebugAction::AddMana { mana, .. } => {
            bound_list("Debug.AddMana.mana", mana.len())?;
        }
        DebugAction::CreateToken { request } => {
            guard_debug_token_request_payload(request)?;
        }
        DebugAction::MoveToZone { .. }
        | DebugAction::RemoveObject { .. }
        | DebugAction::DrawCards { .. }
        | DebugAction::Mill { .. }
        | DebugAction::ShuffleLibrary { .. }
        | DebugAction::Proliferate { .. }
        | DebugAction::SetBasePowerToughness { .. }
        | DebugAction::ModifyCounters { .. }
        | DebugAction::SetTapped { .. }
        | DebugAction::SetPrepared { .. }
        | DebugAction::SetController { .. }
        | DebugAction::SetSummoningSickness { .. }
        | DebugAction::SetFaceState { .. }
        | DebugAction::Attach { .. }
        | DebugAction::Detach { .. }
        | DebugAction::GrantKeyword { .. }
        | DebugAction::RemoveKeyword { .. }
        | DebugAction::SetLife { .. }
        | DebugAction::ModifyPlayerCounters { .. }
        | DebugAction::ModifyEnergy { .. }
        | DebugAction::SetPhase { .. }
        | DebugAction::RunStateBasedActions
        | DebugAction::CreateTokenCopy { .. } => {}
    }
    Ok(())
}

/// Validate client-supplied `GameAction` payload sizes before engine dispatch.
/// Variants carrying only bounded scalars (object ids, indices, booleans) are
/// listed explicitly so newly added variants must be classified at compile time.
pub fn guard_game_action_payload(action: &GameAction) -> Result<(), String> {
    match action {
        GameAction::CastSpell { targets, .. }
        | GameAction::CastSpellWithPaymentMode { targets, .. } => {
            bound_list("CastSpell.targets", targets.len())?;
        }
        GameAction::SelectTargets { targets } => {
            bound_list("SelectTargets.targets", targets.len())?;
        }
        GameAction::DeclareAttackers { attacks } => {
            bound_list("DeclareAttackers.attacks", attacks.len())?;
        }
        GameAction::DeclareBlockers { assignments } => {
            bound_list("DeclareBlockers.assignments", assignments.len())?;
        }
        GameAction::AssignCombatDamage { assignments, .. } => {
            bound_list("AssignCombatDamage.assignments", assignments.len())?;
        }
        GameAction::ReorderHand { order } => {
            bound_list("ReorderHand.order", order.len())?;
        }
        GameAction::OrderTriggers { order } => {
            bound_list("OrderTriggers.order", order.len())?;
        }
        GameAction::SelectCards { cards } => {
            bound_list("SelectCards.cards", cards.len())?;
        }
        GameAction::SelectCoinFlips { keep_indices } => {
            bound_list("SelectCoinFlips.keep_indices", keep_indices.len())?;
        }
        GameAction::SelectModes { indices } => {
            bound_list("SelectModes.indices", indices.len())?;
        }
        GameAction::ChooseOutsideGameCards { selections } => {
            bound_list("ChooseOutsideGameCards.selections", selections.len())?;
        }
        GameAction::ChooseCounterMoveDistribution { selections } => {
            bound_list("ChooseCounterMoveDistribution.selections", selections.len())?;
        }
        GameAction::CrewVehicle { creature_ids, .. } => {
            bound_list("CrewVehicle.creature_ids", creature_ids.len())?;
        }
        GameAction::SaddleMount { creature_ids, .. } => {
            bound_list("SaddleMount.creature_ids", creature_ids.len())?;
        }
        GameAction::SubmitSideboard { main, sideboard } => {
            bound_list("SubmitSideboard.main", main.len())?;
            bound_list("SubmitSideboard.sideboard", sideboard.len())?;
        }
        GameAction::SubmitPilePartition { pile_a, .. } => {
            bound_list("SubmitPilePartition.pile_a", pile_a.len())?;
        }
        GameAction::SelectCategoryPermanents { choices } => {
            bound_list("SelectCategoryPermanents.choices", choices.len())?;
        }
        GameAction::SubmitPhyrexianChoices { choices } => {
            bound_list("SubmitPhyrexianChoices.choices", choices.len())?;
        }
        GameAction::ChooseManaColor { choice, count } => {
            guard_mana_choice_payload("ChooseManaColor.choice", choice)?;
            bound_batch_count("ChooseManaColor.count", *count)?;
        }
        GameAction::PayManaAbilityMana { payment } => {
            bound_list("PayManaAbilityMana.payment", payment.len())?;
        }
        GameAction::SetPhaseStops { stops } => {
            bound_list("SetPhaseStops.stops", stops.len())?;
        }
        GameAction::DistributeAmong { distribution, .. } => {
            bound_list("DistributeAmong.distribution", distribution.len())?;
        }
        GameAction::RetargetSpell { new_targets, .. } => {
            bound_list("RetargetSpell.new_targets", new_targets.len())?;
        }
        GameAction::ChooseOption { choice, .. } => {
            bound_string("ChooseOption.choice", choice)?;
        }
        GameAction::Debug(debug_action) => {
            guard_debug_action_payload(debug_action)?;
        }
        GameAction::PassPriority
        | GameAction::PlayLand { .. }
        | GameAction::Foretell { .. }
        | GameAction::ActivateAbility { .. }
        | GameAction::ChooseUntap { .. }
        | GameAction::ChooseExert { .. }
        | GameAction::ChooseClashOpponent { .. }
        | GameAction::MulliganDecision { .. }
        | GameAction::TapLandForMana { .. }
        | GameAction::UntapLandForMana { .. }
        | GameAction::ChooseTarget { .. }
        | GameAction::ChooseReplacement { .. }
        | GameAction::CancelCast
        | GameAction::Equip { .. }
        | GameAction::ActivateStation { .. }
        | GameAction::Transform { .. }
        | GameAction::PlayFaceDown { .. }
        | GameAction::TurnFaceUp { .. }
        | GameAction::ChoosePlayDraw { .. }
        | GameAction::ChoosePile { .. }
        | GameAction::ChooseBranch { .. }
        | GameAction::ChooseDamageSource { .. }
        | GameAction::DecideOptionalCost { .. }
        | GameAction::ChooseAdventureFace { .. }
        | GameAction::ChooseModalFace { .. }
        | GameAction::ChooseAlternativeCast { .. }
        | GameAction::ChooseCastingVariant { .. }
        | GameAction::KeepAllCopyTargets
        | GameAction::ChoosePermanentTypeSlot { .. }
        | GameAction::ActivateNinjutsu { .. }
        | GameAction::CastSpellAsSneak { .. }
        | GameAction::CastSpellAsSneakWithPaymentMode { .. }
        | GameAction::CastSpellAsWebSlinging { .. }
        | GameAction::CastSpellAsWebSlingingWithPaymentMode { .. }
        | GameAction::CastSpellForFree { .. }
        | GameAction::CastSpellForFreeWithPaymentMode { .. }
        | GameAction::CastSpellAsMiracle { .. }
        | GameAction::CastSpellAsMiracleWithPaymentMode { .. }
        | GameAction::CastSpellAsMadness { .. }
        | GameAction::CastSpellAsMadnessWithPaymentMode { .. }
        | GameAction::DecideOptionalEffect { .. }
        | GameAction::DecideOptionalEffectAndRemember { .. }
        | GameAction::PayUnlessCost { .. }
        | GameAction::ChooseUnlessCostBranch { .. }
        | GameAction::ChooseActivationCostBranch { .. }
        | GameAction::PayCombatTax { .. }
        | GameAction::ChooseRingBearer { .. }
        | GameAction::ChoosePair { .. }
        | GameAction::ChooseDungeon { .. }
        | GameAction::ChooseDungeonRoom { .. }
        | GameAction::UnlockRoomDoor { .. }
        | GameAction::TapForConvoke { .. }
        | GameAction::HarmonizeTap { .. }
        | GameAction::DeclareCompanion { .. }
        | GameAction::CompanionToHand
        | GameAction::DiscoverChoice { .. }
        | GameAction::CascadeChoice { .. }
        | GameAction::ChooseTopOrBottom { .. }
        | GameAction::ChooseLegend { .. }
        | GameAction::ChooseBattleProtector { .. }
        | GameAction::SetAutoPass { .. }
        | GameAction::CancelAutoPass
        | GameAction::SubmitPayAmount { .. }
        | GameAction::LearnDecision { .. }
        | GameAction::ChooseX { .. }
        | GameAction::CastPreparedCopy { .. }
        | GameAction::ChooseSpecializeColor { .. }
        | GameAction::CastParadigmCopy { .. }
        | GameAction::PassParadigmOffer
        | GameAction::GrantDebugPermission { .. }
        | GameAction::RevokeDebugPermission { .. }
        | GameAction::Concede { .. } => {}
    }
    Ok(())
}
