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
use engine::types::counter::CounterType;
use engine::types::game_state::ManaChoice;
use engine::types::proposed_event::TokenCharacteristics;
use serde::Serialize;

/// Max number of entries accepted in any single client-supplied action list
/// (targets, attackers, blockers, selections, reorder permutations, pile
/// partitions, distributions, ...). Chosen far above any realistic action list
/// while still rejecting adversarial payloads.
pub const MAX_ACTION_LIST_LEN: usize = 10_000;

/// Max length, in bytes, of a free-form choice string on the wire (a chosen
/// option / named card / mode label). Comfortably above the longest real card
/// name.
pub const MAX_CHOICE_LEN: usize = 256;

/// Max serialized size for nested debug-only AST payloads that can contain
/// strings, vectors, or filters. Debug actions are still client-supplied game
/// actions, so they must not forward arbitrarily large nested payloads into the
/// engine reducers.
pub const MAX_DEBUG_AST_JSON_LEN: usize = 16 * 1024;

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

fn bound_serialized_json<T: Serialize>(field: &str, value: &T) -> Result<(), String> {
    struct LimitingWriter {
        written: usize,
    }

    impl std::io::Write for LimitingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let Some(written) = self.written.checked_add(buf.len()) else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "serialized size overflow",
                ));
            };
            self.written = written;
            if self.written > MAX_DEBUG_AST_JSON_LEN {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "serialized size limit exceeded",
                ));
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = LimitingWriter { written: 0 };
    serde_json::to_writer(&mut writer, value)
        .map_err(|err| format!("{field} size validation failed or exceeded limit: {err}"))
}

fn guard_counter_type_payload(field: &str, counter_type: &CounterType) -> Result<(), String> {
    match counter_type {
        CounterType::Generic(name) => bound_string(&format!("{field}.Generic"), name)?,
        CounterType::Plus1Plus1
        | CounterType::Minus1Minus1
        | CounterType::PowerToughness { .. }
        | CounterType::Loyalty
        | CounterType::Defense
        | CounterType::Stun
        | CounterType::Lore
        | CounterType::Time
        | CounterType::Fade
        | CounterType::Age
        | CounterType::Shield
        | CounterType::Keyword(_) => {}
    }
    Ok(())
}

fn guard_enter_with_counters_payload(
    field: &str,
    enter_with_counters: &[(CounterType, u32)],
) -> Result<(), String> {
    bound_list(field, enter_with_counters.len())?;
    for (index, (counter_type, _)) in enter_with_counters.iter().enumerate() {
        guard_counter_type_payload(&format!("{field}[{index}].counter_type"), counter_type)?;
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
    for (index, keyword) in characteristics.keywords.iter().enumerate() {
        bound_serialized_json(&format!("{field}.keywords[{index}]"), keyword)?;
    }
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
            guard_enter_with_counters_payload(
                "Debug.CreateToken.request.enter_with_counters",
                enter_with_counters,
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
            guard_enter_with_counters_payload(
                "Debug.CreateToken.request.enter_with_counters",
                enter_with_counters,
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
        DebugAction::CreateToken { request, .. } => {
            guard_debug_token_request_payload(request)?;
        }
        DebugAction::ModifyCounters { counter_type, .. } => {
            guard_counter_type_payload("Debug.ModifyCounters.counter_type", counter_type)?;
        }
        DebugAction::GrantKeyword { keyword, .. } => {
            bound_serialized_json("Debug.GrantKeyword.keyword", keyword)?;
        }
        DebugAction::RemoveKeyword { keyword, .. } => {
            bound_serialized_json("Debug.RemoveKeyword.keyword", keyword)?;
        }
        DebugAction::MoveToZone { .. }
        | DebugAction::RemoveObject { .. }
        | DebugAction::Sacrifice { .. }
        | DebugAction::Reveal { .. }
        | DebugAction::DrawCards { .. }
        | DebugAction::Mill { .. }
        | DebugAction::ShuffleLibrary { .. }
        | DebugAction::Proliferate { .. }
        | DebugAction::SetBasePowerToughness { .. }
        | DebugAction::SetTapped { .. }
        | DebugAction::SetPrepared { .. }
        | DebugAction::SetController { .. }
        | DebugAction::SetSummoningSickness { .. }
        | DebugAction::SetFaceState { .. }
        | DebugAction::Attach { .. }
        | DebugAction::Detach { .. }
        | DebugAction::SetLife { .. }
        | DebugAction::ModifyPlayerCounters { .. }
        | DebugAction::ModifyEnergy { .. }
        | DebugAction::SetInfiniteMana { .. }
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
        GameAction::CastSpell { targets, .. } => {
            bound_list("CastSpell.targets", targets.len())?;
        }
        GameAction::SelectTargets { targets } => {
            bound_list("SelectTargets.targets", targets.len())?;
        }
        GameAction::DeclareAttackers { attacks, bands } => {
            bound_list("DeclareAttackers.attacks", attacks.len())?;
            // CR 702.22c: bound both the number of declared bands and the size
            // of each individual band so a malicious client cannot send an
            // unbounded nested payload.
            bound_list("DeclareAttackers.bands", bands.len())?;
            for (index, band) in bands.iter().enumerate() {
                bound_list(&format!("DeclareAttackers.bands[{index}]"), band.len())?;
            }
        }
        GameAction::DeclareBlockers { assignments } => {
            bound_list("DeclareBlockers.assignments", assignments.len())?;
        }
        GameAction::AssignCombatDamage { assignments, .. } => {
            bound_list("AssignCombatDamage.assignments", assignments.len())?;
        }
        GameAction::AssignBlockerDamage { assignments } => {
            bound_list("AssignBlockerDamage.assignments", assignments.len())?;
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
        GameAction::ChooseCountersToRemove { selections } => {
            bound_list("ChooseCountersToRemove.selections", selections.len())?;
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
            for (index, card) in main.iter().enumerate() {
                if card.name.len() > MAX_CHOICE_LEN {
                    return Err(format!(
                        "SubmitSideboard.main[{index}].name is {} bytes; at most {MAX_CHOICE_LEN} allowed",
                        card.name.len()
                    ));
                }
            }
            for (index, card) in sideboard.iter().enumerate() {
                if card.name.len() > MAX_CHOICE_LEN {
                    return Err(format!(
                        "SubmitSideboard.sideboard[{index}].name is {} bytes; at most {MAX_CHOICE_LEN} allowed",
                        card.name.len()
                    ));
                }
            }
        }
        GameAction::SubmitPilePartition { pile_a, .. } => {
            bound_list("SubmitPilePartition.pile_a", pile_a.len())?;
        }
        GameAction::SelectCategoryPermanents { choices } => {
            bound_list("SelectCategoryPermanents.choices", choices.len())?;
        }
        GameAction::ChooseKeptCreatures { kept } => {
            bound_list("ChooseKeptCreatures.kept", kept.len())?;
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
        GameAction::ChooseRemoveCounterCostDistribution { distribution } => {
            bound_list(
                "ChooseRemoveCounterCostDistribution.distribution",
                distribution.len(),
            )?;
            for (index, choice) in distribution.iter().enumerate() {
                guard_counter_type_payload(
                    &format!("ChooseRemoveCounterCostDistribution.distribution[{index}].counter_type"),
                    &choice.counter_type,
                )?;
            }
        }
        GameAction::RetargetSpell { new_targets, .. } => {
            bound_list("RetargetSpell.new_targets", new_targets.len())?;
        }
        GameAction::ChooseOption { choice, .. } => {
            bound_string("ChooseOption.choice", choice)?;
        }
        GameAction::SubmitSpellbookDraft { card } => {
            bound_string("SubmitSpellbookDraft.card", card)?;
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
        | GameAction::ChooseEnlist { .. }
        | GameAction::ChooseClashOpponent { .. }
        | GameAction::ChoosePileOpponent { .. }
        | GameAction::ChooseAssistPlayer { .. }
        | GameAction::CommitAssistPayment { .. }
        | GameAction::MulliganDecision { .. }
        | GameAction::TapLandForMana { .. }
        | GameAction::UntapLandForMana { .. }
        | GameAction::SpendPoolMana { .. }
        | GameAction::UnspendPoolMana { .. }
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
        | GameAction::SubmitVoteCandidate { .. }
        | GameAction::ChooseBranch { .. }
        | GameAction::SubmitLifeRedistribution { .. }
        | GameAction::ChooseDamageSource { .. }
        | GameAction::DecideOptionalCost { .. }
        | GameAction::RespondToSpliceOffer { .. }
        | GameAction::ChooseAdventureFace { .. }
        | GameAction::ChooseModalFace { .. }
        | GameAction::ChooseAlternativeCast { .. }
        | GameAction::ChooseCastingVariant { .. }
        | GameAction::KeepAllCopyTargets
        | GameAction::ChoosePermanentTypeSlot { .. }
        | GameAction::ActivateNinjutsu { .. }
        | GameAction::CastSpellAsSneak { .. }
        | GameAction::CastSpellAsWebSlinging { .. }
        | GameAction::CastSpellForFree { .. }
        | GameAction::CastSpellAsMiracle { .. }
        | GameAction::CastSpellAsMadness { .. }
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
        | GameAction::ChooseRoomDoor { .. }
        | GameAction::TapForConvoke { .. }
        | GameAction::HarmonizeTap { .. }
        | GameAction::DeclareCompanion { .. }
        | GameAction::CompanionToHand
        | GameAction::RollPlanarDie
        | GameAction::DiscoverChoice { .. }
        | GameAction::GraveyardPaidCastChoice { .. }
        | GameAction::CascadeChoice { .. }
        | GameAction::RippleChoice { .. }
        | GameAction::FreeCastWindowChoice { .. }
        | GameAction::ChooseTopOrBottom { .. }
        // CR 702.140c: mutate merge side carries a single typed enum — nothing
        // client-controlled to bound.
        | GameAction::ChooseMutateMergeSide { .. }
        // CR 702.99a: cipher encode carries a single optional object id — nothing
        // unbounded to validate.
        | GameAction::CipherEncode { .. }
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
        | GameAction::SetPriorityYield { .. }
        | GameAction::SetMayTriggerAutoChoice { .. }
        | GameAction::Concede { .. } => {}
    }
    Ok(())
}
