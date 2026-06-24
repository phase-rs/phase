mod candidates;
mod context;
mod copy;
pub mod filter;

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::game::mana_abilities;
use crate::game::mana_sources;
use crate::types::ability::{AbilityKind, CounterCostSelection};
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::game_state::{CastOfferKind, GameState, PayCostKind, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;

pub use candidates::{
    candidate_actions, candidate_actions_broad, candidate_actions_exact, ActionMetadata,
    CandidateAction, TacticalClass,
};
pub use context::{build_decision_context, AiDecisionContext};
pub use copy::{
    copy_effect_adds_flying, copy_target_filter, copy_target_mana_value_ceiling,
    project_copy_mana_spent_for_x,
};
pub use filter::{
    BasicLegalityFilter, CandidateFilter, FilterCost, FilterPipeline, SimulationFilter,
};

/// Filter `candidate_actions` down to the actions that are actually legal now.
///
/// Runs the default [`FilterPipeline`] — cheap structural checks first, then
/// an `apply_as_current` simulation as a catch-all. The `cheap ⊆ sim`
/// invariant (enforced by `filter::tests::basic_legality_is_subset_of_simulation`)
/// guarantees that no candidate accepted by the simulation is silently
/// dropped by a cheap filter.
pub fn validated_candidate_actions(state: &GameState) -> Vec<CandidateAction> {
    let pipeline = FilterPipeline::default_pipeline();
    pipeline.apply(state, candidate_actions(state))
}

/// CR 702.51a / 702.66a / 702.126a: During `ManaPayment`, every structurally
/// valid `TapForConvoke` candidate is accepted by `apply_as_current` — skip the
/// full-state clone in `SimulationFilter` (issue #3663 Treasure Cruise / Delve).
pub(crate) fn structurally_valid_tap_for_convoke_payment(
    state: &GameState,
    action: &GameAction,
) -> bool {
    use crate::types::game_state::ConvokeMode;
    use crate::types::mana::ManaType;
    use crate::types::zones::Zone;

    let (
        WaitingFor::ManaPayment {
            player,
            convoke_mode: Some(mode),
        },
        GameAction::TapForConvoke {
            object_id,
            mana_type,
        },
    ) = (&state.waiting_for, action)
    else {
        return false;
    };

    let Some(obj) = state.objects.get(object_id) else {
        return false;
    };

    match mode {
        ConvokeMode::Delve => {
            obj.zone == Zone::Graveyard && obj.owner == *player && *mana_type == ManaType::Colorless
        }
        ConvokeMode::Convoke => {
            if !obj.is_convoke_eligible(*player) {
                return false;
            }
            if let Some(color) = mana_sources::mana_type_to_color(*mana_type) {
                if !obj.color.contains(&color) {
                    return false;
                }
                if let Some(shards) = state.pending_cast.as_ref().and_then(|pc| match &pc.cost {
                    crate::types::mana::ManaCost::Cost { shards, .. } => Some(shards.as_slice()),
                    _ => None,
                }) {
                    return shards.iter().any(|shard| shard.contributes_to(color));
                }
                true
            } else {
                *mana_type == ManaType::Colorless
            }
        }
        ConvokeMode::Waterbend => {
            obj.is_waterbend_eligible(*player) && *mana_type == ManaType::Colorless
        }
        ConvokeMode::Improvise => {
            obj.is_improvise_eligible(*player) && *mana_type == ManaType::Colorless
        }
    }
}

fn cheap_reject_candidate(state: &GameState, action: &GameAction) -> bool {
    // CR 103.5 / TL:R 906.6a: For simultaneous-decision states
    // `acting_player()` is None when multiple players are pending. The
    // Priority-branch check below only fires for the Priority variant, so we
    // substitute the first pending player as a representative — downstream
    // dispatch validates the exact pending actor.
    let acting_player = match state.waiting_for.acting_player() {
        Some(p) => p,
        None => {
            let players = state.waiting_for.acting_players();
            if let Some(&first) = players.first() {
                first
            } else {
                return true;
            }
        }
    };

    match (&state.waiting_for, action) {
        (WaitingFor::Priority { player }, _) if *player != acting_player => true,
        (WaitingFor::Priority { .. }, GameAction::CastSpell { object_id, .. })
        | (WaitingFor::Priority { .. }, GameAction::Foretell { object_id, .. })
        | (WaitingFor::Priority { .. }, GameAction::PlayLand { object_id, .. })
        | (WaitingFor::Priority { .. }, GameAction::UnlockRoomDoor { object_id, .. })
        | (WaitingFor::Priority { .. }, GameAction::Transform { object_id })
        | (WaitingFor::Priority { .. }, GameAction::TurnFaceUp { object_id })
        | (WaitingFor::Priority { .. }, GameAction::PlayFaceDown { object_id, .. })
        | (WaitingFor::Priority { .. }, GameAction::TapLandForMana { object_id })
        | (WaitingFor::Priority { .. }, GameAction::UntapLandForMana { object_id })
        | (
            WaitingFor::Priority { .. },
            GameAction::ActivateNinjutsu {
                ninjutsu_object_id: object_id,
                ..
            },
        ) => !state.objects.contains_key(object_id),
        (WaitingFor::Priority { .. }, GameAction::ActivateAbility { source_id, .. })
        | (
            WaitingFor::Priority { .. },
            GameAction::CrewVehicle {
                vehicle_id: source_id,
                ..
            },
        )
        | (
            WaitingFor::Priority { .. },
            GameAction::ActivateStation {
                spacecraft_id: source_id,
                ..
            },
        )
        | (
            WaitingFor::Priority { .. },
            GameAction::Equip {
                equipment_id: source_id,
                ..
            },
        )
        | (WaitingFor::Priority { .. }, GameAction::ChooseRingBearer { target: source_id }) => {
            !state.objects.contains_key(source_id)
        }
        (
            WaitingFor::ReplacementChoice {
                candidate_count, ..
            },
            GameAction::ChooseReplacement { index },
        ) => *index >= *candidate_count,
        // CR 603.3b: Order must be a permutation of 0..triggers.len() — same
        // validity check the engine handler enforces. Reject early so the
        // simulation filter never fires a known-rejected action.
        (WaitingFor::OrderTriggers { triggers, .. }, GameAction::OrderTriggers { order }) => {
            !crate::game::triggers::is_valid_permutation(order, triggers.len())
        }
        (
            WaitingFor::CopyTargetChoice { valid_targets, .. },
            GameAction::ChooseTarget { target },
        ) => !matches_target_choice(target, valid_targets),
        (WaitingFor::ExploreChoice { choosable, .. }, GameAction::ChooseTarget { target }) => {
            !matches_target_choice(target, choosable)
        }
        // CR 303.4 + CR 303.4g + CR 115.1: Validate the chosen attach target
        // for a return-as-Aura pick against the engine-computed legal list.
        (
            WaitingFor::ReturnAsAuraTarget { legal_targets, .. },
            GameAction::ChooseTarget { target },
        ) => !matches_waiting_target_choice(legal_targets, target),
        (WaitingFor::TargetSelection { selection, .. }, GameAction::ChooseTarget { target })
        | (
            WaitingFor::TriggerTargetSelection { selection, .. },
            GameAction::ChooseTarget { target },
        ) => !matches_waiting_target_choice(selection.current_legal_targets.as_slice(), target),
        (WaitingFor::ModeChoice { modal, .. }, GameAction::SelectModes { indices })
        | (WaitingFor::AbilityModeChoice { modal, .. }, GameAction::SelectModes { indices }) => {
            indices.iter().any(|index| *index >= modal.mode_count)
                || indices.len() < modal.min_choices
                || indices.len() > modal.max_choices
        }
        (
            WaitingFor::PhyrexianPayment { shards, .. },
            GameAction::SubmitPhyrexianChoices { choices },
        ) => {
            if choices.len() != shards.len() {
                return true;
            }
            use crate::types::game_state::{ShardChoice, ShardOptions};
            choices.iter().zip(shards.iter()).any(|(choice, shard)| {
                matches!(
                    (choice, shard.options),
                    (ShardChoice::PayLife, ShardOptions::ManaOnly)
                        | (ShardChoice::PayMana, ShardOptions::LifeOnly)
                )
            })
        }
        (WaitingFor::NamedChoice { options, .. }, GameAction::ChooseOption { choice }) => {
            !options.is_empty() && !options.iter().any(|option| option == choice)
        }
        (WaitingFor::ChooseOneOfBranch { branches, .. }, GameAction::ChooseBranch { index }) => {
            *index >= branches.len()
        }
        (
            WaitingFor::ActivationCostOneOfChoice {
                player,
                costs,
                pending_cast,
            },
            GameAction::ChooseActivationCostBranch { index },
        ) => costs
            .get(*index)
            .is_none_or(|cost| !cost.is_payable(state, *player, pending_cast.object_id)),
        (
            WaitingFor::DamageSourceChoice { options, .. },
            GameAction::ChooseDamageSource { source },
        ) => !options.contains(source),
        (WaitingFor::LearnChoice { hand_cards, .. }, GameAction::LearnDecision { choice }) => {
            match choice {
                crate::types::actions::LearnOption::Rummage { card_id } => {
                    !hand_cards.contains(card_id) || !state.objects.contains_key(card_id)
                }
                crate::types::actions::LearnOption::Skip => false,
            }
        }
        (
            WaitingFor::OutsideGameChoice {
                choices,
                count,
                up_to,
                ..
            },
            GameAction::ChooseOutsideGameCards { selections },
        ) => {
            use crate::types::actions::OutsideGameSelection;
            use crate::types::game_state::OutsideGameChoiceSource;
            let valid_count = if *up_to {
                selections.len() <= *count
            } else {
                selections.len() == *count
            };
            let mut sideboard_counts: HashMap<usize, usize> = HashMap::new();
            let mut exile_seen: HashSet<ObjectId> = HashSet::new();
            let mut exile_dup = false;
            for selection in selections {
                match selection {
                    OutsideGameSelection::Sideboard { sideboard_index } => {
                        *sideboard_counts.entry(*sideboard_index).or_insert(0) += 1;
                    }
                    OutsideGameSelection::FaceUpExile { object_id } => {
                        if !exile_seen.insert(*object_id) {
                            exile_dup = true;
                        }
                    }
                }
            }
            let bad_sideboard = sideboard_counts.iter().any(|(idx, count)| {
                choices
                    .iter()
                    .find(|choice| {
                        matches!(
                            &choice.source,
                            OutsideGameChoiceSource::Sideboard { sideboard_index, .. }
                                if sideboard_index == idx
                        )
                    })
                    .is_none_or(|choice| *count > choice.count as usize)
            });
            let bad_exile =
                exile_seen.iter().any(|object_id| {
                    !choices.iter().any(|choice| matches!(
                    &choice.source,
                    OutsideGameChoiceSource::FaceUpExile { object_id: oid } if oid == object_id
                ))
                });
            !valid_count || exile_dup || bad_sideboard || bad_exile
        }
        (WaitingFor::PairChoice { choices, .. }, GameAction::ChoosePair { partner }) => {
            partner.is_some_and(|partner| !choices.contains(&partner))
        }
        (
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { .. },
                ..
            },
            GameAction::DiscoverChoice { .. },
        )
        | (WaitingFor::RevealUntilKeptChoice { .. }, GameAction::DecideOptionalEffect { .. })
        | (WaitingFor::RepeatDecision { .. }, GameAction::DecideOptionalEffect { .. })
        | (
            WaitingFor::CastOffer {
                kind: CastOfferKind::Cascade { .. },
                ..
            },
            GameAction::CascadeChoice { .. },
        )
        | (WaitingFor::MulliganDecision { .. }, GameAction::MulliganDecision { .. })
        | (WaitingFor::BetweenGamesChoosePlayDraw { .. }, GameAction::ChoosePlayDraw { .. })
        | (WaitingFor::TopOrBottomChoice { .. }, GameAction::ChooseTopOrBottom { .. })
        | (WaitingFor::ClashChooseOpponent { .. }, GameAction::ChooseClashOpponent { .. })
        | (WaitingFor::ClashCardPlacement { .. }, GameAction::ChooseTopOrBottom { .. })
        | (WaitingFor::OptionalCostChoice { .. }, GameAction::DecideOptionalCost { .. })
        | (WaitingFor::SpliceOffer { .. }, GameAction::RespondToSpliceOffer { .. })
        | (WaitingFor::DefilerPayment { .. }, GameAction::DecideOptionalCost { .. })
        | (WaitingFor::OptionalEffectChoice { .. }, GameAction::DecideOptionalEffect { .. })
        | (
            WaitingFor::OptionalEffectChoice {
                may_trigger_key: Some(_),
                ..
            },
            GameAction::DecideOptionalEffectAndRemember { .. },
        )
        | (WaitingFor::OpponentMayChoice { .. }, GameAction::DecideOptionalEffect { .. })
        | (WaitingFor::TributeChoice { .. }, GameAction::DecideOptionalEffect { .. })
        | (WaitingFor::UnlessPayment { .. }, GameAction::PayUnlessCost { .. })
        | (WaitingFor::UnlessPaymentChooseCost { .. }, GameAction::ChooseUnlessCostBranch { .. })
        | (WaitingFor::CombatTaxPayment { .. }, GameAction::PayCombatTax { .. })
        | (
            WaitingFor::CastOffer {
                kind: CastOfferKind::Adventure { .. },
                ..
            },
            GameAction::ChooseAdventureFace { .. },
        )
        | (WaitingFor::ModalFaceChoice { .. }, GameAction::ChooseModalFace { .. })
        | (WaitingFor::AlternativeCastChoice { .. }, GameAction::ChooseAlternativeCast { .. })
        | (WaitingFor::CastingVariantChoice { .. }, GameAction::ChooseCastingVariant { .. }) => {
            false
        }
        // CR 107.1c + CR 107.14: Submitted amount must fall within [min, max].
        (WaitingFor::PayAmountChoice { min, max, .. }, GameAction::SubmitPayAmount { amount }) => {
            *amount < *min || *amount > *max
        }
        // CR 103.5: SelectCards is invalid if (a) no pending entry exists for
        // any player whose hand contains all the selected cards, or (b) the
        // count doesn't match the pending entry's owed bottom count. Because
        // the actor identity is carried via authorization upstream, this filter
        // only validates the count against any pending entry whose hand fits.
        (WaitingFor::MulliganBottomCards { pending }, GameAction::SelectCards { cards })
        | (WaitingFor::OpeningHandBottomCards { pending, .. }, GameAction::SelectCards { cards }) => {
            pending.iter().all(|entry| {
                selection_mismatch(
                    cards,
                    &state.players[entry.player.0 as usize].hand,
                    Some(entry.count.into()),
                )
            })
        }
        (
            WaitingFor::ScryChoice { player: _, cards },
            GameAction::SelectCards { cards: chosen },
        )
        | (
            WaitingFor::SurveilChoice { player: _, cards },
            GameAction::SelectCards { cards: chosen },
        ) => selection_mismatch(chosen, cards, None),
        (
            WaitingFor::RevealChoice {
                player: _,
                cards,
                optional,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            // CR 701.20a: Optional reveals accept an empty selection as "decline".
            if *optional && chosen.is_empty() {
                false
            } else {
                selection_mismatch(chosen, cards, Some(1))
            }
        }
        (
            WaitingFor::SearchChoice {
                player: _,
                cards,
                count,
                up_to,
                allows_partial_find,
                constraint,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            // CR 701.23b vs CR 701.23d: hidden-zone stated-quality searches,
            // explicit stated-quality constraints, and "up to" searches may
            // legally find fewer cards than requested — including none. Mirror
            // the submission guard / candidate generation lower bound so the
            // validated legal-action path does not drop legal short picks.
            let lower_bounded = *up_to || *allows_partial_find || constraint.permits_partial_find();
            let exact = if lower_bounded { None } else { Some(*count) };
            selection_mismatch(chosen, cards, exact) || (lower_bounded && chosen.len() > *count)
        }
        (
            WaitingFor::ChooseFromZoneChoice {
                player: _,
                cards,
                count,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        )
        | (
            WaitingFor::ConniveDiscard {
                player: _,
                cards,
                count,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        )
        | (
            WaitingFor::DiscardToHandSize {
                player: _,
                cards,
                count,
            },
            GameAction::SelectCards { cards: chosen },
        ) => selection_mismatch(chosen, cards, Some(*count)),
        // CR 118.3: Single-object RemoveCounter chooses exactly one counter source.
        (
            WaitingFor::PayCost {
                kind:
                    PayCostKind::RemoveCounter {
                        selection: CounterCostSelection::SingleObject,
                        ..
                    },
                choices,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => selection_mismatch(chosen, choices, Some(1)),
        // CR 118.3: "from among" RemoveCounter submits exact per-object
        // counter counts, not a bare object selection.
        (
            WaitingFor::PayCost {
                kind:
                    PayCostKind::RemoveCounter {
                        selection: CounterCostSelection::AmongObjects,
                        count,
                        ..
                    },
                choices,
                ..
            },
            GameAction::ChooseRemoveCounterCostDistribution { distribution },
        ) => remove_counter_distribution_mismatch(distribution, choices, *count),
        (
            WaitingFor::PayCost {
                kind:
                    PayCostKind::RemoveCounter {
                        selection: CounterCostSelection::AmongObjects,
                        ..
                    },
                ..
            },
            GameAction::SelectCards { .. },
        ) => true,
        // CR 118.3: Sacrifice honors the [min_count, count] range.
        (
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                choices,
                count,
                min_count,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            selection_mismatch(chosen, choices, None)
                || chosen.len() < *min_count
                || chosen.len() > *count
        }
        // CR 601.2f + CR 208.1: the aggregate Crew/Saddle/Teamwork tap cost
        // accepts ANY creature subset (drawn from `choices`, no duplicates)
        // whose summed CURRENT positive power satisfies the advertised comparator
        // — not a fixed cardinality. Evaluates through the same `satisfied_by`
        // the payment validator (`handle_tap_creatures_for_spell_cost`) uses, so
        // both seams agree on which subsets are legal.
        (
            WaitingFor::PayCost {
                kind:
                    PayCostKind::TapCreatures {
                        aggregate: Some(aggregate),
                    },
                choices,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let total = crate::game::casting_costs::tap_creatures_total_power(state, chosen);
            selection_mismatch(chosen, choices, None) || !aggregate.satisfied_by(total)
        }
        // CR 118.3 + CR 605.3b: every other PayCost kind selects exactly `count`.
        (WaitingFor::PayCost { choices, count, .. }, GameAction::SelectCards { cards: chosen }) => {
            selection_mismatch(chosen, choices, Some(*count))
        }
        // CR 701.68a: Blight always selects exactly one creature, regardless of N.
        (WaitingFor::BlightChoice { creatures, .. }, GameAction::SelectCards { cards: chosen }) => {
            selection_mismatch(chosen, creatures, Some(1))
        }
        (
            WaitingFor::EffectZoneChoice {
                player: _,
                cards,
                count,
                up_to,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        )
        | (
            WaitingFor::DiscardChoice {
                player: _,
                cards,
                count,
                up_to,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let exact = if *up_to { None } else { Some(*count) };
            selection_mismatch(chosen, cards, exact) || (*up_to && chosen.len() > *count)
        }
        (
            WaitingFor::DrawnThisTurnTopdeckChoice {
                cards,
                count,
                min_count,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            selection_mismatch(chosen, cards, None)
                || chosen.len() > *count
                || chosen.len() < *min_count
        }
        (
            WaitingFor::DigChoice {
                player: _,
                selectable_cards,
                keep_count,
                up_to,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let exact = if *up_to {
                None
            } else {
                Some((*keep_count).min(selectable_cards.len()))
            };
            selection_mismatch(chosen, selectable_cards, exact)
                || (*up_to && chosen.len() > *keep_count)
        }
        (
            WaitingFor::CollectEvidenceChoice {
                player: _, cards, ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => selection_mismatch(chosen, cards, None),
        (
            WaitingFor::WardDiscardChoice {
                player: _, cards, ..
            },
            GameAction::SelectCards { cards: chosen },
        )
        | (
            WaitingFor::WardSacrificeChoice {
                player: _,
                permanents: cards,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => selection_mismatch(chosen, cards, Some(1)),
        (
            WaitingFor::ManifestDreadChoice {
                player: _, cards, ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => selection_mismatch(chosen, cards, Some(1)),
        (
            WaitingFor::DeclareAttackers {
                player,
                valid_attacker_ids,
                ..
            },
            GameAction::DeclareAttackers { attacks, .. },
        ) => {
            *player != acting_player
                || attacks.iter().any(|(attacker, _)| {
                    !valid_attacker_ids.contains(attacker) || !state.objects.contains_key(attacker)
                })
        }
        (
            WaitingFor::DeclareBlockers {
                player,
                valid_blocker_ids,
                valid_block_targets,
                ..
            },
            GameAction::DeclareBlockers { assignments },
        ) => {
            *player != acting_player
                || assignments.iter().any(|(blocker, attacker)| {
                    !valid_blocker_ids.contains(blocker)
                        || !state.objects.contains_key(blocker)
                        || !state.objects.contains_key(attacker)
                        || !valid_block_targets
                            .get(blocker)
                            .is_some_and(|targets| targets.contains(attacker))
                })
        }
        _ => false,
    }
}

fn selection_mismatch<'a>(
    chosen: &[ObjectId],
    options: impl IntoIterator<Item = &'a ObjectId>,
    exact_count: Option<usize>,
) -> bool {
    if exact_count.is_some_and(|count| chosen.len() != count) {
        return true;
    }
    let option_set: HashSet<ObjectId> = options.into_iter().copied().collect();
    let mut seen = HashSet::new();
    chosen
        .iter()
        .any(|card| !option_set.contains(card) || !seen.insert(*card))
}

fn remove_counter_distribution_mismatch(
    distribution: &[crate::types::game_state::CounterCostChoice],
    choices: &[ObjectId],
    count: u32,
) -> bool {
    let mut total = 0u32;
    let mut seen = HashSet::new();
    distribution.is_empty()
        || distribution.iter().any(|choice| {
            if choice.count == 0
                || !choices.contains(&choice.object_id)
                || !seen.insert((choice.object_id, choice.counter_type.clone()))
            {
                return true;
            }
            total = total.saturating_add(choice.count);
            false
        })
        || total != count
}

fn matches_target_choice(
    target: &Option<crate::types::ability::TargetRef>,
    valid_targets: &[ObjectId],
) -> bool {
    match target {
        Some(crate::types::ability::TargetRef::Object(target_id)) => {
            valid_targets.contains(target_id)
        }
        _ => false,
    }
}

fn matches_waiting_target_choice(
    valid_targets: &[crate::types::ability::TargetRef],
    target: &Option<crate::types::ability::TargetRef>,
) -> bool {
    match target {
        Some(target) => valid_targets.contains(target),
        None => true,
    }
}

/// True when an `ActivateAbility` action is a meaningful priority decision.
///
/// Non-mana abilities are always meaningful. Mana abilities are meaningful only
/// when their penalty axis says so (today: sacrifice-for-mana per CR 605.3a +
/// CR 603.6, issue #544).
fn activate_ability_is_meaningful_priority(
    state: &GameState,
    source_id: ObjectId,
    ability_index: usize,
) -> bool {
    state.objects.get(&source_id).is_some_and(|obj| {
        obj.abilities.get(ability_index).is_some_and(|ability| {
            !mana_abilities::is_mana_ability(ability)
                || mana_sources::mana_ability_penalty(ability).is_meaningful_priority_activation()
        })
    })
}

/// True when `actions` contains a priority action that materially changes the
/// game beyond passing or producing standalone mana.
///
/// Sacrifice-for-mana activations are omitted from the flat `legal_actions`
/// list (they live in `legal_actions_by_object` only) but remain meaningful
/// priority decisions, so during `WaitingFor::Priority` this also scans
/// `activatable_object_mana_actions`.
pub fn has_meaningful_priority_action(state: &GameState, actions: &[GameAction]) -> bool {
    if actions.iter().any(|action| match action {
        GameAction::PassPriority => false,
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => activate_ability_is_meaningful_priority(state, *source_id, *ability_index),
        _ => true,
    }) {
        return true;
    }

    // Issue #544: sacrifice-for-mana abilities (KCI, Phyrexian Altar, etc.) are
    // excluded from the flat legal-actions list but must still block auto-pass.
    matches!(state.waiting_for, WaitingFor::Priority { .. })
        && activatable_object_mana_actions(state).iter().any(|action| {
            matches!(
                action,
                GameAction::ActivateAbility {
                    source_id,
                    ability_index,
                } if activate_ability_is_meaningful_priority(state, *source_id, *ability_index)
            )
        })
}

fn auto_passes_initial_priority_by_default(state: &GameState) -> bool {
    state.stack.is_empty() && matches!(state.phase, Phase::Upkeep | Phase::Draw)
}

/// CR 117.1d + CR 601.2g: True when the player has a spell the castability gate
/// accepts via manual mana-ability payment even though the simulation oracle
/// (`SimulationFilter`) rejects the Auto-mode `CastSpell` candidate (issue #562,
/// #583). Frontends surface these via `spell_costs` + manual cast dispatch.
fn has_feasibly_castable_spell(state: &GameState, player: PlayerId) -> bool {
    crate::game::casting::spell_objects_available_to_cast(state, player)
        .iter()
        .any(|&object_id| crate::game::casting::can_cast_object_now(state, player, object_id))
}

/// Determines whether the frontend should auto-pass the current priority window.
///
/// Returns `true` when auto-passing is recommended:
/// - Only `PassPriority` is available (no spells, abilities, or lands to play)
/// - Initial upkeep/draw priority without an explicit phase stop (MTGA-style)
/// - Player's own spell/ability is on top of the stack (MTGA-style: let your
///   own spells resolve without pausing)
///
/// This centralizes the "meaningful action" classification in the engine so
/// frontends don't need to inspect game objects or card types.
pub fn auto_pass_recommended(state: &GameState, actions: &[GameAction]) -> bool {
    let player = match &state.waiting_for {
        WaitingFor::Priority { player } => *player,
        _ => return false,
    };

    if auto_passes_initial_priority_by_default(state) {
        return true;
    }

    if has_feasibly_castable_spell(state, player) {
        return false;
    }

    if !has_meaningful_priority_action(state, actions) {
        return true;
    }

    // MTGA-style: auto-pass when own spell/ability is on top of the stack.
    // The player almost never wants to respond to their own spell — let it resolve.
    // Full control mode (checked by the frontend) overrides this.
    if let Some(top) = state.stack.back() {
        if top.controller == player {
            return true;
        }
    }

    false
}

/// Returns the legal actions for the current game state.
///
/// Mana actions are omitted from the flat list returned by [`legal_actions`].
/// They are still exposed through `legal_actions_by_object` by
/// [`legal_actions_full`] so the frontend can render and dispatch
/// engine-authoritative mana affordances without treating them as meaningful
/// priority decisions.
pub fn legal_actions(state: &GameState) -> Vec<GameAction> {
    legal_actions_with_costs(state).0
}

/// Returns legal actions plus effective mana costs for castable spells.
///
/// The spell costs map contains the post-reduction effective cost for each
/// CastSpell action's object_id, reflecting all modifiers (alt costs, commander
/// tax, battlefield reducers, affinity). Frontends use this to display dynamic
/// mana cost overlays on cards in hand.
pub fn legal_actions_with_costs(
    state: &GameState,
) -> (Vec<GameAction>, HashMap<ObjectId, ManaCost>) {
    let (actions, spell_costs, _grouped) = legal_actions_full(state);
    (actions, spell_costs)
}

/// Tuple returned by `legal_actions_full`: flat actions, spell-cost map,
/// per-source-object action grouping.
pub type LegalActionsFull = (
    Vec<GameAction>,
    HashMap<ObjectId, ManaCost>,
    HashMap<ObjectId, Vec<GameAction>>,
);

fn target_selection_actions_without_simulation(state: &GameState) -> Option<Vec<GameAction>> {
    let (target_slots, current_slot, current_legal_targets) = match &state.waiting_for {
        WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        }
        | WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => (
            target_slots,
            selection.current_slot,
            selection.current_legal_targets.as_slice(),
        ),
        _ => return None,
    };

    let mut actions: Vec<GameAction> = current_legal_targets
        .iter()
        .cloned()
        .map(|target| GameAction::ChooseTarget {
            target: Some(target),
        })
        .collect();

    if target_slots
        .get(current_slot)
        .is_some_and(|slot| slot.optional)
    {
        actions.push(GameAction::ChooseTarget { target: None });
    }

    Some(actions)
}

/// Returns legal actions, spell costs, AND a per-permanent action grouping.
///
/// `legal_actions_by_object` maps each permanent (or hand-zone card) to the
/// engine-authoritative actions the frontend may offer for that object. The
/// grouped map includes mana actions that are intentionally absent from the
/// flat `actions` list; auto-pass consumes the flat list, while board
/// interaction consumes the grouped map.
pub fn legal_actions_full(state: &GameState) -> LegalActionsFull {
    let actions: Vec<GameAction> =
        if let Some(actions) = target_selection_actions_without_simulation(state) {
            actions
        } else {
            validated_candidate_actions(state)
                .into_iter()
                .map(|candidate| candidate.action)
                .filter(|action| !action.is_mana_ability())
                .collect()
        };

    // Build spell costs map. The frontend display layer needs the
    // engine-effective cost (after Affinity / ReduceCost / commander tax / etc.)
    // for every spell the player owns in a castable zone — not just spells the
    // player can pay for right now. Otherwise the UI falls back to the printed
    // mana cost (e.g., Witherbloom, the Balancer would always show {5}{B}{G}
    // instead of the Affinity-reduced cost the engine actually charges).
    //
    // `display_spell_cost` is the single engine-authoritative source for cost
    // display — it suppresses situational restrictions (timing, mana, can't-cast
    // statics) but applies every cost-modifying static the cast pipeline would.
    let mut spell_costs = HashMap::new();
    if let WaitingFor::Priority { player } = &state.waiting_for {
        // Zone pre-filter is performance-only: skips the battlefield/stack/library
        // walk that has no chance of yielding a castable spell. Eligibility
        // (controller, foreign-cast permissions, zone) is decided centrally by
        // `display_spell_cost`. Do NOT filter by `obj.controller` here — Etali /
        // Dire Fleet Daredevil / Light-Paws-style `CastFromZone` permissions let
        // the active player cast cards owned/controlled by an opponent, and a
        // controller pre-filter would silently hide those cost displays.
        for obj in state.objects.values() {
            if !matches!(
                obj.zone,
                crate::types::zones::Zone::Hand
                    | crate::types::zones::Zone::Command
                    | crate::types::zones::Zone::Exile
                    | crate::types::zones::Zone::Graveyard
                    | crate::types::zones::Zone::Library
            ) {
                continue;
            }
            if let Some(cost) = crate::game::casting::display_spell_cost(state, *player, obj.id) {
                spell_costs.insert(obj.id, cost);
            }
        }
    }

    // Group by source object using the engine-authoritative classifier.
    let mut grouped_actions = actions.clone();
    grouped_actions.extend(activatable_object_mana_actions(state));
    let mut grouped: HashMap<ObjectId, Vec<GameAction>> = HashMap::new();
    for action in &grouped_actions {
        if let Some(id) = action.source_object() {
            // Dedup per object. During WaitingFor::ManaPayment the flat
            // candidate list already contains non-land mana abilities (e.g.
            // Birds of Paradise's ActivateAbility, emitted by
            // `mana_payment_actions` so the AI/server can pay), and the
            // `activatable_object_mana_actions` extension above re-derives the
            // same ones. `is_mana_ability()` only strips land taps, so the flat
            // copy is not filtered out — without this guard the per-object map
            // (the frontend ability picker) lists an identical mana ability
            // twice (the convoke "Add one mana of any color" duplicate).
            let bucket = grouped.entry(id).or_default();
            if !bucket.contains(action) {
                bucket.push(action.clone());
            }
        }
    }

    (actions, spell_costs, grouped)
}

/// Returns `legal_actions_full` scoped to a specific viewer. Empty tuple if
/// `viewer` is not the player currently expected to act.
///
/// CR 117.1 — "which player can take actions at any given time is determined by
/// a system of priority. The player with priority may cast spells, activate
/// abilities, and take special actions." `WaitingFor::acting_player()` is the
/// engine's authoritative answer — it covers priority *and* non-priority
/// decision points like target selection during resolution.
///
/// This is the single engine-side authority for "what does player X need to
/// know" and exists to keep game-logic gating out of transport adapters. The
/// P2P multiplayer host broadcasts a filtered state + legal-actions payload
/// per guest; only the acting guest needs a populated legal-actions map.
pub fn legal_actions_for_viewer(state: &GameState, viewer: PlayerId) -> LegalActionsFull {
    // CR 103.5: For simultaneous-decision states (MulliganDecision,
    // MulliganBottomCards, OpeningHandBottomCards), every pending player has a
    // legal action set, so guests in a multiplayer mulligan can see and submit
    // their own decisions concurrently.
    //
    // CR 723.5 + CR 723.8: Under a turn-control effect (Mindslaver, Emrakul,
    // Word of Command, Opposition Agent) the *controller* makes the controlled
    // player's choices while still making their own — but the controlled
    // player remains the active player (CR 723.3), so `acting_players()`
    // reports the controlled seat, not the authorized submitter. Authorize the
    // viewer through `is_authorized_submitter`, which maps every acting seat to
    // its authorized submitter, so the controller receives the controlled
    // turn's legal actions instead of an empty set (which would freeze the
    // controlled turn for them). Coincides with `acting_players().contains`
    // whenever no turn-control effect is active.
    if crate::game::turn_control::is_authorized_submitter(state, viewer) {
        legal_actions_full(state)
    } else {
        (Vec::new(), HashMap::new(), HashMap::new())
    }
}

/// Non-fatal diagnostic describing a wedged decision point.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StuckDecisionDiagnostic {
    pub waiting_for_kind: &'static str,
    pub stuck_players: Vec<PlayerId>,
}

/// Detects an engine-level progress wedge: a decision is owed but no authorized
/// submitter can produce any legal action, so the game cannot advance. This is
/// an engine anomaly detector (a misrouted/unsatisfiable `WaitingFor`), NOT a
/// rules implementation, so it carries no CR citation. Surfaced as a non-fatal
/// diagnostic; does NOT mutate state.
///
/// Returns `None` for `Priority` (passing priority is always legal) and for
/// states with no acting player (e.g. `GameOver`), and only fires when *every*
/// authorized submitter has an empty legal-action set.
pub fn stuck_decision_diagnostic(state: &GameState) -> Option<StuckDecisionDiagnostic> {
    // Cheap pre-gate: `Priority` (passing is always legal) and states with no
    // acting player are never wedged, and this branch enumerates no actions.
    if state.waiting_for.acting_players().is_empty()
        || matches!(state.waiting_for, WaitingFor::Priority { .. })
    {
        return None;
    }
    let submitters = crate::game::turn_control::authorized_submitters(state);
    if submitters.is_empty() {
        return None;
    }
    // Every authorized submitter resolves to the same global legal-action set
    // (`legal_actions_for_viewer` returns `legal_actions_full` for any of them),
    // so compute the emptiness once rather than per submitter. When empty, no
    // submitter can act and all submitter seats are stuck.
    if !legal_actions_full(state).0.is_empty() {
        return None;
    }
    Some(StuckDecisionDiagnostic {
        waiting_for_kind: state.waiting_for.variant_name(),
        stuck_players: submitters,
    })
}

fn mana_action_player(state: &GameState) -> Option<PlayerId> {
    match &state.waiting_for {
        WaitingFor::Priority { player }
        | WaitingFor::ManaPayment { player, .. }
        | WaitingFor::UnlessPayment { player, .. } => Some(*player),
        _ => None,
    }
}

/// CR 605.3a: Enumerate activatable mana abilities for the acting player.
///
/// Mirrors the per-ability scan pattern in `mana_sources::scan_mana_abilities` rather
/// than using the single `mana_ability_index` derived field, since a permanent may have
/// multiple mana abilities. Per-ability tap/sickness guards match `scan_mana_abilities`:
/// only abilities with a tap cost component require the permanent to be untapped and
/// free of summoning sickness (CR 302.6). Mana abilities don't use the stack (CR 605.3a).
fn activatable_object_mana_actions(state: &GameState) -> Vec<GameAction> {
    let Some(player) = mana_action_player(state) else {
        return Vec::new();
    };

    activatable_object_mana_actions_for_player(state, player)
}

fn can_use_tap_land_shortcut(
    state: &GameState,
    object_id: ObjectId,
    option: &mana_sources::ManaSourceOption,
) -> bool {
    if option.atomic_combination.is_some() {
        return false;
    }
    let Some(ability_index) = option.ability_index else {
        return true;
    };
    state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.abilities.get(ability_index))
        .is_some_and(|ability| mana_abilities::mana_sub_cost_of(&ability.cost).is_none())
}

pub(super) fn activatable_object_mana_actions_for_player(
    state: &GameState,
    player: PlayerId,
) -> Vec<GameAction> {
    let mut actions = Vec::new();
    for &obj_id in &state.battlefield {
        let Some(obj) = state.objects.get(&obj_id) else {
            continue;
        };
        if obj.controller != player {
            continue;
        }

        let mut handled_indices = HashSet::new();
        if obj.card_types.core_types.contains(&CoreType::Land) {
            let options = mana_sources::activatable_land_mana_options(state, obj_id, player);
            if options.len() == 1
                && options
                    .first()
                    .is_some_and(|option| can_use_tap_land_shortcut(state, obj_id, option))
            {
                actions.push(GameAction::TapLandForMana { object_id: obj_id });
                if let Some(ability_index) = options[0].ability_index {
                    handled_indices.insert(ability_index);
                }
            } else {
                for option in options {
                    if let Some(ability_index) = option.ability_index {
                        if handled_indices.insert(ability_index) {
                            actions.push(GameAction::ActivateAbility {
                                source_id: obj_id,
                                ability_index,
                            });
                        }
                    }
                }
            }
        }

        for (idx, ability) in obj.abilities.iter().enumerate() {
            if handled_indices.contains(&idx) {
                continue;
            }
            if ability.kind != AbilityKind::Activated || !mana_abilities::is_mana_ability(ability) {
                continue;
            }
            // CR 302.6 + CR 602.5a: Only tap-cost abilities are gated by tapped state and
            // summoning sickness. Free or mana-cost-only mana abilities are always
            // activatable. The summoning-sickness check honors the
            // CanActivateAbilitiesAsThoughHaste static (Tyvar) via the shared predicate.
            if mana_sources::has_tap_component(&ability.cost)
                && (obj.tapped
                    || crate::game::restrictions::summoning_sick_for_tap_ability(state, obj))
            {
                continue;
            }
            // CR 605.3b: Activation restrictions still apply to mana abilities.
            if mana_sources::activation_condition_satisfied(state, player, obj_id, idx, ability)
                && mana_abilities::can_activate_mana_ability_now(
                    state, player, obj_id, idx, ability,
                )
            {
                actions.push(GameAction::ActivateAbility {
                    source_id: obj_id,
                    ability_index: idx,
                });
            }
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::{
        candidate_actions, cheap_reject_candidate, legal_actions, legal_actions_for_viewer,
        legal_actions_full, stuck_decision_diagnostic, validated_candidate_actions,
    };
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ChoiceType, ContinuousModification,
        ControllerRef, Effect, FilterProp, ManaContribution, ManaProduction, QuantityExpr,
        ResolvedAbility, SacrificeCost, SearchSelectionConstraint, StaticDefinition, TargetFilter,
        TargetRef, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{
        CastingVariant, ConvokeMode, GameState, MulliganDecisionEntry, PendingCast, StackEntry,
        StackEntryKind, WaitingFor,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::{Keyword, KeywordKind};
    use crate::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_priority() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    fn create_land(state: &mut GameState, name: &str, subtypes: &[&str]) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types
            .subtypes
            .extend(subtypes.iter().map(|subtype| (*subtype).to_string()));
        id
    }

    fn add_fixed_mana_ability(
        state: &mut GameState,
        object_id: ObjectId,
        color: ManaColor,
    ) -> usize {
        let obj = state.objects.get_mut(&object_id).unwrap();
        let ability_index = obj.abilities.len();
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![color],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        ability_index
    }

    fn bucket_has(
        grouped: &HashMap<ObjectId, Vec<GameAction>>,
        object_id: ObjectId,
        action: &GameAction,
    ) -> bool {
        grouped
            .get(&object_id)
            .is_some_and(|actions| actions.contains(action))
    }

    fn empty_effect(source_id: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Unimplemented {
                name: "test".to_string(),
                description: None,
            },
            Vec::new(),
            source_id,
            PlayerId(0),
        )
    }

    fn set_dummy_pending_cast(state: &mut GameState) {
        let source_id = create_object(
            state,
            CardId(0),
            PlayerId(0),
            "Dummy Spell".to_string(),
            Zone::Hand,
        );
        state.pending_cast = Some(Box::new(PendingCast::new(
            source_id,
            CardId(0),
            empty_effect(source_id),
            ManaCost::generic(1),
        )));
        state.stack.push_back(StackEntry {
            id: source_id,
            source_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(0),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
    }

    #[test]
    fn legal_actions_for_viewer_returns_empty_when_not_acting() {
        // Priority to player 0; any other viewer must receive an empty tuple.
        let state = GameState::new_two_player(42);
        // Baseline: the acting player gets the full result.
        let acting = state
            .waiting_for
            .acting_player()
            .expect("new_two_player opens with a priority state");
        let full = legal_actions_for_viewer(&state, acting);
        let expected = legal_actions_full(&state);
        assert_eq!(full.0.len(), expected.0.len());
        assert_eq!(full.1.len(), expected.1.len());
        assert_eq!(full.2.len(), expected.2.len());

        // Non-acting viewer: empty across all three components.
        let other = PlayerId(acting.0 ^ 1);
        let (actions, costs, grouped) = legal_actions_for_viewer(&state, other);
        assert!(
            actions.is_empty(),
            "non-acting viewer must receive no actions"
        );
        assert!(
            costs.is_empty(),
            "non-acting viewer must receive no spell costs"
        );
        assert!(
            grouped.is_empty(),
            "non-acting viewer must receive no grouped actions"
        );
    }

    /// CR 723.3 + CR 723.5 (issue #2012): under a turn-control effect the
    /// controlled player is still the active/acting seat, but the *controller*
    /// makes their choices. `legal_actions_for_viewer` must authorize the
    /// controller (the authorized submitter), returning the controlled turn's
    /// actions to them — not an empty set, which would freeze the turn.
    #[test]
    fn legal_actions_for_viewer_routes_to_turn_controller() {
        use crate::types::player::PlayerId;

        let mut state = GameState::new_two_player(42);
        let controlled = PlayerId(1);
        let controller = PlayerId(0);

        // CR 723.3: P1 is still the active player while controlled by P0.
        state.active_player = controlled;
        state.turn_decision_controller = Some(controller);
        state.waiting_for = WaitingFor::Priority { player: controlled };
        // The authorized submitter is the controller, not the acting seat.
        state.priority_player = crate::game::turn_control::turn_decision_maker(&state);

        // The acting seat (P1) is NOT the authorized submitter, so it gets none.
        let (controlled_actions, _, _) = legal_actions_for_viewer(&state, controlled);
        assert!(
            controlled_actions.is_empty(),
            "the controlled seat is not the authorized submitter"
        );

        // CR 723.5: the controller receives the controlled turn's full set,
        // matching the unfiltered engine view.
        let (controller_actions, _, _) = legal_actions_for_viewer(&state, controller);
        let full = legal_actions_full(&state);
        assert_eq!(
            controller_actions, full.0,
            "CR 723.5: the controller must receive the controlled player's legal actions"
        );
    }

    /// Issue #537 cross-player AI test (5c): Animate Dead in player B's hand
    /// must surface as a castable action even when the only legal target is
    /// a creature card in player A's graveyard. The cross-player axis stresses
    /// that `find_legal_targets` (zone branch) aggregates graveyards across
    /// every player, not just the caster's.
    ///
    /// Pre-fix, the Enchant filter carried a free-text `Subtype: "creature
    /// card in a graveyard"` that matched no real object, so the CastSpell
    /// action was absent from `legal_actions`. Post-fix, the zone-aware filter
    /// admits the cross-player graveyard creature.
    ///
    /// CR 303.4a + CR 702.5a: the Aura's enchant filter scopes the target set.
    #[test]
    fn legal_actions_offers_animate_dead_with_cross_player_graveyard_target() {
        use crate::types::keywords::Keyword;
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
        use crate::types::phase::Phase;
        use std::str::FromStr;

        // Player B (PlayerId(1)) has priority on their main phase.
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        // Animate Dead in player B's hand.
        let aura_id = create_object(
            &mut state,
            CardId(601),
            PlayerId(1),
            "Animate Dead".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.keywords
                .push(Keyword::from_str("Enchant:creature card in a graveyard").unwrap());
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            };
        }
        state.players[1].mana_pool.add(ManaUnit {
            color: ManaType::Black,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });

        // Creature card in player A's graveyard (cross-player axis).
        let creature_id = create_object(
            &mut state,
            CardId(602),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let actions = legal_actions(&state);
        let cast_present = actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == aura_id));
        assert!(
            cast_present,
            "legal_actions must surface CastSpell for Animate Dead targeting a cross-player graveyard creature; got {:?}",
            actions
        );
    }

    #[test]
    fn legal_actions_for_viewer_gates_on_non_priority_decision_points() {
        // Regression: the viewer-gating wrapper dispatches purely on
        // `acting_player()`, which covers priority *and* non-priority decision
        // points (combat declarations, target selection, mulligan, etc.). If a
        // future refactor breaks `acting_player()` for one of these variants,
        // the wrapper would silently strip legal actions from the player who
        // actually owes the decision. `DeclareAttackers` is the cheapest such
        // variant to construct and stands in for the broader class.
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(1),
            valid_attacker_ids: Vec::new(),
            valid_attack_targets: Vec::new(),
        };
        // Acting player gets the full result (matches `legal_actions_full`).
        let acting = legal_actions_for_viewer(&state, PlayerId(1));
        let expected = legal_actions_full(&state);
        assert_eq!(acting.0.len(), expected.0.len());
        // Non-acting player gets the empty tuple.
        let (actions, costs, grouped) = legal_actions_for_viewer(&state, PlayerId(0));
        assert!(actions.is_empty());
        assert!(costs.is_empty());
        assert!(grouped.is_empty());
    }

    #[test]
    fn legal_actions_for_viewer_empty_on_game_over() {
        // CR 117.1 — only the acting player may act. `WaitingFor::GameOver` has
        // no acting player, so every viewer (including would-be "active" ones)
        // receives the empty tuple.
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::GameOver { winner: None };
        for pid in [PlayerId(0), PlayerId(1)] {
            let (actions, costs, grouped) = legal_actions_for_viewer(&state, pid);
            assert!(
                actions.is_empty(),
                "GameOver: viewer {pid:?} must receive no actions"
            );
            assert!(
                costs.is_empty(),
                "GameOver: viewer {pid:?} must receive no spell costs"
            );
            assert!(
                grouped.is_empty(),
                "GameOver: viewer {pid:?} must receive no grouped actions"
            );
        }
    }

    #[test]
    fn legal_actions_by_object_groups_flat_list_correctly() {
        // The grouped map may include mana actions that are intentionally
        // absent from the flat list, but every grouped entry must still equal
        // source_object() of its action, and every flat action with Some(id)
        // must appear under that id.
        let state = GameState::new_two_player(42);
        let (flat, _, grouped) = legal_actions_full(&state);

        // Each grouped vector contains only actions whose source_object matches the key.
        for (id, actions) in &grouped {
            for action in actions {
                assert_eq!(
                    action.source_object(),
                    Some(*id),
                    "action {} grouped under wrong id",
                    action.variant_name()
                );
            }
        }

        // Every action in the flat list with a source_object appears in the grouped map.
        for action in &flat {
            if let Some(id) = action.source_object() {
                let bucket = grouped.get(&id).unwrap_or_else(|| {
                    panic!("action {} missing from grouped map", action.variant_name())
                });
                assert!(
                    bucket.contains(action),
                    "action {} not found in its own bucket",
                    action.variant_name()
                );
            }
        }

        // Lookup for a non-existent object returns None (defensive — callers may
        // request hand-zone or battlefield ids that have no legal actions).
        assert!(!grouped.contains_key(&ObjectId(99_999)));
    }

    /// Build a Festering Thicket-shaped object in the active player's hand: a
    /// `CoreType::Land` carrying a hand-zone `AbilityKind::Activated` cycling
    /// ability (composite `{2}` + self-discard cost, `Effect::Draw`). Mirrors
    /// `synthesize_cycling`. Two untapped mana lands are added so the `{2}`
    /// cost is payable and the cycling ability is a legal action.
    fn setup_land_with_cycling(state: &mut GameState) -> ObjectId {
        // CR 305.2 + CR 602.1: PlayLand and hand-zone activations are only
        // offered during a main phase with an empty stack.
        state.phase = crate::types::phase::Phase::PreCombatMain;
        // Two mana sources so the {2} cycling cost can be paid.
        for _ in 0..2 {
            let mana_land = create_land(state, "Forest", &["Forest"]);
            add_fixed_mana_ability(state, mana_land, ManaColor::Green);
        }
        // The card in hand.
        let card = create_object(
            state,
            CardId(7),
            PlayerId(0),
            "Festering Thicket".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&card).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        let mut cycling = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(2),
                },
                AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                    self_scope: crate::types::ability::DiscardSelfScope::SourceCard,
                },
            ],
        });
        cycling.activation_zone = Some(Zone::Hand);
        Arc::make_mut(&mut obj.abilities).push(cycling);
        card
    }

    /// #506: with the land drop available, a land carrying cycling offers BOTH
    /// `PlayLand` and the cycling `ActivateAbility`.
    #[test]
    fn legal_actions_offers_playland_and_cycling_for_land_with_cycling() {
        let mut state = setup_priority();
        // CR 305.2: land drop available — no land played this turn.
        state.lands_played_this_turn = 0;
        let card = setup_land_with_cycling(&mut state);
        let card_id = state.objects.get(&card).unwrap().card_id;

        let (_, _, grouped) = legal_actions_full(&state);

        assert!(
            bucket_has(
                &grouped,
                card,
                &GameAction::PlayLand {
                    object_id: card,
                    card_id,
                },
            ),
            "land drop available — PlayLand must be offered"
        );
        assert!(
            bucket_has(
                &grouped,
                card,
                &GameAction::ActivateAbility {
                    source_id: card,
                    ability_index: 0,
                },
            ),
            "cycling ActivateAbility must be offered"
        );
    }

    /// #506: with the land drop spent (CR 305.2b), the same land offers ONLY
    /// the cycling `ActivateAbility` — no `PlayLand`. This is the single-action
    /// condition the frontend confirmation fix targets.
    #[test]
    fn legal_actions_offers_only_cycling_when_land_drop_spent() {
        let mut state = setup_priority();
        let card = setup_land_with_cycling(&mut state);
        let card_id = state.objects.get(&card).unwrap().card_id;
        // CR 305.2b: land drop spent. Set the counter to an unambiguously large
        // value so it exceeds any plausible effective limit regardless of
        // additional-land-drop effects.
        state.lands_played_this_turn = 99;

        let (_, _, grouped) = legal_actions_full(&state);

        assert!(
            !bucket_has(
                &grouped,
                card,
                &GameAction::PlayLand {
                    object_id: card,
                    card_id,
                },
            ),
            "CR 305.2b: land drop spent — PlayLand must NOT be offered"
        );
        assert!(
            bucket_has(
                &grouped,
                card,
                &GameAction::ActivateAbility {
                    source_id: card,
                    ability_index: 0,
                },
            ),
            "cycling ActivateAbility must still be offered"
        );
    }

    #[test]
    fn legal_actions_offer_runtime_granted_typecycling_from_homing_sliver() {
        let mut state = setup_priority();
        state.phase = crate::types::phase::Phase::PreCombatMain;

        let homing_sliver = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Homing Sliver".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&homing_sliver)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::card()
                            .subtype("Sliver".to_string())
                            .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
                    ))
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Typecycling {
                            cost: ManaCost::NoCost,
                            subtype: "Sliver".to_string(),
                        },
                    }]),
            );

        let hand_sliver = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Striking Sliver".to_string(),
            Zone::Hand,
        );
        let printed_len = {
            let obj = state.objects.get_mut(&hand_sliver).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Sliver".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.abilities.len()
        };

        assert!(
            crate::game::off_zone_characteristics::off_zone_has_keyword_kind(
                &state,
                hand_sliver,
                KeywordKind::Typecycling,
            ),
            "Homing Sliver static should grant Typecycling to the Sliver card in hand"
        );

        let transient_index = printed_len;
        assert!(
            crate::game::casting::can_activate_ability_now(
                &state,
                PlayerId(0),
                hand_sliver,
                transient_index,
            ),
            "runtime-granted Typecycling should be activatable at the first transient index"
        );

        let (_, _, grouped) = legal_actions_full(&state);
        assert!(
            bucket_has(
                &grouped,
                hand_sliver,
                &GameAction::ActivateAbility {
                    source_id: hand_sliver,
                    ability_index: transient_index,
                },
            ),
            "legal actions must expose the runtime-granted Slivercycling activation"
        );

        let _result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: hand_sliver,
                ability_index: transient_index,
            },
        )
        .expect("runtime-granted Typecycling activation should be accepted");

        assert!(
            state.stack.iter().any(|entry| {
                matches!(
                    entry.kind,
                    StackEntryKind::ActivatedAbility { source_id, .. } if source_id == hand_sliver
                )
            }),
            "activating runtime-granted Slivercycling should put the ability on the stack"
        );
        assert_eq!(
            state.objects[&hand_sliver].zone,
            Zone::Graveyard,
            "activating runtime-granted Slivercycling should discard the source as a cost"
        );
    }

    #[test]
    fn legal_actions_by_object_exposes_engine_mana_sources_without_flat_actions() {
        let mut state = setup_priority();
        let fetch = create_land(&mut state, "Polluted Delta", &[]);
        let forest = create_land(&mut state, "Forest", &["Forest"]);
        let dual = create_land(&mut state, "Underground Sea", &[]);
        let blue_idx = add_fixed_mana_ability(&mut state, dual, ManaColor::Blue);
        let black_idx = add_fixed_mana_ability(&mut state, dual, ManaColor::Black);

        let (flat, _, grouped) = legal_actions_full(&state);

        assert!(
            !bucket_has(
                &grouped,
                fetch,
                &GameAction::TapLandForMana { object_id: fetch },
            ),
            "fetch land with no mana-producing subtype or explicit mana ability must not be tappable"
        );
        assert!(
            bucket_has(
                &grouped,
                forest,
                &GameAction::TapLandForMana { object_id: forest },
            ),
            "subtype-only basic land fallback must remain tappable"
        );
        assert!(bucket_has(
            &grouped,
            dual,
            &GameAction::ActivateAbility {
                source_id: dual,
                ability_index: blue_idx,
            },
        ));
        assert!(bucket_has(
            &grouped,
            dual,
            &GameAction::ActivateAbility {
                source_id: dual,
                ability_index: black_idx,
            },
        ));
        assert!(
            !flat
                .iter()
                .any(|action| matches!(action, GameAction::TapLandForMana { object_id } if *object_id == forest)),
            "flat legal actions stay free of land mana actions"
        );
        assert!(
            !flat
                .iter()
                .any(|action| matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == dual)),
            "flat legal actions stay free of explicit mana abilities"
        );
    }

    #[test]
    fn legal_actions_by_object_exposes_nonland_mana_abilities_without_flat_actions() {
        let mut state = setup_priority();
        let rock = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mana Rock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&rock)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        let ability_index = add_fixed_mana_ability(&mut state, rock, ManaColor::Green);

        let (flat, _, grouped) = legal_actions_full(&state);

        assert!(bucket_has(
            &grouped,
            rock,
            &GameAction::ActivateAbility {
                source_id: rock,
                ability_index,
            },
        ));
        assert!(!flat.iter().any(
            |action| matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == rock)
        ));
    }

    #[test]
    fn legal_actions_by_object_dedups_mana_ability_during_payment() {
        // Regression: during WaitingFor::ManaPayment the flat candidate list
        // (from `mana_payment_actions`) carries the non-land mana ability so the
        // AI/server can pay, and the `activatable_object_mana_actions` extension
        // re-derives it. `is_mana_ability()` only strips land taps, so without a
        // per-object dedup the grouped map (the frontend ability picker) listed
        // the same ActivateAbility twice — surfacing as Birds of Paradise's "Add
        // one mana of any color" appearing twice in the convoke picker.
        let mut state = setup_priority();
        let rock = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mana Rock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&rock)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        let ability_index = add_fixed_mana_ability(&mut state, rock, ManaColor::Green);
        set_dummy_pending_cast(&mut state);
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };

        let (_, _, grouped) = legal_actions_full(&state);

        let mana_action = GameAction::ActivateAbility {
            source_id: rock,
            ability_index,
        };
        let count = grouped
            .get(&rock)
            .map(|bucket| bucket.iter().filter(|a| **a == mana_action).count())
            .unwrap_or(0);
        assert_eq!(
            count, 1,
            "the per-object map must offer the mana ability exactly once during ManaPayment, got {count}"
        );
    }

    #[test]
    fn legal_actions_by_object_exposes_filter_land_with_payable_mana_sub_cost() {
        let mut state = setup_priority();
        create_land(&mut state, "Forest", &["Forest"]);
        let skycloud = create_land(&mut state, "Skycloud Expanse", &[]);
        Arc::make_mut(&mut state.objects.get_mut(&skycloud).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::White, ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    },
                    AbilityCost::Tap,
                ],
            }),
        );

        let (_, _, grouped) = legal_actions_full(&state);

        assert!(
            bucket_has(
                &grouped,
                skycloud,
                &GameAction::ActivateAbility {
                    source_id: skycloud,
                    ability_index: 0,
                },
            ),
            "Skycloud Expanse should be manually activatable when another mana source can pay its {{1}} cost",
        );
    }

    #[test]
    fn legal_actions_by_object_exposes_no_tap_sacrifice_mana_abilities() {
        let mut state = setup_priority();
        let altar = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Phyrexian Altar".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&altar).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.tapped = true;
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::AnyOneColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            color_options: vec![
                                ManaColor::White,
                                ManaColor::Blue,
                                ManaColor::Black,
                                ManaColor::Red,
                                ManaColor::Green,
                            ],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                    1,
                ))),
            );
        }

        let creature = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let (flat, _, grouped) = legal_actions_full(&state);

        assert!(bucket_has(
            &grouped,
            altar,
            &GameAction::ActivateAbility {
                source_id: altar,
                ability_index: 0,
            },
        ));
        assert!(!flat.iter().any(
            |action| matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == altar)
        ));
    }

    #[test]
    fn legal_actions_by_object_exposes_mana_actions_during_payment_states() {
        for (waiting_for, needs_pending_cast) in [
            (
                WaitingFor::Priority {
                    player: PlayerId(0),
                },
                false,
            ),
            (
                WaitingFor::ManaPayment {
                    player: PlayerId(0),
                    convoke_mode: None,
                },
                true,
            ),
            (
                WaitingFor::UnlessPayment {
                    player: PlayerId(0),
                    cost: AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    },
                    pending_effect: Box::new(empty_effect(ObjectId(0))),
                    trigger_event: None,
                    effect_description: None,
                    remaining: Vec::new(),
                },
                false,
            ),
        ] {
            let mut state = setup_priority();
            if needs_pending_cast {
                set_dummy_pending_cast(&mut state);
            }
            state.waiting_for = waiting_for;
            let forest = create_land(&mut state, "Forest", &["Forest"]);

            let (_, _, grouped) = legal_actions_full(&state);

            assert!(
                bucket_has(
                    &grouped,
                    forest,
                    &GameAction::TapLandForMana { object_id: forest },
                ),
                "mana actions must be exposed during {:?}",
                state.waiting_for
            );
        }
    }

    #[test]
    fn legal_actions_filter_out_reducer_illegal_priority_candidates() {
        let mut state = GameState::new_two_player(42);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let raw_candidates = candidate_actions(&state);
        assert!(raw_candidates
            .iter()
            .any(|candidate| { matches!(candidate.action, GameAction::PassPriority) }));

        let validated_candidates = validated_candidate_actions(&state);
        assert!(validated_candidates.is_empty());
        assert!(legal_actions(&state).is_empty());
    }

    #[test]
    fn legal_actions_preserve_reducer_legal_priority_candidates() {
        let state = GameState::new_two_player(42);

        let validated_candidates = validated_candidate_actions(&state);
        assert!(validated_candidates
            .iter()
            .any(|candidate| { matches!(candidate.action, GameAction::PassPriority) }));

        let actions = legal_actions(&state);
        assert!(actions
            .iter()
            .any(|action| matches!(action, GameAction::PassPriority)));
    }

    #[test]
    fn cheap_reject_candidate_rejects_out_of_range_replacement_choice() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::ReplacementChoice {
            player: PlayerId(0),
            candidate_count: 2,
            candidate_descriptions: Vec::new(),
        };

        assert!(cheap_reject_candidate(
            &state,
            &GameAction::ChooseReplacement { index: 2 }
        ));
        assert!(!cheap_reject_candidate(
            &state,
            &GameAction::ChooseReplacement { index: 1 }
        ));
    }

    #[test]
    fn cheap_reject_candidate_preserves_ambiguous_priority_pass() {
        let state = GameState::new_two_player(42);
        assert!(!cheap_reject_candidate(&state, &GameAction::PassPriority));
    }

    #[test]
    fn cheap_reject_candidate_accepts_up_to_search_counts() {
        let mut state = GameState::new_two_player(42);
        let choices = vec![ObjectId(1), ObjectId(2), ObjectId(3)];
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            cards: choices.clone(),
            count: 2,
            reveal: false,
            up_to: true,
            allows_partial_find: false,
            constraint: SearchSelectionConstraint::None,
            split: None,
        };

        assert!(!cheap_reject_candidate(
            &state,
            &GameAction::SelectCards { cards: vec![] }
        ));
        assert!(!cheap_reject_candidate(
            &state,
            &GameAction::SelectCards {
                cards: vec![choices[0]]
            }
        ));
        assert!(!cheap_reject_candidate(
            &state,
            &GameAction::SelectCards {
                cards: vec![choices[0], choices[1]]
            }
        ));
        assert!(cheap_reject_candidate(
            &state,
            &GameAction::SelectCards {
                cards: choices.clone()
            }
        ));
    }

    #[test]
    fn cheap_reject_candidate_preserves_exact_search_count() {
        let mut state = GameState::new_two_player(42);
        let choices = vec![ObjectId(1), ObjectId(2)];
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            cards: choices.clone(),
            count: 2,
            reveal: false,
            up_to: false,
            allows_partial_find: false,
            constraint: SearchSelectionConstraint::None,
            split: None,
        };

        assert!(cheap_reject_candidate(
            &state,
            &GameAction::SelectCards {
                cards: vec![choices[0]]
            }
        ));
        assert!(!cheap_reject_candidate(
            &state,
            &GameAction::SelectCards { cards: choices }
        ));
    }

    #[test]
    fn cheap_reject_candidate_permits_partial_constrained_search() {
        // CR 701.23b: a stated-quality (MatchEachFilter) search may legally find
        // fewer cards than requested — including none. The validated legal-action
        // path must NOT cheap-reject a short/empty pick, or the AI freezes.
        let mut state = GameState::new_two_player(42);
        let choices = vec![ObjectId(1), ObjectId(2)];
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            cards: choices.clone(),
            count: 2,
            reveal: false,
            up_to: false,
            allows_partial_find: false,
            constraint: SearchSelectionConstraint::MatchEachFilter {
                filters: vec![TargetFilter::Any, TargetFilter::Any],
            },
            split: None,
        };

        // Empty (full fail-to-find) is legal and must survive cheap-reject.
        assert!(!cheap_reject_candidate(
            &state,
            &GameAction::SelectCards { cards: vec![] }
        ));
        // Partial pick (one of two) is legal and must survive cheap-reject.
        assert!(!cheap_reject_candidate(
            &state,
            &GameAction::SelectCards {
                cards: vec![choices[0]]
            }
        ));
        // The full pick is still legal.
        assert!(!cheap_reject_candidate(
            &state,
            &GameAction::SelectCards {
                cards: choices.clone()
            }
        ));
        // Over the requested count remains rejected.
        assert!(cheap_reject_candidate(
            &state,
            &GameAction::SelectCards {
                cards: vec![choices[0], choices[1], ObjectId(3)]
            }
        ));
    }

    #[test]
    fn auto_pass_does_not_skip_non_mana_land_ability() {
        // Shifting Woodland pattern: a land with both a mana ability and a
        // non-mana activated ability (delirium BecomeCopy). Auto-pass must NOT
        // fire when the non-mana ability is a legal action.
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction, TargetFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::mana::ManaColor;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land With Non-Mana Ability".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            // Mana ability (index 0)
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Green],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
            // Non-mana ability (index 1)
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::BecomeCopy {
                    target: TargetFilter::Any,
                    duration: Some(crate::types::ability::Duration::UntilEndOfTurn),
                    mana_value_limit: None,
                    additional_modifications: Vec::new(),
                },
            ));
        }

        // Actions include PassPriority + the non-mana ActivateAbility
        let actions = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id: land,
                ability_index: 1,
            },
        ];
        assert!(
            !super::auto_pass_recommended(&state, &actions),
            "Auto-pass must not fire when a non-mana land ability is available"
        );
        assert!(
            super::has_meaningful_priority_action(&state, &actions),
            "Extracted helper must classify non-mana abilities as meaningful"
        );

        // But if only the mana ability is available, auto-pass should fire
        let mana_only = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id: land,
                ability_index: 0,
            },
        ];
        assert!(
            super::auto_pass_recommended(&state, &mana_only),
            "Auto-pass should fire when only mana abilities are available"
        );
        assert!(
            !super::has_meaningful_priority_action(&state, &mana_only),
            "Extracted helper must ignore standalone mana abilities"
        );

        use crate::types::ability::KeywordAction;
        state.stack.push_back(StackEntry {
            id: ObjectId(100),
            source_id: land,
            controller: PlayerId(0),
            kind: StackEntryKind::KeywordAction {
                action: KeywordAction::Crew {
                    vehicle_id: land,
                    paid_creature_ids: Vec::new(),
                },
            },
        });
        assert!(
            super::auto_pass_recommended(&state, &actions),
            "Existing frontend recommendation keeps the own-stack shortcut"
        );
        assert!(
            super::has_meaningful_priority_action(&state, &actions),
            "The reusable helper must not apply the own-stack shortcut"
        );
    }

    // Issue #544: Krark-Clan Ironworks ("Sacrifice an artifact: Add {C}{C}") is
    // a mana ability whose cost sacrifices a permanent. Even though it is a mana
    // ability, the sacrifice is a meaningful priority decision (CR 605.3a +
    // 603.6), so auto-pass must NOT fire when it is the only available action.
    #[test]
    fn auto_pass_does_not_skip_sacrifice_for_mana_ability() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
            QuantityExpr, TargetFilter, TypeFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::mana::ManaColor;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let kci = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Krark-Clan Ironworks".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&kci).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            // KCI's real parsed cost: a BARE Sacrifice with a Typed(Artifact)
            // target (not Composite, not SelfRef). Drive the real classifier.
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::AnyOneColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            color_options: vec![ManaColor::Red],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                    1,
                ))),
            );
        }

        let actions = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id: kci,
                ability_index: 0,
            },
        ];
        assert!(
            super::has_meaningful_priority_action(&state, &actions),
            "A sacrifice-for-mana ability is a meaningful priority decision (CR 605.3a + 603.6)"
        );
        assert!(
            !super::auto_pass_recommended(&state, &actions),
            "Auto-pass must not fire when only a sacrifice-for-mana ability is available (#544)"
        );

        // Production path: flat `legal_actions` omits mana abilities, so the
        // auto-pass gate must consult `activatable_object_mana_actions` instead
        // of relying on the caller to inject the activation.
        let (flat, _, grouped) = legal_actions_full(&state);
        assert!(
            !flat.iter().any(|a| matches!(
                a,
                GameAction::ActivateAbility { source_id, .. } if *source_id == kci
            )),
            "precondition: KCI activation is grouped-only during priority"
        );
        assert!(
            bucket_has(
                &grouped,
                kci,
                &GameAction::ActivateAbility {
                    source_id: kci,
                    ability_index: 0,
                },
            ),
            "KCI activation must appear in legal_actions_by_object"
        );
        assert!(
            !super::auto_pass_recommended(&state, &flat),
            "Issue #544: auto-pass must not fire from flat legal_actions alone \
             when grouped sacrifice-for-mana is available"
        );
    }

    #[test]
    fn auto_passes_initial_upkeep_and_draw_priority_with_instant_speed_actions() {
        let actions = vec![
            GameAction::PassPriority,
            GameAction::CastSpell {
                object_id: ObjectId(10),
                card_id: CardId(10),
                targets: Vec::new(),

                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            },
        ];

        for phase in [
            crate::types::phase::Phase::Upkeep,
            crate::types::phase::Phase::Draw,
        ] {
            let mut state = GameState::new_two_player(42);
            state.phase = phase;
            state.active_player = PlayerId(0);
            state.priority_player = PlayerId(0);
            state.waiting_for = WaitingFor::Priority {
                player: PlayerId(0),
            };

            assert!(
                super::auto_pass_recommended(&state, &actions),
                "initial {phase:?} priority should auto-pass unless a phase stop/full control gates it"
            );
        }

        let mut main_phase = GameState::new_two_player(42);
        main_phase.phase = crate::types::phase::Phase::PreCombatMain;
        main_phase.active_player = PlayerId(0);
        main_phase.priority_player = PlayerId(0);
        main_phase.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(
            !super::auto_pass_recommended(&main_phase, &actions),
            "main-phase meaningful actions must still stop auto-pass"
        );
    }

    // Witherbloom, the Balancer regression: the commander sits in the command zone
    // with `Keyword::Affinity(Creature)`. Even when the player has no mana available
    // (so no `CastSpell` action is offered), `legal_actions_full` must still expose
    // the engine-effective cost so the UI can display the Affinity-reduced cost
    // instead of falling back to the printed mana cost.
    #[test]
    fn spell_costs_include_commander_affinity_reduction_without_castability() {
        use crate::types::ability::{TypeFilter, TypedFilter};
        use crate::types::card_type::Supertype;
        use crate::types::keywords::Keyword;
        use crate::types::mana::ManaCostShard;

        let mut state = setup_priority();
        state.format_config.command_zone = true;

        let commander_id = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Witherbloom, the Balancer".to_string(),
            Zone::Command,
        );
        {
            let obj = state.objects.get_mut(&commander_id).unwrap();
            obj.is_commander = true;
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.supertypes.push(Supertype::Legendary);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black, ManaCostShard::Green],
                generic: 5,
            };
            obj.keywords.push(Keyword::Affinity(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: None,
                properties: vec![],
            }));
        }

        for i in 0u64..3 {
            let id = create_object(
                &mut state,
                CardId(1100 + i),
                PlayerId(0),
                format!("Bear {i}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let (actions, spell_costs, _grouped) = legal_actions_full(&state);

        let has_cast_action = actions.iter().any(|a| {
            matches!(
                a,
                GameAction::CastSpell { object_id, .. } if *object_id == commander_id
            )
        });
        assert!(
            !has_cast_action,
            "precondition: with no mana available, CastSpell must be absent from legal_actions"
        );

        let displayed = spell_costs
            .get(&commander_id)
            .expect("spell_costs must include the commander even when not currently castable");
        let ManaCost::Cost { generic, shards } = displayed else {
            panic!("expected ManaCost::Cost, got {displayed:?}");
        };
        assert_eq!(
            *generic, 2,
            "Affinity for creatures with 3 creatures on board reduces generic from 5 to 2"
        );
        assert_eq!(
            shards,
            &vec![ManaCostShard::Black, ManaCostShard::Green],
            "colored shards remain untouched by Affinity"
        );
    }

    // Witherbloom's static grants Affinity(Creature) to instant and sorcery spells
    // the controller casts. The display layer must surface that reduction on the
    // cards in hand — including when the player can't currently cast them (e.g.,
    // a sorcery during an opponent's turn, or insufficient mana). Without this
    // coverage, the user "never sees the cost reduced" while Witherbloom is out.
    #[test]
    fn spell_costs_apply_granted_affinity_from_battlefield_static() {
        use crate::types::ability::{
            AbilityKind, Effect, ManaContribution, ManaProduction, TargetFilter, TypeFilter,
            TypedFilter,
        };
        use crate::types::card_type::Supertype;
        use crate::types::keywords::Keyword;
        use crate::types::mana::ManaCostShard;
        use crate::types::statics::StaticMode;
        use crate::types::StaticDefinition;

        let mut state = setup_priority();

        // Witherbloom on the battlefield with the granting static.
        let witherbloom_id = create_object(
            &mut state,
            CardId(3000),
            PlayerId(0),
            "Witherbloom, the Balancer".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&witherbloom_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.supertypes.push(Supertype::Legendary);
            let affected = TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Instant],
                        controller: Some(crate::types::ability::ControllerRef::You),
                        properties: vec![],
                    }),
                    TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Sorcery],
                        controller: Some(crate::types::ability::ControllerRef::You),
                        properties: vec![],
                    }),
                ],
            };
            let granted = Keyword::Affinity(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: None,
                properties: vec![],
            });
            let def = StaticDefinition {
                mode: StaticMode::CastWithKeyword { keyword: granted },
                affected: Some(affected),
                modifications: vec![],
                condition: None,
                per_player_condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: vec![],
                characteristic_defining: false,
                description: Some(
                    "Instant and sorcery spells you cast have affinity for creatures.".to_string(),
                ),
                attack_defended: None,
            };
            obj.static_definitions = vec![def].into();
        }

        // Sorcery in hand with generic cost > 0, and no mana available — so without
        // the display-path fix, no CastSpell action would be produced.
        let sorcery_id = create_object(
            &mut state,
            CardId(3001),
            PlayerId(0),
            "Test Sorcery".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&sorcery_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 3,
            };
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Red],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            ));
        }

        // 2 additional creatures controlled by the player. Total creatures on
        // battlefield: Witherbloom + 2 = 3.
        for i in 0u64..2 {
            let id = create_object(
                &mut state,
                CardId(3100 + i),
                PlayerId(0),
                format!("Bear {i}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let (_actions, spell_costs, _grouped) = legal_actions_full(&state);

        let displayed = spell_costs
            .get(&sorcery_id)
            .expect("spell_costs must surface the granted-Affinity-reduced sorcery cost");
        let ManaCost::Cost { generic, shards } = displayed else {
            panic!("expected ManaCost::Cost, got {displayed:?}");
        };
        assert_eq!(
            *generic, 0,
            "3 creatures (Witherbloom + 2 bears) × Affinity({{1}}) reduces 3 generic to 0"
        );
        assert_eq!(
            shards,
            &vec![ManaCostShard::Red],
            "colored shards remain untouched by Affinity"
        );
    }

    /// Issue #1542: Emergence Zone must expose TapLandForMana alongside its
    /// sacrifice-for-flash activated ability.
    #[test]
    fn emergence_zone_exposes_tap_for_mana() {
        let mut state = setup_priority();
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
        let land_id = create_object(
            &mut state,
            CardId(1542),
            PlayerId(0),
            "Emergence Zone".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            let parsed = parse_oracle_text(
                "{T}: Add {C}.\n\
                 {1}, {T}, Sacrifice this land: You may cast spells this turn as though they had flash.",
                "Emergence Zone",
                &[],
                &[String::from("Land")],
                &[],
            );
            Arc::make_mut(&mut obj.abilities).extend(parsed.abilities);
        }

        let (_, _, grouped) = legal_actions_full(&state);
        let land_actions = grouped
            .get(&land_id)
            .expect("Emergence Zone should expose legal actions");
        assert!(
            land_actions.iter().any(|action| matches!(
                action,
                GameAction::TapLandForMana { object_id } if *object_id == land_id
            )),
            "expected TapLandForMana in grouped actions, got {land_actions:?}"
        );
        assert!(
            land_actions.iter().any(|action| matches!(
                action,
                GameAction::ActivateAbility {
                    source_id,
                    ability_index: 1,
                } if *source_id == land_id
            )),
            "flash sacrifice ability must be activatable when {{1}} is payable"
        );

        let flash_effect = &state.objects[&land_id].abilities[1].effect;
        assert!(
            matches!(*flash_effect.clone(), Effect::GenericEffect { .. }),
            "flash ability must parse as GenericEffect, not CastFromZone — got {flash_effect:?}"
        );
        assert_eq!(state.objects[&land_id].abilities.len(), 2);

        apply_as_current(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .expect("TapLandForMana must succeed when flash ability is also legal");
        assert!(
            state.objects[&land_id].tapped,
            "Emergence Zone should be tapped after TapLandForMana"
        );
        assert!(
            state.players[0].mana_pool.total() >= 1,
            "mana should be added to pool"
        );
    }

    /// Progress-wedge detection: a `NamedChoice` owed by a player with zero
    /// legal options is a wedged decision — `named_choice_actions` yields no
    /// candidates so the only authorized submitter (P0) has an empty
    /// legal-action set. The diagnostic must fire, naming the variant and the
    /// stuck player.
    #[test]
    fn stuck_diagnostic_fires_on_unsatisfiable_named_choice() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        // A "choose a player" prompt (CR 601.2b) with NO offered options — the
        // engine can produce no legal `ChooseOption`, so no submitter can act.
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: ChoiceType::Labeled { options: vec![] },
            options: vec![],
            source_id: None,
        };

        // Precondition: this state really has no legal action for P0.
        assert!(
            legal_actions_for_viewer(&state, PlayerId(0)).0.is_empty(),
            "test premise: the unsatisfiable NamedChoice must offer no legal action"
        );

        let diag = stuck_decision_diagnostic(&state)
            .expect("an owed decision with no legal action must report a stuck diagnostic");
        assert_eq!(diag.waiting_for_kind, "NamedChoice");
        assert_eq!(diag.stuck_players, vec![PlayerId(0)]);
    }

    /// CR 117.1: A normal `Priority` window is never "stuck" — passing priority
    /// is always legal, and the diagnostic explicitly excludes `Priority`.
    #[test]
    fn stuck_diagnostic_none_on_priority() {
        let state = setup_priority();
        assert!(stuck_decision_diagnostic(&state).is_none());
    }

    /// After the game is over there is no acting player, so there is nothing to
    /// be stuck on — the diagnostic returns `None`.
    #[test]
    fn stuck_diagnostic_none_on_game_over() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };
        assert!(stuck_decision_diagnostic(&state).is_none());
    }

    /// False-positive sweep: a NORMAL non-`Priority` decision that legitimately
    /// offers actions must NOT trip the progress-wedge detector. `ManaPayment`
    /// always offers `PassPriority` to finalize payment, so it is never stuck.
    #[test]
    fn stuck_diagnostic_none_on_normal_mana_payment() {
        let mut state = setup_priority();
        let rock = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mana Rock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&rock)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        add_fixed_mana_ability(&mut state, rock, ManaColor::Green);
        set_dummy_pending_cast(&mut state);
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };

        // Premise: a normal ManaPayment offers at least one legal action.
        assert!(
            !legal_actions_for_viewer(&state, PlayerId(0)).0.is_empty(),
            "ManaPayment must offer at least PassPriority"
        );
        assert!(
            stuck_decision_diagnostic(&state).is_none(),
            "a normal ManaPayment decision must not be flagged stuck"
        );
    }

    /// False-positive sweep (CR 103.5): a normal `MulliganDecision` always
    /// offers Keep/Mulligan to each pending player, so the detector must not
    /// fire.
    #[test]
    fn stuck_diagnostic_none_on_normal_mulligan_decision() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![MulliganDecisionEntry {
                player: PlayerId(0),
                mulligan_count: 0,
            }],
            free_first_mulligan: false,
        };

        assert!(
            !legal_actions_for_viewer(&state, PlayerId(0)).0.is_empty(),
            "MulliganDecision must offer Keep/Mulligan"
        );
        assert!(
            stuck_decision_diagnostic(&state).is_none(),
            "a normal MulliganDecision must not be flagged stuck"
        );
    }

    /// False-positive sweep (CR 601.2c): a normal `TargetSelection` step that
    /// presents at least one legal target offers a `ChooseTarget` action, so the
    /// detector must not fire. Exercises the resolution-time decision class (as
    /// opposed to the priority / mulligan classes covered above).
    #[test]
    fn stuck_diagnostic_none_on_normal_target_selection() {
        let mut state = setup_priority();
        // A creature on the battlefield to serve as the single legal target.
        let creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let target = crate::types::ability::TargetRef::Object(creature);
        set_dummy_pending_cast(&mut state);
        let pending_cast = state.pending_cast.clone().unwrap();
        state.waiting_for = WaitingFor::TargetSelection {
            player: PlayerId(0),
            pending_cast,
            target_slots: vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![target.clone()],
                optional: false,
            }],
            mode_labels: Vec::new(),
            selection: crate::types::game_state::TargetSelectionProgress {
                current_slot: 0,
                selected_slots: Vec::new(),
                current_legal_targets: vec![target],
            },
        };

        assert!(
            !legal_actions_for_viewer(&state, PlayerId(0)).0.is_empty(),
            "TargetSelection with a legal target must offer ChooseTarget"
        );
        assert!(
            stuck_decision_diagnostic(&state).is_none(),
            "a normal TargetSelection decision must not be flagged stuck"
        );
    }

    #[test]
    fn target_selection_legal_actions_do_not_simulate_each_target() {
        let mut state = setup_priority();
        let targets: Vec<TargetRef> = (0..25)
            .map(|i| {
                let creature = create_object(
                    &mut state,
                    CardId(100 + i),
                    PlayerId(0),
                    format!("Target {i}"),
                    Zone::Battlefield,
                );
                state
                    .objects
                    .get_mut(&creature)
                    .unwrap()
                    .card_types
                    .core_types
                    .push(CoreType::Creature);
                TargetRef::Object(creature)
            })
            .collect();

        set_dummy_pending_cast(&mut state);
        let pending_cast = state.pending_cast.clone().unwrap();
        state.waiting_for = WaitingFor::TargetSelection {
            player: PlayerId(0),
            pending_cast,
            target_slots: vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: targets.clone(),
                optional: true,
            }],
            mode_labels: Vec::new(),
            selection: crate::types::game_state::TargetSelectionProgress {
                current_slot: 0,
                selected_slots: Vec::new(),
                current_legal_targets: targets,
            },
        };

        crate::game::perf_counters::reset();
        let (actions, spell_costs, grouped) = legal_actions_full(&state);
        let clones = crate::game::perf_counters::snapshot().state_clone_for_legality;

        assert_eq!(clones, 0);
        assert_eq!(actions.len(), 26);
        assert!(spell_costs.is_empty());
        assert!(grouped.is_empty());
        assert!(actions
            .iter()
            .any(|action| matches!(action, GameAction::ChooseTarget { target: None })));
    }

    #[test]
    fn target_selection_legal_actions_do_not_fall_back_to_stale_slot_targets() {
        let mut state = setup_priority();
        let target = TargetRef::Object(create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Stale Target".to_string(),
            Zone::Battlefield,
        ));

        set_dummy_pending_cast(&mut state);
        let pending_cast = state.pending_cast.clone().unwrap();
        state.waiting_for = WaitingFor::TargetSelection {
            player: PlayerId(0),
            pending_cast,
            target_slots: vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![target],
                optional: true,
            }],
            mode_labels: Vec::new(),
            selection: crate::types::game_state::TargetSelectionProgress {
                current_slot: 0,
                selected_slots: Vec::new(),
                current_legal_targets: Vec::new(),
            },
        };

        crate::game::perf_counters::reset();
        let (actions, _spell_costs, _grouped) = legal_actions_full(&state);

        assert_eq!(
            crate::game::perf_counters::snapshot().state_clone_for_legality,
            0
        );
        assert_eq!(actions, vec![GameAction::ChooseTarget { target: None }]);
    }

    /// False-positive sweep (CR 103.5 / TL:R 906.6a): the simultaneous
    /// bottom-cards classes (`MulliganBottomCards`, `OpeningHandBottomCards`)
    /// always offer each pending player a `SelectCards` action, so the detector
    /// must not fire. Both share the `MulliganBottomEntry` shape and the
    /// `bottom_card_actions` generator, so one representative of each is covered
    /// here. The pending player is given enough hand cards to satisfy the owed
    /// bottom `count`.
    #[test]
    fn stuck_diagnostic_none_on_normal_bottom_cards() {
        use crate::types::game_state::{MulliganBottomEntry, OpeningHandBottomReason};

        for waiting_for in [
            WaitingFor::MulliganBottomCards {
                pending: vec![MulliganBottomEntry {
                    player: PlayerId(0),
                    count: 1,
                }],
            },
            WaitingFor::OpeningHandBottomCards {
                pending: vec![MulliganBottomEntry {
                    player: PlayerId(0),
                    count: 1,
                }],
                reason: OpeningHandBottomReason::TinyLeadersMultiCommander,
            },
        ] {
            let mut state = GameState::new_two_player(42);
            // Two cards in hand so the owed single-card bottom is satisfiable.
            for _ in 0..2 {
                create_object(
                    &mut state,
                    CardId(9),
                    PlayerId(0),
                    "Forest".to_string(),
                    Zone::Hand,
                );
            }
            state.waiting_for = waiting_for;

            assert!(
                !legal_actions_for_viewer(&state, PlayerId(0)).0.is_empty(),
                "{} must offer SelectCards",
                state.waiting_for.variant_name()
            );
            assert!(
                stuck_decision_diagnostic(&state).is_none(),
                "a normal {} decision must not be flagged stuck",
                state.waiting_for.variant_name()
            );
        }
    }

    /// Issue #3663 — Delve payment must not clone the full state once per
    /// graveyard card when validating legal actions.
    #[test]
    fn delve_payment_skips_state_clone_per_graveyard_candidate() {
        let mut state = setup_priority();
        for i in 0..25 {
            create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                format!("Graveyard Card {i}"),
                Zone::Graveyard,
            );
        }
        set_dummy_pending_cast(&mut state);
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: Some(ConvokeMode::Delve),
        };

        crate::game::perf_counters::reset();
        let candidates = validated_candidate_actions(&state);
        let delve_taps = candidates
            .iter()
            .filter(|c| matches!(c.action, GameAction::TapForConvoke { .. }))
            .count();
        assert!(
            delve_taps >= 25,
            "expected delve tap candidates for each graveyard card, got {delve_taps}"
        );
        let clones = crate::game::perf_counters::snapshot().state_clone_for_legality;
        assert!(
            clones < 5,
            "delve validation should not clone state per graveyard card (got {clones} clones)"
        );
    }
}
